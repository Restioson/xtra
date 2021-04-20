//! xtra is a tiny, fast, and safe actor system.

#![cfg_attr(docsrs, feature(doc_cfg, external_doc))]
#![deny(unsafe_code, missing_docs)]

pub mod address;
mod context;
mod envelope;
mod manager;
pub mod message_channel;
/// This module contains types representing the strength of an address's reference counting, which
/// influences whether the address will keep the actor alive for as long as it lives.
pub mod refcount;
pub mod sink;
/// This module contains a trait to spawn actors, implemented for all major async runtimes by default.
pub mod spawn;

/// Commonly used types from xtra
pub mod prelude {
    pub use crate::address::Address;
    pub use crate::context::Context;
    pub use crate::message_channel::{MessageChannel, StrongMessageChannel, WeakMessageChannel};
    #[doc(no_inline)]
    pub use crate::{Actor, Handler, Message};
}

pub use self::address::{Address, Disconnected, WeakAddress};
pub use self::context::{ActorShutdown, Context};
pub use self::manager::ActorManager;

/// A message that can be sent to an [`Actor`](trait.Actor.html) for processing. They are processed
/// one at a time. Only actors implementing the corresponding [`Handler<M>`](trait.Handler.html)
/// trait can be sent a given message.
///
/// # Example
///
/// ```no_run
/// # use xtra::Message;
/// struct MyResult;
/// struct MyMessage;
///
/// impl Message for MyMessage {
///     type Result = MyResult;
/// }
/// ```
pub trait Message: Send + 'static {
    /// The return type of the message. It will be returned when the [`Address::send`](address/struct.Address.html#method.send)
    /// method is called.
    type Result: Send;
}

/// A trait indicating that an [`Actor`](trait.Actor.html) can handle a given [`Message`](trait.Message.html)
/// asynchronously, and the logic to handle the message.
///
/// This is an [`async_trait`](https://docs.rs/async-trait), so implementations should
/// be annotated `#[async_trait]`.
///
/// # Example
///
/// ```
/// # use xtra::prelude::*;
/// # use xtra::spawn::Smol;
/// # struct MyActor;
/// # impl Actor for MyActor {}
/// struct Msg;
///
/// impl Message for Msg {
///     type Result = u32;
/// }
///
/// #[async_trait::async_trait]
/// impl Handler<Msg> for MyActor {
///     async fn handle(&mut self, message: Msg, ctx: &mut Context<Self>) -> u32 {
///         20
///     }
/// }
///
/// fn main() {
///     smol::block_on(async {
///         let addr = MyActor.create(None).spawn(&mut Smol::Global);
///         assert_eq!(addr.send(Msg).await, Ok(20));
///     })
/// }
/// ```
#[async_trait::async_trait]
pub trait Handler<M: Message>: Actor {
    /// Handle a given message, returning its result.
    ///
    /// This is an [`async_trait`](https://docs.rs/async-trait).
    /// See the trait documentation to see an example of how this method can be declared.
    async fn handle(&mut self, message: M, ctx: &mut Context<Self>) -> M::Result;
}

