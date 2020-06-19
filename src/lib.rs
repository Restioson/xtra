//! Xtra is a tiny, fast, and safe actor system.

#![cfg_attr(
    feature = "nightly",
    feature(
        generic_associated_types,
        specialization,
        type_alias_impl_trait,
        doc_cfg,
    )
)]
#![cfg_attr(doc, feature(doc_cfg, external_doc))]
#![deny(missing_docs, unsafe_code)]

mod message_channel;
pub use message_channel::{MessageChannel, MessageChannelExt, WeakMessageChannel};

mod envelope;

mod address;
pub use address::{Address, AddressExt, Disconnected, MessageResponseFuture, WeakAddress};

mod context;
pub use context::Context;

mod manager;
pub use manager::ActorManager;

/// Commonly used types from `xtra`
pub mod prelude {
    pub use crate::address::{Address, AddressExt};
    pub use crate::message_channel::{MessageChannel, MessageChannelExt};
    pub use crate::{Actor, Context, Handler, Message, SyncHandler};
}

use futures::future::Future;
#[cfg(feature = "nightly")]
use futures::future::{self, Ready};
use std::pin::Pin;

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
    /// The return type of the message. It will be returned when the [`Address::send`](struct.Address.html#method.send)
    /// method is called.
    type Result: Send;
}

/// A trait indicating that an [`Actor`](trait.Actor.html) can handle a given [`Message`](trait.Message.html)
/// synchronously, and the logic to handle the message. A `SyncHandler` implementation automatically
/// creates a corresponding [`Handler`](trait.Handler.html) impl. This, however, is not just sugar
/// over the asynchronous  [`Handler`](trait.Handler.html) trait -- it is also slightly faster than
/// it for handling due to how they get specialized under the hood.
///
/// # Example
///
/// ```
/// # use xtra::prelude::*;
/// # struct MyActor;
/// # impl Actor for MyActor {}
/// struct Msg;
///
/// impl Message for Msg {
///     type Result = u32;
/// }
///
/// impl SyncHandler<Msg> for MyActor {
///     fn handle(&mut self, message: Msg, ctx: &mut Context<Self>) -> u32 {
///         20
///     }
/// }
///
/// #[smol_potat::main]
/// async fn main() {
///     let addr = MyActor.spawn();
///     assert_eq!(addr.send(Msg).await, Ok(20));
/// }
/// ```
pub trait SyncHandler<M: Message>: Actor {
    /// Handle a given message, returning its result.
    fn handle(&mut self, message: M, ctx: &mut Context<Self>) -> M::Result;
}

/// A pinned, boxed future returned from [`Handler::handle`](trait.Handler.html#method.handle)
pub type ResponderFut<'a, R> = Pin<Box<dyn Future<Output = R> + Send + 'a>>;

/// A trait indicating that an [`Actor`](trait.Actor.html) can handle a given [`Message`](trait.Message.html)
/// asynchronously, and the logic to handle the message. If the message should be handled synchronously,
/// then the [`SyncHandler`](trait.SyncHandler.html) trait should rather be implemented.
///
/// Without the `nightly` feature enabled, this is an [`async_trait`](https://github.com/dtolnay/async-trait/),
/// so implementations should be annotated `#[async_trait]`.
///
/// # Example
///
/// ```
/// # use xtra::prelude::*;
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
/// #[smol_potat::main]
/// async fn main() {
///     let addr = MyActor.spawn();
///     assert_eq!(addr.send(Msg).await, Ok(20));
/// }
/// ```
#[cfg(not(feature = "nightly"))]
pub trait Handler<M: Message>: Actor {
    /// Handle a given message, returning its result.
    ///
    /// Without the `nightly` feature enabled, this is an [`async_trait`](https://github.com/dtolnay/async-trait/).
    /// See the trait documentation to see an example of how this method can be declared.
    fn handle<'s, 'c, 'handler>(
        &'s mut self,
        message: M,
        ctx: &'c mut Context<Self>,
    ) -> ResponderFut<'handler, M::Result>
    where
        's: 'handler,
        'c: 'handler;
}

