use std::future::Future;
use std::marker::PhantomData;
use std::ops::ControlFlow;
use std::pin::Pin;
use std::task::Poll;
use std::{mem, task};

use futures_core::future::BoxFuture;
use futures_util::future::{self, Either};
use futures_util::FutureExt;

use crate::envelope::{Shutdown, Span};
use crate::inbox::rx::{RxRefCounter, RxStrong};
use crate::inbox::ActorMessage;
use crate::recv_future::{Message, ReceiveFuture};
use crate::{inbox, Actor, Address, Error, WeakAddress};

/// `Context` is used to control how the actor is managed and to get the actor's address from inside
/// of a message handler. Keep in mind that if a free-floating `Context` (i.e not running an actor via
/// [`Context::run`] exists, **it will prevent the actor's channel from being closed**, as more
/// actors that could still then be added to the address, so closing early, while maybe intuitive,
/// would be subtly wrong.
pub struct Context<A> {
    /// Whether this actor is running. If set to `false`, [`Context::tick`] will return
    /// `ControlFlow::Break` and [`Context::run`] will shut down the actor. This will not result
    /// in other actors on the same address stopping, though - [`Context::stop_all`] must be used
    /// to achieve this.
    pub running: bool,
    /// The actor's mailbox.
    mailbox: inbox::Receiver<A, RxStrong>,
}

impl<A: Actor> Context<A> {
    /// Creates a new actor context with a given mailbox capacity, returning an address to the actor
    /// and the context.
    ///
    /// This can be used to e.g. construct actors with cyclic dependencies.
    ///
    /// # Example
    ///
    /// ```rust
    /// # use xtra::prelude::*;
    /// #
    /// struct ActorA {
    ///     address: Address<ActorB>
    /// }
    ///
    /// struct ActorB {
    ///     address: Address<ActorA>
    /// }
    ///
    /// # #[async_trait] impl Actor for ActorA {type Stop = (); async fn stopped(self) -> Self::Stop {} }
    /// # #[async_trait] impl Actor for ActorB {type Stop = (); async fn stopped(self) -> Self::Stop {} }
    /// # async { // This does not actually run because there is nothing to assert
    /// let (addr_a, ctx_a) = Context::<ActorA>::new(Some(5));
    /// let (addr_b, ctx_b) = Context::<ActorB>::new(Some(5));
    ///
    /// smol::spawn(ctx_a.run(ActorA { address: addr_b })).detach();
    /// smol::spawn(ctx_b.run(ActorB { address: addr_a })).detach();
    /// # };
    ///
    /// ```
    pub fn new(message_cap: Option<usize>) -> (Address<A>, Self) {
        let (tx, rx) = inbox::new(message_cap);

        let context = Context {
            running: true,
            mailbox: rx,
        };

        (Address(tx), context)
    }

    /// Stop this actor as soon as it has finished processing current message. This means that the
    /// [`Actor::stopped`] method will be called. This will not stop all actors on the address.
    pub fn stop_self(&mut self) {
        self.running = false;
    }

    /// Stop all actors on this address.
    ///
    /// This is equivalent to calling [`Context::stop_self`] on all actors active on this address.
    pub fn stop_all(&mut self) {
        // We only need to shut down if there are still any strong senders left
        if let Some(sender) = self.mailbox.sender() {
            sender.stop_all_receivers();
        }
    }

    /// Get an address to the current actor if there are still external addresses to the actor.
    pub fn address(&self) -> Result<Address<A>, Error> {
        self.mailbox
            .sender()
            .ok_or(Error::Disconnected)
            .map(Address)
    }

    /// Get a weak address to the current actor.
    pub fn weak_address(&self) -> WeakAddress<A> {
        Address(self.mailbox.weak_sender())
    }

    /// Run the given actor's main loop, handling incoming messages to its mailbox.
    pub async fn run(mut self, mut actor: A) -> A::Stop {
        actor.started(&mut self).await;

        // Idk why anyone would do this, but we have to check that they didn't already stop the actor
        // in the started method, otherwise it would kinda be a bug
        if !self.running {
            return actor.stopped().await;
        }

        loop {
            match self.yield_once(&mut actor).await {
                ControlFlow::Continue(()) => {}
                ControlFlow::Break(()) => {
                    return actor.stopped().await;
                }
            }
        }
    }

    /// Get for the next message from the actor's mailbox.
    pub fn next_message(&self) -> ReceiveFuture<A> {
        ReceiveFuture::new(
            self.mailbox.inner.clone(),
            self.mailbox.broadcast_mailbox.clone(), // It is important to clone the `Arc` here otherwise the future will read from a new broadcast mailbox.
            self.mailbox.rc.increment(self.mailbox.inner.as_ref()),
        )
    }

