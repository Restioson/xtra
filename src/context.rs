use std::future::Future;
use std::marker::PhantomData;
use std::ops::ControlFlow;
use std::pin::Pin;
use std::task::Poll;
use std::{mem, task};

use futures_core::future::BoxFuture;
use futures_util::FutureExt;

use crate::chan::ActorMessage;
use crate::envelope::Shutdown;
use crate::instrumentation::Span;
use crate::{Actor, Mailbox};

/// `Context` is used to control how the actor is managed and to get the actor's address from inside
/// of a message handler.
pub struct Context<'m, A> {
    pub(crate) running: bool,
    pub(crate) mailbox: &'m mut Mailbox<A>,
}

impl<'m, A: Actor> Context<'m, A> {
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
        if let Some(address) = self.mailbox.address().try_upgrade() {
            address.0.shutdown_all_receivers();
        }
    }

    /// Get a reference to the [`Mailbox`] of this actor.
    pub fn mailbox(&mut self) -> &mut Mailbox<A> {
        self.mailbox
    }
}

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
    /// # let (addr, mut mailbox) = Mailbox::unbounded();
    /// # drop(addr);
    /// # tokio::runtime::Runtime::new().unwrap().block_on(async {
    /// # actor.started(&mut mailbox).await;
    /// #
    /// # loop {
    /// # let msg = mailbox.next().await;
    ///  let mut fut = xtra::tick(msg, &mut actor, &mut mailbox);
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
        let span = mem::replace(&mut self.span, tracing::Span::none());
        *self = match mem::replace(&mut self.state, TickState::Done) {
            TickState::New { msg, act, mailbox } => TickFuture::running(msg, act, mailbox),
            state => TickFuture { state, span },
        };

        &self.span
    }

    fn running(
        msg: ActorMessage<A>,
        act: &'a mut A,
        mailbox: &'a mut Mailbox<A>,
    ) -> TickFuture<'a, A> {
        let (fut, span) = match msg {
            ActorMessage::ToOneActor(msg) => msg.handle(act, mailbox),
            ActorMessage::ToAllActors(msg) => msg.handle(act, mailbox),
            ActorMessage::Shutdown => Shutdown::<A>::handle(),
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
        mailbox: &'a mut Mailbox<A>,
    },
    Running {
        fut: BoxFuture<'a, ControlFlow<()>>,
        phantom: PhantomData<fn(&'a A)>,
    },
    Done,
}

impl<'a, A> TickFuture<'a, A> {
    pub(crate) fn new(msg: ActorMessage<A>, act: &'a mut A, mailbox: &'a mut Mailbox<A>) -> Self {
        TickFuture {
            state: TickState::New { msg, act, mailbox },
            span: Span::none(),
        }
    }
}

impl<'a, A> Future for TickFuture<'a, A> {
    type Output = ControlFlow<()>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut task::Context<'_>) -> Poll<Self::Output> {
        match mem::replace(&mut self.state, TickState::Done) {
            TickState::New { msg, act, mailbox } => {
                *self = TickFuture::running(msg, act, mailbox);
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
