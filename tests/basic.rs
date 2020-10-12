use async_trait::async_trait;

use xtra::prelude::*;
#[cfg(feature = "with-tokio-0_2")]
use xtra::spawn::Tokio;

#[derive(Clone, Debug, Eq, PartialEq)]
struct Accumulator(usize);

impl Actor for Accumulator {}

struct Inc;
impl Message for Inc {
    type Result = ();
}

struct Report;
impl Message for Report {
    type Result = Accumulator;
}

#[async_trait]
impl Handler<Inc> for Accumulator {
    async fn handle(&mut self, _: Inc, _ctx: &mut Context<Self>) {
        self.0 += 1;
    }
}

#[async_trait]
impl Handler<Report> for Accumulator {
    async fn handle(&mut self, _: Report, _ctx: &mut Context<Self>) -> Self {
        self.clone()
    }
}

#[cfg(feature = "with-tokio-0_2")]
#[tokio::test]
async fn accumulate_to_ten() {
    let addr = Accumulator(0).create(None).spawn(&mut Tokio::Global);
    for _ in 0..10 {
        addr.do_send(Inc).unwrap();
    }

    assert_eq!(addr.send(Report).await.unwrap().0, 10);
}
