use criterion::async_executor::SmolExecutor;
use criterion::{criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion, Throughput};
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
    let mut group = c.benchmark_group("send_zst");

    for num_messages in [1, 10, 100, 1000] {
        let (address, task) = Counter(0).create(Some(num_messages)).run();
        let _task = smol::spawn(task);

        group.bench_with_input(
            BenchmarkId::from_parameter(num_messages),
            &num_messages,
            |b, &num_messages| {
                b.to_async(SmolExecutor).iter(|| async {
                    for _ in 0..num_messages {
                        address.send(IncrementZst).await;
                    }
                });
            },
        );
    }
}

criterion_group!(benches, throughput);
criterion_main!(benches);
