#![feature(
    generic_associated_types,
    weak_counts,
    specialization,
    type_alias_impl_trait
)]
#![feature(doc_cfg, doc_spotlight)]

mod envelope;

mod address;
pub use address::{Address, AddressExt, Disconnected, WeakAddress};

mod context;
pub use context::Context;

mod manager;
pub use manager::ActorManager;

pub mod prelude {
    pub use crate::address::{Address, AddressExt};
    pub use crate::{Actor, Context, Handler, Message, SyncHandler};
}

use futures::future::{self, Future};

/// A message that can be sent to an [`Actor`](trait.Actor.html) for processing. They are processed
/// one at a time. Only actors implementing the corresponding [`Handler<M>`](trait.Handler.html)
/// trait can be sent a given message.
pub trait Message: Send + 'static {
    /// The return type of the message. It will be returned when the [`Address::send`](struct.Address.html#method.send)
    /// method is called.
    type Result: Send;
}

/// A trait indicating that an [`Actor`](trait.Actor.html) can handle a given [`Message`](trait.Message.html)
/// synchronously, and the logic to handle the message. A `SyncHandler` implementation automatically
/// creates a corresponding [`Handler`](trait.Handler.html) impl. This, however, is not just sugar
/// over the asynchronous  [`Handler`](trait.Handler.html) trait -- it is also slightly faster than
/// it for handling due to how they get specialized under the hood.
pub trait SyncHandler<M: Message>: Actor {
    /// Handle a given message, returning its result.
    fn handle(&mut self, message: M, ctx: &mut Context<Self>) -> M::Result;
}

/// A trait indicating that an [`Actor`](trait.Actor.html) can handle a given [`Message`](trait.Message.html)
/// asynchronously, and the logic to handle the message.
pub trait Handler<M: Message>: Actor {
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

impl<M: Message, T: SyncHandler<M>> Handler<M> for T {
    type Responder<'a> = impl Future<Output = M::Result> + 'a;

    fn handle(&mut self, message: M, ctx: &mut Context<Self>) -> Self::Responder<'_> {
        let res = SyncHandler::handle(self, message, ctx);
        future::ready(res)
    }
}

/// An actor which can handle [`Message`s](trait.Message.html) one at a time. Actors can only be
/// communicated with by sending [`Message`s](trait.Message.html) through their [`Address`es](struct.Address.html).
/// They can modify their private state, respond to messages, and spawn other actors. They can also
/// stop themselves through their [`Context`](struct.Context.html) by calling [`Context::stop`](struct.Context.html#method.stop).
/// This will result in any attempt to send messages to the actor in future failing.
pub trait Actor: 'static + Sized {
    /// Called as soon as the actor has been started.
    #[allow(unused_variables)]
    fn started(&mut self, ctx: &mut Context<Self>) {}

    /// Called when the actor calls the [`Context::stop`](struct.Context.html#method.stop). This method
    /// can prevent the actor from stopping by returning [`KeepRunning::Yes`](enum.KeepRunning.html#variant.Yes).
    ///
    /// **Note:** this method will *only* be called when `Context::stop` is called. Other, general
    /// destructor behaviour should be encapsulated in the [`Actor::stopped`](trait.Actor.html#method.stopped)
    /// method.
    #[allow(unused_variables)]
    fn stopping(&mut self, ctx: &mut Context<Self>) -> KeepRunning {
        KeepRunning::No
    }

    /// Called when the actor is in the process of stopping. This could be because
    /// [`KeepRunning::No`](enum.KeepRunning.html#variant.No) was returned from the
    /// [`Actor::stopping`](trait.Actor.html#method.stopping) method, or because there are no more
    /// strong addresses ([`Address`](struct.Address.html), as opposed to [`WeakAddress`](struct.WeakAddress.html).
    /// This should be used for any final cleanup before the actor is dropped.
    #[allow(unused_variables)]
    fn stopped(&mut self, ctx: &mut Context<Self>) {}

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
    fn create(self) -> (Address<Self>, ActorManager<Self>) {
        ActorManager::start(self)
    }
}

/// Whether to keep the actor running after it has been put into a stopping state.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum KeepRunning {
    Yes,
    No,
}
