use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use futures_core::FusedFuture;
use futures_util::FutureExt;

use crate::envelope::BroadcastEnvelopeConcrete;
use crate::inbox::tx::TxRefCounter;
use crate::inbox::{SendFuture, SentMessage};
use crate::send_future::private::SetPriority;
use crate::{inbox, Error, Handler};

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
pub struct BroadcastFuture<A, Rc: TxRefCounter> {
    inner: SendFuture<A, Rc>,
}

impl<A, Rc> BroadcastFuture<A, Rc>
where
    Rc: TxRefCounter,
{
    pub(crate) fn new<M>(message: M, sender: inbox::Sender<A, Rc>) -> Self
    where
        A: Handler<M, Return = ()>,
        M: Clone + Send + Sync + 'static,
    {
        let envelope = BroadcastEnvelopeConcrete::<A, M>::new(message, 0);

        Self {
            inner: sender.send(SentMessage::msg_to_all::<M>(Arc::new(envelope))),
        }
    }

    /// Set the priority of this broadcast.
    ///
    /// By default, broadcasts are sent with a priority of 0.
    pub fn priority(mut self, priority: u32) -> Self {
        self.inner.set_priority(priority);

        self
    }
}

impl<A, Rc> Future for BroadcastFuture<A, Rc>
where
    Rc: TxRefCounter,
{
    type Output = Result<(), Error>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.get_mut().inner.poll_unpin(cx)
    }
}

impl<A, Rc> FusedFuture for BroadcastFuture<A, Rc>
where
    Rc: TxRefCounter,
    Self: Future,
{
    fn is_terminated(&self) -> bool {
        self.inner.is_terminated()
    }
}
