//! Latency is prioritised over most accurate prioritisation. Specifically, at most one low priority
//! message may be handled before piled-up higher priority messages will be handled.

mod priority;
mod ptr;
mod waiting_receiver;
mod waiting_sender;

use std::cmp::Ordering;
use std::collections::{BinaryHeap, VecDeque};
use std::sync::atomic::AtomicUsize;
use std::sync::{atomic, Arc, Mutex, Weak};
use std::{cmp, mem};

use event_listener::{Event, EventListener};
pub use priority::{ByPriority, HasPriority, Priority};
pub use ptr::{Ptr, RefCounter, Rx, TxEither, TxStrong, TxWeak};
pub use waiting_receiver::WaitingReceiver;
pub use waiting_sender::WaitingSender;

use crate::envelope::{BroadcastEnvelope, MessageEnvelope, Shutdown};
use crate::{Actor, Error};

pub type MessageToOne<A> = Box<dyn MessageEnvelope<Actor = A>>;
pub type MessageToAll<A> = Arc<dyn BroadcastEnvelope<Actor = A>>;
pub type BroadcastQueue<A> = spin::Mutex<BinaryHeap<ByPriority<MessageToAll<A>>>>;

/// Create an actor mailbox, returning a sender and receiver for it.
///
/// The given capacity is applied separately for unicast and broadcast messages.
pub fn new<A>(capacity: Option<usize>) -> (Ptr<A, TxStrong>, Ptr<A, Rx>) {
    let inner = Arc::new(Chan::new(capacity));

    let tx = Ptr::<A, TxStrong>::new(inner.clone());
    let rx = Ptr::<A, Rx>::new(inner);

    (tx, rx)
}

// Public because of private::RefCounterInner. This should never actually be exported, though.
pub struct Chan<A> {
    chan: Mutex<Inner<A>>,
    on_shutdown: Event,
    sender_count: AtomicUsize,
    receiver_count: AtomicUsize,
}

impl<A> Chan<A> {
    pub fn new(capacity: Option<usize>) -> Self {
        Self {
            chan: Mutex::new(Inner::new(capacity)),
            on_shutdown: Event::new(),
            sender_count: AtomicUsize::new(0),
            receiver_count: AtomicUsize::new(0),
        }
    }

    /// Callback to be invoked every time a receiver is created
    pub fn on_receiver_created(&self) {
        self.receiver_count.fetch_add(1, atomic::Ordering::Relaxed);
    }

    /// Callback to be invoked every time a receiver is destroyed.
    pub fn on_receiver_dropped(&self) {
        // Memory orderings copied from Arc::drop
        if self.receiver_count.fetch_sub(1, atomic::Ordering::Release) != 1 {
            return;
        }

        atomic::fence(atomic::Ordering::Acquire);

        self.shutdown_waiting_senders();
    }

    /// Callback to be invoked every time a sender is created.
    pub fn on_sender_created(&self) {
        // Memory orderings copied from Arc::clone
        self.sender_count.fetch_add(1, atomic::Ordering::Relaxed);
    }

    /// Callback to be invoked every time a sender is destroyed (i.e. dropped).
    pub fn on_sender_dropped(&self) {
        // Memory orderings copied from Arc::drop
        if self.sender_count.fetch_sub(1, atomic::Ordering::Release) != 1 {
            return;
        }

        atomic::fence(atomic::Ordering::Acquire);

        self.shutdown_waiting_receivers();
    }

    /// Creates a new broadcast mailbox on this channel.
    pub fn new_broadcast_mailbox(&self) -> Arc<BroadcastQueue<A>> {
        let mailbox = Arc::new(spin::Mutex::new(BinaryHeap::new()));
        self.chan
            .lock()
            .unwrap()
            .broadcast_queues
            .push(Arc::downgrade(&mailbox));

        mailbox
    }

    pub fn try_send_to_one(
        &self,
        mut message: MessageToOne<A>,
    ) -> Result<Result<(), MailboxFull<MessageToOne<A>>>, Error> {
        if !self.is_connected() {
            return Err(Error::Disconnected);
        }

        message.start_span();

        let mut inner = self.chan.lock().unwrap();

        let unfulfilled_msg = if let Err(msg) = inner.try_fulfill_receiver(message) {
            msg
        } else {
            return Ok(Ok(()));
        };

        if inner.is_unicast_full() {
            let (handle, waiting) = WaitingSender::new(unfulfilled_msg);
            inner.waiting_send_to_one.push_back(handle);

            return Ok(Err(MailboxFull(waiting)));
        }

        inner.unicast_queue.push(ByPriority(unfulfilled_msg));

        Ok(Ok(()))
    }

