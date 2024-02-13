use std::future::Future;
use std::time::{Duration, Instant};

use futures_util::FutureExt;
use xtra::prelude::*;
use xtra::refcount::Strong;
use xtra::{ActorErasedSending, ActorNamedSending, SendFuture};

#[derive(xtra::Actor)]
struct Counter {
    count: usize,
}

struct Increment;
#[allow(dead_code)]
struct IncrementWithData(usize);
struct GetCount;

impl Handler<Increment> for Counter {
    type Return = ();

    async fn handle(&mut self, _: Increment, _ctx: &mut Context<Self>) {
        self.count += 1;
    }
}

impl Handler<IncrementWithData> for Counter {
    type Return = ();

    async fn handle(&mut self, _: IncrementWithData, _ctx: &mut Context<Self>) {
        self.count += 1;
    }
}

impl Handler<GetCount> for Counter {
    type Return = usize;

    async fn handle(&mut self, _: GetCount, _ctx: &mut Context<Self>) -> usize {
        let count = self.count;
        self.count = 0;
        count
    }
}

#[derive(xtra::Actor)]
struct SendTimer {
    time: Duration,
}

struct GetTime;

impl Handler<GetTime> for SendTimer {
    type Return = Duration;

    async fn handle(&mut self, _time: GetTime, _ctx: &mut Context<Self>) -> Duration {
        self.time
    }
}

#[derive(xtra::Actor)]
struct ReturnTimer;

struct TimeReturn;

impl Handler<TimeReturn> for ReturnTimer {
    type Return = Instant;

    async fn handle(&mut self, _time: TimeReturn, _ctx: &mut Context<Self>) -> Instant {
        Instant::now()
    }
}

const COUNT: usize = 10_000_000; // May take a while on some machines

async fn do_address_benchmark<R>(
    name: &str,
    f: impl Fn(&Address<Counter>) -> SendFuture<ActorNamedSending<Counter, Strong>, R>,
) where
    SendFuture<ActorNamedSending<Counter, Strong>, R>: Future,
{
    let addr = xtra::spawn_tokio(Counter { count: 0 }, Mailbox::unbounded());

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
    f: impl Fn(&Address<Counter>) -> SendFuture<ActorNamedSending<Counter, Strong>, R>,
) where
    SendFuture<ActorNamedSending<Counter, Strong>, R>: Future,
{
    let (addr, mailbox) = Mailbox::unbounded();
    let start = Instant::now();
    for _ in 0..workers {
        tokio::spawn(xtra::run(mailbox.clone(), Counter { count: 0 }));
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
    f: impl Fn(&MessageChannel<M, ()>) -> SendFuture<ActorErasedSending, RM>,
) where
    Counter: Handler<M, Return = ()> + Send,
    M: Send + 'static,
    SendFuture<ActorErasedSending, RM>: Future,
{
    let addr = xtra::spawn_tokio(Counter { count: 0 }, Mailbox::unbounded());
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
    do_address_benchmark("address detach (ZST message)", |addr| {
        addr.send(Increment).detach()
    })
    .await;

    do_address_benchmark("address detach (8-byte message)", |addr| {
        addr.send(IncrementWithData(0)).detach()
    })
    .await;

    do_parallel_address_benchmark("address detach 2 workers (ZST message)", 2, |addr| {
        addr.send(Increment).detach()
    })
    .await;

    do_parallel_address_benchmark("address detach 2 workers (8-byte message)", 2, |addr| {
        addr.send(IncrementWithData(0)).detach()
    })
    .await;

    do_channel_benchmark("channel detach (ZST message)", |chan| {
        chan.send(Increment).detach()
    })
    .await;

    do_channel_benchmark("channel detach (8-byte message)", |chan| {
        chan.send(IncrementWithData(0)).detach()
    })
    .await;

    do_channel_benchmark("channel send (ZST message)", |chan| chan.send(Increment)).await;

    do_channel_benchmark("channel send (8-byte message)", |chan| {
        chan.send(IncrementWithData(0))
    })
    .await;
}
