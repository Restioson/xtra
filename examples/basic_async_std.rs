use xtra::prelude::*;
use xtra::spawn::AsyncStd;

struct Printer {
    times: usize,
}

impl Printer {
    fn new() -> Self {
        Printer { times: 0 }
    }
}

impl Actor for Printer {}

struct Print(String);
impl Message for Print {
    type Result = ();
}

#[async_trait]
impl Handler<Print> for Printer {
    async fn handle(&mut self, print: Print, _ctx: &mut Context<Self>) {
        self.times += 1;
        println!("Printing {}. Printed {} times so far.", print.0, self.times);
    }
}

#[async_std::main]
async fn main() {
    let addr = Printer::new().create(None).spawn(&mut AsyncStd);
    loop {
        addr.send(Print("hello".to_string()))
            .await
            .expect("Printer should not be dropped");
    }
}
