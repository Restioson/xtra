use std::ops::ControlFlow;
use std::time::Duration;
use tokio::time::Instant;
use xtra::prelude::*;
use xtra::ActorManager;

struct Counter {
    count: usize,
}

impl Counter {
    fn new() -> Self {
        Counter { count: 0 }
    }
}

#[async_trait]
impl Actor for Counter {
    type Stop = ();

    async fn stopped(self) -> Self::Stop {}
}

struct Inc;

#[async_trait]
impl Handler<Inc> for Counter {
    type Return = ();

    async fn handle(&mut self, _inc: Inc, _ctx: &mut Context<Self>) {
        // Do some "work"
        tokio::time::sleep(Duration::from_millis(50)).await;
        self.count += 1;
    }
}

#[tokio::main]
async fn main() {
    let ActorManager {
        address,
        mut actor,
        mut ctx,
    } = Counter::new().create(None);

    tokio::spawn(async move {
        loop {
            let start = Instant::now();
            let msg = ctx.mailbox.next().await;
            println!("Got message in {}us", start.elapsed().as_micros());

            let before = actor.count;
            let start = Instant::now();
            let ctrl = ctx.tick(msg, &mut actor).await;

            if let ControlFlow::Break(_) = ctrl {
                println!("Goodbye!");
                break;
            }

            println!(
                "Count changed from {} to {} in {:.2}ms\n",
                before,
                actor.count,
                start.elapsed().as_secs_f32() * 1000.0
            );
        }
    });

    for _ in 0..100 {
        address
            .send(Inc)
            .await
            .expect("Counter should not be dropped");
    }

    // Wait for the actor to stop
    drop(address);
    tokio::time::sleep(Duration::from_secs(1)).await;
}
