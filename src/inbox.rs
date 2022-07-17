//! Latency is prioritised over most accurate prioritisation. Specifically, at most one low priority
//! message may be handled before piled-up higher priority messages will be handled.

pub mod rx;
pub mod tx;

use std::cmp::Ordering;
use std::collections::{BinaryHeap, VecDeque};
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex, Weak};
use std::{cmp, mem};

use event_listener::Event;
pub use rx::Receiver;
pub use tx::{SendFuture, Sender};

use crate::envelope::{BroadcastEnvelope, MessageEnvelope};
use crate::inbox::rx::{RxStrong, WaitingReceiver};
use crate::inbox::tx::{TxStrong, WaitingSender};

type Spinlock<T> = spin::Mutex<T>;
type BroadcastQueue<A> = Spinlock<BinaryHeap<ByPriority<Arc<dyn BroadcastEnvelope<Actor = A>>>>>;

/// Create an actor mailbox, returning a sender and receiver for it. The given capacity is applied
/// severally to each send type - priority, ordered, and broadcast.
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
        sender_count: AtomicUsize::new(0),
        receiver_count: AtomicUsize::new(0),
    });

    let tx = Sender::new(inner.clone());
    let rx = Receiver::new(inner, broadcast_mailbox);

    (tx, rx)
}

// Public because of private::RefCounterInner. This should never actually be exported, though.
pub struct Chan<A> {
    chan: Mutex<ChanInner<A>>,
    capacity: Option<usize>,
    on_shutdown: Event,
    sender_count: AtomicUsize,
    receiver_count: AtomicUsize,
}

impl<A> Chan<A> {
    fn is_full(&self, len: usize) -> bool {
        self.capacity.map_or(false, |cap| len >= cap)
    }
}

struct ChanInner<A> {
    ordered_queue: VecDeque<Box<dyn MessageEnvelope<Actor = A>>>,
    waiting_senders: VecDeque<Weak<Spinlock<WaitingSender<A>>>>,
    waiting_receivers: VecDeque<Weak<Spinlock<WaitingReceiver<A>>>>,
    priority_queue: BinaryHeap<ByPriority<Box<dyn MessageEnvelope<Actor = A>>>>,
    broadcast_queues: Vec<Weak<BroadcastQueue<A>>>,
    broadcast_tail: usize,
}

impl<A> ChanInner<A> {
    fn pop_priority(
        &mut self,
        capacity: Option<usize>,
    ) -> Option<Box<dyn MessageEnvelope<Actor = A>>> {
        // If len < cap after popping this message, try fulfill at most one waiting sender
        if capacity.map_or(false, |cap| cap == self.priority_queue.len()) {
            match self.try_fulfill_sender(MessageType::Priority) {
                Some(SentMessage::ToOneActor(msg)) => self.priority_queue.push(ByPriority(msg)),
                Some(_) => unreachable!(),
                None => {}
            }
        }

        Some(self.priority_queue.pop()?.0)
    }

    fn pop_ordered(
        &mut self,
        capacity: Option<usize>,
    ) -> Option<Box<dyn MessageEnvelope<Actor = A>>> {
        // If len < cap after popping this message, try fulfill at most one waiting sender
        if capacity.map_or(false, |cap| cap == self.ordered_queue.len()) {
            match self.try_fulfill_sender(MessageType::Ordered) {
                Some(SentMessage::ToOneActor(msg)) => self.ordered_queue.push_back(msg),
                Some(_) => unreachable!(),
                None => {}
            }
        }

        self.ordered_queue.pop_front()
    }

    fn try_advance_broadcast_tail(&mut self, capacity: Option<usize>) {
        let mut longest = 0;
        for queue in &self.broadcast_queues {
            if let Some(queue) = queue.upgrade() {
                longest = cmp::max(longest, queue.lock().len());
            }
        }

        self.broadcast_tail = longest;

        // If len < cap, try fulfill a waiting sender
        if capacity.map_or(false, |cap| longest < cap) {
            match self.try_fulfill_sender(MessageType::Broadcast) {
                Some(SentMessage::ToAllActors(m)) => self.send_broadcast(m),
                Some(_) => unreachable!(),
                None => {}
            }
        }
    }

