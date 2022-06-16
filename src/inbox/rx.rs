use crate::inbox::tx::Sender;
use crate::inbox::{
    ActorMessage, BroadcastQueue, Chan, HasPriority, Priority, Spinlock, WaitForSender,
    WaitingReceiver, WakeReason,
};
use std::cmp;
use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::sync::Arc;

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

    fn try_recv(&self) -> Result<ActorMessage<A>, WaitForSender<A>> {
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

        // Try to take from default channel if both shared & broadcast are below default priority
        if cmp::max(shared_priority, broadcast_priority) < Priority::default() {
            if let Some(msg) = inner.pop_default() {
                return Ok(msg.into());
            }
        }

        // Choose which channel to take from
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
                Err(WaitForSender(waiting))
            }
        }
    }

    pub(crate) async fn receive(&self) -> ActorMessage<A> {
        let waiting = match self.try_recv() {
            Ok(msg) => return msg,
            Err(waiting) => waiting,
        };

        // What happens if the channel is unlocked, then a message is sent, and only THEN is this awaited?
        // Surely, since the waker has not yet been stored, it will miss wakeup and poll forever?
        // No, if a message is sent, the `next_message` field will be set to `Some`, which will
        // cause the future to return `Ready` on its *first poll* - so, wakeup is not required.
        match waiting.await {
            WakeReason::StolenMessage(msg) => msg.into(),
            WakeReason::BroadcastMessage => self.broadcast_mailbox.lock().pop().unwrap().0.into(),
            WakeReason::Shutdown => ActorMessage::Shutdown,
        }
    }
}
