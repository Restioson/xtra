use futures::Future;
use std::time::{Duration, Instant};
use xtra::prelude::*;

struct Counter {
    count: usize,
}

impl Actor for Counter {}

struct Increment;

impl Message for Increment {
    type Result = ();
}

struct IncrementAsync;

impl Message for IncrementAsync {
    type Result = ();
}

struct GetCount;

impl Message for GetCount {
    type Result = usize;
}

impl SyncHandler<Increment> for Counter {
    fn handle(&mut self, _: Increment, _ctx: &mut Context<Self>) {
        self.count += 1;
    }
}

#[async_trait::async_trait]
impl Handler<IncrementAsync> for Counter {
    async fn handle(&mut self, _: IncrementAsync, _ctx: &mut Context<Self>) {
        self.count += 1;
    }
}

impl SyncHandler<GetCount> for Counter {
    fn handle(&mut self, _: GetCount, _ctx: &mut Context<Self>) -> usize {
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

impl SyncHandler<TimeSend> for SendTimer {
    fn handle(&mut self, time: TimeSend, _ctx: &mut Context<Self>) {
        self.time += time.0.elapsed();
    }
}

struct GetTime;

impl Message for GetTime {
    type Result = Duration;
}

impl SyncHandler<GetTime> for SendTimer {
    fn handle(&mut self, _time: GetTime, _ctx: &mut Context<Self>) -> Duration {
        self.time
    }
}

struct ReturnTimer;

impl Actor for ReturnTimer {}

struct TimeReturn;

impl Message for TimeReturn {
    type Result = Instant;
}

impl SyncHandler<TimeReturn> for ReturnTimer {
    fn handle(&mut self, _time: TimeReturn, _ctx: &mut Context<Self>) -> Instant {
        Instant::now()
    }
}

#[tokio::main]
async fn main() {
    const COUNT: usize = 50_000_000; // May take a while on some machines

    /* Time do_send */

    let addr = Counter { count: 0 }.spawn();

    let start = Instant::now();
    for _ in 0..COUNT {
        let _ = addr.do_send(Increment);
    }

    // awaiting on GetCount will make sure all previous messages are processed first BUT introduces
    // future tokio reschedule time because of the .await
    let total_count = addr.send(GetCount).await.unwrap();

    let duration = Instant::now() - start;
    let average_ns = duration.as_nanos() / total_count as u128; // <150ns on my machine
    println!("do_send avg time of processing: {}ns", average_ns);
    assert_eq!(total_count, COUNT, "total_count should equal COUNT!");

    /* Time do_send async */

    let addr = Counter { count: 0 }.spawn();

    let start = Instant::now();
    for _ in 0..COUNT {
        let _ = addr.do_send(IncrementAsync);
    }

    // awaiting on GetCount will make sure all previous messages are processed first BUT introduces
    // future tokio reschedule time because of the .await
    let total_count = addr.send(GetCount).await.unwrap();

    let duration = Instant::now() - start;
    let average_ns = duration.as_nanos() / total_count as u128; // <170ns on my machine
    println!("do_send async avg time of processing: {}ns", average_ns);
    assert_eq!(total_count, COUNT, "total_count should equal COUNT!");

    /* Time channel do_send */

    let addr = Counter { count: 0 }.spawn();
    let chan = addr.channel();

    let start = Instant::now();
    for _ in 0..COUNT {
        let _ = chan.do_send(Increment);
    }

    // awaiting on GetCount will make sure all previous messages are processed first BUT introduces
    // future tokio reschedule time because of the .await
    let total_count = addr.send(GetCount).await.unwrap();

    let duration = Instant::now() - start;
    let average_ns = duration.as_nanos() / total_count as u128; // <170ns on my machine
    println!("channel do_send avg time of processing: {}ns", average_ns);
    assert_eq!(total_count, COUNT, "total_count should equal COUNT!");

    /* Time channel do_send async */

    let addr = Counter { count: 0 }.spawn();
    let chan = addr.channel();

    let start = Instant::now();
    for _ in 0..COUNT {
        let _ = chan.do_send(IncrementAsync);
    }

    // awaiting on GetCount will make sure all previous messages are processed first BUT introduces
    // future tokio reschedule time because of the .await
    let total_count = addr.send(GetCount).await.unwrap();

    let duration = Instant::now() - start;
    let average_ns = duration.as_nanos() / total_count as u128; // <170ns on my machine
    println!(
        "channel do_send async avg time of processing: {}ns",
        average_ns
    );
    assert_eq!(total_count, COUNT, "total_count should equal COUNT!");

    /* Time send avg time of processing */

    let addr = Counter { count: 0 }.spawn();

    let start = Instant::now();
    for _ in 0..COUNT {
        let _ = addr.send(Increment);
    }

    // awaiting on GetCount will make sure all previous messages are processed first BUT introduces
    // future tokio reschedule time because of the .await
    let total_count = addr.send(GetCount).await.unwrap();

    let duration = Instant::now() - start;
    let average_ns = duration.as_nanos() / total_count as u128; // 350~300ns on my machine
    println!("send avg time of processing: {}ns", average_ns);
    assert_eq!(total_count, COUNT, "total_count should equal COUNT!");
}
