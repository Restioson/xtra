use std::future::Future;
use std::mem;
use std::pin::Pin;
use std::sync::{atomic, Arc};
use std::task::{Context, Poll, Waker};

use futures_core::FusedFuture;

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

/// A [`WaitingReceiver`] is handed out by the channel any time [`Chan::try_recv`] is called on an empty mailbox.
///
/// [`WaitingReceiver`] implements [`Future`] which will resolve once a message lands in the mailbox.
pub struct WaitingReceiver<A> {
    state: Arc<spin::Mutex<WaitingState<A>>>,
}

/// A [`FulfillHandle`] is the counter-part to a [`WaitingReceiver`].
///
/// It is stored internally in the channel and used by the channel implementation to notify a
/// [`WaitingReceiver`] once a new message hits the mailbox.
pub struct FulfillHandle<A> {
    state: Weak<spin::Mutex<WaitingState<A>>>,
}

impl<A> FulfillHandle<A> {
    /// Shutdown the connected [`WaitingReceiver`].
    ///
    /// This will wake the corresponding task and notify them that the channel is shutting down.
    pub(crate) fn shutdown(&self) {
        let _ = self.fulfill(WakeReason::Shutdown);
    }

    /// Notify the connected [`WaitingReceiver`] about a new broadcast message.
    ///
    /// Broadcast mailboxes are stored outside of the main channel implementation which is why this
    /// messages does not take any arguments.
    pub(crate) fn fulfill_message_to_all_actors(&self) -> Result<(), ()> {
        match self.fulfill(WakeReason::MessageToAllActors) {
            Ok(()) => Ok(()),
            Err(WakeReason::MessageToAllActors) => Err(()),
            _ => unreachable!(),
        }
    }

    /// Notify the connected [`WaitingReceiver`] about a new message.
    ///
    /// A new message was sent into the channel and the connected [`WaitingReceiver`] is the chosen
    /// one to handle it, likely because it has been waiting the longest.
    ///
    /// This function will return the message in an `Err` if the [`WaitingReceiver`] has since called
    /// [`cancel`](WaitingReceiver::cancel) and is therefore unable to handle the message.
    pub(crate) fn fulfill_message_to_one_actor(
        &self,
        msg: Box<dyn MessageEnvelope<Actor = A>>,
    ) -> Result<(), Box<dyn MessageEnvelope<Actor = A>>> {
        match self.fulfill(WakeReason::MessageToOneActor(msg)) {
            Ok(()) => Ok(()),
            Err(WakeReason::MessageToOneActor(msg)) => Err(msg),
            _ => unreachable!(),
        }
    }

    fn fulfill(&self, reason: WakeReason<A>) -> Result<(), WakeReason<A>> {
        let this = match self.state.upgrade() {
            Some(this) => this,
            None => return Err(reason),
        };

        let mut this = this.lock();

        if this.cancelled {
            return Err(reason);
        }

        match this.wake_reason {
            Some(_) => unreachable!("Waiting receiver was fulfilled but not popped!"),
            None => this.wake_reason = Some(reason),
        }

        if let Some(waker) = this.waker.take() {
            waker.wake();
        }

        Ok(())
    }
}

impl<A> WaitingReceiver<A> {
    pub fn new() -> (WaitingReceiver<A>, FulfillHandle<A>) {
        let state = Arc::new(spin::Mutex::new(WaitingState::default()));

        let handle = FulfillHandle {
            state: Arc::downgrade(&state),
        };
        let receiver = WaitingReceiver { state };

        (receiver, handle)
    }

    /// Cancel this [`WaitingReceiver`].
    ///
    /// This may return a message in case the underlying state has been fulfilled with one but this
    /// receiver has not been polled since.
    ///
    /// It is important to call this message over just dropping the [`WaitingReceiver`] as this
    /// message would otherwise be dropped.
    fn cancel(&self) -> Option<Box<dyn MessageEnvelope<Actor = A>>> {
        let mut state = self.state.lock();

        state.cancelled = true;

        match state.wake_reason.take()? {
            WakeReason::MessageToOneActor(msg) => Some(msg),
            _ => None,
        }
    }

    fn poll<Rc>(
        &self,
        receiver: &Receiver<A, Rc>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<ActorMessage<A>>>
    where
        Rc: RxRefCounter,
    {
        let mut this = self.state.lock();

        match this.wake_reason.take() {
            Some(WakeReason::MessageToOneActor(msg)) => {
                Poll::Ready(Some(ActorMessage::ToOneActor(msg)))
            }
            Some(WakeReason::MessageToAllActors) => {
                match receiver
                    .inner
                    .pop_broadcast_message(&receiver.broadcast_mailbox)
                {
                    Some(msg) => Poll::Ready(Some(ActorMessage::ToAllActors(msg))),
                    None => Poll::Ready(None),
                }
            }
            Some(WakeReason::Shutdown) => Poll::Ready(Some(ActorMessage::Shutdown)),
            None => {
                this.waker = Some(cx.waker().clone());

                Poll::Pending
            }
        }
    }
}

struct WaitingState<A> {
    waker: Option<Waker>,
    wake_reason: Option<WakeReason<A>>,
    cancelled: bool,
}

impl<A> Default for WaitingState<A> {
    fn default() -> Self {
        WaitingState {
            waker: None,
            wake_reason: None,
            cancelled: false,
        }
    }
}

pub enum WakeReason<A> {
    MessageToOneActor(Box<dyn MessageEnvelope<Actor = A>>),
    // should be fetched from own receiver
    MessageToAllActors,
    Shutdown,
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
