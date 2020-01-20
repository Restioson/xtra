#![feature(generic_associated_types, type_alias_impl_trait)]

use futures::Future;
use std::time::Instant;
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
    let average_ns = duration.as_nanos() / total_count as u128; // ~180ns on my machine (-5%)
    println!("do_send avg time: {}ns", average_ns);

    let addr = Counter { count: 0 }.spawn();

    let start = Instant::now();
    for _ in 0..COUNT {
        let _ = addr.do_send_async(Increment);
    }

    // awaiting on GetCount will make sure all previous messages are processed first
    let total_count = addr.send(GetCount).await.unwrap();

    let duration = Instant::now() - start;
    let average_ns = duration.as_nanos() / total_count as u128; // ~190ns on my machine (+5%)
    println!("do_send_async avg time: {}ns", average_ns);
}
