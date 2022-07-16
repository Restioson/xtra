use std::future::Future;
use std::time::{Duration, Instgant};

use futures_util::FutureExt;
use xtra::prelude::*;
use xtra::spawn::Tokio;
use xtra::SendFuture;
use xtra::{ActorErasedSending, NameableSending};

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

async fn do_address_benchmark<R>(
    name: &str,
    f: impl Fn(&Address<Counter>) -> SendFuture<(), NameableSending<Counter, ()>, R>,
) where
    SendFuture<(), NameableSending<Counter, ()>, R>: Future,
{
    let addr = Counter { count: 0 }.create(None).spawn(&mut Tokio::Global);

    let start = Instant::now();

    // rounding overflow
    for _ in 0..COUNT {
        let _ = f(&addr).now_or_never();
    }

    // awaiting on GetCount will make sure all previous messages are processed first BUT introduces
    // future tokio reschedule time because of the .await
    let total_count = addr.send(GetCount).await.unwrap();

    let duration = Instant::now() - start;
    let average_ns = duration.as_nanos() / COUNT as u128; // <120-170ns on my machine
    println!("{} avg time of processing: {}ns", name, average_ns);
    assert_eq!(total_count, COUNT, "total_count should equal COUNT!");
}

async fn do_parallel_address_benchmark<R>(
    name: &str,
    workers: usize,
    f: impl Fn(&Address<Counter>) -> SendFuture<(), NameableSending<Counter, ()>, R>,
) where
    SendFuture<(), NameableSending<Counter, ()>, R>: Future,
{
    let (addr, ctx) = Context::new(None);
    let start = Instant::now();
    for _ in 0..workers {
        tokio::spawn(xtra::run(ctx.clone(), Counter { count: 0 }));
    }

    for _ in 0..COUNT {
        let _ = f(&addr).await;
    }

    // awaiting on GetCount will make sure all previous messages are processed first BUT introduces
    // future tokio reschedule time because of the .await
    let _ = addr.send(GetCount).await.unwrap();

    let duration = Instant::now() - start;
    let average_ns = duration.as_nanos() / COUNT as u128; // <120-170ns on my machine
    println!("{} avg time of processing: {}ns", name, average_ns);
}

async fn do_channel_benchmark<M, RM>(
    name: &str,
    f: impl Fn(&MessageChannel<M, ()>) -> SendFuture<(), ActorErasedSending<()>, RM>,
) where
    Counter: Handler<M, Return = ()> + Send,
    M: Send + 'static,
    SendFuture<(), ActorErasedSending<()>, RM>: Future,
{
    let addr = Counter { count: 0 }.create(None).spawn(&mut Tokio::Global);
    let chan = MessageChannel::new(addr.clone());

    let start = Instant::now();
    for _ in 0..COUNT {
        let _ = f(&chan).await;
    }

    // awaiting on GetCount will make sure all previous messages are processed first BUT introduces
    // future tokio reschedule time because of the .await
    let total_count = addr.send::<GetCount>(GetCount).await.unwrap();

    let duration = Instant::now() - start;
    let average_ns = duration.as_nanos() / total_count as u128; // <120-170ns on my machine
    println!("{} avg time of processing: {}ns", name, average_ns);
    assert_eq!(total_count, COUNT, "total_count should equal COUNT!");
}

#[tokio::main]
async fn main() {
    do_address_benchmark("address split_receiver (ZST message)", |addr| {
        addr.send(Increment).split_receiver()
    })
    .await;

    do_address_benchmark("address split_receiver (8-byte message)", |addr| {
        addr.send(IncrementWithData(0)).split_receiver()
    })
    .await;

    do_parallel_address_benchmark(
        "address split_receiver 2 workers (ZST message)",
        2,
        |addr| addr.send(Increment).split_receiver(),
    )
    .await;

    do_parallel_address_benchmark(
        "address split_receiver 2 workers (8-byte message)",
        2,
        |addr| addr.send(IncrementWithData(0)).split_receiver(),
    )
    .await;

    do_channel_benchmark("channel split_receiver (ZST message)", |chan| {
        chan.send(Increment).split_receiver()
    })
    .await;

    do_channel_benchmark("channel split_receiver (8-byte message)", |chan| {
        chan.send(IncrementWithData(0)).split_receiver()
    })
    .await;

    do_channel_benchmark("channel send (ZST message)", |chan| chan.send(Increment)).await;

    do_channel_benchmark("channel send (8-byte message)", |chan| {
        chan.send(IncrementWithData(0))
    })
    .await;
}
