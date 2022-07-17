use std::future::Future;
use std::mem;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use futures_core::FusedFuture;
use futures_util::FutureExt;

use crate::envelope::BroadcastEnvelopeConcrete;
use crate::inbox::{Chan, SendFuture, SentMessage};
use crate::{Error, Handler};

/// A [`Future`] that represents the state of broadcasting a message to all actors connected to an
/// [`Address`](crate::Address).
///
/// By default, broadcasts happen with a priority of `0`. This can be configured via
/// [`BroadcastFuture::priority`]. Messages will be processed by actors from high to low priority on
/// a _best-effort_ basis, i.e. there is no strict guarantee due to various pathological cases.
///
/// This future resolves once the message has been queued in the broadcast queue. In
/// case the mailbox of an actor is bounded, this future yields `Pending` until a slot for this
/// message is available.
#[must_use = "Futures do nothing unless polled"]
pub struct BroadcastFuture<A, M> {
    inner: Inner<A, M>,
}

impl<A, M> BroadcastFuture<A, M> {
    pub(crate) fn new(message: M, chan: Arc<Chan<A>>) -> Self {
        Self {
            inner: Inner::Initial {
                message,
                chan,
                priority: None,
            },
        }
    }

    /// Set the priority of this broadcast.
    ///
    /// By default, broadcasts are sent with a priority of 0.
    pub fn priority(self, priority: u32) -> Self {
        match self.inner {
            Inner::Initial {
                message,
                chan: sender,
                ..
            } => Self {
                inner: Inner::Initial {
                    message,
                    chan: sender,
                    priority: Some(priority),
                },
            },
            _ => panic!("setting priority after polling is unsupported"),
        }
    }
}

enum Inner<A, M> {
    Initial {
        message: M,
        chan: Arc<Chan<A>>,
        priority: Option<u32>,
    },
    Sending(SendFuture<A>),
    Done,
}

impl<A, M> Future for BroadcastFuture<A, M>
where
    M: Clone + Send + Sync + 'static + Unpin,
    A: Handler<M, Return = ()>,
{
    type Output = Result<(), Error>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();

        match mem::replace(&mut this.inner, Inner::Done) {
            Inner::Initial {
                message,
                chan,
                priority,
            } => {
                let envelope =
                    BroadcastEnvelopeConcrete::<A, M>::new(message, priority.unwrap_or(0));
                this.inner = Inner::Sending(SendFuture::New {
                    chan,
                    msg: SentMessage::ToAllActors(Arc::new(envelope)),
                });
                this.poll_unpin(cx)
            }
            Inner::Sending(mut send_fut) => match send_fut.poll_unpin(cx) {
                Poll::Ready(result) => Poll::Ready(result),
                Poll::Pending => {
                    this.inner = Inner::Sending(send_fut);
                    Poll::Pending
                }
            },
            Inner::Done => {
                panic!("Polled after completion")
            }
        }
    }
}

impl<A, M> FusedFuture for BroadcastFuture<A, M>
where
    Self: Future,
{
    fn is_terminated(&self) -> bool {
        matches!(self.inner, Inner::Done)
    }
}