    fn send_broadcast(&mut self, m: Arc<dyn BroadcastEnvelope<Actor = A>>) {
        self.broadcast_queues.retain(|queue| match queue.upgrade() {
            Some(q) => {
                q.lock().push(ByPriority(m.clone()));
                true
            }
            None => false, // The corresponding receiver has been dropped - remove it
        });

        let waiting = mem::take(&mut self.waiting_receivers);
        for rx in waiting.into_iter().flat_map(|w| w.upgrade()) {
            let _ = rx.lock().fulfill(WakeReason::MessageToAllActors);
        }

        self.broadcast_tail += 1;
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

    fn try_fulfill_sender(&mut self, for_type: MessageType) -> Option<SentMessage<A>> {
        self.waiting_senders
            .retain(|tx| Weak::strong_count(tx) != 0);

        loop {
            let pos = if for_type == MessageType::Ordered {
                self.waiting_senders
                    .iter()
                    .position(|tx| match tx.upgrade() {
                        Some(tx) => matches!(tx.lock().peek(), SentMessage::ToOneActor(m) if m.priority() == Priority::default()),
                        None => false,
                    })?
            } else {
                self.waiting_senders
                    .iter()
                    .enumerate()
                    .max_by_key(|(_idx, tx)| match tx.upgrade() {
                        Some(tx) => match tx.lock().peek() {
                            SentMessage::ToOneActor(m)
                                if for_type == MessageType::Priority
                                    && m.priority() > Priority::default() =>
                            {
                                Some(m.priority())
                            }
                            SentMessage::ToAllActors(m) if for_type == MessageType::Broadcast => {
                                Some(m.priority())
                            }
                            _ => None,
                        },
                        None => None,
                    })?
                    .0
            };

            if let Some(tx) = self.waiting_senders.remove(pos).unwrap().upgrade() {
                return Some(tx.lock().fulfill(true));
            }
        }
    }
}

#[derive(Eq, PartialEq)]
enum MessageType {
    Broadcast,
    Ordered,
    Priority,
}

pub enum SentMessage<A> {
    ToOneActor(Box<dyn MessageEnvelope<Actor = A>>),
    ToAllActors(Arc<dyn BroadcastEnvelope<Actor = A>>),
}

impl<A> From<SentMessage<A>> for WakeReason<A> {
    fn from(msg: SentMessage<A>) -> Self {
        match msg {
            SentMessage::ToOneActor(m) => WakeReason::MessageToOneActor(m),
            SentMessage::ToAllActors(_) => WakeReason::MessageToAllActors,
        }
    }
}

impl<A> From<Box<dyn MessageEnvelope<Actor = A>>> for SentMessage<A> {
    fn from(msg: Box<dyn MessageEnvelope<Actor = A>>) -> Self {
        SentMessage::ToOneActor(msg)
    }
}

enum TrySendFail<A> {
    Full(Arc<Spinlock<WaitingSender<A>>>),
    Disconnected,
}

pub enum ActorMessage<A> {
    ToOneActor(Box<dyn MessageEnvelope<Actor = A>>),
    ToAllActors(Arc<dyn BroadcastEnvelope<Actor = A>>),
    Shutdown,
}

impl<A> From<Box<dyn MessageEnvelope<Actor = A>>> for ActorMessage<A> {
    fn from(msg: Box<dyn MessageEnvelope<Actor = A>>) -> Self {
        ActorMessage::ToOneActor(msg)
    }
}

impl<A> From<Arc<dyn BroadcastEnvelope<Actor = A>>> for ActorMessage<A> {
    fn from(msg: Arc<dyn BroadcastEnvelope<Actor = A>>) -> Self {
        ActorMessage::ToAllActors(msg)
    }
}

enum WakeReason<A> {
    MessageToOneActor(Box<dyn MessageEnvelope<Actor = A>>),
    // should be fetched from own receiver
    MessageToAllActors,
    Shutdown,
    // ReceiveFuture::cancel was called
    Cancelled,
}

#[derive(Eq, PartialEq, Ord, PartialOrd, Copy, Clone)]
pub enum Priority {
    Valued(u32),
    Shutdown,
}

impl Default for Priority {
    fn default() -> Self {
        Priority::Valued(0)
    }
}

pub trait HasPriority {
    fn priority(&self) -> Priority;
}

/// A wrapper struct that allows comparison and ordering for anything thas has a priority, i.e. implements [`HasPriority`].
struct ByPriority<T>(pub T);

impl<T> HasPriority for ByPriority<T>
where
    T: HasPriority,
{
    fn priority(&self) -> Priority {
        self.0.priority()
    }
}

impl<T> PartialEq for ByPriority<T>
where
    T: HasPriority,
{
    fn eq(&self, other: &Self) -> bool {
        self.0.priority().eq(&other.0.priority())
    }
}

impl<T> Eq for ByPriority<T> where T: HasPriority {}

impl<T> PartialOrd for ByPriority<T>
where
    T: HasPriority,
{
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        self.0.priority().partial_cmp(&other.0.priority())
    }
}

impl<T> Ord for ByPriority<T>
where
    T: HasPriority,
{
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.priority().cmp(&other.0.priority())
    }
}
