use criterion::{criterion_group, criterion_main, Criterion, Throughput, BatchSize};
use xtra::{Actor, Address, Context, Handler, Message};

struct Counter(u64);

impl Actor for Counter {}

struct IncrementZst;
impl Message for IncrementZst {
    type Result = ();
}

struct Finish;
impl Message for Finish {
    type Result = u64;
}

#[async_trait::async_trait]
impl Handler<IncrementZst> for Counter {
    async fn handle(&mut self, _: IncrementZst, _ctx: &mut Context<Self>) {
        self.0 += 1;
    }
}

#[async_trait::async_trait]
impl Handler<Finish> for Counter {
    async fn handle(&mut self, _: Finish, _ctx: &mut Context<Self>) -> u64 {
        self.0
    }
}

fn throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("Throughput");
    group.throughput(Throughput::Elements(100));
    group.bench_function("inc 100", |b| {
        let setup = || {
            let (addr, ctx) = Counter(0).create(Some(100)).run();
            for _ in 0..100 {
                addr.do_send(IncrementZst).unwrap();
            }

            (addr, ctx)
        };

        let iter = |(addr, fut): (Address<_>, _)| {
            let _g = smol::spawn(fut);
            pollster::block_on(addr.send(Finish)).unwrap();
        };

        b.iter_batched(setup, iter, BatchSize::SmallInput);
    });
}

criterion_group!(benches, throughput);
criterion_main!(benches);
