use xtra::prelude::*;

struct Initialized(Address<ActorA>);

struct Hello;

#[derive(xtra::Actor)]
struct ActorA {
    actor_b: Address<ActorB>,
}

impl Handler<Hello> for ActorA {
    type Return = ();

    async fn handle(&mut self, _: Hello, ctx: &mut Context<Self>) {
        println!("ActorA: Hello");
        xtra::join(ctx.mailbox(), self, self.actor_b.send(Hello))
            .await
            .unwrap();
    }
}

#[derive(xtra::Actor)]
struct ActorB;

impl Handler<Initialized> for ActorB {
    type Return = ();

    async fn handle(&mut self, m: Initialized, ctx: &mut Context<Self>) {
        println!("ActorB: Initialized");
        let actor_a = m.0;
        xtra::join(ctx.mailbox(), self, actor_a.send(Hello))
            .await
            .unwrap();
    }
}

impl Handler<Hello> for ActorB {
    type Return = ();

    async fn handle(&mut self, _: Hello, _: &mut Context<Self>) {
        println!("ActorB: Hello");
    }
}

fn main() {
    smol::block_on(async {
        let actor_b = xtra::spawn_smol(ActorB, Mailbox::unbounded());
        let actor_a = xtra::spawn_smol(
            ActorA {
                actor_b: actor_b.clone(),
            },
            Mailbox::unbounded(),
        );
        actor_b.send(Initialized(actor_a.clone())).await.unwrap();
    })
}