    /// Handle one message and return whether to exit from the manage loop or not.
    ///
    /// Note that this will immediately create the message handler span if the `instrumentation`
    /// feature is enabled.
    pub fn tick<'a>(&'a mut self, msg: Message<A>, actor: &'a mut A) -> TickFuture<'a, A> {
        TickFuture::new(msg.0, actor, self)
    }

    /// Yields to the manager to handle one message, returning the actor should be shut down or not.
    pub async fn yield_once(&mut self, act: &mut A) -> ControlFlow<()> {
        self.tick(self.next_message().await, act).await
    }

    /// Joins on a future by handling all incoming messages whilst polling it. The future will
    /// always be polled to completion, even if the actor is stopped. If the actor is stopped,
    /// handling of messages will cease, and only the future will be polled. It is somewhat
    /// analagous to [`futures::join`](futures_util::future::join), but it will not wait for the
    /// incoming stream of messages from addresses to end before returning - it will return as soon
    /// as the provided future does. It will never cancel the handling of a message, even if the
    /// provided future completes first.
    ///
    /// This function explicitly breaks the invariant that an actor only handles one message at a
    /// time, as it lets other messages be handled inside of a handler. It is recommended to make
    /// sure that all internal state is finalised and consistent before calling this method.
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
    ///         let addr = ctx.address().unwrap();
    ///         let join = ctx.join(self, future::ready::<()>(()));
    ///         let _ = addr.send(Stop).split_receiver().await;
    ///
    ///         // Actor is stopping, but the join should still evaluate correctly
    ///         join.now_or_never().is_some()
    ///     }
    /// }
    ///
    /// # #[cfg(feature = "smol")]
    /// # smol::block_on(async {
    /// let addr = xtra::spawn_smol(MyActor, None);
    /// assert!(addr.is_connected());
    /// assert_eq!(addr.send(Joining).await, Ok(true)); // Assert that the join did evaluate the future
    /// # })
    pub async fn join<F, R>(&mut self, actor: &mut A, fut: F) -> R
    where
        F: Future<Output = R>,
    {
        futures_util::pin_mut!(fut);
        match self.select(actor, fut).await {
            Either::Left(res) => res,
            Either::Right(fut) => fut.await,
        }
    }

    /// Handle any incoming messages for the actor while running a given future. This is similar to
    /// [`Context::join`], but will exit if the actor is stopped, returning the future. Returns
    /// `Ok` with the result of the future if it was successfully completed, or `Err` with the
    /// future if the actor was stopped before it could complete. In this way it is analagous to
    /// [`futures::select`](futures_util::future::select). However, it will never cancel the current
    /// message it is handling whilst waiting for the provided future, even if the provided future
    /// completes first.
    ///
    /// This function explicitly breaks the invariant that an actor only handles one message at a
    /// time, as it lets other messages be handled inside of a handler. It is recommended to make
    /// sure that all internal state is finalised and consistent before calling this method.
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
    ///         match ctx.select(self, future::ready(1)).await {
    ///             Either::Left(ans) => println!("Answer is: {}", ans),
    ///             Either::Right(_) => panic!("How did we get here?"),
    ///         };
    ///
    ///         let addr = ctx.address().unwrap();
    ///         let select = ctx.select(self, future::pending::<()>());
    ///         let _ = addr.send(Stop).split_receiver().await;
    ///
    ///         // Actor is stopping, so this will return Err, even though the future will
    ///         // usually never complete.
    ///         matches!(select.await, Either::Right(_))
    ///     }
    /// }
    ///
    /// # #[cfg(feature = "smol")]
    /// # smol::block_on(async {
    /// let addr = xtra::spawn_smol(MyActor, None);
    /// assert!(addr.is_connected());
    /// assert_eq!(addr.send(Selecting).await, Ok(true)); // Assert that the select did end early
    /// # })
    ///
    /// ```
    pub async fn select<F, R>(&mut self, actor: &mut A, mut fut: F) -> Either<R, F>
    where
        F: Future<Output = R> + Unpin,
    {
        while self.running {
            let (msg, unfinished) = {
                let mut next_msg = self.next_message();
                match future::select(fut, &mut next_msg).await {
                    Either::Left((future_res, _)) => {
                        // Check if `ReceiveFuture` was in the process of handling a message.
                        // If yes, dispatch it, otherwise continue.
                        if let Some(message) = next_msg.now_or_never() {
                            self.tick(message, actor).await;
                        }

                        return Either::Left(future_res);
                    }
                    Either::Right(tuple) => tuple,
                }
            };

            self.tick(msg, actor).await;
            fut = unfinished;
        }

        Either::Right(fut)
    }

