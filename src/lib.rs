#![feature(specialization)]

#[macro_use]
extern crate rental;

mod envelope;

mod address;
pub use address::{Address, Disconnected};

mod context;
pub use context::Context;

mod manager;
pub use manager::ActorManager;

/// A message that can be sent to an [`Actor`](struct.Actor.html) for processing. They are processed
/// one at a time. Only actors implementing the corresponding [`Handler<M>`](trait.Handler.html)
/// trait can be sent a given message.
pub trait Message: Send + 'static {
    /// The return type of the message. It will be returned when the [`Address::send`](struct.Address.html#method.send)
    /// is called.
    type Result: Send;
}

pub trait MessageResponder<M: Message> {}

impl<M: Message> MessageResponder<M> for M::Result {}

pub trait SyncResponder<M: Message>: MessageResponder<M> {
    fn cast(self) -> M::Result;
}

impl<M: Message> SyncResponder<M> for M::Result {
    fn cast(self) -> M::Result {
        self
    }
}

/// A trait indicating that an [`Actor`](struct.Actor.html) can handle a given [`Message`](trait.Message.html),
/// and the logic to handle the message.
pub trait Handler<'a, M: Message>: Actor {
    // TODO doc
    type Responder: 'a;

    /// Handle a given message, returning its result.
    fn handle(&'a mut self, message: M, ctx: &'a mut Context<Self>) -> Self::Responder;
}

pub trait Actor: Send + 'static {
    /// Called as soon as the actor has been started.
    fn started(&mut self, _ctx: &mut Context<Self>) {}

    /// Called when the actor is in the process of stopping. The key difference between this method
    /// and [`Actor::stopped`](trait.Actor.html#method.stopped) is that the actor can prevent itself
    /// from stopping by returning [`KeepRunning::Yes`](enum.KeepRunning.html#variant.Yes) from this
    /// function.
    fn stopping(&mut self, _ctx: &mut Context<Self>) -> KeepRunning {
        KeepRunning::No
    }

    /// Called when the actor is in the process of stopping. The key difference between this method
    /// and [`Actor::stopping`](trait.Actor.html#method.stopping) is that the actor cannot be rescued
    /// from being stopped at this stage. This should be used for any final cleanup before the actor
    /// is dropped.
    fn stopped(&mut self, _ctx: &mut Context<Self>) {}

    fn spawn(self) -> Address<Self>
    where
        Self: Sized,
    {
        ActorManager::spawn(self)
    }

    fn start(self) -> (Address<Self>, ActorManager<Self>)
    where
        Self: Sized,
    {
        ActorManager::start(self)
    }
}

/// Whether to keep the actor running after [`Context::stop`](struct.Context.html#method.stop) has
/// been called, or all the [`Address`es](struct.Address.html) to an actor have been dropped.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum KeepRunning {
    Yes,
    No,
}
