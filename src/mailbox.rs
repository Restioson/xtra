use std::future::Future;
use std::mem;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use futures_core::FusedFuture;
use futures_util::FutureExt;

use crate::inbox::{ActorMessage, BroadcastQueue, ChanPtr, Rx, TxStrong, TxWeak, WaitingReceiver};

pub struct Mailbox<A> {
    inner: ChanPtr<A, Rx>,
    broadcast_mailbox: Arc<BroadcastQueue<A>>,
}

impl<A> Mailbox<A> {
    pub(super) fn new(inner: ChanPtr<A, Rx>) -> Self {
        Mailbox {
            broadcast_mailbox: inner.new_broadcast_mailbox(),
            inner,
        }
    }

    pub fn sender(&self) -> Option<ChanPtr<A, TxStrong>> {
        self.inner.try_to_tx_strong()
    }
}

impl<A> Mailbox<A> {
    pub fn weak_sender(&self) -> ChanPtr<A, TxWeak> {
        self.inner.to_tx_weak()
    }

    pub fn receive(&self) -> ReceiveFuture<A> {
        ReceiveFuture::New {
            channel: self.inner.clone(),
            broadcast_mailbox: self.broadcast_mailbox.clone(),
        }
    }
}

impl<A> Clone for Mailbox<A> {
    fn clone(&self) -> Self {
        Mailbox {
            inner: self.inner.clone(),
            broadcast_mailbox: self.inner.new_broadcast_mailbox(),
        }
    }
}

pub enum ReceiveFuture<A> {
    New {
        channel: ChanPtr<A, Rx>,
        broadcast_mailbox: Arc<BroadcastQueue<A>>,
    },
    Waiting(Waiting<A>),
    Done,
}

/// Dedicated "waiting" state for the [`ReceiveFuture`].
///
/// This type encapsulates the waiting for a notification from the channel about a new message that
/// can be received. This notification may arrive in the [`WaitingReceiver`] before we poll it again.
///
/// To avoid losing a message, this type implements [`Drop`] and re-queues the message into the
/// mailbox in such a scenario.
pub struct Waiting<A> {
    channel: ChanPtr<A, Rx>,
    broadcast_mailbox: Arc<BroadcastQueue<A>>,
    waiting_receiver: WaitingReceiver<A>,
}

impl<A> Future for Waiting<A> {
    type Output = Result<ActorMessage<A>, (ChanPtr<A, Rx>, Arc<BroadcastQueue<A>>)>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();

        let result = match futures_util::ready!(this.waiting_receiver.poll(
            &this.channel,
            &this.broadcast_mailbox,
            cx
        )) {
            None => Err((this.channel.clone(), this.broadcast_mailbox.clone())), // TODO: Optimise this clone with an `Option` where we call `take`?
            Some(msg) => Ok(msg),
        };

        Poll::Ready(result)
    }
}

impl<A> Drop for Waiting<A> {
    fn drop(&mut self) {
        if let Some(msg) = self.waiting_receiver.cancel() {
            self.channel.requeue_message(msg);
        }
    }
}

impl<A> Future for ReceiveFuture<A> {
    type Output = ActorMessage<A>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<ActorMessage<A>> {
        let this = self.get_mut();

        loop {
            match mem::replace(this, ReceiveFuture::Done) {
                ReceiveFuture::New {
                    channel,
                    broadcast_mailbox,
                } => match channel.try_recv(broadcast_mailbox.as_ref()) {
                    Ok(message) => return Poll::Ready(message),
                    Err(waiting) => {
                        *this = ReceiveFuture::Waiting(Waiting {
                            channel,
                            broadcast_mailbox,
                            waiting_receiver: waiting,
                        });
                    }
                },
                ReceiveFuture::Waiting(mut inner) => match inner.poll_unpin(cx) {
                    Poll::Ready(Ok(msg)) => return Poll::Ready(msg),
                    Poll::Ready(Err((channel, broadcast_mailbox))) => {
                        // False positive wake up, try receive again.
                        *this = ReceiveFuture::New {
                            channel,
                            broadcast_mailbox,
                        };
                    }
                    Poll::Pending => {
                        *this = ReceiveFuture::Waiting(inner);
                        return Poll::Pending;
                    }
                },
                ReceiveFuture::Done => panic!("polled after completion"),
            }
        }
    }
}

impl<A> FusedFuture for ReceiveFuture<A> {
    fn is_terminated(&self) -> bool {
        matches!(self, ReceiveFuture::Done)
    }
}
