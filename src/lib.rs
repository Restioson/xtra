//! xtra is a tiny, fast, and safe actor system.

#![cfg_attr(docsrs, feature(doc_cfg, external_doc))]
#![deny(unsafe_code, missing_docs)]

pub use self::address::{Address, Disconnected, WeakAddress};
pub use self::context::{ActorShutdown, Context};
pub use self::manager::ActorManager;
pub use self::receiver::Receiver;
pub use self::send_future::NameableSending;
pub use self::send_future::SendFuture;

pub mod address;
mod context;
mod drop_notice;
mod envelope;
mod manager;
pub mod message_channel;
mod receiver;
/// This module contains types representing the strength of an address's reference counting, which
/// influences whether the address will keep the actor alive for as long as it lives.
pub mod refcount;
mod send_future;
pub mod sink;
/// This module contains a trait to spawn actors, implemented for all major async runtimes by default.
pub mod spawn;
#[cfg(feature = "with-tracing-0_1")]
/// Integration with [`tracing`](https://tracing.rs).
pub mod tracing;

/// Commonly used types from xtra
pub mod prelude {
    pub use crate::address::Address;
    pub use crate::context::Context;
    pub use crate::message_channel::{MessageChannel, StrongMessageChannel, WeakMessageChannel};
    #[cfg(feature = "with-tracing-0_1")]
    pub use crate::tracing::InstrumentedExt;
    #[doc(no_inline)]
    pub use crate::{Actor, Handler};

    pub use async_trait::async_trait;
}

/// A trait indicating that an [`Actor`](trait.Actor.html) can handle a given [`Message`](trait.Message.html)
/// asynchronously, and the logic to handle the message.
///
/// This is an [`async_trait`](https://docs.rs/async-trait), so implementations should
/// be annotated `#[async_trait]`.
///
/// # Example
///
/// ```rust
/// # use xtra::prelude::*;
/// # struct MyActor;
/// # #[async_trait] impl Actor for MyActor {type Stop = (); async fn stopped(self) -> Self::Stop {} }
/// struct Msg;
///
/// #[async_trait]
/// impl Handler<Msg> for MyActor {
///     type Return = u32;
///
///     async fn handle(&mut self, message: Msg, ctx: &mut Context<Self>) -> u32 {
///         20
///     }
/// }
///
/// fn main() {
/// #   #[cfg(feature = "with-smol-1")]
///     smol::block_on(async {
///         let addr = MyActor.create(None).spawn(&mut xtra::spawn::Smol::Global);
///         assert_eq!(addr.send(Msg).await, Ok(20));
///     })
/// }
/// ```
#[async_trait::async_trait]
pub trait Handler<M>: Actor {
    /// The return value of this handler.
    type Return: Send + 'static;

    /// Handle a given message, returning its result.
    ///
    /// This is an [`async_trait`](https://docs.rs/async-trait).
    /// See the trait documentation to see an example of how this method can be declared.
    async fn handle(&mut self, message: M, ctx: &mut Context<Self>) -> Self::Return;
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
/// # use std::time::Duration;
/// struct MyActor;
///
/// #[async_trait]
/// impl Actor for MyActor {
///     type Stop = ();
///     async fn started(&mut self, ctx: &mut Context<Self>) {
///         println!("Started!");
///     }
///
///     async fn stopped(self) -> Self::Stop {
///         println!("Finally stopping.");
///     }
/// }
///
/// struct Goodbye;
///
/// #[async_trait]
/// impl Handler<Goodbye> for MyActor {
///     type Return = ();
///
///     async fn handle(&mut self, _: Goodbye, ctx: &mut Context<Self>) {
///         println!("Goodbye!");
///         ctx.stop_all();
///     }
/// }
///
/// // Will print "Started!", "Goodbye!", and then "Finally stopping."
/// # #[cfg(feature = "with-smol-1")]
/// smol::block_on(async {
///     let addr = MyActor.create(None).spawn(&mut xtra::spawn::Smol::Global);
///     addr.send(Goodbye).await;
///
///     smol::Timer::after(Duration::from_secs(1)).await; // Give it time to run
/// })
/// ```
///
/// For longer examples, see the `examples` directory.
#[async_trait::async_trait]
pub trait Actor: 'static + Send + Sized {
    /// Value returned from the actor when [`Actor::stopped`] is called.
    type Stop: Send + 'static;

    /// Called as soon as the actor has been started.
    #[allow(unused_variables)]
    async fn started(&mut self, ctx: &mut Context<Self>) {}

    /// Called when the actor is in the process of stopping. This could be because
    /// [`KeepRunning::StopAll`](enum.KeepRunning.html#variant.StopAll) or
    /// [`KeepRunning::StopSelf`](enum.KeepRunning.html#variant.StopSelf) was returned from the
    /// [`Actor::stopping`](trait.Actor.html#method.stopping) method, or because there are no more
    /// strong addresses ([`Address`](address/struct.Address.html), as opposed to
    /// [`WeakAddress`](address/type.WeakAddress.html). This should be used for any final cleanup before
    /// the actor is dropped.
    #[allow(unused_variables)]
    async fn stopped(self) -> Self::Stop;

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
    /// # #[async_trait] impl Actor for MyActor {type Stop = (); async fn stopped(self) -> Self::Stop {} }
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

/// Whether or not to keep an attached stream forwarding.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum KeepRunning {
    /// Keep forwarding
    Yes,
    /// Stop the forwarding task
    No,
}

/// True is converted to yes, and false is converted to no.
impl From<bool> for KeepRunning {
    fn from(b: bool) -> Self {
        if b {
            KeepRunning::Yes
        } else {
            KeepRunning::No
        }
    }
}

impl From<()> for KeepRunning {
    fn from(_: ()) -> KeepRunning {
        KeepRunning::Yes
    }
}

mod private {
    use crate::refcount::{Either, RefCounter, Strong, Weak};
    use crate::sink::AddressSink;
    use crate::{Actor, Address};

    pub trait Sealed {}

    impl Sealed for Strong {}
    impl Sealed for Weak {}
    impl Sealed for Either {}
    impl<A: Actor, Rc: RefCounter> Sealed for Address<A, Rc> {}
    impl<A: Actor, Rc: RefCounter> Sealed for AddressSink<A, Rc> {}
}
