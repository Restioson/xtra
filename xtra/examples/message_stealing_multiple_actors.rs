//! Set the SMOL_THREADS environment variable to have more threads, else each receiving task will
//! switch only after it has received many messages.

use std::time::Duration;

use xtra::prelude::*;
use xtra::Mailbox;

#[derive(xtra::Actor)]
struct Printer {
    times: usize,
    id: usize,
}

impl Printer {
    fn new(id: usize) -> Self {
        Printer {
            times: 0,
            id: id + 1,
        }
    }
}

struct Print(String);

impl Handler<Print> for Printer {
    type Return = ();

    async fn handle(&mut self, print: Print, ctx: &mut Context<Self>) {
        self.times += 1;
        println!(
            "Printing {} from printer {}. Printed {} times so far.",
            print.0, self.id, self.times
        );

        if self.times == 10 {
            println!("Actor {} stopping!", self.id);
            ctx.stop_all();
        }
    }
}


async fn run() {
    let (addr, mailbox) = Mailbox::bounded(32);
    smol::spawn(xtra::run(mailbox.clone(), Printer::new(0))).detach();
    smol::spawn(xtra::run(mailbox.clone(), Printer::new(1))).detach();
    smol::spawn(xtra::run(mailbox.clone(), Printer::new(2))).detach();
    smol::spawn(xtra::run(mailbox, Printer::new(3))).detach();

    while addr.send(Print("hello".to_string())).await.is_ok() {}
    println!("Stopping to send");

    // Give a second for everything to shut down
    std::thread::sleep(Duration::from_secs(1));
}

fn main() {
    smol::block_on(run())
}