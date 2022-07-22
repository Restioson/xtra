//! Latency is prioritised over most accurate prioritisation. Specifically, at most one low priority
//! message may be handled before piled-up higher priority messages will be handled.

pub mod rx;
pub mod tx;
mod waiting_receiver;

use std::cmp::Ordering;
use std::collections::{BinaryHeap, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::AtomicUsize;
use std::sync::{atomic, Arc, Mutex, Weak};
use std::task::{Context, Poll, Waker};
use std::{cmp, mem};

use event_listener::{Event, EventListener};
pub use rx::Receiver;
pub use tx::Sender;

use crate::envelope::{BroadcastEnvelope, MessageEnvelope, Shutdown};
use crate::inbox::rx::RxStrong;
use crate::inbox::tx::TxStrong;
use crate::inbox::waiting_receiver::{FulfillHandle, WaitingReceiver};
use crate::{Actor, Error};

type Spinlock<T> = spin::Mutex<T>;
type BroadcastQueue<A> = Spinlock<BinaryHeap<ByPriority<Arc<dyn BroadcastEnvelope<Actor = A>>>>>;

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
    on_shutdown: Event,
    sender_count: AtomicUsize,
    receiver_count: AtomicUsize,
}

impl<A> Chan<A> {
    pub fn new(capacity: Option<usize>) -> Self {
        Self {
            chan: Mutex::new(ChanInner::new(capacity)),
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

    pub fn try_send(
        &self,
        mut message: SentMessage<A>,
    ) -> Result<Result<(), MailboxFull<A>>, Error> {
        if !self.is_connected() {
            return Err(Error::Disconnected);
        }

        message.start_span();

        let mut inner = self.chan.lock().unwrap();

        let result = match message {
            SentMessage::ToAllActors(m) => {
                if inner.is_broadcast_full() {
                    let waiting = WaitingSender::new(SentMessage::ToAllActors(m));
                    inner.waiting_senders.push_back(Arc::downgrade(&waiting));

                    return Ok(Err(MailboxFull(waiting)));
                }

                inner.send_broadcast(m);
                Ok(())
            }
            SentMessage::ToOneActor(msg) => {
                let unfulfilled_msg = if let Err(msg) = inner.try_fulfill_receiver(msg) {
                    msg
                } else {
                    return Ok(Ok(()));
                };

                match unfulfilled_msg {
                    m if m.priority() == Priority::default() && !inner.is_ordered_full() => {
                        inner.ordered_queue.push_back(m);
                    }
                    m if m.priority() != Priority::default() && !inner.is_priority_full() => {
                        inner.priority_queue.push(ByPriority(m));
                    }
                    _ => {
                        let waiting = WaitingSender::new(SentMessage::ToOneActor(unfulfilled_msg));
                        inner.waiting_senders.push_back(Arc::downgrade(&waiting));

                        return Ok(Err(MailboxFull(waiting)));
                    }
                };

                Ok(())
            }
        };

        Ok(result)
    }

    fn try_recv(
        &self,
        broadcast_mailbox: &BroadcastQueue<A>,
    ) -> Result<ActorMessage<A>, WaitingReceiver<A>> {
        // lock braodcast mailbox for as short as possible
        let broadcast_priority = {
            // Peek priorities in order to figure out which channel should be taken from
            broadcast_mailbox.lock().peek().map(|it| it.priority())
        };

        let mut inner = self.chan.lock().unwrap();

        let shared_priority: Option<Priority> = inner.priority_queue.peek().map(|it| it.priority());

        // Try take from ordered channel
        if cmp::max(shared_priority, broadcast_priority) <= Some(Priority::Valued(0)) {
            if let Some(msg) = inner.pop_ordered() {
                return Ok(msg.into());
            }
        }

        // Choose which priority channel to take from
        match shared_priority.cmp(&broadcast_priority) {
            // Shared priority is greater or equal (and it is not empty)
            Ordering::Greater | Ordering::Equal if shared_priority.is_some() => {
                Ok(inner.pop_priority().unwrap().into())
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

    fn is_connected(&self) -> bool {
        self.receiver_count.load(atomic::Ordering::SeqCst) > 0
            && self.sender_count.load(atomic::Ordering::SeqCst) > 0
    }

    fn len(&self) -> usize {
        let inner = self.chan.lock().unwrap();
        inner.broadcast_tail + inner.ordered_queue.len() + inner.priority_queue.len()
    }

    fn capacity(&self) -> Option<usize> {
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

    fn shutdown_all_receivers(&self)
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

    /// Re-queue the given message.
    ///
    /// Normally, messages are delivered from the inbox straight to the actor. It can however happen
    /// that this process gets cancelled. In that case, this function can be used to re-queue the
    /// given message so it does not get lost.
    ///
    /// Note that the ordering of messages in the queues may be slightly off with this function.
    fn requeue_message(&self, msg: Box<dyn MessageEnvelope<Actor = A>>) {
        let mut inner = match self.chan.lock() {
            Ok(lock) => lock,
            Err(_) => return, // If we can't lock the inner channel, there is nothing we can do.
        };

        if let Err(msg) = inner.try_fulfill_receiver(msg) {
            if msg.priority() == Priority::default() {
                // Preserve ordering as much as possible by pushing to the front
                inner.ordered_queue.push_front(msg)
            } else {
                inner.priority_queue.push(ByPriority(msg));
            }
        }
    }
}

struct ChanInner<A> {
    capacity: Option<usize>,
    ordered_queue: VecDeque<Box<dyn MessageEnvelope<Actor = A>>>,
    waiting_senders: VecDeque<Weak<Spinlock<WaitingSender<A>>>>,
    waiting_receivers_handles: VecDeque<FulfillHandle<A>>,
    priority_queue: BinaryHeap<ByPriority<Box<dyn MessageEnvelope<Actor = A>>>>,
    broadcast_queues: Vec<Weak<BroadcastQueue<A>>>,
    broadcast_tail: usize,
}

impl<A> ChanInner<A> {
    fn new(capacity: Option<usize>) -> Self {
        Self {
            capacity,
            ordered_queue: VecDeque::default(),
            waiting_senders: VecDeque::default(),
            waiting_receivers_handles: VecDeque::default(),
            priority_queue: BinaryHeap::default(),
            broadcast_queues: Vec::default(),
            broadcast_tail: 0,
        }
    }

    fn pop_priority(&mut self) -> Option<Box<dyn MessageEnvelope<Actor = A>>> {
        // If len < cap after popping this message, try fulfill at most one waiting sender
        if self
            .capacity
            .map_or(false, |cap| cap == self.priority_queue.len())
        {
            match self.try_fulfill_sender(MessageType::Priority) {
                Some(SentMessage::ToOneActor(msg)) => self.priority_queue.push(ByPriority(msg)),
                Some(_) => unreachable!(),
                None => {}
            }
        }

        Some(self.priority_queue.pop()?.0)
    }

    fn pop_ordered(&mut self) -> Option<Box<dyn MessageEnvelope<Actor = A>>> {
        // If len < cap after popping this message, try fulfill at most one waiting sender
        if self
            .capacity
            .map_or(false, |cap| cap == self.ordered_queue.len())
        {
            match self.try_fulfill_sender(MessageType::Ordered) {
                Some(SentMessage::ToOneActor(msg)) => self.ordered_queue.push_back(msg),
                Some(_) => unreachable!(),
                None => {}
            }
        }

        self.ordered_queue.pop_front()
    }

    fn pop_broadcast(
        &mut self,
        broadcast_mailbox: &BroadcastQueue<A>,
    ) -> Option<Arc<dyn BroadcastEnvelope<Actor = A>>> {
        let message = broadcast_mailbox.lock().pop();

        // Advance the broadcast tail if we successfully took a message.
        if message.is_some() {
            self.broadcast_tail = self.longest_broadcast_queue();

            // If len < cap, try fulfill a waiting sender
            if self.capacity.map_or(false, |cap| self.broadcast_tail < cap) {
                match self.try_fulfill_sender(MessageType::Broadcast) {
                    Some(SentMessage::ToAllActors(m)) => self.send_broadcast(m),
                    Some(_) => unreachable!(),
                    None => {}
                }
            }
        }

        Some(message?.0)
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

    fn send_broadcast(&mut self, m: Arc<dyn BroadcastEnvelope<Actor = A>>) {
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

    fn try_fulfill_receiver(
        &mut self,
        mut msg: Box<dyn MessageEnvelope<Actor = A>>,
    ) -> Result<(), Box<dyn MessageEnvelope<Actor = A>>> {
        while let Some(rx) = self.waiting_receivers_handles.pop_front() {
            match rx.notify_new_message(msg) {
                Ok(()) => return Ok(()),
                Err(unfulfilled_msg) => msg = unfulfilled_msg,
            }
        }

        Err(msg)
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
                return Some(tx.lock().fulfill());
            }
        }
    }

    fn is_broadcast_full(&self) -> bool {
        self.capacity
            .map_or(false, |cap| self.broadcast_tail >= cap)
    }

    fn is_ordered_full(&self) -> bool {
        self.capacity
            .map_or(false, |cap| self.ordered_queue.len() >= cap)
    }

    fn is_priority_full(&self) -> bool {
        self.capacity
            .map_or(false, |cap| self.priority_queue.len() >= cap)
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

impl<A> SentMessage<A> {
    pub fn start_span(&mut self) {
        #[cfg(feature = "instrumentation")]
        match self {
            SentMessage::ToOneActor(m) => {
                m.start_span();
            }
            SentMessage::ToAllActors(m) => {
                Arc::get_mut(m)
                    .expect("calling after try_send not supported")
                    .start_span();
            }
        };
    }
}

impl<A> From<Box<dyn MessageEnvelope<Actor = A>>> for SentMessage<A> {
    fn from(msg: Box<dyn MessageEnvelope<Actor = A>>) -> Self {
        SentMessage::ToOneActor(msg)
    }
}

/// An error returned in case the mailbox of an actor is full.
pub struct MailboxFull<A>(pub Arc<Spinlock<WaitingSender<A>>>);

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

pub enum WaitingSender<A> {
    Active {
        waker: Option<Waker>,
        message: SentMessage<A>,
    },
    Delivered,
    Closed,
}

impl<A> WaitingSender<A> {
    pub fn new(message: SentMessage<A>) -> Arc<Spinlock<Self>> {
        let sender = WaitingSender::Active {
            waker: None,
            message,
        };
        Arc::new(Spinlock::new(sender))
    }

    pub fn peek(&self) -> &SentMessage<A> {
        match self {
            WaitingSender::Active { message, .. } => message,
            _ => panic!("WaitingSender should have message"),
        }
    }

    pub fn fulfill(&mut self) -> SentMessage<A> {
        match mem::replace(self, Self::Delivered) {
            WaitingSender::Active { mut waker, message } => {
                if let Some(waker) = waker.take() {
                    waker.wake();
                }

                message
            }
            WaitingSender::Delivered | WaitingSender::Closed => {
                panic!("WaitingSender is already fulfilled or closed")
            }
        }
    }

    /// Mark this [`WaitingSender`] as closed.
    ///
    /// Should be called when the last [`Receiver`](crate::inbox::Receiver) goes away.
    pub fn set_closed(&mut self) {
        if let WaitingSender::Active {
            waker: Some(waker), ..
        } = mem::replace(self, Self::Closed)
        {
            waker.wake();
        }
    }
}

impl<A> Future for WaitingSender<A> {
    type Output = Result<(), Error>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();

        match this {
            WaitingSender::Active { waker, .. } => {
                *waker = Some(cx.waker().clone());
                Poll::Pending
            }
            WaitingSender::Delivered => Poll::Ready(Ok(())),
            WaitingSender::Closed => Poll::Ready(Err(Error::Disconnected)),
        }
    }
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