    pub(crate) fn flow(&self) -> ControlFlow<()> {
        if self.running {
            ControlFlow::Continue(())
        } else {
            ControlFlow::Break(())
        }
    }
}

// Need this manual impl to avoid `A: Clone` bound.
impl<A> Clone for Context<A> {
    fn clone(&self) -> Self {
        Self {
            running: self.running,
            mailbox: self.mailbox.clone(),
        }
    }
}

#[must_use = "Futures do nothing unless polled"]
pub struct TickFuture<'a, A> {
    state: TickState<'a, A>,
    span: Span,
}

impl<'a, A> TickFuture<'a, A> {
    /// Return the handler's [`tracing::Span`](https://docs.rs/tracing/latest/tracing/struct.Span.html),
    /// creating it if it has not already been created. This can be used to log messages into the
    /// span when required, such as if it is cancelled later due to a timeout.
    ///
    /// ```rust
    /// # use std::ops::ControlFlow;
    /// # use std::time::Duration;
    /// # use tokio::time::timeout;
    /// # use xtra::prelude::*;
    /// #
    /// # struct MyActor;
    /// # #[async_trait::async_trait] impl Actor for MyActor { type Stop = (); async fn stopped(self) {} }
    /// #
    /// # let mut actor = MyActor;
    /// # let (addr, mut ctx) = Context::<MyActor>::new(None);
    /// # drop(addr);
    /// # tokio::runtime::Runtime::new().unwrap().block_on(async {
    /// # actor.started(&mut ctx).await;
    /// #
    /// # loop {
    /// # let msg = ctx.next_message().await;
    ///  let mut fut = ctx.tick(msg, &mut actor);
    ///  let span = fut.get_or_create_span().clone();
    ///  match timeout(Duration::from_secs(1), fut).await {
    ///      Ok(ControlFlow::Continue(())) => (),
    ///      Ok(ControlFlow::Break(())) => break actor.stopped().await,
    ///      Err(_elapsed) => {
    ///          let _entered = span.enter();
    ///          span.record("interrupted", &"timed_out");
    ///          tracing::warn!(timeout_seconds = 1, "Handler execution timed out");
    ///      }
    ///  }
    /// # } })
    /// ```
    ///
    #[cfg(feature = "instrumentation")]
    pub fn get_or_create_span(&mut self) -> &tracing::Span {
        let span = mem::replace(&mut self.span.0, tracing::Span::none());
        *self = match mem::replace(&mut self.state, TickState::Done) {
            TickState::New { msg, act, ctx } => TickFuture::running(msg, act, ctx),
            state => TickFuture {
                state,
                span: Span(span),
            },
        };

        &self.span.0
    }

    fn running(msg: ActorMessage<A>, act: &'a mut A, ctx: &'a mut Context<A>) -> TickFuture<'a, A> {
        let (fut, span) = match msg {
            ActorMessage::ToOneActor(msg) => msg.handle(act, ctx),
            ActorMessage::ToAllActors(msg) => msg.handle(act, ctx),
            ActorMessage::Shutdown => Shutdown::handle(ctx),
        };

        TickFuture {
            state: TickState::Running {
                fut,
                phantom: PhantomData,
            },
            span,
        }
    }
}

enum TickState<'a, A> {
    New {
        msg: ActorMessage<A>,
        act: &'a mut A,
        ctx: &'a mut Context<A>,
    },
    Running {
        fut: BoxFuture<'a, ControlFlow<()>>,
        phantom: PhantomData<fn(&'a A)>,
    },
    Done,
}

impl<'a, A> TickFuture<'a, A> {
    fn new(msg: ActorMessage<A>, act: &'a mut A, ctx: &'a mut Context<A>) -> Self {
        TickFuture {
            state: TickState::New { msg, act, ctx },
            span: Span(
                #[cfg(feature = "instrumentation")]
                tracing::Span::none(),
            ),
        }
    }
}

impl<'a, A> Future for TickFuture<'a, A> {
    type Output = ControlFlow<()>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut task::Context<'_>) -> Poll<Self::Output> {
        match mem::replace(&mut self.state, TickState::Done) {
            TickState::New { msg, act, ctx } => {
                *self = TickFuture::running(msg, act, ctx);
                self.poll(cx)
            }
            TickState::Running { mut fut, phantom } => {
                match self.span.in_scope(|| fut.poll_unpin(cx)) {
                    Poll::Ready(flow) => Poll::Ready(flow),
                    Poll::Pending => {
                        self.state = TickState::Running { fut, phantom };
                        Poll::Pending
                    }
                }
            }
            TickState::Done => panic!("Polled after completion"),
        }
    }
}
