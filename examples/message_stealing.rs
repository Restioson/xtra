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

        let millis = rand::thread_rng().gen_range(0, 100);
        smol::Timer::after(Duration::from_millis(millis)).await;
    }
}

fn main() {
    let addr = Printer::spawn_many(Printer::new, Some(32), 4);

    for _ in 0..3 {
        let addr = addr.clone();
        std::thread::spawn(move || {
            loop {
                addr.do_send(Print("hello".to_string()))
                    .expect("Printer should not be dropped");
            }
        });
    }

    loop {
        addr.do_send(Print("hello".to_string()))
            .expect("Printer should not be dropped");
    }
}
