use xtra::prelude::*;

#[derive(Default)]
struct MessageCounter {
    num_messages: usize,
}

// With a manual `Actor` implementation, we can specify a `Stop` type and thus return something from the `stopped` lifecycle callback.

impl Actor for MessageCounter {
    type Stop = usize;

    async fn stopped(self) -> Self::Stop {
        self.num_messages
    }
}

struct Ping;
struct Stop;

impl Handler<Ping> for MessageCounter {
    type Return = ();

    async fn handle(&mut self, _: Ping, _: &mut Context<Self>) -> Self::Return {
        self.num_messages += 1;
    }
}

impl Handler<Stop> for MessageCounter {
    type Return = ();

    async fn handle(&mut self, _: Stop, ctx: &mut Context<Self>) -> Self::Return {
        ctx.stop_self();
    }
}

#[tokio::main]
async fn main() {
    let (address, mailbox) = Mailbox::unbounded();
    let run_future = xtra::run(mailbox, MessageCounter::default()); // `run_future` will resolve to `Actor::Stop`.
    let handle = tokio::spawn(run_future);

    address.send(Ping).await.unwrap();
    address.send(Ping).await.unwrap();
    address.send(Ping).await.unwrap();
    address.send(Stop).await.unwrap();

    let num_messages = handle.await.unwrap();

    assert_eq!(num_messages, 3);
}