    pub fn try_send_to_all(
        &self,
        mut message: MessageToAll<A>,
    ) -> Result<Result<(), MailboxFull<MessageToAll<A>>>, Error> {
        if !self.is_connected() {
            return Err(Error::Disconnected);
        }

        Arc::get_mut(&mut message)
            .expect("calling after try_send not supported")
            .start_span();

        let mut inner = self.chan.lock().unwrap();

        if inner.is_broadcast_full() {
            let (handle, waiting) = WaitingSender::new(message);
            inner.waiting_send_to_all.push_back(handle);

            return Ok(Err(MailboxFull(waiting)));
        }

        inner.send_broadcast(message);

        Ok(Ok(()))
    }

    pub fn try_recv(
        &self,
        broadcast_mailbox: &BroadcastQueue<A>,
    ) -> Result<ActorMessage<A>, WaitingReceiver<A>> {
        // Lock `ChanInner` as the first thing. This avoids race conditions in modifying the broadcast mailbox.
        let mut inner = self.chan.lock().unwrap();

        // lock broadcast mailbox for as short as possible
        let broadcast_priority = {
            // Peek priorities in order to figure out which channel should be taken from
            broadcast_mailbox.lock().peek().map(|it| it.priority())
        };

        let shared_priority: Option<Priority> = inner.unicast_queue.peek().map(|it| it.priority());

        // Choose which priority channel to take from
        match shared_priority.cmp(&broadcast_priority) {
            // Shared priority is greater or equal (and it is not empty)
            Ordering::Greater | Ordering::Equal if shared_priority.is_some() => {
                Ok(inner.pop_unicast().unwrap().into())
            }
            // Shared priority is less - take from broadcast
            Ordering::Less => Ok(inner.pop_broadcast(broadcast_mailbox).unwrap().into()),
            // Equal, but both are empty, so wait or exit if shutdown
            _ => {
                // on_shutdown is only notified with inner locked, and it's locked here, so no race
                if self.sender_count.load(atomic::Ordering::SeqCst) == 0 {
                    return Ok(ActorMessage::Shutdown);
                }

                let (receiver, handle) = WaitingReceiver::new();

                inner.waiting_receivers_handles.push_back(handle);
                Err(receiver)
            }
        }
    }

    pub fn is_connected(&self) -> bool {
        self.receiver_count.load(atomic::Ordering::SeqCst) > 0
            && self.sender_count.load(atomic::Ordering::SeqCst) > 0
    }

    pub fn len(&self) -> usize {
        let inner = self.chan.lock().unwrap();
        inner.broadcast_tail + inner.unicast_queue.len()
    }

    pub fn capacity(&self) -> Option<usize> {
        self.chan.lock().unwrap().capacity
    }

    /// Shutdown all [`WaitingReceiver`]s in this channel.
    fn shutdown_waiting_receivers(&self) {
        let waiting_rx = {
            let mut inner = match self.chan.lock() {
                Ok(lock) => lock,
                Err(_) => return, // Poisoned, ignore
            };

            // We don't need to notify on_shutdown here, as that is only used by senders
            // Receivers will be woken with the fulfills below, or they will realise there are
            // no senders when they check the tx refcount

            mem::take(&mut inner.waiting_receivers_handles)
        };

        for rx in waiting_rx {
            rx.notify_channel_shutdown();
        }
    }

    pub fn shutdown_all_receivers(&self)
    where
        A: Actor,
    {
        self.chan
            .lock()
            .unwrap()
            .send_broadcast(Arc::new(Shutdown::new()));
    }

    /// Shutdown all [`WaitingSender`]s in this channel.
    fn shutdown_waiting_senders(&self) {
        let mut inner = match self.chan.lock() {
            Ok(lock) => lock,
            Err(_) => return, // Poisoned, ignore
        };

        self.on_shutdown.notify(usize::MAX);

        // Let any outstanding messages drop
        inner.unicast_queue.clear();
        inner.broadcast_queues.clear();

        // Close (and potentially wake) outstanding waiting senders
        inner.waiting_send_to_one.clear();
        inner.waiting_send_to_all.clear();
    }

    pub fn disconnect_listener(&self) -> Option<EventListener> {
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

    /// Re-queue the given message.
    ///
    /// Normally, messages are delivered from the inbox straight to the actor. It can however happen
    /// that this process gets cancelled. In that case, this function can be used to re-queue the
    /// given message so it does not get lost.
    ///
    /// Note that the ordering of messages in the queues may be slightly off with this function.
    pub fn requeue_message(&self, msg: MessageToOne<A>) {
        let mut inner = match self.chan.lock() {
            Ok(lock) => lock,
            Err(_) => return, // If we can't lock the inner channel, there is nothing we can do.
        };

        if let Err(msg) = inner.try_fulfill_receiver(msg) {
            inner.unicast_queue.push(ByPriority(msg));
        }
    }

    pub fn next_broadcast_message(
        &self,
        broadcast_mailbox: &BroadcastQueue<A>,
    ) -> Option<Arc<dyn BroadcastEnvelope<Actor = A>>> {
        self.chan.lock().unwrap().pop_broadcast(broadcast_mailbox)
    }
}

struct Inner<A> {
    capacity: Option<usize>,
    waiting_send_to_one: VecDeque<waiting_sender::Handle<MessageToOne<A>>>,
    waiting_send_to_all: VecDeque<waiting_sender::Handle<MessageToAll<A>>>,
    waiting_receivers_handles: VecDeque<waiting_receiver::Handle<A>>,
    unicast_queue: BinaryHeap<ByPriority<MessageToOne<A>>>,
    broadcast_queues: Vec<Weak<BroadcastQueue<A>>>,
    broadcast_tail: usize,
}

impl<A> Inner<A> {
    fn new(capacity: Option<usize>) -> Self {
        Self {
            capacity,
            waiting_send_to_one: VecDeque::default(),
            waiting_send_to_all: VecDeque::default(),
            waiting_receivers_handles: VecDeque::default(),
            unicast_queue: BinaryHeap::default(),
            broadcast_queues: Vec::default(),
            broadcast_tail: 0,
        }
    }

