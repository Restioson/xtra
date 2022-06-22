use futures_util::stream::repeat;
use futures_util::StreamExt;
use xtra::prelude::*;
use xtra::spawn::Tokio;

#[derive(Default)]
struct Accumulator {
    sum: u32,
}

#[async_trait]
impl Actor for Accumulator {
    type Stop = ();

    async fn stopped(self) -> Self::Stop {}
}

struct Add(u32);

struct GetSum;

#[async_trait]
impl Handler<Add> for Accumulator {
    type Return = ();

    async fn handle(&mut self, Add(number): Add, _ctx: &mut Context<Self>) {
        self.sum += number;
    }
}

#[async_trait]
impl Handler<GetSum> for Accumulator {
    type Return = u32;

    async fn handle(&mut self, _: GetSum, _ctx: &mut Context<Self>) -> Self::Return {
        self.sum
    }
}

#[tokio::main]
async fn main() {
    let addr = Accumulator::default()
        .create(None)
        .spawn(&mut Tokio::Global);

    repeat(10)
        .take(4)
        .map(|number| Ok(Add(number)))
        .forward(addr.clone().into_sink())
        .await
        .unwrap();

    let sum = addr.send(GetSum).await.unwrap();
    println!("Sum is {}!", sum);
}
