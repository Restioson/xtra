use std::future::Future;
use std::marker::PhantomData;
use std::ops::ControlFlow;
use std::pin::Pin;
use std::task::Poll;
use std::{mem, task};

use futures_core::future::BoxFuture;
use futures_core::FusedFuture;
use futures_util::future::{self, Either};
use futures_util::FutureExt;
#[cfg(feature = "timing")]
use {crate::Handler, futures_timer::Delay, std::time::Duration};

use crate::envelope::{HandlerSpan, Shutdown};
use crate::inbox::rx::{ReceiveFuture as InboxReceiveFuture, RxStrong};
use crate::inbox::ActorMessage;
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
    /// smol::spawn(xtra::run(ctx_a, ActorA { address: addr_b })).detach();
    /// smol::spawn(xtra::run(ctx_b, ActorB { address: addr_a })).detach();
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

    /// Get for the next message from the actor's mailbox.
    pub fn next_message(&self) -> ReceiveFuture<A> {
        ReceiveFuture(self.mailbox.receive())
    }

    /// Handle one message and return whether to exit from the manage loop or not.
    ///
    /// Note that this will immediately create the message handler span if the `instrumentation`
    /// feature is enabled.
    pub fn tick<'a>(&'a mut self, msg: Message<A>, actor: &'a mut A) -> TickFuture<'a, A> {
        TickFuture::new(msg.0, actor, self)
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

/// A message sent to a given actor, or a notification that it should shut down.
pub struct Message<A>(ActorMessage<A>);

impl<A: 'static> Message<A> {
    pub fn handle<'c>(self, actor: &'c mut A, ctx: &'c mut Context<A>) -> TickFuture<'c, A> {
        TickFuture::new(self.0, actor, ctx)
    }
}

/// A future which will resolve to the next message to be handled by the actor.
#[must_use = "Futures do nothing unless polled"]
pub struct ReceiveFuture<A>(InboxReceiveFuture<A, RxStrong>);

impl<'c, A> ReceiveFuture<A> {
    /// Cancel the receiving future, returning a message if it had been fulfilled with one, but had
    /// not yet been polled after wakeup. Future calls to `Future::poll` will return `Poll::Pending`,
    /// and `FusedFuture::is_terminated` will return `true`.
    ///
    /// This is important to do over `Drop`, as with `Drop` messages may be sent back into the
    /// channel and could be observed as received out of order, if multiple receives are concurrent
    /// on one thread.
    #[must_use = "If dropped, messages could be lost"]
    pub fn cancel(&mut self) -> Option<Message<A>> {
        self.0.cancel().map(Message)
    }
}

impl<A> Future for ReceiveFuture<A> {
    type Output = Message<A>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut task::Context<'_>) -> Poll<Self::Output> {
        self.0.poll_unpin(cx).map(Message)
    }
}

impl<A> FusedFuture for ReceiveFuture<A> {
    fn is_terminated(&self) -> bool {
        self.0.is_terminated()
    }
}

pub struct TickFuture<'a, A> {
    state: TickState<'a, A>,
    span: HandlerSpan,
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
                span: HandlerSpan(span),
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
            span: HandlerSpan(
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
