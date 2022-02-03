use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use smol_timeout::TimeoutExt;

use xtra::prelude::*;
use xtra::spawn::Smol;
use xtra::KeepRunning;

#[derive(Clone, Debug, Eq, PartialEq)]
struct Accumulator(usize);

#[async_trait::async_trait]
impl Actor for Accumulator {
    type Stop = ();

    async fn stopped(self) -> Self::Stop {}
}

struct Inc;
impl Message for Inc {
    type Result = ();
}

struct Report;
impl Message for Report {
    type Result = Accumulator;
}

#[async_trait]
impl Handler<Inc> for Accumulator {
    async fn handle(&mut self, _: Inc, _ctx: &mut Context<Self>) {
        self.0 += 1;
    }
}

#[async_trait]
impl Handler<Report> for Accumulator {
    async fn handle(&mut self, _: Report, _ctx: &mut Context<Self>) -> Self {
        self.clone()
    }
}

#[smol_potat::test]
async fn accumulate_to_ten() {
    let addr = Accumulator(0).create(None).spawn(&mut Smol::Global);
    for _ in 0..10 {
        addr.do_send(Inc).unwrap();
    }

    assert_eq!(addr.send(Report).await.unwrap().0, 10);
}

struct DropTester(Arc<AtomicUsize>);

impl Drop for DropTester {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[async_trait]
impl Actor for DropTester {
    type Stop = ();

    async fn stopping(&mut self, _ctx: &mut Context<Self>) -> KeepRunning {
        self.0.fetch_add(1, Ordering::SeqCst);
        KeepRunning::StopAll
    }

    async fn stopped(self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

struct Stop;

impl Message for Stop {
    type Result = ();
}

#[async_trait]
impl Handler<Stop> for DropTester {
    async fn handle(&mut self, _: Stop, ctx: &mut Context<Self>) {
        ctx.stop();
    }
}

#[smol_potat::test]
async fn test_stop_and_drop() {
    // Drop the address
    let drop_count = Arc::new(AtomicUsize::new(0));
    let (addr, fut) = DropTester(drop_count.clone()).create(None).run();
    let handle = smol::spawn(fut);
    drop(addr);
    handle.await;
    assert_eq!(drop_count.load(Ordering::SeqCst), 2);

    // Send a stop message
    let drop_count = Arc::new(AtomicUsize::new(0));
    let (addr, fut) = DropTester(drop_count.clone()).create(None).run();
    let handle = smol::spawn(fut);
    addr.do_send(Stop).unwrap();
    handle.await;
    assert_eq!(drop_count.load(Ordering::SeqCst), 3);

    // Drop address before future has even begun
    let drop_count = Arc::new(AtomicUsize::new(0));
    let (addr, fut) = DropTester(drop_count.clone()).create(None).run();
    drop(addr);
    smol::spawn(fut).await;
    assert_eq!(drop_count.load(Ordering::SeqCst), 2);

    // Send a stop message before future has even begun
    let drop_count = Arc::new(AtomicUsize::new(0));
    let (addr, fut) = DropTester(drop_count.clone()).create(None).run();
    addr.do_send(Stop).unwrap();
    smol::spawn(fut).await;
    assert_eq!(drop_count.load(Ordering::SeqCst), 3);
}

struct StreamCancelMessage;

impl Message for StreamCancelMessage {
    type Result = KeepRunning;
}

struct StreamCancelTester;

#[async_trait]
impl Actor for StreamCancelTester {
    type Stop = ();

    async fn stopped(self) -> Self::Stop {}
}

#[async_trait]
impl Handler<StreamCancelMessage> for StreamCancelTester {
    async fn handle(&mut self, _: StreamCancelMessage, _: &mut Context<Self>) -> KeepRunning {
        KeepRunning::Yes
    }
}

#[smol_potat::test]
async fn test_stream_cancel_join() {
    let (_tx, rx) = flume::unbounded::<StreamCancelMessage>();
    let stream = rx.into_stream();
    let addr = StreamCancelTester {}.create(None).spawn(&mut Smol::Global);
    let jh = addr.join();
    let addr2 = addr.clone().downgrade();
    // attach a stream that blocks forever
    let handle = smol::spawn(async move { addr2.attach_stream(stream).await });
    drop(addr); // stop the actor

    // Attach stream should return immediately
    assert!(handle.timeout(Duration::from_secs(2)).await.is_some()); // None == timeout

    // Join should also return right away
    assert!(jh.timeout(Duration::from_secs(2)).await.is_some());
}
