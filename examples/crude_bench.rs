#![feature(generic_associated_types, type_alias_impl_trait)]
#![feature(asm)]

use futures::Future;
use std::time::{Instant, Duration};
use xtra::{Actor, AsyncHandler, Context, Handler, Message};

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

impl Handler<Increment> for Counter {
    fn handle(&mut self, _: Increment, _ctx: &mut Context<Self>) {
        self.count += 1;
    }
}

impl AsyncHandler<Increment> for Counter {
    type Responder<'a> = impl Future<Output = ()> + 'a;

    fn handle(&mut self, _: Increment, _ctx: &mut Context<Self>) -> Self::Responder<'_> {
        self.count += 1;
        async {} // Slower if you put count in here and make it async move (compiler optimisations?)
    }
}

impl Handler<GetCount> for Counter {
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

impl Handler<TimeSend> for SendTimer {
    fn handle(&mut self, time: TimeSend, _ctx: &mut Context<Self>) {
        self.time += time.0.elapsed();
    }
}

struct GetTime;

impl Message for GetTime {
    type Result = Duration;
}

impl Handler<GetTime> for SendTimer {
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

impl Handler<TimeReturn> for ReturnTimer {
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
        addr.do_send(Increment);
    }

    // awaiting on GetCount will make sure all previous messages are processed first BUT introduces
    // future tokio reschedule time because of the .await
    let total_count = addr.send(GetCount).await.unwrap();

    let duration = Instant::now() - start;
    let average_ns = duration.as_nanos() / total_count as u128; // ~150ns on my machine
    println!("do_send avg time of processing: {}ns", average_ns);



    /* Time do_send_async */

    let addr = Counter { count: 0 }.spawn();

    let start = Instant::now();
    for _ in 0..COUNT {
        addr.do_send_async(Increment);
    }

    // awaiting on GetCount will make sure all previous messages are processed first BUT introduces
    // future tokio reschedule time because of the .await
    let total_count = addr.send(GetCount).await.unwrap();

    let duration = Instant::now() - start;
    let average_ns = duration.as_nanos() / total_count as u128; // ~170ns on my machine
    println!("do_send_async avg time of processing: {}ns", average_ns);



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
    let average_ns = duration.as_nanos() / total_count as u128; // ~350ns on my machine
    println!("send avg time of processing: {}ns", average_ns);





    /* Time send latency */

    let addr = SendTimer { time: Duration::new(0, 0) }.spawn();

    for _ in 0..COUNT {
        let _ = addr.send(TimeSend(Instant::now())).await;
    }

    let duration = addr.send(GetTime).await.unwrap();
    let average_ns = duration.as_nanos() / total_count as u128;

    println!("send_await avg time to processing: {}ns", average_ns);




    /* Time return latency */

    let addr = ReturnTimer.spawn();
    let mut duration = Duration::new(0, 0);

    for _ in 0..COUNT {
        let d = addr.send(TimeReturn).await.unwrap();
        duration += d.elapsed();
    }

    let average_ns = duration.as_nanos() / total_count as u128;

    println!("send_await avg time to respond after processing: {}ns", average_ns);

    /* Time send.await avg time of processing
     *
     * This particular benchmark includes the time it takes for tokio to reschedule the future, so
     * it is ridiculously slower than the `send avg time of processing` benchmark.
     */

    const COUNT2: usize = 1_000_000;
    let addr = Counter { count: 0 }.spawn();

    let start = Instant::now();
    for _ in 0..COUNT2 {
        let _ = addr.send(Increment).await;
    }

    // awaiting on GetCount will make sure all previous messages are processed first
    let total_count = addr.send(GetCount).await.unwrap();

    let duration = Instant::now() - start;
    let average_ns = duration.as_nanos() / total_count as u128; // ~1microsecond on my computer
    println!("send_await avg time of processing: {}ns", average_ns);
}