/// A trait indicating that an [`Actor`](trait.Actor.html) can handle a given [`Message`](trait.Message.html)
/// asynchronously, and the logic to handle the message. If the message should be handled synchronously,
/// then the [`SyncHandler`](trait.SyncHandler.html) trait should rather be implemented.
///
/// For an example, see `examples/nightly.rs`.
#[cfg(feature = "nightly")]
pub trait Handler<M: Message>: Actor {
    /// The responding future of the asynchronous actor. This should probably look like:
    /// ```not_a_test
    /// type Responder<'a>: Future<Output = M::Result> + Send
    /// ```
    type Responder<'a>: Future<Output = M::Result> + Send;

    /// Handle a given message, returning a future eventually resolving to its result. The signature
    /// of this function should probably look like:
    /// ```not_a_test
    /// fn handle(&mut self, message: M, ctx: &mut Context<Self>) -> Self::Responder<'_>
    /// ```
    /// or:
    /// ```not_a_test
    /// fn handle<'a>(&'a mut self, message: M, ctx: &'a mut Context<Self>) -> Self::Responder<'a>
    /// ```
    fn handle<'a>(&'a mut self, message: M, ctx: &'a mut Context<Self>) -> Self::Responder<'a>;
}

impl<M: Message, T: SyncHandler<M> + Send> Handler<M> for T {
    #[cfg(not(feature = "nightly"))]
    fn handle<'a, 'b, 'c>(
        &'a mut self,
        message: M,
        ctx: &'b mut Context<Self>,
    ) -> ResponderFut<'c, M::Result>
    where
        'a: 'c,
        'b: 'c,
    {
        let res: M::Result = SyncHandler::handle(self, message, ctx);
        Box::pin(futures::future::ready(res))
    }

    #[cfg(feature = "nightly")]
    type Responder<'a> = Ready<M::Result>;

    #[cfg(feature = "nightly")]
    fn handle(&mut self, message: M, ctx: &mut Context<Self>) -> Self::Responder<'_> {
        let res: M::Result = SyncHandler::handle(self, message, ctx);
        future::ready(res)
    }
}

