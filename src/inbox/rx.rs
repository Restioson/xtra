use crate::inbox::*;
use futures_core::FusedFuture;
use futures_util::FutureExt;
use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{atomic, Arc};
use std::task::{Context, Poll, Waker};
use std::{cmp, mem};

pub struct Receiver<A, Rc: RxRefCounter> {
    pub(super) inner: Arc<Chan<A>>,
    pub(super) broadcast_mailbox: Arc<BroadcastQueue<A>>,
    pub(super) rc: Rc,
}

impl<A> Receiver<A, RxStrong> {
    pub(super) fn new(inner: Arc<Chan<A>>, broadcast_mailbox: Arc<BroadcastQueue<A>>) -> Self {
        let rc = RxStrong(());
        rc.increment(&inner);

        Receiver {
            inner,
            broadcast_mailbox,
            rc,
        }
    }
}

impl<A, Rc: RxRefCounter> Receiver<A, Rc> {
    pub fn sender(&self) -> Option<Sender<A, TxStrong>> {
        Some(Sender {
            inner: self.inner.clone(),
            rc: TxStrong::try_new(&self.inner)?,
        })
    }

    /// Clone this receiver, keeping its broadcast mailbox.
    pub fn cloned_same_broadcast_mailbox(&self) -> Receiver<A, Rc> {
        Receiver {
            inner: self.inner.clone(),
            broadcast_mailbox: self.broadcast_mailbox.clone(),
            rc: self.rc.increment(&self.inner),
        }
    }

    /// Clone this receiver, giving the clone a new broadcast mailbox.
    pub fn cloned_new_broadcast_mailbox(&self) -> Receiver<A, Rc> {
        let new_mailbox = Arc::new(Spinlock::new(BinaryHeap::new()));
        self.inner
            .chan
            .lock()
            .unwrap()
            .broadcast_queues
            .push(Arc::downgrade(&new_mailbox));

        Receiver {
            inner: self.inner.clone(),
            broadcast_mailbox: new_mailbox,
            rc: self.rc.increment(&self.inner),
        }
    }

    pub fn receive(&self) -> ReceiveFuture<A, Rc> {
        ReceiveFuture::new(self.cloned_same_broadcast_mailbox())
    }

    fn try_recv(&self) -> Result<ActorMessage<A>, Arc<Spinlock<WaitingReceiver<A>>>> {
        // Peek priorities in order to figure out which channel should be taken from
        let mut broadcast = self.broadcast_mailbox.lock();
        let broadcast_priority = broadcast.peek().map_or(Priority::Min, |it| it.priority());

        let mut inner = self.inner.chan.lock().unwrap();

        let shared_priority: Priority = inner
            .priority_queue
            .peek()
            .map_or(Priority::Min, |it| it.priority());

        // Try take from ordered channel
        if cmp::max(shared_priority, broadcast_priority) <= Priority::default() {
            if let Some(msg) = inner.pop_ordered(self.inner.capacity) {
                return Ok(msg.into());
            }
        }

        // Choose which priority channel to take from
        match shared_priority.cmp(&broadcast_priority) {
            // Shared priority is greater or equal (and it is not empty)
            Ordering::Greater | Ordering::Equal if shared_priority != Priority::Min => {
                Ok(inner.pop_priority(self.inner.capacity).unwrap().into())
            }
            // Shared priority is less - take from broadcast
            Ordering::Less => {
                let msg = broadcast.pop().unwrap().0;
                drop(broadcast);
                inner.try_advance_broadcast_tail(self.inner.capacity);

                Ok(msg.into())
            }
            // Equal, but both are empty, so wait or exit if shutdown
            _ => {
                // Shutdown is only edited when inner is locked, and we have it locked now, so no
                // chance of races here
                if self.inner.is_shutdown() {
                    return Ok(ActorMessage::Shutdown);
                }

                let waiting = Arc::new(Spinlock::new(WaitingReceiver::default()));
                inner.waiting_receivers.push_back(Arc::downgrade(&waiting));
                Err(waiting)
            }
        }
    }
}

impl<A, Rc: RxRefCounter> Drop for Receiver<A, Rc> {
    fn drop(&mut self) {
        if self.rc.decrement(&self.inner) {
            self.inner.shutdown();
        }
    }
}

#[must_use = "Futures do nothing unless polled"]
pub struct ReceiveFuture<A, Rc: RxRefCounter>(ReceiveFutureInner<A, Rc>);

impl<A, Rc: RxRefCounter> ReceiveFuture<A, Rc> {
    fn new(rx: Receiver<A, Rc>) -> Self {
        ReceiveFuture(ReceiveFutureInner::New(rx))
    }
}

enum ReceiveFutureInner<A, Rc: RxRefCounter> {
    New(Receiver<A, Rc>),
    Waiting {
        rx: Receiver<A, Rc>,
        waiting: Arc<Spinlock<WaitingReceiver<A>>>,
    },
    Complete,
}

