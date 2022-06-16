//! Latency is prioritised over most accurate prioritisation. Specifically, at most one low priority
//! message may be handled before piled-up higher priority messages will be handled.
// TODO(bounded)

use crate::envelope::{BroadcastEnvelope, MessageEnvelope};
use crate::Disconnected;
use std::cmp::Ordering;
use std::collections::{BinaryHeap, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, Weak};
use std::task::{Context, Poll, Waker};
use std::{cmp, mem};
use futures_core::FusedFuture;
use futures_sink::Sink;
use futures_util::FutureExt;

type Spinlock<T> = spin::Mutex<T>;
type StolenMessage<A> = Box<dyn MessageEnvelope<Actor = A>>;
type BroadcastMessage<A> = Arc<dyn BroadcastEnvelope<Actor = A>>;
type BroadcastQueue<A> = Spinlock<BinaryHeap<BroadcastMessageWrapper<A>>>;

// TODO(priority)
#[derive(PartialEq, Eq, Ord, PartialOrd, Copy, Clone)]
pub(crate) enum Priority {
    Min,
    Valued(i32),
    Max,
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
        // If this message will result in the cap no long being reached, pop one waiting
        // sender, if there is one, and fulfill it, adding its message to the queue
        if self.is_full() {
            self.pop_sender()
                .and_then(|waiting| waiting.lock().fulfill())
                .map(|message| self.default_queue.push_back(message));
        }

        self.default_queue.pop_front()
    }

    fn pop_receiver(&mut self) -> Option<Arc<Spinlock<WaitingReceiver<A>>>> {
        while !self.waiting_receivers.is_empty() {
            if let Some(rx) = self.waiting_receivers.pop_front().and_then(|w| w.upgrade()) {
                return Some(rx);
            }
        }

        None
    }

    fn pop_sender(&mut self) -> Option<Arc<Spinlock<WaitingSender<A>>>> {
        while !self.waiting_senders.is_empty() {
            if let Some(tx) = self.waiting_senders.pop_front().and_then(|w| w.upgrade()) {
                return Some(tx);
            }
        }

        None
    }

    fn try_send(&mut self, message: StolenMessage<A>) -> Result<(), TrySendFail<A>> {
        if self.shutdown {
            return Err(TrySendFail::Disconnected);
        }

        match self.pop_receiver() {
            Some(waiting) => {
                // Contention is not anticipated here
                waiting.lock().fulfill(WakeReason::StolenMessage(message));
                return Ok(());
            }
            None if !self.is_full() => {
                self.default_queue.push_back(message);
                return Ok(());
            }
            _ => {
                // No space, must wait
                let waiting = WaitingSender::new(message);
                self.waiting_senders.push_back(Arc::downgrade(&waiting));
                Err(TrySendFail::Full(waiting))
            }
        }
    }
}

enum TrySendFail<A> {
    Full(Arc<Spinlock<WaitingSender<A>>>),
    Disconnected,
}

pub(crate) struct Sender<A>(Arc<Mutex<Inner<A>>>);

impl<A> Clone for Sender<A> {
    fn clone(&self) -> Self {
        Sender(self.0.clone())
    }
}

impl<A> Sender<A> {
    pub(crate) fn send(&self, message: StolenMessage<A>) -> SendFuture<A> {
        SendFuture::new(message, self.0.clone())
    }

    pub(crate) fn send_priority(
        &self,
        message: StolenMessage<A>,
        priority: i32,
    ) -> Result<(), Disconnected> {
        let waiting = {
            let mut inner = self.0.lock().unwrap();
            if inner.shutdown {
                return Err(Disconnected);
            }

            match inner.pop_receiver() {
                Some(actor) => actor,
                None => {
                    let msg = StolenMessageWithPriority::new(Priority::Valued(priority), message);
                    inner.priority_queue.push(msg);
                    return Ok(());
                }
            }
        };

        waiting.lock().fulfill(WakeReason::StolenMessage(message));

        Ok(())
    }

    pub(crate) fn broadcast(&self, message: BroadcastMessage<A>) -> Result<(), Disconnected> {
        let waiting_receivers = {
            let mut inner = self.0.lock().unwrap();

            if inner.shutdown {
                return Err(Disconnected);
            }

            inner
                .broadcast_queues
                .retain(|queue| match queue.upgrade() {
                    Some(q) => {
                        q.lock().push(BroadcastMessageWrapper(message.clone()));
                        true
                    }
                    None => false, // The corresponding receiver has been dropped - remove it
                });

            mem::take(&mut inner.waiting_receivers)
        };

        for rx in waiting_receivers.into_iter().flat_map(|w| w.upgrade()) {
            rx.lock().fulfill(WakeReason::BroadcastMessage);
        }

        Ok(())
    }

    pub(crate) fn into_sink(self) -> SendSink<A> {
        SendSink {
            inner: self.0,
            future: SendFuture(SendFutureInner::Complete),
        }
    }

