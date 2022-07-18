//! Much of this code is taken from https://github.com/dbrgn/tracing-test//
//!
//!
//! # tracing_test license
//!
//! Copyright (C) 2020-2022 Threema GmbH, Danilo Bargen
//!
//! Permission is hereby granted, free of charge, to any person obtaining a copy of
//! this software and associated documentation files (the "Software"), to deal in
//! the Software without restriction, including without limitation the rights to
//! use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies
//! of the Software, and to permit persons to whom the Software is furnished to do
//! so, subject to the following conditions:
//!
//! The above copyright notice and this permission notice shall be included in all
//! copies or substantial portions of the Software.
//!
//! THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
//! IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
//! FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
//! AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
//! LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
//! OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
//! SOFTWARE.

use std::io;
use std::sync::{Arc, Mutex, MutexGuard};

use tracing::{Dispatch, Instrument};
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::FmtSubscriber;
use xtra::prelude::*;
use xtra::spawn::TokioGlobalSpawnExt;

#[derive(Debug)]
pub struct MockWriter {
    buf: Arc<Mutex<Vec<u8>>>,
}

impl MockWriter {
    /// Create a new `MockWriter` that writes into the specified buffer (behind a mutex).
    pub fn new(buf: Arc<Mutex<Vec<u8>>>) -> Self {
        Self { buf }
    }

    /// Give access to the internal buffer (behind a `MutexGuard`).
    fn buf(&self) -> io::Result<MutexGuard<Vec<u8>>> {
        // Note: The `lock` will block. This would be a problem in production code,
        // but is fine in tests.
        self.buf
            .lock()
            .map_err(|_| io::Error::from(io::ErrorKind::Other))
    }
}

impl io::Write for MockWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // Lock target buffer
        let mut target = self.buf()?;

        // Write to stdout in order to show up in tests
        print!("{}", String::from_utf8(buf.to_vec()).unwrap());

        // Write to buffer
        target.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.buf()?.flush()
    }
}

impl MakeWriter<'_> for MockWriter {
    type Writer = Self;

    fn make_writer(&self) -> Self::Writer {
        MockWriter::new(self.buf.clone())
    }
}

/// Return a new subscriber that writes to the specified [`MockWriter`].
///
/// [`MockWriter`]: struct.MockWriter.html
pub fn get_subscriber(mock_writer: MockWriter, env_filter: &str) -> Dispatch {
    FmtSubscriber::builder()
        .with_env_filter(env_filter)
        .with_writer(mock_writer)
        .with_level(true)
        .with_ansi(false)
        .without_time()
        .into()
}

pub fn with_logs<F>(buf: &Mutex<Vec<u8>>, f: F)
where
    F: Fn(&[&str]),
{
    let buf = buf.lock().unwrap();
    let logs: Vec<&str> = std::str::from_utf8(&buf)
        .expect("Logs contain invalid UTF8")
        .lines()
        .collect();
    f(&logs)
}

struct Tracer;

#[async_trait]
impl Actor for Tracer {
    type Stop = ();
    async fn stopped(self) {}
}

struct Hello(&'static str);

#[async_trait]
impl Handler<Hello> for Tracer {
    type Return = ();

    async fn handle(&mut self, message: Hello, _ctx: &mut Context<Self>) {
        tracing::info!("Hello {}", message.0)
    }
}

#[tokio::test]
async fn assert_send_is_child_of_span() {
    let buf = Arc::new(Mutex::new(vec![]));
    let mock_writer = MockWriter::new(buf.clone());
    let subscriber = get_subscriber(mock_writer, "instrumentation=trace,xtra=trace");
    let _g = tracing::dispatcher::set_default(&subscriber);

    let addr = Tracer.create(None).spawn_global();
    let _ = addr
        .send(Hello("world"))
        .instrument(tracing::info_span!("user_span"))
        .await;

    with_logs(&buf, |lines: &[&str]| {
        assert_eq!(
            lines,
            [
                " INFO user_span:xtra_actor_request\
                {actor=instrumentation::Tracer message_type=instrumentation::Hello}:\
                xtra_message_handler\
                {actor=instrumentation::Tracer message_type=instrumentation::Hello}: \
                instrumentation: Hello world"
            ]
        );
    });
}
