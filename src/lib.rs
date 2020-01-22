#![feature(generic_associated_types, doc_cfg, doc_spotlight)]

mod envelope;

mod address;
pub use address::{Address, AddressExt, WeakAddress, Disconnected};

mod context;
pub use context::Context;

mod manager;
pub use manager::ActorManager;

pub mod prelude {
    pub use crate::address::{Address, AddressExt};
    pub use crate::{Actor, Context, Handler, AsyncHandler, Message};
}

use futures::future::Future;

/// A message that can be sent to an [`Actor`](trait.Actor.html) for processing. They are processed
/// one at a time. Only actors implementing the corresponding [`Handler<M>`](trait.Handler.html)
/// trait can be sent a given message.
pub trait Message: Send + 'static {
    /// The return type of the message. It will be returned when the [`Address::send`](struct.Address.html#method.send)
    /// or [`Address::send_async`](struct.Address.html#method.send_async) methods are called.
    type Result: Send;
}

/// A trait indicating that an [`Actor`](trait.Actor.html) can handle a given [`Message`](trait.Message.html)
/// synchronously, and the logic to handle the message.
pub trait Handler<M: Message>: Actor {
    /// Handle a given message, returning its result.
    fn handle(&mut self, message: M, ctx: &mut Context<Self>) -> M::Result;
}

/// A trait indicating that an [`Actor`](trait.Actor.html) can handle a given [`Message`](trait.Message.html)
/// asynchronously, and the logic to handle the message.
pub trait AsyncHandler<M: Message>: Actor {
    /// The responding future of the asynchronous actor. This should probably look like:
    /// ```ignore
    /// type Responder<'a>: Future<Output = M::Result> + Send
    /// ```
    type Responder<'a>: Future<Output = M::Result> + Send;

    /// Handle a given message, returning a future eventually resolving to its result. The signature
    /// of this function should probably look like:
    /// ```ignore
    /// fn handle(&mut self, message: M, ctx: &mut Context<Self>) -> Self::Responder<'_>
    /// ```
    fn handle<'a>(&'a mut self, message: M, ctx: &'a mut Context<Self>) -> Self::Responder<'a>;
}

/// An actor which can handle [`Message`s](trait.Message.html) one at a time. Actors can only be
/// communicated with by sending [`Message`s](trait.Message.html) through their [`Address`ess](struct.Address.html).
/// They can modify their private state, respond to messages, and spawn other actors. They can also
/// stop themselves through their [`Context`](struct.Context.html) by calling [`Context::stop`](struct.Context.html#method.stop).
/// This will result in any attempt to send messages to the actor in future failing.
pub trait Actor: 'static + Sized {
    /// Called as soon as the actor has been started.
    #[allow(unused_variables)]
    fn started(&mut self, ctx: &mut Context<Self>) {}

    /// Called when the actor is in the process of stopping. This could be used for any cleanup
    /// before the actor is dropped that requires access to the actor's context. This method can also
    /// prevent the actor from being dropped by returning [`KeepRunning::Yes`](enum.KeepRunning.html#variant.Yes).
    /// Be aware that if [`KeepRunning::Yes`](enum.KeepRunning.html#variant.Yes) is returned
    #[allow(unused_variables)]
    fn stopping(&mut self, reason: StopReason, ctx: &mut Context<Self>) -> KeepRunning {
        KeepRunning::No
    }

    /// Called when the actor is in the process of stopping. This should be used for any final
    /// cleanup before the actor is dropped.
    fn stopped(&mut self) {}

    /// Spawns the actor onto the global runtime executor (i.e, `tokio` or `async_std`'s executors).
    #[doc(cfg(feature = "with-tokio-0_2"))]
    #[doc(cfg(feature = "with-async_std-1"))]
    #[cfg(any(doc, feature = "with-tokio-0_2", feature = "with-async_std-1"))]
    fn spawn(self) -> Address<Self>
    where
        Self: Send,
    {
        ActorManager::spawn(self)
    }

    /// Returns the actor's address and manager in a ready-to-start state. To spawn the actor, the
    /// [`ActorManager::manage`](struct.ActorManager.html#method.manage) method must be called and
    /// the future it returns spawned onto an executor.
    fn start(self) -> (Address<Self>, ActorManager<Self>) {
        ActorManager::start(self)
    }
}

/// Whether to keep the actor running after it has been put into a stopping state.
pub enum KeepRunning {
    Yes,
    No,
}

/// The reason that the actor was stopped.
pub enum StopReason {
    /// There are no more strong addresses to the actor. Assuming that there are no weak addresses ([`WeakAddress`](struct.WeakAddress.html)),
    /// this means that the actor may never receive any work to do, since there would be no way to
    /// send it messages to process. If it is desirable that the actor handles messages indefinitely,
    /// then strong addresses ([`Address`](struct.Address.html)) should be used.
    NoMoreAddresses,

    /// [`Context::stop`](struct.Context.html#stop) was called by the actor itself.
    StopCalled,
}
