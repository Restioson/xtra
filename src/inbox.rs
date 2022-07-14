//! Latency is prioritised over most accurate prioritisation. Specifically, at most one low priority
//! message may be handled before piled-up higher priority messages will be handled.

pub mod rx;
pub mod tx;

use std::cmp::Ordering;
use std::collections::{BinaryHeap, VecDeque};
use std::sync::atomic::AtomicUsize;
use std::sync::{atomic, Arc, Mutex, Weak};
use std::{cmp, mem};

use event_listener::{Event, EventListener};
pub use rx::Receiver;
pub use tx::{SendFuture, Sender};

use crate::envelope::{BroadcastEnvelope, MessageEnvelope, Shutdown};
use crate::inbox::rx::{RxStrong, WaitingReceiver};
use crate::inbox::tx::{TxStrong, WaitingSender};
use crate::Actor;

type Spinlock<T> = spin::Mutex<T>;
pub type MessageToOneActor<A> = Box<dyn MessageEnvelope<Actor = A>>;
type BroadcastQueue<A> = Spinlock<BinaryHeap<MessageToAllActors<A>>>;

/// Create an actor mailbox, returning a sender and receiver for it. The given capacity is applied
/// severally to each send type - priority, ordered, and broadcast.
pub fn new<A>(capacity: Option<usize>) -> (Sender<A, TxStrong>, Receiver<A, RxStrong>) {
    let inner = Arc::new(Chan::new(capacity));

    let tx = Sender::new(inner.clone());
    let rx = Receiver::new(inner);

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
    fn new(capacity: Option<usize>) -> Self {
        Self {
            capacity,
            chan: Mutex::new(ChanInner::default()),
            on_shutdown: Event::new(),
            sender_count: AtomicUsize::new(0),
            receiver_count: AtomicUsize::new(0),
        }
    }

    /// Creates a new broadcast mailbox on this channel.
    fn new_broadcast_mailbox(&self) -> Arc<BroadcastQueue<A>> {
        let mailbox = Arc::new(Spinlock::new(BinaryHeap::new()));
        self.chan
            .lock()
            .unwrap()
            .broadcast_queues
            .push(Arc::downgrade(&mailbox));

        mailbox
    }

    fn try_send(&self, message: SentMessage<A>) -> Result<(), TrySendFail<A>> {
        if !self.is_connected() {
            return Err(TrySendFail::Disconnected);
        }

        let mut inner = self.chan.lock().unwrap();

        match message {
            SentMessage::ToAllActors(m) if !self.is_full(inner.broadcast_tail) => {
                inner.send_broadcast(MessageToAllActors(m));
                Ok(())
            }
            SentMessage::ToAllActors(m) => {
                // on_shutdown is only notified with inner locked, and it's locked here, so no race
                let waiting = WaitingSender::new(SentMessage::ToAllActors(m));
                inner.waiting_senders.push_back(Arc::downgrade(&waiting));
                Err(TrySendFail::Full(waiting))
            }
            msg => {
                let res = inner.try_fulfill_receiver(msg.into());
                match res {
                    Ok(()) => Ok(()),
                    Err(WakeReason::MessageToOneActor(m))
                        if m.priority == 0 && !self.is_full(inner.ordered_queue.len()) =>
                    {
                        inner.ordered_queue.push_back(m.val);
                        Ok(())
                    }
                    Err(WakeReason::MessageToOneActor(m))
                        if m.priority != 0 && !self.is_full(inner.priority_queue.len()) =>
                    {
                        inner.priority_queue.push(m);
                        Ok(())
                    }
                    Err(WakeReason::MessageToOneActor(m)) => {
                        let waiting = WaitingSender::new(m.into());
                        inner.waiting_senders.push_back(Arc::downgrade(&waiting));
                        Err(TrySendFail::Full(waiting))
                    }
                    _ => unreachable!(),
                }
            }
        }
    }

    fn try_recv(
        &self,
        broadcast_mailbox: &BroadcastQueue<A>,
    ) -> Result<ActorMessage<A>, Arc<Spinlock<WaitingReceiver<A>>>> {
        let mut broadcast = broadcast_mailbox.lock();

        // Peek priorities in order to figure out which channel should be taken from
        let broadcast_priority = broadcast.peek().map(|it| it.priority());

        let mut inner = self.chan.lock().unwrap();

        let shared_priority: Option<Priority> = inner.priority_queue.peek().map(|it| it.priority());

        // Try take from ordered channel
        if cmp::max(shared_priority, broadcast_priority) <= Some(Priority::Valued(0)) {
            if let Some(msg) = inner.pop_ordered(self.capacity) {
                return Ok(msg.into());
            }
        }

        // Choose which priority channel to take from
        match shared_priority.cmp(&broadcast_priority) {
            // Shared priority is greater or equal (and it is not empty)
            Ordering::Greater | Ordering::Equal if shared_priority.is_some() => {
                Ok(inner.pop_priority(self.capacity).unwrap().into())
            }
            // Shared priority is less - take from broadcast
            Ordering::Less => {
                let msg = broadcast.pop().unwrap().0;
                drop(broadcast);
                inner.try_advance_broadcast_tail(self.capacity);

                Ok(msg.into())
            }
            // Equal, but both are empty, so wait or exit if shutdown
            _ => {
                // on_shutdown is only notified with inner locked, and it's locked here, so no race
                if self.sender_count.load(atomic::Ordering::SeqCst) == 0 {
                    return Ok(ActorMessage::Shutdown);
                }

                let waiting = Arc::new(Spinlock::new(WaitingReceiver::default()));
                inner.waiting_receivers.push_back(Arc::downgrade(&waiting));
                Err(waiting)
            }
        }
    }

    fn is_connected(&self) -> bool {
        self.receiver_count.load(atomic::Ordering::SeqCst) > 0
            && self.sender_count.load(atomic::Ordering::SeqCst) > 0
    }

    fn len(&self) -> usize {
        let inner = self.chan.lock().unwrap();
        inner.broadcast_tail + inner.ordered_queue.len() + inner.priority_queue.len()
    }

    fn is_full(&self, len: usize) -> bool {
        self.capacity.map_or(false, |cap| len >= cap)
    }

    /// Shutdown all [`WaitingReceiver`](crate::inbox::rx::WaitingReceiver)s in this channel.
    fn shutdown_waiting_receivers(&self) {
        let waiting_rx = {
            let mut inner = match self.chan.lock() {
                Ok(lock) => lock,
                Err(_) => return, // Poisoned, ignore
            };

            // We don't need to notify on_shutdown here, as that is only used by senders
            // Receivers will be woken with the fulfills below, or they will realise there are
            // no senders when they check the tx refcount

            mem::take(&mut inner.waiting_receivers)
        };

        for rx in waiting_rx.into_iter().flat_map(|w| w.upgrade()) {
            let _ = rx.lock().fulfill(WakeReason::Shutdown);
        }
    }

    fn shutdown_all_receivers(&self)
    where
        A: Actor,
    {
        self.chan
            .lock()
            .unwrap()
            .send_broadcast(MessageToAllActors(Arc::new(Shutdown::new())));
    }

    /// Shutdown all [`WaitingSender`](crate::inbox::tx::WaitingSender)s in this channel.
    fn shutdown_waiting_senders(&self) {
        let waiting_tx = {
            let mut inner = match self.chan.lock() {
                Ok(lock) => lock,
                Err(_) => return, // Poisoned, ignore
            };

            self.on_shutdown.notify(usize::MAX);

            // Let any outstanding messages drop
            inner.ordered_queue.clear();
            inner.priority_queue.clear();
            inner.broadcast_queues.clear();

            mem::take(&mut inner.waiting_senders)
        };

        for tx in waiting_tx.into_iter().flat_map(|w| w.upgrade()) {
            tx.lock().set_closed();
        }
    }

    fn disconnect_listener(&self) -> Option<EventListener> {
        // Listener is created before checking connectivity to avoid the following race scenario:
        //
        // 1. is_connected returns true
        // 2. on_shutdown is notified
        // 3. listener is registered
        //
        // The listener would never be woken in this scenario, as the notification preceded its
        // creation.
        let listener = self.on_shutdown.listen();

        if self.is_connected() {
            Some(listener)
        } else {
            None
        }
    }

    fn pop_broadcast_message(
        &self,
        broadcast_mailbox: &BroadcastQueue<A>,
    ) -> Option<MessageToAllActors<A>> {
        let message = broadcast_mailbox.lock().pop();

        // Advance the broadcast tail if we successfully took a message.
        if message.is_some() {
            self.chan
                .lock()
                .unwrap()
                .try_advance_broadcast_tail(self.capacity);
        }

        message
    }

    /// Re-queue the given message.
    ///
    /// Normally, messages are delivered from the inbox straight to the actor. It can however happen
    /// that this process gets cancelled. In that case, this function can be used to re-queue the
    /// given message so it does not get lost.
    ///
    /// Note that the ordering of messages in the queues may be slightly off with this function.
    fn requeue_message(&self, msg: PriorityMessageToOne<A>) {
        let mut inner = match self.chan.lock() {
            Ok(lock) => lock,
            Err(_) => return, // If we can't lock the inner channel, there is nothing we can do.
        };

        let res = inner.try_fulfill_receiver(WakeReason::MessageToOneActor(msg));
        match res {
            Ok(()) => (),
            Err(WakeReason::MessageToOneActor(msg)) => {
                if msg.priority == 0 {
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

struct ChanInner<A> {
    ordered_queue: VecDeque<MessageToOneActor<A>>,
    waiting_senders: VecDeque<Weak<Spinlock<WaitingSender<A>>>>,
    waiting_receivers: VecDeque<Weak<Spinlock<WaitingReceiver<A>>>>,
    priority_queue: BinaryHeap<PriorityMessageToOne<A>>,
    broadcast_queues: Vec<Weak<BroadcastQueue<A>>>,
    broadcast_tail: usize,
}

// Manual impl to avoid `A: Default` bound.
impl<A> Default for ChanInner<A> {
    fn default() -> Self {
        Self {
            ordered_queue: VecDeque::default(),
            waiting_senders: VecDeque::default(),
            waiting_receivers: VecDeque::default(),
            priority_queue: BinaryHeap::default(),
            broadcast_queues: Vec::default(),
            broadcast_tail: 0,
        }
    }
}

impl<A> ChanInner<A> {
    fn pop_priority(&mut self, capacity: Option<usize>) -> Option<MessageToOneActor<A>> {
        // If len < cap after popping this message, try fulfill at most one waiting sender
        if capacity.map_or(false, |cap| cap == self.priority_queue.len()) {
            match self.try_fulfill_sender(MessageType::Priority) {
                Some(SentMessage::ToOneActor(msg)) => self.priority_queue.push(msg),
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
                Some(SentMessage::ToOneActor(msg)) => self.ordered_queue.push_back(msg.val),
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
                Some(SentMessage::ToAllActors(m)) => self.send_broadcast(MessageToAllActors(m)),
                Some(_) => unreachable!(),
                None => {}
            }
        }
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
                        Some(tx) => matches!(tx.lock().peek(), SentMessage::ToOneActor(m) if m.priority == 0),
                        None => false,
                    })?
            } else {
                self.waiting_senders
                    .iter()
                    .enumerate()
                    .max_by_key(|(_idx, tx)| match tx.upgrade() {
                        Some(tx) => match tx.lock().peek() {
                            SentMessage::ToOneActor(m)
                                if for_type == MessageType::Priority && m.priority > 0 =>
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
    ToOneActor(PriorityMessageToOne<A>),
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

impl<A> From<PriorityMessageToOne<A>> for SentMessage<A> {
    fn from(msg: PriorityMessageToOne<A>) -> Self {
        SentMessage::ToOneActor(msg)
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

pub enum WakeReason<A> {
    MessageToOneActor(PriorityMessageToOne<A>),
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

pub trait HasPriority {
    fn priority(&self) -> Priority;
}

impl<A> HasPriority for PriorityMessageToOne<A> {
    fn priority(&self) -> Priority {
        Priority::Valued(self.priority)
    }
}

pub struct PriorityMessageToOne<A> {
    pub priority: u32,
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
