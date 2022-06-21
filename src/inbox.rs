//! Latency is prioritised over most accurate prioritisation. Specifically, at most one low priority
//! message may be handled before piled-up higher priority messages will be handled.

pub mod rx;
pub mod tx;

pub use rx::Receiver;
pub use tx::{SendFuture, Sender};

use crate::envelope::{BroadcastEnvelope, MessageEnvelope};
use crate::inbox::rx::{RxStrong, WaitingReceiver};
use crate::inbox::tx::{TxStrong, WaitingSender};
use event_listener::Event;
use std::cmp::Ordering;
use std::collections::{BinaryHeap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::sync::{atomic, Arc, Mutex, Weak};
use std::{cmp, mem};

type Spinlock<T> = spin::Mutex<T>;
pub type MessageToOneActor<A> = Box<dyn MessageEnvelope<Actor = A>>;
type BroadcastQueue<A> = Spinlock<BinaryHeap<MessageToAllActors<A>>>;

/// TODO(doc): also should wait till we clarify how bounding will work
pub fn new<A>(capacity: Option<usize>) -> (Sender<A, TxStrong>, Receiver<A, RxStrong>) {
    let broadcast_mailbox = Arc::new(Spinlock::new(BinaryHeap::new()));
    let inner = Arc::new(Chan {
        chan: Mutex::new(ChanInner {
            ordered_queue: VecDeque::new(),
            waiting_senders: VecDeque::new(),
            waiting_receivers: VecDeque::new(),
            priority_queue: BinaryHeap::new(),
            broadcast_queues: vec![Arc::downgrade(&broadcast_mailbox)],
            broadcast_tail: 0,
        }),
        capacity,
        on_shutdown: Event::new(),
        shutdown: AtomicBool::new(false),
        sender_count: AtomicUsize::new(0),
        receiver_count: AtomicUsize::new(0),
    });

    let tx = Sender::new(inner.clone());
    let rx = Receiver::new(inner, broadcast_mailbox);

    (tx, rx)
}

// TODO(priority): can we let the user specify a custom type?
#[derive(PartialEq, Eq, Ord, PartialOrd, Copy, Clone)]
pub enum Priority {
    Min,
    Valued(i32),
}

impl Default for Priority {
    fn default() -> Self {
        Priority::Valued(0)
    }
}

// Public because of private::RefCounterInner. This should never actually be exported, though.
pub struct Chan<A> {
    chan: Mutex<ChanInner<A>>,
    capacity: Option<usize>,
    on_shutdown: Event,
    shutdown: AtomicBool,
    sender_count: AtomicUsize,
    receiver_count: AtomicUsize,
}

impl<A> Chan<A> {
    fn is_full(&self, len: usize) -> bool {
        self.capacity.map_or(false, |cap| len >= cap)
    }

    fn is_shutdown(&self) -> bool {
        // TODO(atomic) what ordering to use here
        self.shutdown.load(atomic::Ordering::SeqCst)
    }

    pub fn shutdown(&self) {
        let waiting_receivers = {
            let mut inner = self.chan.lock().unwrap();

            // TODO(atomic) what ordering to use here?
            self.shutdown.store(true, atomic::Ordering::SeqCst);
            self.on_shutdown.notify(usize::MAX);

            mem::take(&mut inner.waiting_receivers)
        };

        for rx in waiting_receivers.into_iter().flat_map(|w| w.upgrade()) {
            let _ = rx.lock().fulfill(WakeReason::Shutdown);
        }
    }

    pub fn shutdown_and_drain(&self) {
        let waiting_rx = {
            let mut inner = self.chan.lock().unwrap();

            // TODO(atomic) what ordering to use here?
            self.shutdown.store(true, atomic::Ordering::SeqCst);
            self.on_shutdown.notify(usize::MAX);

            for queue in inner
                .broadcast_queues
                .drain(..)
                .flat_map(|weak| weak.upgrade())
            {
                *queue.lock() = BinaryHeap::new();
            }

            mem::take(&mut inner.waiting_receivers)
        };

        for rx in waiting_rx.into_iter().flat_map(|w| w.upgrade()) {
            let _ = rx.lock().fulfill(WakeReason::Shutdown);
        }
    }
}

// TODO(perf): try and reduce contention on Inner as much as possible
// Might be able to move some stuff out to atomics, or lock it separately. This should net some
// overall performance gains, but these likely won't in crude_bench or any throughput testing
struct ChanInner<A> {
    ordered_queue: VecDeque<MessageToOneActor<A>>,
    waiting_senders: VecDeque<Weak<Spinlock<WaitingSender<A>>>>,
    waiting_receivers: VecDeque<Weak<Spinlock<WaitingReceiver<A>>>>,
    priority_queue: BinaryHeap<PriorityMessageToOne<A>>,
    broadcast_queues: Vec<Weak<BroadcastQueue<A>>>,
    broadcast_tail: usize,
}

impl<A> ChanInner<A> {
    fn is_full(&self, capacity: Option<usize>) -> bool {
        capacity.map_or(false, |cap| self.ordered_queue.len() >= cap)
    }

    fn pop_priority(&mut self) -> Option<MessageToOneActor<A>> {
        Some(self.priority_queue.pop()?.val)
    }

    fn pop_ordered(&mut self, capacity: Option<usize>) -> Option<MessageToOneActor<A>> {
        // If len < cap after popping this message, try fulfill at most one waiting sender
        if self.is_full(capacity) {
            match self.try_fulfill_sender(MessageType::Ordered) {
                Some(SentMessage::Ordered(msg)) => self.ordered_queue.push_back(msg),
                Some(_) => unreachable!(),
                None => {}
            }
        }

        self.ordered_queue.pop_front()
    }

    fn try_advance_broadcast_tail(&mut self) {
        let mut longest = 0;
        for queue in &self.broadcast_queues {
            if let Some(queue) = queue.upgrade() {
                longest = cmp::max(longest, queue.lock().len());
            }
        }

        self.broadcast_tail = longest;
    }

    fn try_fulfill_receiver(&mut self, mut reason: WakeReason<A>) -> Result<(), WakeReason<A>> {
        while let Some(rx) = self.waiting_receivers.pop_front() {
            if let Some(rx) = rx.upgrade() {
                reason = match rx.lock().fulfill(reason) {
                    Ok(()) => return Ok(()),
                    Err(reason) => reason,
                }
            }
        }

        Err(reason)
    }

    // TODO get actor message type enum
    fn try_fulfill_sender(&mut self, for_type: MessageType) -> Option<SentMessage<A>> {
        let mut i = 0;
        while i < self.waiting_senders.len() {
            let should_pop = match self.waiting_senders.get(i).unwrap().upgrade() {
                Some(tx) => {
                    let tx = tx.lock();
                    match tx.peek() {
                        SentMessage::Ordered(_) if for_type == MessageType::Ordered => true,
                        SentMessage::Prioritized(_) if for_type == MessageType::Priority => true,
                        SentMessage::ToAllActors(_) if for_type == MessageType::Broadcast => true,
                        _ => {
                            i += 1;
                            false
                        }
                    }
                }
                None => {
                    self.waiting_senders.remove(i);
                    false
                }
            };

            if should_pop {
                // TODO could unwrap here
                return Some(self.waiting_senders.remove(i)?.upgrade()?.lock().fulfill());
            }
        }

        None
    }
}

#[derive(Eq, PartialEq)]
enum MessageType {
    Broadcast,
    Ordered,
    Priority,
}

pub enum SentMessage<A> {
    Ordered(MessageToOneActor<A>),
    Prioritized(PriorityMessageToOne<A>),
    ToAllActors(Arc<dyn BroadcastEnvelope<Actor = A>>),
}

impl<A> From<SentMessage<A>> for WakeReason<A> {
    fn from(msg: SentMessage<A>) -> Self {
        match msg {
            SentMessage::Ordered(m) => {
                WakeReason::MessageToOneActor(PriorityMessageToOne::new(Priority::default(), m))
            }
            SentMessage::Prioritized(m) => WakeReason::MessageToOneActor(m),
            SentMessage::ToAllActors(_) => WakeReason::MessageToAllActors,
        }
    }
}

impl<A> From<PriorityMessageToOne<A>> for SentMessage<A> {
    fn from(msg: PriorityMessageToOne<A>) -> Self {
        if msg.priority == Priority::default() {
            SentMessage::Ordered(msg.val)
        } else {
            SentMessage::Prioritized(msg)
        }
    }
}

enum TrySendFail<A> {
    Full(Arc<Spinlock<WaitingSender<A>>>),
    Disconnected,
}

pub enum ActorMessage<A> {
    ToOneActor(MessageToOneActor<A>),
    ToAllActors(Arc<dyn BroadcastEnvelope<Actor = A>>),
    Shutdown,
}

impl<A> From<MessageToOneActor<A>> for ActorMessage<A> {
    fn from(msg: MessageToOneActor<A>) -> Self {
        ActorMessage::ToOneActor(msg)
    }
}

impl<A> From<PriorityMessageToOne<A>> for ActorMessage<A> {
    fn from(msg: PriorityMessageToOne<A>) -> Self {
        ActorMessage::ToOneActor(msg.val)
    }
}

impl<A> From<Arc<dyn BroadcastEnvelope<Actor = A>>> for ActorMessage<A> {
    fn from(msg: Arc<dyn BroadcastEnvelope<Actor = A>>) -> Self {
        ActorMessage::ToAllActors(msg)
    }
}

enum WakeReason<A> {
    MessageToOneActor(PriorityMessageToOne<A>),
    // should be fetched from own receiver
    MessageToAllActors,
    Shutdown,
    // ReceiveFuture::cancel was called
    Cancelled,
}

pub trait HasPriority {
    fn priority(&self) -> Priority;
}

impl<A> HasPriority for PriorityMessageToOne<A> {
    fn priority(&self) -> Priority {
        self.priority
    }
}

pub struct PriorityMessageToOne<A> {
    priority: Priority,
    val: MessageToOneActor<A>,
}

impl<A> PriorityMessageToOne<A> {
    pub fn new(priority: Priority, val: MessageToOneActor<A>) -> Self {
        PriorityMessageToOne { priority, val }
    }
}

impl<A> PartialEq for PriorityMessageToOne<A> {
    fn eq(&self, other: &Self) -> bool {
        self.priority == other.priority
    }
}

impl<A> Eq for PriorityMessageToOne<A> {}

impl<A> PartialOrd for PriorityMessageToOne<A> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl<A> Ord for PriorityMessageToOne<A> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.priority.cmp(&other.priority)
    }
}