    fn pop_unicast(&mut self) -> Option<Box<dyn MessageEnvelope<Actor = A>>> {
        let msg = self.unicast_queue.pop()?.0;

        if !self.is_unicast_full() {
            if let Some(msg) = self.try_take_waiting_unicast_message() {
                self.unicast_queue.push(ByPriority(msg))
            }
        }

        Some(msg)
    }

    pub fn pop_broadcast(
        &mut self,
        broadcast_mailbox: &BroadcastQueue<A>,
    ) -> Option<Arc<dyn BroadcastEnvelope<Actor = A>>> {
        let message = broadcast_mailbox.lock().pop()?.0;

        self.broadcast_tail = self.longest_broadcast_queue();

        if !self.is_broadcast_full() {
            if let Some(m) = self.try_take_waiting_broadcast_message() {
                self.send_broadcast(m)
            }
        }

        Some(message)
    }

    fn longest_broadcast_queue(&self) -> usize {
        let mut longest = 0;

        for queue in &self.broadcast_queues {
            if let Some(queue) = queue.upgrade() {
                longest = cmp::max(longest, queue.lock().len());
            }
        }

        longest
    }

    fn send_broadcast(&mut self, m: MessageToAll<A>) {
        self.broadcast_queues.retain(|queue| match queue.upgrade() {
            Some(q) => {
                q.lock().push(ByPriority(m.clone()));
                true
            }
            None => false, // The corresponding receiver has been dropped - remove it
        });

        for rx in mem::take(&mut self.waiting_receivers_handles) {
            let _ = rx.notify_new_broadcast();
        }

        self.broadcast_tail += 1;
    }

    fn try_fulfill_receiver(&mut self, mut msg: MessageToOne<A>) -> Result<(), MessageToOne<A>> {
        while let Some(rx) = self.waiting_receivers_handles.pop_front() {
            match rx.notify_new_message(msg) {
                Ok(()) => return Ok(()),
                Err(unfulfilled_msg) => msg = unfulfilled_msg,
            }
        }

        Err(msg)
    }

    fn try_take_waiting_unicast_message(&mut self) -> Option<MessageToOne<A>> {
        loop {
            if let Some(msg) =
                find_remove_highest_priority(&mut self.waiting_send_to_one)?.take_message()
            {
                return Some(msg);
            }
        }
    }

    fn try_take_waiting_broadcast_message(&mut self) -> Option<MessageToAll<A>> {
        loop {
            if let Some(msg) =
                find_remove_highest_priority(&mut self.waiting_send_to_all)?.take_message()
            {
                return Some(msg);
            }
        }
    }

    fn is_broadcast_full(&self) -> bool {
        self.capacity
            .map_or(false, |cap| self.broadcast_tail >= cap)
    }

    fn is_unicast_full(&self) -> bool {
        self.capacity
            .map_or(false, |cap| self.unicast_queue.len() >= cap)
    }
}

fn find_remove_highest_priority<M>(
    queue: &mut VecDeque<waiting_sender::Handle<M>>,
) -> Option<waiting_sender::Handle<M>>
where
    M: HasPriority,
{
    queue.retain(|handle| handle.is_active()); // Only process handles which are still active.

    let pos = queue
        .iter()
        .enumerate()
        .max_by_key(|(_, handle)| handle.priority())?
        .0;

    queue.remove(pos)
}

/// An error returned in case the mailbox of an actor is full.
pub struct MailboxFull<M>(pub WaitingSender<M>);

pub enum ActorMessage<A> {
    ToOneActor(MessageToOne<A>),
    ToAllActors(MessageToAll<A>),
    Shutdown,
}

impl<A> From<MessageToOne<A>> for ActorMessage<A> {
    fn from(msg: MessageToOne<A>) -> Self {
        ActorMessage::ToOneActor(msg)
    }
}

impl<A> From<MessageToAll<A>> for ActorMessage<A> {
    fn from(msg: MessageToAll<A>) -> Self {
        ActorMessage::ToAllActors(msg)
    }
}
