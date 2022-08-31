//! xtra is a tiny, fast, and safe actor system.

#![cfg_attr(docsrs, feature(doc_cfg))]
#![deny(unsafe_code, missing_docs)]

use std::fmt;
use std::future::Future;
use std::ops::ControlFlow;

use futures_util::future::Either;
use futures_util::{future, FutureExt};

pub use self::address::{Address, WeakAddress};
pub use self::context::Context;
pub use self::mailbox::Mailbox;
pub use self::scoped_task::scoped;
pub use self::send_future::{ActorErasedSending, ActorNamedSending, Receiver, SendFuture};
pub use self::spawn::*;
use crate::context::TickFuture; // Star export so we don't have to write `cfg` attributes here.

pub mod address;
mod context;
mod envelope;
mod inbox;
mod instrumentation;
mod mailbox;
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
    pub use crate::{Actor, Handler, Mailbox};
}

/// This module contains types representing the strength of an address's reference counting, which
/// influences whether the address will keep the actor alive for as long as it lives.
pub mod refcount {
    pub use crate::inbox::{RefCounter, TxEither as Either, TxStrong as Strong, TxWeak as Weak};
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

use crate::recv_future::Message;

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
///         let addr = xtra::spawn_smol(MyActor, Mailbox::unbounded());
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
///     async fn started(&mut self, ctx: &mut Mailbox<Self>) {
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
///     let addr = xtra::spawn_smol(MyActor, Mailbox::unbounded());
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
    async fn started(&mut self, mailbox: &mut Mailbox<Self>) {}

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

/// Run the provided actor.
///
/// This is the primary event loop of an actor which takes messages out of the mailbox and hands
/// them to the actor.
pub async fn run<A>(mut mailbox: Mailbox<A>, mut actor: A) -> A::Stop
where
    A: Actor,
{
    actor.started(&mut mailbox).await;

    while let ControlFlow::Continue(()) = yield_once(&mut mailbox, &mut actor).await {}

    actor.stopped().await
}

/// Yields to the manager to handle one message, returning the actor should be shut down or not.
pub async fn yield_once<A>(mailbox: &mut Mailbox<A>, actor: &mut A) -> ControlFlow<(), ()>
where
    A: Actor,
{
    let message = mailbox.next().await;

    tick(message, actor, mailbox).await
}

/// Handle one message and return whether to exit from the manage loop or not.
///
/// Note that this will immediately create the message handler span if the `instrumentation`
/// feature is enabled.
pub fn tick<'a, A>(
    message: Message<A>,
    actor: &'a mut A,
    mailbox: &'a mut Mailbox<A>,
) -> TickFuture<'a, A>
where
    A: Actor,
{
    TickFuture::new(message.0, actor, mailbox)
}