struct MessageToAllActors<A>(Arc<dyn BroadcastEnvelope<Actor = A>>);

impl<A> HasPriority for MessageToAllActors<A> {
    fn priority(&self) -> Priority {
        self.0.priority()
    }
}

impl<A> Eq for MessageToAllActors<A> {}

impl<A> PartialEq<Self> for MessageToAllActors<A> {
    fn eq(&self, other: &Self) -> bool {
        self.0.priority() == other.0.priority()
    }
}

impl<A> PartialOrd<Self> for MessageToAllActors<A> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl<A> Ord for MessageToAllActors<A> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.priority().cmp(&other.priority())
    }
}

// TODO(test) migrate to basic.rs
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
        let rx2_shallow = rx2.cloned_same_broadcast_mailbox();

        let orig = Arc::new(BroadcastEnvelopeConcrete::new("Hi", Priority::Valued(1)));
        let orig = orig as Arc<dyn BroadcastEnvelope<Actor = MyActor>>;
        tx.send(SentMessage::ToAllActors(orig.clone()))
            .await
            .unwrap();

        let eq = |msg| assert_eq!(&msg as *const _ as *const (), &msg as *const _ as *const ());

        match rx.receive().await {
            ActorMessage::ToAllActors(msg) => eq(msg),
            _ => panic!("Expected broadcast message, got something else"),
        }

        let rx2_shallow_recv = rx2_shallow.receive();

        match rx2.receive().await {
            ActorMessage::ToAllActors(msg) => eq(msg),
            _ => panic!("Expected broadcast message, got something else"),
        }

        assert!(rx.receive().now_or_never().is_none());
        assert!(rx2.receive().now_or_never().is_none());
        assert!(rx2_shallow_recv.now_or_never().is_none());
    }
}
