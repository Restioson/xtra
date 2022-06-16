//! Latency is prioritised over most accurate prioritisation. Specifically, at most one low priority
//! message may be handled before piled-up higher priority messages will be handled.
// TODO(bounded)

mod rx;
mod tx;

pub(crate) use rx::Receiver;
pub(crate) use tx::{SendFuture, Sender};

use crate::envelope::{BroadcastEnvelope, MessageEnvelope};

use crate::inbox::tx::WaitingSender;
use std::cmp::Ordering;
use std::collections::{BinaryHeap, VecDeque};

use std::sync::{Arc, Mutex, Weak};

use crate::inbox::rx::WaitingReceiver;

type Chan<A> = Arc<Mutex<Inner<A>>>;
type Spinlock<T> = spin::Mutex<T>;
type StolenMessage<A> = Box<dyn MessageEnvelope<Actor = A>>;
type BroadcastMessage<A> = Arc<dyn BroadcastEnvelope<Actor = A>>;
type BroadcastQueue<A> = Spinlock<BinaryHeap<BroadcastMessageWrapper<A>>>;

// TODO(priority)
#[derive(PartialEq, Eq, Ord, PartialOrd, Copy, Clone)]
pub(crate) enum Priority {
    Min,
    Valued(i32),
}

impl Default for Priority {
    fn default() -> Self {
        Priority::Valued(0)
    }
}

pub(crate) fn new<A>(bound: Option<usize>) -> (Sender<A>, Receiver<A>) {
    let broadcast_mailbox = Arc::new(Spinlock::new(BinaryHeap::new()));
    let inner = Inner {
        default_queue: VecDeque::new(),
        bound,
        waiting_senders: VecDeque::new(),
        waiting_receivers: VecDeque::new(),
        priority_queue: BinaryHeap::new(),
        broadcast_queues: vec![Arc::downgrade(&broadcast_mailbox)],
        shutdown: false,
    };

    let inner = Arc::new(Mutex::new(inner));

    let rx = Receiver {
        inner: inner.clone(),
        broadcast_mailbox,
    };

    (Sender(inner), rx)
}

// TODO(perf): try and reduce contention on Inner as much as possible
// Might be able to move some stuff out to atomics, or lock it separately. This should net some
// overall performance gains, but these likely won't in crude_bench or any throughput testing
struct Inner<A> {
    default_queue: VecDeque<StolenMessage<A>>,
    bound: Option<usize>,
    waiting_senders: VecDeque<Weak<Spinlock<WaitingSender<A>>>>,
    waiting_receivers: VecDeque<Weak<Spinlock<WaitingReceiver<A>>>>,
    priority_queue: BinaryHeap<StolenMessageWithPriority<A>>,
    broadcast_queues: Vec<Weak<BroadcastQueue<A>>>,
    shutdown: bool,
}

impl<A> Inner<A> {
    fn is_full(&self) -> bool {
        self.bound
            .map(|cap| self.default_queue.len() >= cap)
            .unwrap_or(false)
    }

    fn pop_priority(&mut self) -> Option<StolenMessage<A>> {
        self.priority_queue.pop().map(|msg| msg.val)
    }

    fn pop_default(&mut self) -> Option<StolenMessage<A>> {
        // If len < cap after popping this message, try fulfill at most one waiting sender
        if self.is_full() {
            self.try_fulfill_sender();
        }

        self.default_queue.pop_front()
    }

    fn try_fulfill_receiver(&mut self, mut reason: WakeReason<A>) -> Result<(), WakeReason<A>> {
        while !self.waiting_receivers.is_empty() {
            if let Some(rx) = self.waiting_receivers.pop_front().and_then(|w| w.upgrade()) {
                reason = match rx.lock().fulfill(reason) {
                    Ok(()) => return Ok(()),
                    Err(reason) => reason,
                }
            }
        }

        Err(reason)
    }

    fn try_fulfill_sender(&mut self) {
        while !self.waiting_senders.is_empty() {
            if let Some(msg) = self
                .waiting_senders
                .pop_front()
                .and_then(|w| w.upgrade())
                .and_then(|tx| tx.lock().fulfill())
            {
                self.default_queue.push_back(msg);
                return;
            }
        }
    }
}

enum TrySendFail<A> {
    Full(Arc<Spinlock<WaitingSender<A>>>),
    Disconnected,
}

