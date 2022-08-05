use std::future::Future;
use std::mem;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use futures_core::FusedFuture;
use futures_util::FutureExt;

use crate::envelope::BroadcastEnvelope;
use crate::inbox::tx::{TxStrong, TxWeak};
use crate::inbox::waiting_receiver::WaitingReceiver;
use crate::inbox::{ActorMessage, BroadcastQueue, Chan, Sender};

pub struct Receiver<A> {
    inner: Arc<Chan<A>>,
    broadcast_mailbox: Arc<BroadcastQueue<A>>,
}

impl<A> Receiver<A> {
    pub fn next_broadcast_message(&self) -> Option<Arc<dyn BroadcastEnvelope<Actor = A>>> {
        self.inner
            .chan
            .lock()
            .unwrap()
            .pop_broadcast(&self.broadcast_mailbox)
    }
}

impl<A> Receiver<A> {
    pub(super) fn new(inner: Arc<Chan<A>>) -> Self {
        inner.on_receiver_created();

        Receiver {
            broadcast_mailbox: inner.new_broadcast_mailbox(),
            inner,
        }
    }
}

impl<A> Receiver<A> {
    pub fn sender(&self) -> Option<Sender<A, TxStrong>> {
        Sender::try_new_strong(self.inner.clone())
    }

    pub fn weak_sender(&self) -> Sender<A, TxWeak> {
        Sender::new_weak(self.inner.clone())
    }

    pub fn receive(&self) -> ReceiveFuture<A> {
        let receiver_with_same_broadcast_mailbox = Receiver {
            inner: self.inner.clone(),
            broadcast_mailbox: self.broadcast_mailbox.clone(),
        };
        self.inner.on_receiver_created();

        ReceiveFuture::New(receiver_with_same_broadcast_mailbox)
    }
}

impl<A> Clone for Receiver<A> {
    fn clone(&self) -> Self {
        self.inner.on_receiver_created();

        Receiver {
            inner: self.inner.clone(),
            broadcast_mailbox: self.inner.new_broadcast_mailbox(),
        }
    }
}

impl<A> Drop for Receiver<A> {
    fn drop(&mut self) {
        self.inner.on_receiver_dropped()
    }
}

pub enum ReceiveFuture<A> {
    New(Receiver<A>),
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
    channel_receiver: Receiver<A>,
    waiting_receiver: WaitingReceiver<A>,
}

impl<A> Future for Waiting<A> {
    type Output = Result<ActorMessage<A>, Receiver<A>>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();

        let result =
            match futures_util::ready!(this.waiting_receiver.poll(&this.channel_receiver, cx)) {
                None => Err(this.channel_receiver.clone()), // TODO: Optimise this clone with an `Option` where we call `take`?
                Some(msg) => Ok(msg),
            };

        Poll::Ready(result)
    }
}

impl<A> Drop for Waiting<A> {
    fn drop(&mut self) {
        if let Some(msg) = self.waiting_receiver.cancel() {
            self.channel_receiver.inner.requeue_message(msg);
        }
    }
}

impl<A> Future for ReceiveFuture<A> {
    type Output = ActorMessage<A>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<ActorMessage<A>> {
        let this = self.get_mut();

        loop {
            match mem::replace(this, ReceiveFuture::Done) {
                ReceiveFuture::New(rx) => match rx.inner.try_recv(rx.broadcast_mailbox.as_ref()) {
                    Ok(message) => return Poll::Ready(message),
                    Err(waiting) => {
                        *this = ReceiveFuture::Waiting(Waiting {
                            channel_receiver: rx,
                            waiting_receiver: waiting,
                        });
                    }
                },
                ReceiveFuture::Waiting(mut waiting) => match waiting.poll_unpin(cx) {
                    Poll::Ready(Ok(msg)) => return Poll::Ready(msg),
                    Poll::Ready(Err(rx)) => {
                        // False positive wake up, try receive again.
                        *this = ReceiveFuture::New(rx);
                    }
                    Poll::Pending => {
                        *this = ReceiveFuture::Waiting(waiting);
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
