use std::ops::ControlFlow;
use xtra::prelude::*;

#[derive(Default)]
struct Printer {
    times: usize,
}

impl Actor for Printer {}

struct Print(String);

impl Message for Print {
    type Result = ();
}

#[async_trait::async_trait]
impl Handler<Print> for Printer {
    async fn handle(&mut self, print: Print, _ctx: &mut Context<Self>) {
        self.times += 1;
        println!("Printing {}. Printed {} times so far.", print.0, self.times);
    }
}

#[tokio::main]
async fn main() {
    let (address, mut context) = Context::new(None);

    let mut actor = Printer::default();

    tokio::spawn(async move {
        actor.started(&mut context).await;

        let mut inbox = context.inbox();

        while let Some(msg) = inbox.next().await {
            match context.tick(msg, &mut actor).await {
                ControlFlow::Continue(()) => continue,
                ControlFlow::Break(()) => break,
            }
        }

        actor.stopped().await;
    });

    loop {
        address
            .send(Print("hello".to_string()))
            .await
            .expect("Printer should not be dropped");
    }
}
