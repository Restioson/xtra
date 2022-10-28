use std::future::Future;
use std::marker::PhantomData;
use std::mem;
use std::ops::ControlFlow;
use std::pin::Pin;
use std::task::{Context, Poll};

use futures_core::future::BoxFuture;
use futures_util::FutureExt;

use crate::chan::ActorMessage;
use crate::envelope::Shutdown;
use crate::instrumentation::Span;
use crate::mailbox::Mailbox;
use crate::Message;

impl<A> Message<A> {
    pub fn dispatch_to(self, actor: &mut A) -> DispatchFuture<'_, A> {
        DispatchFuture::new(
            self.inner,
            actor,
            Mailbox::from_parts(self.channel, self.broadcast_mailbox),
        )
    }
}

#[must_use = "Futures do nothing unless polled"]
pub struct DispatchFuture<'a, A> {
    state: State<'a, A>,
    span: Span,
}

impl<'a, A> DispatchFuture<'a, A> {
    /// Returns a [`Span`] that covers the entire dispatching and handling of the message to the actor.
    ///
    /// This can be used to log messages into the span when required, such as if it is cancelled later due to a timeout.
    ///
    /// In case this future has not yet been polled, a new span will be created which is why this function takes `&mut self`.
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
    ///  let mut fut = msg.dispatch_to(&mut actor);
    ///  let span = fut.span().clone();
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
    pub fn span(&mut self) -> &Span {
        let span = mem::replace(&mut self.span, Span::none());
        *self = match mem::replace(&mut self.state, State::Done) {
            State::New { msg, act, mailbox } => DispatchFuture::running(msg, act, mailbox),
            state => DispatchFuture { state, span },
        };

        &self.span
    }

    fn running(msg: ActorMessage<A>, act: &'a mut A, mailbox: Mailbox<A>) -> DispatchFuture<'a, A> {
        let (fut, span) = match msg {
            ActorMessage::ToOneActor(msg) => msg.handle(act, mailbox),
            ActorMessage::ToAllActors(msg) => msg.handle(act, mailbox),
            ActorMessage::Shutdown => Shutdown::<A>::handle(),
        };

        DispatchFuture {
            state: State::Running {
                fut,
                phantom: PhantomData,
            },
            span,
        }
    }
}

enum State<'a, A> {
    New {
        msg: ActorMessage<A>,
        act: &'a mut A,
        mailbox: Mailbox<A>,
    },
    Running {
        fut: BoxFuture<'a, ControlFlow<()>>,
        phantom: PhantomData<fn(&'a A)>,
    },
    Done,
}

impl<'a, A> DispatchFuture<'a, A> {
    pub fn new(msg: ActorMessage<A>, act: &'a mut A, mailbox: Mailbox<A>) -> Self {
        DispatchFuture {
            state: State::New { msg, act, mailbox },
            span: Span::none(),
        }
    }
}

impl<'a, A> Future for DispatchFuture<'a, A> {
    type Output = ControlFlow<()>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match mem::replace(&mut self.state, State::Done) {
            State::New { msg, act, mailbox } => {
                *self = DispatchFuture::running(msg, act, mailbox);
                self.poll(cx)
            }
            State::Running { mut fut, phantom } => {
                match self.span.in_scope(|| fut.poll_unpin(cx)) {
                    Poll::Ready(flow) => Poll::Ready(flow),
                    Poll::Pending => {
                        self.state = State::Running { fut, phantom };
                        Poll::Pending
                    }
                }
            }
            State::Done => panic!("Polled after completion"),
        }
    }
}
