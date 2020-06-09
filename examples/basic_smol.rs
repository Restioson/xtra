use xtra::prelude::*;

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

impl SyncHandler<Print> for Printer {
    fn handle(&mut self, print: Print, _ctx: &mut Context<Self>) {
        self.times += 1;
        println!("Printing {}. Printed {} times so far.", print.0, self.times);
    }
}

fn main() {
    smol::run(async {
        let addr = Printer::new().spawn();
        loop {
            addr.send(Print("hello".to_string()))
                .await
                .expect("Printer should not be dropped");
        }
    })
}
