use xtra::prelude::*;

struct Initialized(Address<ActorA>);

struct Hello;

struct ActorA {
    actor_b: Address<ActorB>,
}

#[async_trait]
impl Actor for ActorA {
    type Stop = ();

    async fn stopped(self) -> Self::Stop {}
}

#[async_trait]
impl Handler<Hello> for ActorA {
    type Return = ();

    async fn handle(&mut self, _: Hello, ctx: &mut Context<Self>) {
        println!("ActorA: Hello");
        let fut = self.actor_b.send(Hello);
        ctx.join(self, fut).await.unwrap();
    }
}

struct ActorB;

#[async_trait]
impl Actor for ActorB {
    type Stop = ();

    async fn stopped(self) -> Self::Stop {}
}

#[async_trait]
impl Handler<Initialized> for ActorB {
    type Return = ();

    async fn handle(&mut self, m: Initialized, ctx: &mut Context<Self>) {
        println!("ActorB: Initialized");
        let actor_a = m.0;
        ctx.join(self, actor_a.send(Hello)).await.unwrap();
    }
}

#[async_trait]
impl Handler<Hello> for ActorB {
    type Return = ();

    async fn handle(&mut self, _: Hello, _: &mut Context<Self>) {
        println!("ActorB: Hello");
    }
}

fn main() {
    smol::block_on(async {
        let actor_b = xtra::spawn_smol(ActorB, None);
        let actor_a = xtra::spawn_smol(
            ActorA {
                actor_b: actor_b.clone(),
            },
            None,
        );
        actor_b.send(Initialized(actor_a.clone())).await.unwrap();
    })
}