/// Handle any incoming messages for the actor while running a given future. This is similar to
/// [`join`], but will exit if the actor is stopped, returning the future. Returns
/// `Ok` with the result of the future if it was successfully completed, or `Err` with the
/// future if the actor was stopped before it could complete. It is analagous to
/// [`futures::select`](futures_util::future::select).
///
/// ## Example
///
/// ```rust
/// # use std::time::Duration;
/// use futures_util::future::Either;
/// # use xtra::prelude::*;
/// # use smol::future;
/// # struct MyActor;
/// # #[async_trait] impl Actor for MyActor { type Stop = (); async fn stopped(self) {} }
///
/// struct Stop;
/// struct Selecting;
///
/// #[async_trait]
/// impl Handler<Stop> for MyActor {
///     type Return = ();
///
///     async fn handle(&mut self, _msg: Stop, ctx: &mut Context<Self>) {
///         ctx.stop_self();
///     }
/// }
///
/// #[async_trait]
/// impl Handler<Selecting> for MyActor {
///     type Return = bool;
///
///     async fn handle(&mut self, _msg: Selecting, ctx: &mut Context<Self>) -> bool {
///         // Actor is still running, so this will return Either::Left
///         match xtra::select(ctx.mailbox(), self, future::ready(1)).await {
///             Either::Left(ans) => println!("Answer is: {}", ans),
///             Either::Right(_) => panic!("How did we get here?"),
///         };
///
///         let addr = ctx.mailbox().address();
///         let select = xtra::select(ctx.mailbox(), self, future::pending::<()>());
///         let _ = addr.send(Stop).split_receiver().await;
///
///         // Actor is stopping, so this will return Err, even though the future will
///         // usually never complete.
///         matches!(select.await, Either::Right(_))
///     }
/// }
///
/// # #[cfg(feature = "with-smol-1")]
/// # smol::block_on(async {
/// let addr = MyActor.create(None).spawn(&mut xtra::spawn::Smol::Global);
/// assert!(addr.is_connected());
/// assert_eq!(addr.send(Selecting).await, Ok(true)); // Assert that the select did end early
/// # })
///
/// ```
pub async fn select<A, F, R>(mailbox: &mut Mailbox<A>, actor: &mut A, mut fut: F) -> Either<R, F>
where
    F: Future<Output = R> + Unpin,
    A: Actor,
{
    let mut control_flow = ControlFlow::Continue(());

    while control_flow.is_continue() {
        let (msg, unfinished) = {
            let mut next_msg = mailbox.next();
            match future::select(fut, &mut next_msg).await {
                Either::Left((future_res, _)) => {
                    if let Some(msg) = next_msg.now_or_never() {
                        TickFuture::new(msg.0, actor, mailbox).await;
                    }

                    return Either::Left(future_res);
                }
                Either::Right(tuple) => tuple,
            }
        };

        control_flow = TickFuture::new(msg.0, actor, mailbox).await;
        fut = unfinished;
    }

    Either::Right(fut)
}

/// Joins on a future by handling all incoming messages whilst polling it. The future will
/// always be polled to completion, even if the actor is stopped. If the actor is stopped,
/// handling of messages will cease, and only the future will be polled. It is somewhat
/// analagous to [`futures::join`](futures_util::future::join), but it will not wait for the incoming stream of messages
/// from addresses to end before returning - it will return as soon as the provided future does.
///
/// ## Example
///
/// ```rust
/// # use std::time::Duration;
/// use futures_util::FutureExt;
/// # use xtra::prelude::*;
/// # use smol::future;
/// # struct MyActor;
/// # #[async_trait] impl Actor for MyActor { type Stop = (); async fn stopped(self) {} }
///
/// struct Stop;
/// struct Joining;
///
/// #[async_trait]
/// impl Handler<Stop> for MyActor {
///     type Return = ();
///
///     async fn handle(&mut self, _msg: Stop, ctx: &mut Context<Self>) {
///         ctx.stop_self();
///     }
/// }
///
/// #[async_trait]
/// impl Handler<Joining> for MyActor {
///     type Return = bool;
///
///     async fn handle(&mut self, _msg: Joining, ctx: &mut Context<Self>) -> bool {
///         let addr = ctx.mailbox().address();
///         let join = xtra::join(ctx.mailbox(), self, future::ready::<()>(()));
///         let _ = addr.send(Stop).split_receiver().await;
///
///         // Actor is stopping, but the join should still evaluate correctly
///         join.now_or_never().is_some()
///     }
/// }
///
/// # #[cfg(feature = "with-smol-1")]
/// # smol::block_on(async {
/// let addr = MyActor.create(None).spawn(&mut xtra::spawn::Smol::Global);
/// assert!(addr.is_connected());
/// assert_eq!(addr.send(Joining).await, Ok(true)); // Assert that the join did evaluate the future
/// # })
pub async fn join<A, F, R>(mailbox: &mut Mailbox<A>, actor: &mut A, fut: F) -> R
where
    F: Future<Output = R>,
    A: Actor,
{
    futures_util::pin_mut!(fut);
    match select(mailbox, actor, fut).await {
        Either::Left(res) => res,
        Either::Right(fut) => fut.await,
    }
}
