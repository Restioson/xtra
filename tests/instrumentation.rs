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

use std::sync::{Arc, Mutex, MutexGuard};
use std::{fmt, io};

use tracing::{Dispatch, Instrument};
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::FmtSubscriber;
use xtra::prelude::*;

#[tokio::test]
async fn assert_send_is_child_of_span() {
    let (subscriber, buf) = get_subscriber("instrumentation=trace,xtra=trace");
    let _g = tracing::dispatcher::set_default(&subscriber);

    let addr = xtra::spawn_tokio(Tracer, Mailbox::unbounded());
    let _ = addr
        .send(Hello("world"))
        .instrument(tracing::info_span!("user_span"))
        .await;

    assert_eq!(
        buf,
        [" INFO user_span:xtra_actor_request\
                {actor_type=instrumentation::Tracer message_type=instrumentation::Hello}:\
                xtra_message_handler: instrumentation: Hello world"]
    );
}

#[tokio::test]
async fn assert_handler_span_is_child_of_caller_span_with_min_level_info() {
    let (subscriber, buf) = get_subscriber("instrumentation=info,xtra=info");
    let _g = tracing::dispatcher::set_default(&subscriber);

    let addr = xtra::spawn_tokio(Tracer, Mailbox::unbounded());
    let _ = addr
        .send(CreateInfoSpan)
        .instrument(tracing::info_span!("sender_span"))
        .await;

    assert_eq!(buf, [" INFO sender_span:info_span: instrumentation: Test!"]);
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

struct CreateInfoSpan;

#[async_trait]
impl Handler<CreateInfoSpan> for Tracer {
    type Return = ();

    async fn handle(&mut self, _msg: CreateInfoSpan, _ctx: &mut Context<Self>) {
        tracing::info_span!("info_span").in_scope(|| tracing::info!("Test!"));
    }
}

#[derive(Debug, Default)]
struct BufferWriter {
    buf: Buffer,
}

#[derive(Default, Clone)]
struct Buffer(Arc<Mutex<Vec<u8>>>);

impl Buffer {
    fn as_str(&self) -> String {
        let buf = self.0.lock().unwrap().clone();

        String::from_utf8(buf).expect("Logs contain invalid UTF8")
    }
}

impl<const N: usize> PartialEq<[&str; N]> for Buffer {
    fn eq(&self, other: &[&str; N]) -> bool {
        self.as_str().lines().collect::<Vec<_>>().eq(other)
    }
}

impl fmt::Debug for Buffer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_str().lines().collect::<Vec<_>>().fmt(f)
    }
}

impl BufferWriter {
    /// Give access to the internal buffer (behind a `MutexGuard`).
    fn buf(&self) -> io::Result<MutexGuard<Vec<u8>>> {
        // Note: The `lock` will block. This would be a problem in production code,
        // but is fine in tests.
        self.buf
            .0
            .lock()
            .map_err(|_| io::Error::from(io::ErrorKind::Other))
    }
}

impl io::Write for BufferWriter {
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

impl MakeWriter<'_> for BufferWriter {
    type Writer = Self;

    fn make_writer(&self) -> Self::Writer {
        BufferWriter {
            buf: self.buf.clone(),
        }
    }
}

/// Return a new subscriber with the [`Buffer`] that all logs will be written to.
fn get_subscriber(env_filter: &str) -> (Dispatch, Buffer) {
    let writer = BufferWriter::default();
    let buffer = writer.buf.clone();

    let dispatch = FmtSubscriber::builder()
        .with_env_filter(env_filter)
        .with_writer(writer)
        .with_level(true)
        .with_ansi(false)
        .without_time()
        .into();

    (dispatch, buffer)
}
