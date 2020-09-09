//! xtra is a tiny, fast, and safe actor system.

#![cfg_attr(docsrs, feature(doc_cfg, external_doc))]
#![deny(unsafe_code)] // TODO(doc): reapply missing_docs

pub mod message_channel;
pub mod sink;

mod envelope;

pub mod address;
pub use address::{Address, Disconnected, WeakAddress};

mod context;
pub use context::{Context, ActorShutdown};

mod manager;
pub mod spawn;

pub use manager::ActorManager;


/// Commonly used types from xtra
pub mod prelude {
    pub use crate::address::Address;
    pub use crate::message_channel::{StrongMessageChannel, WeakMessageChannel, MessageChannel};
    pub use crate::{Actor, Context, Handler, Message};
}

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
/// asynchronously, and the logic to handle the message. If the message should be handled synchronously,
/// then the [`SyncHandler`](trait.SyncHandler.html) trait should rather be implemented.
///
/// This is an [`async_trait`](https://github.com/dtolnay/async-trait/), so implementations should
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
/// smol::block_on(async {
///         let addr = MyActor.create(None).spawn(Smol::Global);
///         assert_eq!(addr.send(Msg).await, Ok(20));
///     })
/// }
/// ```
#[async_trait::async_trait]
pub trait Handler<M: Message>: Actor {
    /// Handle a given message, returning its result.
    ///
    /// This is an [`async_trait`](https://github.com/dtolnay/async-trait/).
    /// See the trait documentation to see an example of how this method can be declared.
    async fn handle(&mut self, message: M, ctx: &mut Context<Self>) -> M::Result;
}

/// An actor which can handle [`Message`s](trait.Message.html) one at a time. Actors can only be
/// communicated with by sending [`Message`s](trait.Message.html) through their [`Address`es](struct.Address.html).
/// They can modify their private state, respond to messages, and spawn other actors. They can also
/// stop themselves through their [`Context`](struct.Context.html) by calling [`Context::stop`](struct.Context.html#method.stop).
/// This will result in any attempt to send messages to the actor in future failing.
///
/// This is an [`async_trait`](https://github.com/dtolnay/async-trait/), so implementations should
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
///         KeepRunning::No
///     }
///
///     async fn stopped(&mut self, ctx: &mut Context<Self>) {
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
///     let addr = MyActor.create(None).spawn(Smol::Global);
///     addr.send(Goodbye).await;
///
///     Timer::after(Duration::from_secs(1)).await; // Give it time to run
/// })
///
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
    /// # #[async_trait::async_trait]
    /// # impl Actor for MyActor {
    /// async fn stopping(&mut self, ctx: &mut Context<Self>) -> KeepRunning {
    ///     self.is_running.into() // bool can be converted to KeepRunning with Into
    /// }
    /// # }
    /// ```
    #[allow(unused_variables)]
    async fn stopping(&mut self, ctx: &mut Context<Self>) -> KeepRunning {
        KeepRunning::No
    }

    /// Called when the actor is in the process of stopping. This could be because
    /// [`KeepRunning::No`](enum.KeepRunning.html#variant.No) was returned from the
    /// [`Actor::stopping`](trait.Actor.html#method.stopping) method, or because there are no more
    /// strong addresses ([`Address`](struct.Address.html), as opposed to [`WeakAddress`](struct.WeakAddress.html).
    /// This should be used for any final cleanup before the actor is dropped.
    #[allow(unused_variables)]
    async fn stopped(&mut self, ctx: &mut Context<Self>) {}

    /// Returns the actor's address and manager in a ready-to-start state, given the cap for the
    /// actor's mailbox. If `None` is passed, it will be of unbounded size. To spawn the actor,
    /// the [`ActorManager::manage`](struct.ActorManager.html#method.manage) method must be called
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
    ///     let (addr, fut) = MyActor.create(None).manage();
    ///     smol::spawn(fut).detach(); // Actually spawn the actor onto an executor
    ///     Timer::after(Duration::from_secs(1)).await; // Give it time to run
    /// })
    /// ```
    fn create(self, message_cap: Option<usize>) -> ActorManager<Self> {
        let (address, ctx) = Context::new(message_cap);
        ActorManager { address, actor: self, ctx }
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