/// An actor which can handle [`Message`s](trait.Message.html) one at a time. Actors can only be
/// communicated with by sending [`Message`s](trait.Message.html) through their [`Address`es](address/struct.Address.html).
/// They can modify their private state, respond to messages, and spawn other actors. They can also
/// stop themselves through their [`Context`](struct.Context.html) by calling [`Context::stop`](struct.Context.html#method.stop).
/// This will result in any attempt to send messages to the actor in future failing.
///
/// This is an [`async_trait`](https://docs.rs/async-trait), so implementations should
/// be annotated `#[async_trait]`.
///
/// # Example
///
/// ```rust
/// # use xtra::{KeepRunning, prelude::*};
/// # use xtra::spawn::Smol;
/// # use std::time::Duration;
/// # use smol::Timer;
/// struct MyActor;
///
/// #[async_trait::async_trait]
/// impl Actor for MyActor {
///     async fn started(&mut self, ctx: &mut Context<Self>) {
///         println!("Started!");
///     }
///
///     async fn stopping(&mut self, ctx: &mut Context<Self>) -> KeepRunning {
///         println!("Decided not to keep running");
///         KeepRunning::StopAll
///     }
///
///     async fn stopped(self) {
///         println!("Finally stopping.");
///     }
/// }
///
/// struct Goodbye;
///
/// impl Message for Goodbye {
///     type Result = ();
/// }
///
/// #[async_trait::async_trait]
/// impl Handler<Goodbye> for MyActor {
///     async fn handle(&mut self, _: Goodbye, ctx: &mut Context<Self>) {
///         println!("Goodbye!");
///         ctx.stop();
///     }
/// }
///
/// // Will print "Started!", "Goodbye!", "Decided not to keep running", and then "Finally stopping."
/// smol::block_on(async {
///     let addr = MyActor.create(None).spawn(&mut Smol::Global);
///     addr.send(Goodbye).await;
///
///     Timer::after(Duration::from_secs(1)).await; // Give it time to run
/// })
/// ```
///
/// For longer examples, see the `examples` directory.
#[async_trait::async_trait]
pub trait Actor: 'static + Send + Sized {
    /// Called as soon as the actor has been started.
    #[allow(unused_variables)]
    async fn started(&mut self, ctx: &mut Context<Self>) {}

    /// Called when the actor calls the [`Context::stop`](struct.Context.html#method.stop). This method
    /// can prevent the actor from stopping by returning [`KeepRunning::Yes`](enum.KeepRunning.html#variant.Yes).
    /// If this method returns [`KeepRunning::StopSelf`](enum.KeepRunning.html#variant.StopSelf),
    /// this actor will be stopped. If it returns
    /// [`KeepRunning::StopAll`](enum.KeepRunning.html#variant.StopAll), then all actors on the same
    /// address as this actor will be stopped. This can take a little bit of time to propagate.
    ///
    /// **Note:** this method will *only* be called when [`Context::stop`](struct.Context.html#method.stop)
    /// is called from this actor. If the last strong address to the actor is dropped, or
    /// [`Context::stop`](struct.Context.html#method.stop) is called from another actor on the same
    /// address, this will *not* be called. Therefore, Other, general destructor behaviour should be
    /// encapsulated in the [`Actor::stopped`](trait.Actor.html#method.stopped)
    /// method.
    ///
    /// # Example
    /// ```no_run
    /// # use xtra::prelude::*;
    /// # use xtra::KeepRunning;
    /// # struct MyActor { is_running: bool };
    /// # #[async_trait::async_trait]
    /// # impl Actor for MyActor {
    /// async fn stopping(&mut self, ctx: &mut Context<Self>) -> KeepRunning {
    ///     self.is_running.into() // bool can be converted to KeepRunning with Into
    /// }
    /// # }
    /// ```
    #[allow(unused_variables)]
    async fn stopping(&mut self, ctx: &mut Context<Self>) -> KeepRunning {
        KeepRunning::StopAll
    }

    /// Called when the actor is in the process of stopping. This could be because
    /// [`KeepRunning::StopAll`](enum.KeepRunning.html#variant.StopAll) or
    /// [`KeepRunning::StopSelf`](enum.KeepRunning.html#variant.StopSelf) was returned from the
    /// [`Actor::stopping`](trait.Actor.html#method.stopping) method, or because there are no more
    /// strong addresses ([`Address`](address/struct.Address.html), as opposed to
    /// [`WeakAddress`](address/type.WeakAddress.html). This should be used for any final cleanup before
    /// the actor is dropped.
    #[allow(unused_variables)]
    async fn stopped(self) {}

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
    fn create(self, message_cap: Option<usize>) -> ActorManager<Self> {
        let (address, ctx) = Context::new(message_cap);
        ActorManager {
            address,
            actor: self,
            ctx,
        }
    }
}

/// Whether to keep the actor running after it has been put into a stopping state.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum KeepRunning {
    /// Keep the actor running and prevent it from being stopped
    Yes,
    /// Stop only this actor
    StopSelf,
    /// Stop all actors on this address
    StopAll,
}

/// True is converted to yes, and false is converted to stop all.
impl From<bool> for KeepRunning {
    fn from(b: bool) -> Self {
        if b {
            KeepRunning::Yes
        } else {
            KeepRunning::StopAll
        }
    }
}

impl From<()> for KeepRunning {
    fn from(_: ()) -> KeepRunning {
        KeepRunning::Yes
    }
}
