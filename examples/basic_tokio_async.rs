#![feature(type_alias_impl_trait)]

use xtra::{Actor, Context, Handler, Message};
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

impl<'a> Handler<'a, Print> for Printer {
    type Responder = impl Future<Output = ()> + 'a;

    fn handle(&'a mut self, print: Print, _ctx: &'a mut Context<Self>) -> Self::Responder {
        async {
            self.times += 1;
            println!("Printing {}. Printed {} times so far.", print.0, self.times);
        }
    }
}

#[tokio::main]
async fn main() {
    let addr = Printer::new().spawn();
    loop {
        addr.send_async(Print("hello".to_string()))
            .await
            .expect("Printer should not be dropped");
    }
}