    pub(crate) fn shutdown(&self) {
        let waiting_receivers = {
            let mut inner = self.0.lock().unwrap();
            inner.shutdown = true;
            mem::take(&mut inner.waiting_receivers)
        };

        for rx in waiting_receivers.into_iter().flat_map(|w| w.upgrade()) {
            rx.lock().fulfill(WakeReason::BroadcastMessage);
        }
    }

    pub(crate) fn is_connected(&self) -> bool {
        !self.0.lock().unwrap().shutdown
    }
}

pub(crate) struct Receiver<A> {
    inner: Arc<Mutex<Inner<A>>>,
    broadcast_mailbox: Arc<BroadcastQueue<A>>,
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

impl<A> From<BroadcastMessage<A>> for ActorMessage<A> {
    fn from(msg: BroadcastMessage<A>) -> Self {
        ActorMessage::BroadcastMessage(msg)
    }
}

enum WakeReason<A> {
    StolenMessage(StolenMessage<A>),
    // should be fetched from own receiver
    BroadcastMessage,
    Shutdown,
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

struct WaitingReceiver<A> {
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
    fn fulfill(&mut self, message: WakeReason<A>) {
        self.wake_reason = Some(message);

        if let Some(waker) = self.waker.take() {
            waker.wake();
        }
    }
}

struct WaitForSender<A>(Arc<Spinlock<WaitingReceiver<A>>>);

impl<A> Future for WaitForSender<A> {
    type Output = WakeReason<A>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut inner = self.0.lock();
        match inner.wake_reason.take() {
            // Message has been delivered - waiting is done
            Some(reason) => Poll::Ready(reason),
            None => {
                // Message has not yet been delivered
                inner.waker = Some(cx.waker().clone());
                Poll::Pending
            }
        }
    }
}

struct WaitingSender<A> {
    waker: Option<Waker>,
    message: Option<StolenMessage<A>>,
}

impl<A> WaitingSender<A> {
    fn new(message: StolenMessage<A>) -> Arc<Spinlock<Self>> {
        let sender = WaitingSender {
            waker: None,
            message: Some(message),
        };
        Arc::new(Spinlock::new(sender))
    }

    fn fulfill(&mut self) -> Option<StolenMessage<A>> {
        if let Some(waker) = self.waker.take() {
            waker.wake();
        }

        self.message.take()
    }
}

pub struct SendFuture<A>(SendFutureInner<A>);

impl<A> SendFuture<A> {
    fn new(msg: StolenMessage<A>, inner: Arc<Mutex<Inner<A>>>) -> Self {
        SendFuture(SendFutureInner::New { msg, inner })
    }
}

enum SendFutureInner<A> {
    New {
        msg: StolenMessage<A>,
        inner: Arc<Mutex<Inner<A>>>,
    },
    WaitingToSend(Arc<Spinlock<WaitingSender<A>>>),
    Complete,
}

impl<A> Default for SendFutureInner<A> {
    fn default() -> Self {
        SendFutureInner::Complete
    }
}

impl<A> Future for SendFuture<A> {
    type Output = Result<(), Disconnected>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.0 = match mem::take(&mut self.0) {
            SendFutureInner::New { msg, inner } => {
                match inner.lock().unwrap().try_send(msg) {
                Ok(()) => return Poll::Ready(Ok(())),
                Err(TrySendFail::Disconnected) => return Poll::Ready(Err(Disconnected)),
                Err(TrySendFail::Full(waiting)) => SendFutureInner::WaitingToSend(waiting),
            }},
            SendFutureInner::WaitingToSend(waiting) => {
                {
                    let mut inner = waiting.lock();

                    match inner.message {
                        Some(_) => inner.waker = Some(cx.waker().clone()), // The message has not yet been taken
                        None => return Poll::Ready(Ok(())),
                    }
                }

                SendFutureInner::WaitingToSend(waiting)
            }
            SendFutureInner::Complete => SendFutureInner::Complete,
        };

        Poll::Pending
    }
}

impl<A> FusedFuture for SendFuture<A> {
    fn is_terminated(&self) -> bool {
        matches!(self.0, SendFutureInner::Complete)
    }
}

pub(crate) struct SendSink<A> {
    inner: Arc<Mutex<Inner<A>>>,
    future: SendFuture<A>,
}

impl<A> Sink<StolenMessage<A>> for SendSink<A> {
    type Error = Disconnected;

    fn poll_ready(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        if let Poll::Ready(Err(Disconnected)) = self.future.poll_unpin(cx) {
            Poll::Ready(Err(Disconnected))
        } else if self.future.is_terminated() {
            Poll::Ready(Ok(()))  // TODO check disconnected?
        } else {
            Poll::Pending
        }
    }

    fn start_send(mut self: Pin<&mut Self>, msg: StolenMessage<A>) -> Result<(), Self::Error> {
        self.future = SendFuture::new(msg, self.inner.clone());
        Ok(())
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.poll_ready(cx)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.poll_flush(cx)
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
        assert!(Priority::Min < Priority::Max);
        assert!(Priority::Max > Priority::Valued(i32::MAX));
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
