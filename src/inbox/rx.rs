use std::future::Future;
use std::mem;
use std::pin::Pin;
use std::sync::{atomic, Arc};
use std::task::{Context, Poll, Waker};

use futures_core::FusedFuture;
use futures_util::FutureExt;

use crate::inbox::tx::TxWeak;
use crate::inbox::*;

pub struct Receiver<A, Rc: RxRefCounter> {
    inner: Arc<Chan<A>>,
    broadcast_mailbox: Arc<BroadcastQueue<A>>,
    rc: Rc,
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
        waiting: Arc<Spinlock<WaitingReceiver<A>>>,
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
                ReceiveState::Waiting { rx, waiting } => {
                    let poll = { waiting.lock().poll_unpin(cx) };

                    let actor_message = match poll {
                        Poll::Ready(WakeReason::MessageToOneActor(msg)) => msg.into(),
                        Poll::Ready(WakeReason::Shutdown) => ActorMessage::Shutdown,
                        Poll::Ready(WakeReason::Cancelled) => {
                            unreachable!("Waiting receive future cannot be interrupted")
                        }
                        Poll::Ready(WakeReason::MessageToAllActors) => {
                            match rx.inner.pop_broadcast_message(&rx.broadcast_mailbox) {
                                Some(msg) => ActorMessage::ToAllActors(msg),
                                None => {
                                    // We got woken but failed to pop a message, try receiving again.
                                    this.0 = ReceiveState::New(rx);
                                    continue;
                                }
                            }
                        }
                        Poll::Pending => {
                            this.0 = ReceiveState::Waiting { rx, waiting };
                            return Poll::Pending;
                        }
                    };

                    return Poll::Ready(actor_message);
                }
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
            if let Some(WakeReason::MessageToOneActor(msg)) = waiting.lock().cancel() {
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

pub struct WaitingReceiver<A> {
    waker: Option<Waker>,
    wake_reason: Option<WakeReason<A>>,
}

impl<A> Default for WaitingReceiver<A> {
    fn default() -> Self {
        WaitingReceiver {
            waker: None,
            wake_reason: None,
        }
    }
}

impl<A> WaitingReceiver<A> {
    pub(super) fn fulfill(&mut self, reason: WakeReason<A>) -> Result<(), WakeReason<A>> {
        match self.wake_reason {
            // Receive was interrupted, so this cannot be fulfilled
            Some(WakeReason::Cancelled) => return Err(reason),
            Some(_) => unreachable!("Waiting receiver was fulfilled but not popped!"),
            None => self.wake_reason = Some(reason),
        }

        if let Some(waker) = self.waker.take() {
            waker.wake();
        }

        Ok(())
    }

    /// Cancel this [`WaitingReceiver`] returning its current, internal state.
    fn cancel(&mut self) -> Option<WakeReason<A>> {
        mem::replace(&mut self.wake_reason, Some(WakeReason::Cancelled))
    }
}

impl<A> Future for WaitingReceiver<A> {
    type Output = WakeReason<A>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();

        match this.wake_reason.take() {
            Some(reason) => Poll::Ready(reason),
            None => {
                this.waker = Some(cx.waker().clone());

                Poll::Pending
            }
        }
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