impl<A, Rc: RxRefCounter> Future for ReceiveFuture<A, Rc> {
    type Output = ActorMessage<A>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<ActorMessage<A>> {
        match mem::replace(&mut self.0, ReceiveFutureInner::Complete) {
            ReceiveFutureInner::New(rx) => match rx.try_recv() {
                Ok(message) => Poll::Ready(message),
                Err(waiting) => {
                    // Start waiting. The waiting receiver should be immediately polled, in case a
                    // send operation happened between `try_recv` and here, in which case the
                    // WaitingReceiver would be fulfilled, but not properly woken.
                    self.0 = ReceiveFutureInner::Waiting { rx, waiting };
                    self.poll_unpin(cx)
                }
            },
            ReceiveFutureInner::Waiting { rx, waiting } => {
                {
                    let mut inner = waiting.lock();

                    match inner.wake_reason.take() {
                        // Message has been delivered
                        Some(reason) => {
                            return Poll::Ready(match reason {
                                WakeReason::MessageToOneActor(msg) => msg.into(),
                                WakeReason::MessageToAllActors => {
                                    // The broadcast message could have been taken by another
                                    // receive future from the same receiver (or from another
                                    // receiver sharing the same broadcast mailbox)
                                    let pop = rx.broadcast_mailbox.lock().pop();
                                    match pop {
                                        Some(msg) => {
                                            rx.inner
                                                .chan
                                                .lock()
                                                .unwrap()
                                                .try_advance_broadcast_tail(rx.inner.capacity);
                                            ActorMessage::ToAllActors(msg.0)
                                        }
                                        None => {
                                            // If it was taken, try receive again
                                            self.0 = ReceiveFutureInner::New(rx);
                                            drop(inner);
                                            return self.poll_unpin(cx);
                                        }
                                    }
                                }
                                WakeReason::Shutdown => ActorMessage::Shutdown,
                                WakeReason::Cancelled => {
                                    unreachable!("Waiting receive future cannot be interrupted")
                                }
                            });
                        }
                        // Message has not been delivered - continue waiting
                        None => inner.waker = Some(cx.waker().clone()),
                    }
                }

                self.0 = ReceiveFutureInner::Waiting { rx, waiting };
                Poll::Pending
            }
            ReceiveFutureInner::Complete => Poll::Pending,
        }
    }
}

impl<A, Rc: RxRefCounter> ReceiveFuture<A, Rc> {
    /// Cancel the receiving future, returning a message if it had been fulfilled with one, but had
    /// not yet been polled after wakeup. Future calls to `Future::poll` will return `Poll::Pending`,
    /// and `FusedFuture::is_terminated` will return `true`.
    ///
    /// This is important to do over `Drop`, as with `Drop` messages may be sent back into the
    /// channel and could be observed as received out of order, if multiple receives are concurrent
    /// on one thread.
    #[must_use = "If dropped, messages could be lost"]
    pub fn cancel(&mut self) -> Option<ActorMessage<A>> {
        if let ReceiveFutureInner::Waiting { waiting, .. } =
            mem::replace(&mut self.0, ReceiveFutureInner::Complete)
        {
            if let Some(WakeReason::MessageToOneActor(msg)) = waiting.lock().cancel() {
                return Some(msg.into());
            }
        }

        None
    }
}

impl<A, Rc: RxRefCounter> Drop for ReceiveFuture<A, Rc> {
    fn drop(&mut self) {
        if let ReceiveFutureInner::Waiting { waiting, rx } =
            mem::replace(&mut self.0, ReceiveFutureInner::Complete)
        {
            let mut inner = match rx.inner.chan.lock() {
                Ok(lock) => lock,
                Err(_) => return, // Poisoned - ignore
            };

            // This receive future was woken with a message - send in back into the channel.
            // Ordering is compromised somewhat when concurrent receives are involved, and the cap
            // may overflow (probably not enough to cause backpressure issues), but this is better
            // than dropping a message.
            if let Some(WakeReason::MessageToOneActor(msg)) = waiting.lock().cancel() {
                match inner.try_fulfill_receiver(WakeReason::MessageToOneActor(msg)) {
                    Ok(()) => (),
                    Err(WakeReason::MessageToOneActor(msg)) => {
                        if msg.priority == Priority::default() {
                            // Preserve ordering as much as possible by pushing to the front
                            inner.ordered_queue.push_front(msg.val)
                        } else {
                            inner.priority_queue.push(msg);
                        }
                    }
                    Err(_) => unreachable!("Got wrong wake reason back from try_fulfill_receiver"),
                }
            }
        }
    }
}

impl<A, Rc: RxRefCounter> FusedFuture for ReceiveFuture<A, Rc> {
    fn is_terminated(&self) -> bool {
        matches!(self.0, ReceiveFutureInner::Complete)
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

    /// Signify that this waiting receiver was cancelled through [`ReceiveFuture::cancel`]
    fn cancel(&mut self) -> Option<WakeReason<A>> {
        mem::replace(&mut self.wake_reason, Some(WakeReason::Cancelled))
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