pub(crate) enum ActorMessage<A> {
    StolenMessage(StolenMessage<A>),
    BroadcastMessage(BroadcastMessage<A>),
    Shutdown,
}

impl<A> From<StolenMessage<A>> for ActorMessage<A> {
    fn from(msg: StolenMessage<A>) -> Self {
        ActorMessage::StolenMessage(msg)
    }
}

impl<A> From<StolenMessageWithPriority<A>> for ActorMessage<A> {
    fn from(msg: StolenMessageWithPriority<A>) -> Self {
        ActorMessage::StolenMessage(msg.val)
    }
}

impl<A> From<BroadcastMessage<A>> for ActorMessage<A> {
    fn from(msg: BroadcastMessage<A>) -> Self {
        ActorMessage::BroadcastMessage(msg)
    }
}

enum WakeReason<A> {
    StolenMessage(StolenMessageWithPriority<A>),
    // should be fetched from own receiver
    BroadcastMessage,
    Shutdown,
    // ReceiveFuture::cancel was called
    Cancelled,
}

pub(crate) trait HasPriority {
    fn priority(&self) -> Priority;
}

impl<A> HasPriority for StolenMessageWithPriority<A> {
    fn priority(&self) -> Priority {
        self.priority
    }
}

struct StolenMessageWithPriority<A> {
    priority: Priority,
    val: StolenMessage<A>,
}

impl<A> StolenMessageWithPriority<A> {
    fn new(priority: Priority, val: StolenMessage<A>) -> Self {
        StolenMessageWithPriority { priority, val }
    }
}

impl<A> PartialEq for StolenMessageWithPriority<A> {
    fn eq(&self, other: &Self) -> bool {
        self.priority == other.priority // TODO(eq)
    }
}

impl<A> Eq for StolenMessageWithPriority<A> {}

impl<A> PartialOrd for StolenMessageWithPriority<A> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl<A> Ord for StolenMessageWithPriority<A> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.priority.cmp(&other.priority)
    }
}

struct BroadcastMessageWrapper<A>(BroadcastMessage<A>);

impl<A> HasPriority for BroadcastMessageWrapper<A> {
    fn priority(&self) -> Priority {
        self.0.priority()
    }
}

impl<A> Eq for BroadcastMessageWrapper<A> {}

impl<A> PartialEq<Self> for BroadcastMessageWrapper<A> {
    fn eq(&self, other: &Self) -> bool {
        self.0.priority() == other.0.priority() // TODO(eq)
    }
}

impl<A> PartialOrd<Self> for BroadcastMessageWrapper<A> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl<A> Ord for BroadcastMessageWrapper<A> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.priority().cmp(&other.priority())
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::envelope::BroadcastEnvelopeConcrete;
    use crate::prelude::{Context, *};
    use futures_util::FutureExt;

    struct MyActor;

    #[async_trait]
    impl Actor for MyActor {
        type Stop = ();

        async fn stopped(self) {}
    }

    #[async_trait]
    impl Handler<&'static str> for MyActor {
        type Return = ();

        async fn handle(&mut self, message: &'static str, _ctx: &mut Context<Self>) {
            println!("{}", message);
        }
    }

    #[test]
    fn test_priority() {
        assert!(Priority::Min < Priority::Valued(0));
        assert!(Priority::Valued(1) > Priority::Valued(0));
    }

    #[tokio::test]
    async fn test_broadcast() {
        let (tx, rx) = new(None);
        let rx2 = rx.cloned_new_broadcast_mailbox();

        let orig = Arc::new(BroadcastEnvelopeConcrete::new("Hi", 1));
        let orig = orig as Arc<dyn BroadcastEnvelope<Actor = MyActor>>;
        tx.broadcast(orig.clone()).unwrap();

        match rx.receive().await {
            ActorMessage::BroadcastMessage(msg) => assert!(Arc::ptr_eq(&msg, &orig)),
            _ => panic!("Expected broadcast message, got something else"),
        }

        match rx2.receive().await {
            ActorMessage::BroadcastMessage(msg) => assert!(Arc::ptr_eq(&msg, &orig)),
            _ => panic!("Expected broadcast message, got something else"),
        }

        assert!(rx.receive().now_or_never().is_none());
        assert!(rx2.receive().now_or_never().is_none());
    }
}
