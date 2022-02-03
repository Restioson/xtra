use std::future::Future;

use crate::address::Address;
use crate::context::Context;
use crate::envelope::{BroadcastMessageEnvelope, MessageEnvelope};
use crate::spawn::Spawner;
use crate::Actor;

/// A message that can be sent by an Address to the manage loop
pub(crate) enum AddressMessage<A> {
    /// A message from the last address telling the actor that it should shut down
    LastAddress,
    /// A message being sent to the actor. To read about envelopes and why we use them, check out
    /// `envelope.rs`
    Message(Box<dyn MessageEnvelope<Actor = A>>),
}

/// A message that can be sent by another actor on the same address to the manage loop
pub(crate) enum BroadcastMessage<A> {
    /// A message from another actor on the same address telling the actor to unconditionally shut
    /// down.
    Shutdown,
    Message(Box<dyn BroadcastMessageEnvelope<Actor = A>>),
}

impl<A> Clone for BroadcastMessage<A> {
    fn clone(&self) -> Self {
        use self::BroadcastMessage::*;
        match self {
            Shutdown => Shutdown,
            Message(msg) => Message(msg.clone()),
        }
    }
}

/// If and how to continue the manage loop
#[derive(PartialEq, Eq, Debug, Copy, Clone)]
pub(crate) enum ContinueManageLoop {
    Yes,
    ExitImmediately,
}

/// A manager for the actor which handles incoming messages and stores the context. Its managing
/// loop can be started with [`ActorManager::run`](struct.ActorManager.html#method.run).
pub struct ActorManager<A: Actor> {
    pub(crate) address: Address<A>,
    pub(crate) actor: A,
    pub(crate) ctx: Context<A>,
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
    /// # #[async_trait::async_trait] impl Actor for MyActor {type Stop = (); async fn stopped(self) -> Self::Stop {} }
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
