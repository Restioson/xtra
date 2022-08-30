use std::future::Future;

use async_trait::async_trait;
use criterion::{criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion};
use tokio::runtime::Runtime;
use tokio::sync::mpsc;
use xtra::{Actor, Context, Handler};

struct Counter(u64);

#[async_trait]
impl Actor for Counter {
    type Stop = ();
    async fn stopped(self) -> Self::Stop {}
}

#[derive(Debug)]
struct Increment {}
struct Stop;

#[async_trait::async_trait]
impl Handler<Increment> for Counter {
    type Return = ();

    async fn handle(&mut self, _: Increment, _ctx: &mut Context<Self>) {
        self.0 += 1;
    }
}

#[async_trait::async_trait]
impl Handler<Stop> for Counter {
    type Return = ();

    async fn handle(&mut self, _: Stop, ctx: &mut Context<Self>) {
        ctx.stop_self();
    }
}

fn mpsc_counter() -> (mpsc::UnboundedSender<Increment>, impl Future<Output = ()>) {
    let (sender, mut receiver) = mpsc::unbounded_channel();

    let actor = async move {
        let mut _counter = 0;

        while let Some(Increment {}) = receiver.recv().await {
            _counter += 1;
        }
    };

    (sender, actor)
}

fn xtra_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("increment");
    let runtime = Runtime::new().unwrap();
    let _g = runtime.enter();

    for num_messages in [100, 1000, 10000] {
        group.bench_with_input(
            BenchmarkId::new("xtra", num_messages),
            &num_messages,
            |b, &num_messages| {
                b.to_async(&runtime).iter_batched(
                    || {
                        let (xtra_address, xtra_context) = Context::new(None);
                        runtime.spawn(xtra_context.run(Counter(0)));

                        xtra_address
                    },
                    |xtra_address| async move {
                        for _ in 0..num_messages - 1 {
                            let _ = xtra_address.send(Increment {}).await.unwrap();
                        }

                        xtra_address.send(Stop).await.unwrap();
                    },
                    BatchSize::SmallInput,
                );
            },
        );
    }
}

fn mpsc_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("increment");
    let runtime = Runtime::new().unwrap();
    let _g = runtime.enter();

    for num_messages in [100, 1000, 10000] {
        group.bench_with_input(
            BenchmarkId::new("mpsc", num_messages),
            &num_messages,
            |b, &num_messages| {
                b.to_async(&runtime).iter_batched(
                    || {
                        let (mpsc_address, mpsc_actor) = mpsc_counter();
                        runtime.spawn(mpsc_actor);

                        mpsc_address
                    },
                    |mpsc_address| async move {
                        for _ in 0..num_messages - 1 {
                            mpsc_address.send(Increment {}).unwrap();
                        }

                        drop(mpsc_address);
                    },
                    BatchSize::SmallInput,
                );
            },
        );
    }
}

criterion_group!(benches, xtra_throughput, mpsc_throughput);
criterion_main!(benches);
