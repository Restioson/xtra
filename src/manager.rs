use crate::envelope::MessageEnvelope;
use crate::{Actor, Address, Context};
use futures::Future;
use crate::spawn::ActorSpawner;

/// A message that can be sent by an [`Address`](struct.Address.html) to the [`ActorManager`](struct.ActorManager.html)
pub(crate) enum ManagerMessage<A: Actor> {
    /// The address sending this is being dropped and is the only external strong address in existence
    /// other than the one held by the [`Context`](struct.Context.html). This notifies the
    /// [`ActorManager`](struct.ActorManager.html) so that it can check if the actor should be
    /// dropped
    LastAddress,
    /// A message being sent to the actor. To read about envelopes and why we use them, check out
    /// `envelope.rs`
    Message(Box<dyn MessageEnvelope<Actor = A>>),
    /// A notification queued with `Context::notify_later`
    LateNotification(Box<dyn MessageEnvelope<Actor = A>>),
}

/// If and how to continue the manage loop
#[derive(PartialEq, Eq, Debug, Copy, Clone)]
pub(crate) enum ContinueManageLoop {
    Yes,
    ExitImmediately,
    ProcessNotifications,
}

/// A manager for the actor which handles incoming messages and stores the context. Its managing
/// loop can be started with [`ActorManager::manage`](struct.ActorManager.html#method.manage).
pub struct ActorManager<A: Actor> {
    pub(crate) address: Address<A>,
    pub(crate) actor: A,
    pub(crate) ctx: Context<A>,
}

impl<A: Actor> ActorManager<A> {
    pub fn spawn<S: ActorSpawner>(self, spawner: S) -> Address<A> {
        let (addr, fut) = self.manage();
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
    ///     let (addr, fut) = MyActor.create(None).manage();
    ///     smol::spawn(fut).detach(); // Actually spawn the actor onto an executor
    /// });
    /// ```
    pub fn manage(self) -> (Address<A>, impl Future<Output = ()>) {
        (self.address, self.ctx.run(self.actor))
    }
}
