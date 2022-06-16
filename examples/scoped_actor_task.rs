use futures_util::future;
use xtra::prelude::*;
use xtra::scoped_task::ActorScopedExt;
use xtra::spawn::Tokio;

struct MyActor;

#[async_trait]
impl Actor for MyActor {
    type Stop = ();

    async fn stopped(self) {}
}

struct Print(String);

#[async_trait]
impl Handler<Print> for MyActor {
    type Return = ();

    async fn handle(&mut self, print: Print, _ctx: &mut Context<Self>) {
        println!("Printing {}", print.0);
    }
}

struct DropChecker;

impl Drop for DropChecker {
    fn drop(&mut self) {
        println!("The other task has been ended.");
    }
}

#[tokio::main]
async fn main() {
    let addr = MyActor.create(None).spawn(&mut Tokio::Global);
    addr.send(Print("hello".to_string()))
        .await
        .expect("Actor should not be dropped");

    let task = async {
        let _checker = DropChecker;
        future::pending::<()>().await
    };

    // This task will end as soon as the actor stops
    let task = tokio::spawn(xtra::scoped(&addr, task));

    drop(addr);
    assert!(task.await.unwrap().is_none()); // should print "The other task has been ended."
}
