use futures_util::stream::repeat;
use futures_util::StreamExt;
use xtra::prelude::*;

#[derive(Default, xtra::Actor)]
struct Accumulator {
    sum: u32,
}

struct Add(u32);

struct GetSum;

impl Handler<Add> for Accumulator {
    type Return = ();

    async fn handle(&mut self, Add(number): Add, _ctx: &mut Context<Self>) {
        self.sum += number;
    }
}

impl Handler<GetSum> for Accumulator {
    type Return = u32;

    async fn handle(&mut self, _: GetSum, _ctx: &mut Context<Self>) -> Self::Return {
        self.sum
    }
}

#[tokio::main]
async fn main() {
    let addr = xtra::spawn_tokio(Accumulator::default(), Mailbox::unbounded());

    repeat(10)
        .take(4)
        .map(|number| Ok(Add(number)))
        .forward(addr.clone().into_sink())
        .await
        .unwrap();

    let sum = addr.send(GetSum).await.unwrap();
    println!("Sum is {}!", sum);
}
