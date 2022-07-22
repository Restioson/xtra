use std::future::Future;
use std::mem;
use std::pin::Pin;
use std::sync::{atomic, Arc};
use std::task::{Context, Poll};

use futures_core::FusedFuture;
use futures_util::FutureExt;

use crate::envelope::BroadcastEnvelope;
use crate::inbox::tx::{TxStrong, TxWeak};
use crate::inbox::waiting_receiver::WaitingReceiver;
use crate::inbox::{ActorMessage, BroadcastQueue, Chan, Sender};

pub struct Receiver<A, Rc: RxRefCounter> {
    inner: Arc<Chan<A>>,
    broadcast_mailbox: Arc<BroadcastQueue<A>>,
    rc: Rc,
}

impl<A, Rc: RxRefCounter> Receiver<A, Rc> {
    pub fn next_broadcast_message(&self) -> Option<Arc<dyn BroadcastEnvelope<Actor = A>>> {
        self.inner.pop_broadcast_message(&self.broadcast_mailbox)
    }
}

impl<A> Receiver<A, RxStrong> {
    pub(super) fn new(inner: Arc<Chan<A>>) -> Self {
        let rc = RxStrong(());
        rc.increment(&inner);

        Receiver {
            broadcast_mailbox: inner.new_broadcast_mailbox(),
            inner,
            rc,
        }
    }
}

impl<A, Rc: RxRefCounter> Receiver<A, Rc> {
    pub fn sender(&self) -> Option<Sender<A, TxStrong>> {
        Sender::try_new_strong(self.inner.clone())
    }

    pub fn weak_sender(&self) -> Sender<A, TxWeak> {
        Sender::new_weak(self.inner.clone())
    }

    pub fn receive(&self) -> ReceiveFuture<A, Rc> {
        let receiver_with_same_broadcast_mailbox = Receiver {
            inner: self.inner.clone(),
            broadcast_mailbox: self.broadcast_mailbox.clone(),
            rc: self.rc.increment(&self.inner),
        };

        ReceiveFuture::New(receiver_with_same_broadcast_mailbox)
    }
}

impl<A, Rc: RxRefCounter> Clone for Receiver<A, Rc> {
    fn clone(&self) -> Self {
        Receiver {
            inner: self.inner.clone(),
            broadcast_mailbox: self.inner.new_broadcast_mailbox(),
            rc: self.rc.increment(&self.inner),
        }
    }
}

impl<A, Rc: RxRefCounter> Drop for Receiver<A, Rc> {
    fn drop(&mut self) {
        if self.rc.decrement(&self.inner) {
            self.inner.shutdown_waiting_senders()
        }
    }
}

pub enum ReceiveFuture<A, Rc: RxRefCounter> {
    New(Receiver<A, Rc>),
    Waiting(Waiting<A, Rc>),
    Done,
}

/// Dedicated "waiting" state for the [`ReceiveFuture`].
///
/// This type encapsulates the waiting for a notification from the channel about a new message that
/// can be received. This notification may arrive in the [`WaitingReceiver`] before we poll it again.
///
/// To avoid losing a message, this type implements [`Drop`] and re-queues the message into the
/// mailbox in such a scenario.
pub struct Waiting<A, Rc>
where
    Rc: RxRefCounter,
{
    channel_receiver: Receiver<A, Rc>,
    waiting_receiver: WaitingReceiver<A>,
}

impl<A, Rc> Future for Waiting<A, Rc>
where
    Rc: RxRefCounter,
{
    type Output = Result<ActorMessage<A>, Receiver<A, Rc>>;

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

impl<A, Rc> Drop for Waiting<A, Rc>
where
    Rc: RxRefCounter,
{
    fn drop(&mut self) {
        if let Some(msg) = self.waiting_receiver.cancel() {
            self.channel_receiver.inner.requeue_message(msg);
        }
    }
}

impl<A, Rc: RxRefCounter> Future for ReceiveFuture<A, Rc> {
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

impl<A, Rc: RxRefCounter> FusedFuture for ReceiveFuture<A, Rc> {
    fn is_terminated(&self) -> bool {
        matches!(self, ReceiveFuture::Done)
    }
}

pub trait RxRefCounter: Unpin {
    fn increment<A>(&self, inner: &Chan<A>) -> Self;
    #[must_use = "If decrement returns false, the address must be disconnected"]
    fn decrement<A>(&self, inner: &Chan<A>) -> bool;
}

pub struct RxStrong(());

impl RxRefCounter for RxStrong {
    fn increment<A>(&self, inner: &Chan<A>) -> Self {
        inner.receiver_count.fetch_add(1, atomic::Ordering::Relaxed);
        RxStrong(())
    }

    fn decrement<A>(&self, inner: &Chan<A>) -> bool {
        // Memory orderings copied from Arc::drop
        if inner.receiver_count.fetch_sub(1, atomic::Ordering::Release) != 1 {
            return false;
        }

        atomic::fence(atomic::Ordering::Acquire);
        true
    }
}

pub struct RxWeak(());

impl RxRefCounter for RxWeak {
    fn increment<A>(&self, _inner: &Chan<A>) -> Self {
        RxWeak(())
    }

    fn decrement<A>(&self, _inner: &Chan<A>) -> bool {
        false
    }
}
