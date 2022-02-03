use xtra::prelude::*;
use xtra::spawn::SmolGlobalSpawnExt;

struct Printer {
    times: usize,
}

impl Printer {
    fn new() -> Self {
        Printer { times: 0 }
    }
}

#[async_trait::async_trait]
impl Actor for Printer {
    type Stop = ();

    async fn stopped(self) -> Self::Stop {}
}

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

fn main() {
    smol::block_on(async {
        let addr = Printer::new().create(None).spawn_global();
        loop {
            addr.send(Print("hello".to_string()))
                .await
                .expect("Printer should not be dropped");
        }
    })
}
