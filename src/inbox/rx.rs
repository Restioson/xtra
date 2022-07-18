use std::future::Future;
use std::mem;
use std::pin::Pin;
use std::sync::{atomic, Arc};
use std::task::{Context, Poll};

use futures_core::FusedFuture;

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

        ReceiveFuture::new(receiver_with_same_broadcast_mailbox)
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

#[must_use = "Futures do nothing unless polled"]
pub struct ReceiveFuture<A, Rc: RxRefCounter>(ReceiveState<A, Rc>);

impl<A, Rc: RxRefCounter> ReceiveFuture<A, Rc> {
    fn new(rx: Receiver<A, Rc>) -> Self {
        ReceiveFuture(ReceiveState::New(rx))
    }
}

enum ReceiveState<A, Rc: RxRefCounter> {
    New(Receiver<A, Rc>),
    Waiting {
        rx: Receiver<A, Rc>,
        waiting: WaitingReceiver<A>,
    },
    Complete,
}

impl<A, Rc: RxRefCounter> Future for ReceiveFuture<A, Rc> {
    type Output = ActorMessage<A>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<ActorMessage<A>> {
        let this = self.get_mut();

        loop {
            match mem::replace(&mut this.0, ReceiveState::Complete) {
                ReceiveState::New(rx) => {
                    match rx.inner.try_recv(rx.broadcast_mailbox.as_ref()) {
                        Ok(message) => return Poll::Ready(message),
                        Err(waiting) => {
                            // Start waiting. The waiting receiver should be immediately polled, in case a
                            // send operation happened between `try_recv` and here, in which case the
                            // WaitingReceiver would be fulfilled, but not properly woken.
                            this.0 = ReceiveState::Waiting { rx, waiting };
                        }
                    }
                }
                ReceiveState::Waiting { rx, waiting } => match waiting.poll(&rx, cx) {
                    Poll::Ready(Some(msg)) => return Poll::Ready(msg),
                    Poll::Ready(None) => {
                        // False positive wake up, try receive again.
                        this.0 = ReceiveState::New(rx);
                        continue;
                    }
                    Poll::Pending => {
                        this.0 = ReceiveState::Waiting { rx, waiting };
                        return Poll::Pending;
                    }
                },
                ReceiveState::Complete => return Poll::Pending,
            }
        }
    }
}

impl<A, Rc: RxRefCounter> Drop for ReceiveFuture<A, Rc> {
    fn drop(&mut self) {
        if let ReceiveState::Waiting { waiting, rx } =
            mem::replace(&mut self.0, ReceiveState::Complete)
        {
            if let Some(msg) = waiting.cancel() {
                rx.inner.requeue_message(msg);
            }
        }
    }
}

impl<A, Rc: RxRefCounter> FusedFuture for ReceiveFuture<A, Rc> {
    fn is_terminated(&self) -> bool {
        matches!(self.0, ReceiveState::Complete)
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
