use std::time::Duration;

use futures_core::Stream;
use futures_util::stream::repeat;
use futures_util::StreamExt;
use xtra::prelude::*;
use xtra::Error;

#[derive(Default, xtra::Actor)]
struct Greeter;

struct Greet;

#[async_trait]
impl Handler<Greet> for Greeter {
    type Return = ();

    async fn handle(&mut self, _: Greet, _ctx: &mut Context<Self>) {
        println!("Hello!");
    }
}

#[tokio::main]
async fn main() {
    let addr = xtra::spawn_tokio(Greeter::default(), Mailbox::unbounded());
    greeter_stream(500).forward(addr.into_sink()).await.unwrap();
}

fn greeter_stream(delay: u64) -> impl Stream<Item = Result<Greet, Error>> {
    repeat(Duration::from_millis(delay))
        .then(tokio::time::sleep)
        .map(|_| Ok(Greet))
}
