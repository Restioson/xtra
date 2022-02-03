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

/// Extension trait for [`Actor`] to create an instance of [`ActorManager`].
pub trait Create: Actor {
    /// Returns the actor's address and manager in a ready-to-start state, given the cap for the
    /// actor's mailbox. If `None` is passed, it will be of unbounded size. To spawn the actor,
    /// the [`ActorManager::spawn`](struct.ActorManager.html#method.spawn) must be called, or
    /// the [`ActorManager::run`](struct.ActorManager.html#method.run) method must be called
    /// and the future it returns spawned onto an executor.
    /// # Example
    ///
    /// ```rust
    /// # use xtra::{KeepRunning, prelude::*};
    /// # use std::time::Duration;
    /// # use smol::Timer;
    /// # struct MyActor;
    /// # impl Actor for MyActor {}
    /// smol::block_on(async {
    ///     let (addr, fut) = MyActor.create(None).run();
    ///     smol::spawn(fut).detach(); // Actually spawn the actor onto an executor
    ///     Timer::after(Duration::from_secs(1)).await; // Give it time to run
    /// })
    /// ```
    fn create(self, message_cap: Option<usize>) -> ActorManager<Self>;
}

impl<A> Create for A
where
    A: Actor,
{
    fn create(self, message_cap: Option<usize>) -> ActorManager<Self> {
        let (address, ctx) = Context::new(message_cap);
        ActorManager {
            address,
            actor: self,
            ctx,
        }
    }
}

/// A manager for the actor which handles incoming messages and stores the context. Its managing
/// loop can be started with [`ActorManager::run`](struct.ActorManager.html#method.run).
pub struct ActorManager<A: Actor> {
    address: Address<A>,
    actor: A,
    ctx: Context<A>,
}

impl<A: Actor> ActorManager<A> {
    /// Spawn the actor's main loop on the given runtime. This will allow it to handle messages.
    pub fn spawn<S: Spawner>(self, spawner: &mut S) -> Address<A> {
        let (addr, fut) = self.run();
        spawner.spawn(fut);
        addr
    }

    /// Starts the manager loop, returning the actor's address and its manage future. This will
    /// start the actor and allow it to respond to messages.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use xtra::prelude::*;
    /// struct MyActor;
    /// impl Actor for MyActor {}
    ///
    /// smol::block_on(async {
    ///     let (addr, fut) = MyActor.create(None).run();
    ///     smol::spawn(fut).detach(); // Actually spawn the actor onto an executor
    /// });
    /// ```
    pub fn run(self) -> (Address<A>, impl Future<Output = ()>) {
        (self.address, self.ctx.run(self.actor))
    }
}
