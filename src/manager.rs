use std::future::Future;

use crate::address::Address;
use crate::context::Context;
use crate::spawn::Spawner;
use crate::Actor;

/// A manager for the actor which handles incoming messages and stores the context. Its managing
/// loop can be started with [`ActorManager::run`](struct.ActorManager.html#method.run).
pub struct ActorManager<A: Actor> {
    /// The address of the actor.
    pub address: Address<A>,
    /// The actor itself.
    pub actor: A,
    /// The context of the actor.
    pub ctx: Context<A>,
}

impl<A: Actor<Stop = ()>> ActorManager<A> {
    /// Spawn the actor's main loop on the given runtime. This will allow it to handle messages.
    pub fn spawn<S: Spawner>(self, spawner: &mut S) -> Address<A> {
        let (addr, fut) = self.run();
        spawner.spawn(fut);
        addr
    }
}

impl<A: Actor> ActorManager<A> {
    /// Starts the manager loop, returning the actor's address and its manage future. This will
    /// start the actor and allow it to respond to messages.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use xtra::prelude::*;
    /// struct MyActor;
    /// # #[async_trait] impl Actor for MyActor {type Stop = (); async fn stopped(self) -> Self::Stop {} }
    ///
    /// smol::block_on(async {
    ///     let (addr, fut) = MyActor.create(None).run();
    ///     smol::spawn(fut).detach(); // Actually spawn the actor onto an executor
    /// });
    /// ```
    pub fn run(self) -> (Address<A>, impl Future<Output = A::Stop>) {
        (self.address, self.ctx.run(self.actor))
    }
}
