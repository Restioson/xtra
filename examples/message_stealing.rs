//! Set the SMOL_THREADS environment variable to have more threads, else each receiving task will
//! switch only after it has received many messages.

use xtra::prelude::*;
use std::time::Duration;
use rand::Rng;

struct Printer {
    times: usize,
    id: usize,
}

impl Printer {
    fn new(id: usize) -> Self {
        Printer { times: 0, id: id + 1 }
    }
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
        println!(
            "Printing {} from printer {}. Printed {} times so far.",
            print.0,
            self.id,
            self.times
        );
    }
}

fn main() {
    let (addr, mut ctx) = Context::new(Some(32));
    for n in 0..4 {
        smol::spawn(ctx.attach(Printer::new(n)));
    }

    loop {
        addr.do_send(Print("hello".to_string()))
            .expect("Printer should not be dropped");
    }
}
