use async_trait::async_trait;
use xtra::prelude::*;

struct Initialized(Address<ActorA>);
impl Message for Initialized {
    type Result = ();
}

struct Hello;
impl Message for Hello {
    type Result = ();
}

struct ActorA {
    actor_b: Address<ActorB>,
}
impl Actor for ActorA {}

#[async_trait]
impl Handler<Hello> for ActorA {
    async fn handle(&mut self, _: Hello, ctx: &mut Context<Self>) {
        println!("ActorA: Hello");
        ctx.handle_while(self, self.actor_b.send(Hello)).await.unwrap();
    }
}

struct ActorB;
impl Actor for ActorB {}

#[async_trait]
impl Handler<Initialized> for ActorB {
    async fn handle(&mut self, m: Initialized, ctx: &mut Context<Self>) {
        println!("ActorB: Initialized");
        let actor_a = m.0;
        ctx.handle_while(self, actor_a.send(Hello)).await.unwrap();
    }
}

#[async_trait]
impl Handler<Hello> for ActorB {
    async fn handle(&mut self, _: Hello, _: &mut Context<Self>) {
        println!("ActorB: Hello");
    }
}

#[tokio::main]
async fn main() {
    let actor_b = ActorB {}.spawn();
    let actor_a = ActorA {
        actor_b: actor_b.clone(),
    }
        .spawn();
    actor_b.send(Initialized(actor_a.clone())).await.unwrap();
}
