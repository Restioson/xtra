use std::time::{Duration, Instant};

use xtra::prelude::*;
use xtra::spawn::Tokio;

struct Counter {
    count: usize,
}

impl Actor for Counter {}

struct Increment;

impl Message for Increment {
    type Result = ();
}

struct IncrementWithData(usize);

impl Message for IncrementWithData {
    type Result = ();
}

struct GetCount;

impl Message for GetCount {
    type Result = usize;
}

#[async_trait::async_trait]
impl Handler<Increment> for Counter {
    async fn handle(&mut self, _: Increment, _ctx: &mut Context<Self>) {
        self.count += 1;
    }
}

#[async_trait::async_trait]
impl Handler<IncrementWithData> for Counter {
    async fn handle(&mut self, _: IncrementWithData, _ctx: &mut Context<Self>) {
        self.count += 1;
    }
}

#[async_trait::async_trait]
impl Handler<GetCount> for Counter {
    async fn handle(&mut self, _: GetCount, _ctx: &mut Context<Self>) -> usize {
        let count = self.count;
        self.count = 0;
        count
    }
}

struct SendTimer {
    time: Duration,
}

impl Actor for SendTimer {}

struct GetTime;

impl Message for GetTime {
    type Result = Duration;
}

#[async_trait::async_trait]
impl Handler<GetTime> for SendTimer {
    async fn handle(&mut self, _time: GetTime, _ctx: &mut Context<Self>) -> Duration {
        self.time
    }
}

struct ReturnTimer;

impl Actor for ReturnTimer {}

struct TimeReturn;

impl Message for TimeReturn {
    type Result = Instant;
}

#[async_trait::async_trait]
impl Handler<TimeReturn> for ReturnTimer {
    async fn handle(&mut self, _time: TimeReturn, _ctx: &mut Context<Self>) -> Instant {
        Instant::now()
    }
}

const COUNT: usize = 50_000_000; // May take a while on some machines

async fn do_address_benchmark(name: &str, f: fn(&Address<Counter>) -> ()) {
    let addr = Counter { count: 0 }.create(None).spawn(&mut Tokio::Global);

    let start = Instant::now();

    // rounding overflow
    for _ in 0..COUNT {
        f(&addr);
    }

    // awaiting on GetCount will make sure all previous messages are processed first BUT introduces
    // future tokio reschedule time because of the .await
    let total_count = addr.send(GetCount).await.unwrap();

    let duration = Instant::now() - start;
    let average_ns = duration.as_nanos() / COUNT as u128; // <120-170ns on my machine
    println!("{} avg time of processing: {}ns", name, average_ns);
    assert_eq!(total_count, COUNT, "total_count should equal COUNT!");
}

async fn do_parallel_address_benchmark(name: &str, workers: usize, f: fn(&Address<Counter>) -> ()) {
    let (addr, mut ctx) = Context::new(None);
    let start = Instant::now();
    for _ in 0..workers {
        tokio::spawn(ctx.attach(Counter { count: 0 }));
    }

    for _ in 0..COUNT {
        f(&addr);
    }

    // awaiting on GetCount will make sure all previous messages are processed first BUT introduces
    // future tokio reschedule time because of the .await
    let _ = addr.send(GetCount).await.unwrap();

    let duration = Instant::now() - start;
    let average_ns = duration.as_nanos() / COUNT as u128; // <120-170ns on my machine
    println!("{} avg time of processing: {}ns", name, average_ns);
}

async fn do_channel_benchmark<M: Message, F: Fn(&dyn MessageChannel<M>)>(name: &str, f: F)
where
    Counter: Handler<M>,
{
    let addr = Counter { count: 0 }.create(None).spawn(&mut Tokio::Global);
    let chan = &addr as &dyn MessageChannel<M>;

    let start = Instant::now();
    for _ in 0..COUNT {
        f(chan);
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
    do_address_benchmark("address do_send (ZST message)", |addr| {
        let _ = addr.do_send(Increment);
    })
    .await;

    do_address_benchmark("address do_send (8-byte message)", |addr| {
        let _ = addr.do_send(IncrementWithData(0));
    })
    .await;

    do_parallel_address_benchmark("address do_send 2 workers (ZST message)", 2, |addr| {
        let _ = addr.do_send(Increment);
    })
    .await;

    do_parallel_address_benchmark("address do_send 2 workers (8-byte message)", 2, |addr| {
        let _ = addr.do_send(IncrementWithData(0));
    })
    .await;

    do_channel_benchmark("channel do_send (ZST message)", |chan| {
        let _ = chan.do_send(Increment);
    })
    .await;

    do_channel_benchmark("channel do_send (8-byte message)", |chan| {
        let _ = chan.do_send(IncrementWithData(0));
    })
    .await;

    do_channel_benchmark("channel send (ZST message)", |chan| {
        let _ = chan.send(Increment);
    })
    .await;

    do_channel_benchmark("channel send (8-byte message)", |chan| {
        let _ = chan.send(IncrementWithData(0));
    })
    .await;
}
