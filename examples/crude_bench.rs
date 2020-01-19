use xtra::{Actor, Context, Handler, Message};
use std::time::Instant;

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
    type Responder = ();

    fn handle(&mut self, _: Increment, _ctx: &mut Context<Self>) {
        self.count += 1;
    }
}

impl Handler<GetCount> for Counter {
    type Responder = usize;

    fn handle(&mut self, _: GetCount, _ctx: &mut Context<Self>) -> usize {
        self.count
    }
}

#[tokio::main]
async fn main() {
    const COUNT: usize = 50_000_000; // May take a while on some machines

    let addr = Counter { count: 0 }.spawn();

    let start = Instant::now();
    for _ in 0..COUNT {
        let _ = addr.do_send(Increment);
    }

    // awaiting on GetCount will make sure all previous messages are processed first
    let total_count = addr.send(GetCount).await.unwrap();

    let duration = Instant::now() - start;

    let average_ns = duration.as_nanos() / total_count as u128; // <200ns on my machine

    println!("avg time: {}ns", average_ns);
}
