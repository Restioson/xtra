//! xtra is a tiny, fast, and safe actor system.

#![cfg_attr(docsrs, feature(doc_cfg))]
#![deny(unsafe_code, missing_docs)]

use std::fmt;

pub use self::address::{Address, WeakAddress};
pub use self::context::Context;
pub use self::scoped_task::scoped;
pub use self::send_future::{ActorErasedSending, ActorNamedSending, Receiver, SendFuture};
pub use self::spawn::*; // Star export so we don't have to write `cfg` attributes here.

pub mod address;
mod context;
mod envelope;
mod inbox;
pub mod message_channel;
mod recv_future;
/// This module contains a way to scope a future to the lifetime of an actor, stopping it before it
/// completes if the actor it is associated with stops too.
pub mod scoped_task;
mod send_future;
mod spawn;

/// Commonly used types from xtra
pub mod prelude {
    pub use async_trait::async_trait;

    pub use crate::address::Address;
    pub use crate::context::Context;
    pub use crate::message_channel::MessageChannel;
    #[doc(no_inline)]
    pub use crate::{Actor, Handler};
}

/// This module contains types representing the strength of an address's reference counting, which
/// influences whether the address will keep the actor alive for as long as it lives.
pub mod refcount {
    pub use crate::inbox::tx::{
        TxEither as Either, TxRefCounter as RefCounter, TxStrong as Strong, TxWeak as Weak,
    };
}

/// Provides a default implementation of the [`Actor`] trait for the given type with a [`Stop`](Actor::Stop) type of `()` and empty lifecycle functions.
///
/// The [`Actor`] custom derive takes away some boilerplate for a standard actor:
///
/// ```rust
/// #[derive(xtra::Actor)]
/// pub struct MyActor;
/// #
/// # fn assert_actor<T: xtra::Actor>() { }
/// #
/// # fn main() {
/// #    assert_actor::<MyActor>()
/// # }
/// ```
/// This macro will generate the following [`Actor`] implementation:
///
/// ```rust,no_run
/// # use xtra::prelude::*;
/// pub struct MyActor;
///
/// #[async_trait]
/// impl xtra::Actor for MyActor {
///     type Stop = ();
///
///     async fn stopped(self) { }
/// }
/// ```
///
/// Please note that implementing the [`Actor`] trait is still very easy and this macro purposely does not support a plethora of usecases but is meant to handle the most common ones.
/// For example, whilst it does support actors with type parameters, lifetimes are entirely unsupported.
#[cfg(feature = "macros")]
pub use macros::Actor;

/// Defines that an [`Actor`] can handle a given message `M`.
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
/// #   #[cfg(feature = "smol")]
///     smol::block_on(async {
///         let addr = xtra::spawn_smol(MyActor, None);
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

/// An actor which can handle message one at a time. Actors can only be
/// communicated with by sending messages through their [`Address`]es.
/// They can modify their private state, respond to messages, and spawn other actors. They can also
/// stop themselves through their [`Context`] by calling [`Context::stop_self`].
/// This will result in any attempt to send messages to the actor in future failing.
///
/// This is an [`async_trait`], so implementations should be annotated `#[async_trait]`.
///
/// # Example
///
/// ```rust
/// # use xtra::prelude::*;
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
/// # #[cfg(feature = "smol")]
/// smol::block_on(async {
///     let addr = xtra::spawn_smol(MyActor, None);
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

    /// Called at the end of an actor's event loop.
    ///
    /// An actor's event loop can stop for several reasons:
    ///
    /// - The actor called [`Context::stop_self`].
    /// - An actor called [`Context::stop_all`].
    /// - The last [`Address`] with a [`Strong`](crate::refcount::Strong) reference count was dropped.
    async fn stopped(self) -> Self::Stop;
}

/// An error related to the actor system
#[derive(Clone, Eq, PartialEq, Debug)]
pub enum Error {
    /// The actor is no longer running and disconnected from the sending address.
    Disconnected,
    /// The message request operation was interrupted. This happens when the message result sender
    /// is dropped. Therefore, it should never be returned from send futures split from their
    /// receivers with [`SendFuture::split_receiver`]. This could be due to the actor's event loop
    /// being shut down, or due to a custom timeout. Unlike [`Error::Disconnected`], it does not
    /// necessarily imply that any retries or further attempts to interact with the actor will
    /// result in an error.
    Interrupted,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Disconnected => f.write_str("Actor address disconnected"),
            Error::Interrupted => f.write_str("Message request interrupted"),
        }
    }
}

impl std::error::Error for Error {}
