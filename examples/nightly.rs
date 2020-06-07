#![feature(type_alias_impl_trait, generic_associated_types)]

use xtra::prelude::*;
use futures::Future;

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

impl Handler<Print> for Printer {
    type Responder<'a> = impl Future<Output = ()> + 'a;
    fn handle(&mut self, print: Print, _ctx: &mut Context<Self>) -> Self::Responder<'_> {
        async move {
            self.times += 1;
            println!("Printing {}. Printed {} times so far.", print.0, self.times);
        }
    }
}

#[tokio::main]
async fn main() {
    let addr = Printer::new().spawn();
    loop {
        addr.send(Print("hello".to_string()))
            .await
            .expect("Printer should not be dropped");
    }
}
