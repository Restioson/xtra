use async_trait::async_trait;
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use tokio::runtime::Runtime;
use xtra::{Actor, Context, Handler};

struct Counter(u64);

#[async_trait]
impl Actor for Counter {
    type Stop = ();
    async fn stopped(self) {}
}

struct IncrementZst;
struct Finish;

#[async_trait::async_trait]
impl Handler<IncrementZst> for Counter {
    type Return = ();

    async fn handle(&mut self, _: IncrementZst, _ctx: &mut Context<Self>) {
        self.0 += 1;
    }
}

#[async_trait::async_trait]
impl Handler<Finish> for Counter {
    type Return = u64;

    async fn handle(&mut self, _: Finish, _ctx: &mut Context<Self>) -> u64 {
        self.0
    }
}

fn throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("send_zst");
    let runtime = Runtime::new().unwrap();
    let _g = runtime.enter();

    for num_messages in [1, 10, 100, 1000] {
        let (address, task) = Counter(0).create(Some(num_messages)).run();
        let _task = smol::spawn(task);

        group.bench_with_input(
            BenchmarkId::from_parameter(num_messages),
            &num_messages,
            |b, &num_messages| {
                b.to_async(&runtime).iter(|| async {
                    for _ in 0..num_messages - 1 {
                        let _ = address.send(IncrementZst).split_receiver().await;
                    }

                    address.send(IncrementZst).await.unwrap()
                });
            },
        );
    }
}

criterion_group!(benches, throughput);
criterion_main!(benches);
