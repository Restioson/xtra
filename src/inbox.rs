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
        shutdown: AtomicBool::new(false),
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
    shutdown: AtomicBool,
    sender_count: AtomicUsize,
    receiver_count: AtomicUsize,
}

impl<A> Chan<A> {
    fn is_full(&self, len: usize) -> bool {
        self.capacity.map_or(false, |cap| len >= cap)
    }

    fn is_shutdown(&self) -> bool {
        self.shutdown.load(atomic::Ordering::SeqCst)
    }

    pub fn shutdown(&self) {
        let waiting_receivers = {
            let mut inner = match self.chan.lock() {
                Ok(lock) => lock,
                Err(_) => return, // Poisoned, ignore
            };

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

struct ChanInner<A> {
    ordered_queue: VecDeque<MessageToOneActor<A>>,
    waiting_senders: VecDeque<Weak<Spinlock<WaitingSender<A>>>>,
    waiting_receivers: VecDeque<Weak<Spinlock<WaitingReceiver<A>>>>,
    priority_queue: BinaryHeap<PriorityMessageToOne<A>>,
    broadcast_queues: Vec<Weak<BroadcastQueue<A>>>,
    broadcast_tail: usize,
}

impl<A> ChanInner<A> {
    fn pop_priority(&mut self, capacity: Option<usize>) -> Option<MessageToOneActor<A>> {
        // If len < cap after popping this message, try fulfill at most one waiting sender
        if capacity.map_or(false, |cap| cap == self.priority_queue.len()) {
            match self.try_fulfill_sender(MessageType::Priority) {
                Some(SentMessage::Prioritized(msg)) => self.priority_queue.push(msg),
                Some(_) => unreachable!(),
                None => {}
            }
        }

        Some(self.priority_queue.pop()?.val)
    }

    fn pop_ordered(&mut self, capacity: Option<usize>) -> Option<MessageToOneActor<A>> {
        // If len < cap after popping this message, try fulfill at most one waiting sender
        if capacity.map_or(false, |cap| cap == self.ordered_queue.len()) {
            match self.try_fulfill_sender(MessageType::Ordered) {
                Some(SentMessage::Ordered(msg)) => self.ordered_queue.push_back(msg),
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

        // If len < cap, try fulfill a waiting sender
        if capacity.map_or(false, |cap| longest < cap) {
            match self.try_fulfill_sender(MessageType::Broadcast) {
                Some(SentMessage::ToAllActors(m)) => self.send_broadcast(MessageToAllActors(m)),
                Some(_) => unreachable!(),
                None => {}
            }
        }

        self.broadcast_tail = longest;
    }

    fn send_broadcast(&mut self, m: MessageToAllActors<A>) {
        self.broadcast_queues.retain(|queue| match queue.upgrade() {
            Some(q) => {
                q.lock().push(m.clone());
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
                        Some(tx) => matches!(tx.lock().peek(), SentMessage::Ordered(_)),
                        None => false,
                    })?
            } else {
                self.waiting_senders
                    .iter()
                    .enumerate()
                    .max_by_key(|(_idx, tx)| match tx.upgrade() {
                        Some(tx) => match tx.lock().peek() {
                            SentMessage::Prioritized(m) if for_type == MessageType::Priority => {
                                Some(m.priority)
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
                return Some(tx.lock().fulfill());
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
    Ordered(MessageToOneActor<A>),
    Prioritized(PriorityMessageToOne<A>),
    ToAllActors(Arc<dyn BroadcastEnvelope<Actor = A>>),
}

impl<A> From<SentMessage<A>> for WakeReason<A> {
    fn from(msg: SentMessage<A>) -> Self {
        match msg {
            SentMessage::Ordered(m) => {
                WakeReason::MessageToOneActor(PriorityMessageToOne::new(0, m))
            }
            SentMessage::Prioritized(m) => WakeReason::MessageToOneActor(m),
            SentMessage::ToAllActors(_) => WakeReason::MessageToAllActors,
        }
    }
}

impl<A> From<PriorityMessageToOne<A>> for SentMessage<A> {
    fn from(msg: PriorityMessageToOne<A>) -> Self {
        if msg.priority == 0 {
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
    fn priority(&self) -> u32;
}

impl<A> HasPriority for PriorityMessageToOne<A> {
    fn priority(&self) -> u32 {
        self.priority
    }
}

pub struct PriorityMessageToOne<A> {
    priority: u32,
    val: MessageToOneActor<A>,
}

impl<A> PriorityMessageToOne<A> {
    pub fn new(priority: u32, val: MessageToOneActor<A>) -> Self {
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

impl<A> Clone for MessageToAllActors<A> {
    fn clone(&self) -> Self {
        MessageToAllActors(self.0.clone())
    }
}

impl<A> HasPriority for MessageToAllActors<A> {
    fn priority(&self) -> u32 {
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
