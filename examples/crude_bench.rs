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

struct TimeSend(Instant);

impl Message for TimeSend {
    type Result = ();
}

#[async_trait::async_trait]
impl Handler<TimeSend> for SendTimer {
    async fn handle(&mut self, time: TimeSend, _ctx: &mut Context<Self>) {
        self.time += time.0.elapsed();
    }
}

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

async fn do_channel_benchmark<F: Fn(&dyn MessageChannel<Increment>)>(name: &str, f: F) {
    let addr = Counter { count: 0 }.create(None).spawn(&mut Tokio::Global);
    let chan = &addr as &dyn MessageChannel<Increment>;

    let start = Instant::now();
    for _ in 0..COUNT {
        f(chan);
    }

    // awaiting on GetCount will make sure all previous messages are processed first BUT introduces
    // future tokio reschedule time because of the .await
    let total_count = addr.send(GetCount).await.unwrap();

    let duration = Instant::now() - start;
    let average_ns = duration.as_nanos() / total_count as u128; // <120-170ns on my machine
    println!("{} avg time of processing: {}ns", name, average_ns);
    assert_eq!(total_count, COUNT, "total_count should equal COUNT!");
}


#[tokio::main]
async fn main() {
    do_address_benchmark("address do_send", |addr| {
        let _ = addr.do_send(Increment);
    }).await;

    do_parallel_address_benchmark("address do_send 2 workers", 2, |addr| {
        let _ = addr.do_send(Increment);
    }).await;

    do_channel_benchmark("channel do_send", |chan| {
        let _ = chan.do_send(Increment);
    }).await;

    do_channel_benchmark("channel send", |chan| {
        let _ = chan.send(Increment);
    }).await;
}
