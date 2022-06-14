use std::time::{Duration, Instant};

use futures_core::future::BoxFuture;
use std::future::Future;
use xtra::prelude::*;
use xtra::spawn::Tokio;
use xtra::{Disconnected, NameableSending};
use xtra::Receiver;
use xtra::SendFuture;

struct Counter {
    count: usize,
}

#[async_trait]
impl Actor for Counter {
    type Stop = ();

    async fn stopped(self) -> Self::Stop {}
}

struct Increment;
struct IncrementWithData(usize);
struct GetCount;

#[async_trait]
impl Handler<Increment> for Counter {
    type Return = ();

    async fn handle(&mut self, _: Increment, _ctx: &mut Context<Self>) {
        self.count += 1;
    }
}

#[async_trait]
impl Handler<IncrementWithData> for Counter {
    type Return = ();

    async fn handle(&mut self, _: IncrementWithData, _ctx: &mut Context<Self>) {
        self.count += 1;
    }
}

#[async_trait]
impl Handler<GetCount> for Counter {
    type Return = usize;

    async fn handle(&mut self, _: GetCount, _ctx: &mut Context<Self>) -> usize {
        println!("Handling GetCount with count = {}", self.count);
        let count = self.count;
        self.count = 0;
        count
    }
}

struct SendTimer {
    time: Duration,
}

#[async_trait]
impl Actor for SendTimer {
    type Stop = ();

    async fn stopped(self) -> Self::Stop {}
}

struct GetTime;

#[async_trait]
impl Handler<GetTime> for SendTimer {
    type Return = Duration;

    async fn handle(&mut self, _time: GetTime, _ctx: &mut Context<Self>) -> Duration {
        self.time
    }
}

struct ReturnTimer;

#[async_trait]
impl Actor for ReturnTimer {
    type Stop = ();

    async fn stopped(self) -> Self::Stop {}
}

struct TimeReturn;

#[async_trait]
impl Handler<TimeReturn> for ReturnTimer {
    type Return = Instant;

    async fn handle(&mut self, _time: TimeReturn, _ctx: &mut Context<Self>) -> Instant {
        Instant::now()
    }
}

const COUNT: usize = 10_000_000; // May take a while on some machines

fn do_address_benchmark(
    name: &str,
    f: impl Fn(&Address<Counter>) -> Result<(), Disconnected>,
) where
{
    let addr = Counter { count: 0 }.create(None).spawn(&mut Tokio::Global);

    let start = Instant::now();

    // rounding overflow
    for _ in 0..COUNT {
        let _ = f(&addr);
    }

    println!("Time to send avg: {}ns", start.elapsed().as_nanos() / COUNT as u128);

    // awaiting on GetCount will make sure all previous messages are processed first BUT introduces
    // future tokio reschedule time because of the .await
    let total_count = pollster::block_on(addr.send(GetCount)).unwrap();

    let average_ns = start.elapsed().as_nanos() / COUNT as u128; // <120-170ns on my machine
    println!("{} avg time of processing: {}ns", name, average_ns);
    assert_eq!(total_count, COUNT, "total_count should equal COUNT!");
}

fn main() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _g = rt.enter();

    do_address_benchmark("address split_receiver (ZST message)", |addr| {
        addr.do_send(Increment)//.split_receiver()
    });

    // do_address_benchmark("address split_receiver (8-byte message)", |addr| {
    //     addr.do_send(IncrementWithData(0))//.split_receiver()
    // })
    // .await;
    //
    // do_parallel_address_benchmark(
    //     "address split_receiver 2 workers (ZST message)",
    //     2,
    //     |addr| addr.do_send(Increment)//.split_receiver(),
    // )
    // .await;
    //
    // do_parallel_address_benchmark(
    //     "address split_receiver 2 workers (8-byte message)",
    //     2,
    //     |addr| addr.do_send(IncrementWithData(0))//.split_receiver(),
    // )
    // .await;

    // do_channel_benchmark("channel split_receiver (ZST message)", |chan| {
    //     chan.do_send(Increment)//.split_receiver()
    // })
    // .await;
    //
    // do_channel_benchmark("channel split_receiver (8-byte message)", |chan| {
    //     chan.do_send(IncrementWithData(0))//.split_receiver()
    // })
    // .await;
    //
    // do_channel_benchmark("channel send (ZST message)", |chan| chan.send(Increment)).await;
    //
    // do_channel_benchmark("channel send (8-byte message)", |chan| {
    //     chan.do_send(IncrementWithData(0))
    // })
    // .await;
}