/// An actor which can handle [`Message`s](trait.Message.html) one at a time. Actors can only be
/// communicated with by sending [`Message`s](trait.Message.html) through their [`Address`es](struct.Address.html).
/// They can modify their private state, respond to messages, and spawn other actors. They can also
/// stop themselves through their [`Context`](struct.Context.html) by calling [`Context::stop`](struct.Context.html#method.stop).
/// This will result in any attempt to send messages to the actor in future failing.
///
/// # Example
///
/// ```rust
/// # use xtra::{KeepRunning, prelude::*};
/// # use std::time::Duration;
/// # use smol::Timer;
/// struct MyActor;
///
/// impl Actor for MyActor {
///     fn started(&mut self, ctx: &mut Context<Self>) {
///         println!("Started!");
///     }
///
///     fn stopping(&mut self, ctx: &mut Context<Self>) -> KeepRunning {
///         println!("Decided not to keep running");
///         KeepRunning::No
///     }
///
///     fn stopped(&mut self, ctx: &mut Context<Self>) {
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
/// impl SyncHandler<Goodbye> for MyActor {
///     fn handle(&mut self, _: Goodbye, ctx: &mut Context<Self>) {
///         println!("Goodbye!");
///         ctx.stop();
///     }
/// }
///
/// // Will print "Started!", "Goodbye!", "Decided not to keep running", and then "Finally stopping."
/// #[smol_potat::main]
/// async fn main() {
///     let addr = MyActor.spawn();
///     addr.send(Goodbye).await;
///
///     Timer::after(Duration::from_secs(1)).await; // Give it time to run
/// }
/// ```
///
/// For longer examples, see the `examples` directory.
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
    ///
    /// # Example
    /// ```no_run
    /// # use xtra::prelude::*;
    /// # use xtra::KeepRunning;
    /// # struct MyActor { is_running: bool };
    /// # impl Actor for MyActor {
    /// fn stopping(&mut self, ctx: &mut Context<Self>) -> KeepRunning {
    ///     self.is_running.into() // bool can be converted to KeepRunning with Into
    /// }
    /// # }
    /// ```
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
    ///
    /// # Example
    ///
    /// ```rust
    /// # use xtra::{KeepRunning, prelude::*};
    /// # use std::time::Duration;
    /// # use smol::Timer;
    /// struct MyActor;
    ///
    /// impl Actor for MyActor {
    ///     fn started(&mut self, ctx: &mut Context<Self>) {
    ///         println!("Started!");
    ///     }
    /// }
    ///
    /// # struct Msg;
    /// # impl Message for Msg {
    /// #    type Result = ();
    /// # }
    /// # impl SyncHandler<Msg> for MyActor {
    /// #     fn handle(&mut self, _: Msg, _ctx: &mut Context<Self>) {}
    /// # }
    ///
    /// #[smol_potat::main]
    /// async fn main() {
    ///     let addr: Address<MyActor> = MyActor.spawn(); // Will print "Started!"
    ///     addr.do_send(Msg).unwrap();
    ///
    ///     Timer::after(Duration::from_secs(1)).await; // Give it time to run
    /// }
    /// ```
    #[cfg(any(
        doc,
        feature = "with-tokio-0_2",
        feature = "with-async_std-1",
        feature = "with-wasm_bindgen-0_2",
        feature = "with-smol-0_1"
    ))]
    #[cfg_attr(doc, doc(cfg(feature = "with-tokio-0_2")))]
    #[cfg_attr(doc, doc(cfg(feature = "with-async_std-1")))]
    #[cfg_attr(doc, doc(cfg(feature = "with-wasm_bindgen-0_2")))]
    #[cfg_attr(doc, doc(cfg(feature = "with-smol-0_1")))]
    fn spawn(self) -> Address<Self>
    where
        Self: Send,
    {
        ActorManager::spawn(self)
    }

    /// Returns the actor's address and manager in a ready-to-start state. To spawn the actor, the
    /// [`ActorManager::manage`](struct.ActorManager.html#method.manage) method must be called and
    /// the future it returns spawned onto an executor.
    /// # Example
    ///
    /// ```rust
    /// # use xtra::{KeepRunning, prelude::*};
    /// # use std::time::Duration;
    /// # use smol::Timer;
    /// # struct MyActor;
    /// # impl Actor for MyActor {}
    /// #[smol_potat::main]
    /// async fn main() {
    ///     let (addr, mgr) = MyActor.create();
    ///     smol::Task::spawn(mgr.manage()).detach(); // Actually spawn the actor onto an executor
    ///
    ///     Timer::after(Duration::from_secs(1)).await; // Give it time to run
    /// }
    /// ```
    fn create(self) -> (Address<Self>, ActorManager<Self>) {
        ActorManager::start(self)
    }
}

/// Whether to keep the actor running after it has been put into a stopping state.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum KeepRunning {
    /// Keep the actor running and prevent it from being stopped
    Yes,
    /// Stop the actor
    No,
}

impl From<bool> for KeepRunning {
    fn from(b: bool) -> Self {
        if b {
            KeepRunning::Yes
        } else {
            KeepRunning::No
        }
    }
}

impl Into<bool> for KeepRunning {
    fn into(self) -> bool {
        match self {
            KeepRunning::Yes => true,
            KeepRunning::No => false,
        }
    }
}

impl From<()> for KeepRunning {
    fn from(_: ()) -> KeepRunning {
        KeepRunning::Yes
    }
}