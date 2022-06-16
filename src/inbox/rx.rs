use crate::inbox::*;
use futures_util::FutureExt;
use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};
use std::{cmp, mem};
use futures_core::FusedFuture;

pub(crate) struct Receiver<A> {
    pub(super) inner: Chan<A>,
    pub(super) broadcast_mailbox: Arc<BroadcastQueue<A>>,
}

impl<A> Receiver<A> {
    pub(crate) fn sender(&self) -> Sender<A> {
        Sender(self.inner.clone())
    }

    pub(crate) fn cloned_same_broadcast_mailbox(&self) -> Receiver<A> {
        Receiver {
            inner: self.inner.clone(),
            broadcast_mailbox: self.broadcast_mailbox.clone(),
        }
    }

    pub(crate) fn cloned_new_broadcast_mailbox(&self) -> Receiver<A> {
        let new_mailbox = Arc::new(Spinlock::new(BinaryHeap::new()));
        let weak = Arc::downgrade(&new_mailbox);
        self.inner.lock().unwrap().broadcast_queues.push(weak);

        Receiver {
            inner: self.inner.clone(),
            broadcast_mailbox: new_mailbox,
        }
    }

    pub(crate) fn receive(&self) -> ReceiveFuture<A> {
        ReceiveFuture::new(self.cloned_same_broadcast_mailbox())
    }

    fn try_recv(&self) -> Result<ActorMessage<A>, Arc<Spinlock<WaitingReceiver<A>>>> {
        let mut inner = self.inner.lock().unwrap();
        let mut broadcast = self.broadcast_mailbox.lock();

        if inner.shutdown {
            return Ok(ActorMessage::Shutdown);
        }

        // Peek priorities in order to figure out which channel should be taken from
        let broadcast_priority = broadcast
            .peek()
            .map(|it| it.priority())
            .unwrap_or(Priority::Min);
        let shared_priority: Priority = inner
            .priority_queue
            .peek()
            .map(|it| it.priority())
            .unwrap_or(Priority::Min);

        // Try take from default channel
        if cmp::max(shared_priority, broadcast_priority) <= Priority::default() {
            if let Some(msg) = inner.pop_default() {
                return Ok(msg.into());
            }
        }

        // Choose which priority channel to take from
        match shared_priority.cmp(&broadcast_priority) {
            // Shared priority is greater or equal (and it is not empty)
            Ordering::Greater | Ordering::Equal if shared_priority != Priority::Min => {
                Ok(inner.pop_priority().unwrap().into())
            }
            // Shared priority is less - take from broadcast
            Ordering::Less => Ok(broadcast.pop().unwrap().0.into()),
            // Equal, but both are empty, so wait
            _ => {
                let waiting = Arc::new(Spinlock::new(WaitingReceiver::default()));
                inner.waiting_receivers.push_back(Arc::downgrade(&waiting));
                Err(waiting)
            }
        }
    }

    fn pop_broadcast(&self) -> Option<ActorMessage<A>> {
        self.broadcast_mailbox
            .lock()
            .pop()
            .map(|msg| ActorMessage::BroadcastMessage(msg.0))
    }
}

#[must_use = "Futures do nothing unless polled"]
pub(crate) struct ReceiveFuture<A>(ReceiveFutureInner<A>);

impl<A> ReceiveFuture<A> {
    fn new(rx: Receiver<A>) -> Self {
        ReceiveFuture(ReceiveFutureInner::New(rx))
    }
}

enum ReceiveFutureInner<A> {
    New(Receiver<A>),
    Waiting {
        rx: Receiver<A>,
        waiting: Arc<Spinlock<WaitingReceiver<A>>>,
    },
    Complete,
}

impl<A> Future for ReceiveFuture<A> {
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
                                WakeReason::StolenMessage(msg) => msg.into(),
                                WakeReason::BroadcastMessage => rx.pop_broadcast().unwrap(),
                                WakeReason::Shutdown => ActorMessage::Shutdown,
                                WakeReason::Cancelled => {
                                    unreachable!("Waiting receive future cannot be interrupted")
                                }
                            })
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

impl<A> ReceiveFuture<A> {
    /// Cancel the receiving future, returning a message if it had been fulfilled with one, but had
    /// not yet been polled after wakeup. Future calls to `Future::poll` will return `Poll::Pending`,
    /// and `FusedFuture::is_terminated` will return `true`.
    ///
    /// This is important to do over `Drop`, as with `Drop` messages may be sent back into the
    /// channel and could be observed as received out of order, if multiple receives are concurrent
    /// on one thread.
    #[must_use = "If dropped, messages could be lost"]
    fn cancel(&mut self) -> Option<ActorMessage<A>> {
        if let ReceiveFutureInner::Waiting { waiting, .. } =
            mem::replace(&mut self.0, ReceiveFutureInner::Complete)
        {
            if let Some(WakeReason::StolenMessage(msg)) = waiting.lock().cancel() {
                return Some(msg.into());
            }
        }

        None
    }
}

impl<A> Drop for ReceiveFuture<A> {
    fn drop(&mut self) {
        if let ReceiveFutureInner::Waiting { waiting, rx } =
            mem::replace(&mut self.0, ReceiveFutureInner::Complete)
        {
            let mut inner = rx.inner.lock().unwrap();

            // This receive future was woken with a message - send in back into the channel.
            // Ordering is compromised somewhat when concurrent receives are involved, and the cap
            // may overflow (probably not enough to cause backpressure issues), but this is better
            // than dropping a message.
            if let Some(WakeReason::StolenMessage(msg)) = waiting.lock().cancel() {
                match inner.try_fulfill_receiver(WakeReason::StolenMessage(msg)) {
                    Ok(()) => (),
                    Err(WakeReason::StolenMessage(msg)) => {
                        if msg.priority == Priority::default() {
                            // Preserve ordering as much as possible by pushing to the front
                            inner.default_queue.push_front(msg.val)
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

impl<A> FusedFuture for ReceiveFuture<A> {
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
