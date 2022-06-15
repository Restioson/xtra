//! Latency is prioritised over most accurate prioritisation. Specifically, at most one low priority
//! message may be handled before piled-up higher priority messages will be handled.
// TODO(bounded)

mod heap;

use crate::envelope::{BroadcastEnvelope, MessageEnvelope};
use std::cmp::Ordering;
use std::collections::VecDeque;
use std::future::Future;
use std::mem;
use std::pin::Pin;
use std::sync::{Arc, Mutex, Weak};
use std::task::{Context, Poll, Waker};
use tokio::time::Instant;
use heap::BinaryHeap;

type Spinlock<T> = spin::Mutex<T>;
type MpmcMessage<A> = Box<dyn MessageEnvelope<Actor = A>>;
type BroadcastMessage<A> = Arc<dyn BroadcastEnvelope<Actor = A>>;
type BroadcastQueue<A> = Spinlock<BinaryHeap<BroadcastMessageWrapper<A>>>;

#[derive(PartialEq, Eq, Ord, PartialOrd, Copy, Clone)]
pub(crate) enum Priority {
    Min,
    Valued(u32),
    Max,
}

impl Default for Priority {
    fn default() -> Self {
        Priority::Min
    }
}

#[derive(Debug)]
pub(crate) struct Disconnected;

pub(crate) fn unbounded<A>() -> (Sender<A>, Receiver<A>) {
    let broadcast_mailbox = Arc::new(Spinlock::new(BinaryHeap::new()));
    let inner = Inner {
        mpmc_queue: BinaryHeap::new(),
        broadcast_queues: vec![Arc::downgrade(&broadcast_mailbox)],
        waiting_actors: VecDeque::new(),
        shutdown: false,
    };

    let inner = Arc::new(Mutex::new(inner));

    let rx = Receiver {
        inner: inner.clone(),
        broadcast_mailbox,
    };

    (Sender(inner), rx)
}

struct Inner<A> {
    mpmc_queue: BinaryHeap<MpmcMessageWithPriority<A>>,
    broadcast_queues: Vec<Weak<BroadcastQueue<A>>>,
    waiting_actors: VecDeque<Arc<Spinlock<WaitingActor<A>>>>,
    shutdown: bool,
}

pub(crate) struct Sender<A>(Arc<Mutex<Inner<A>>>);

impl<A> Clone for Sender<A> {
    fn clone(&self) -> Self {
        Sender(self.0.clone())
    }
}

impl<A> Sender<A> {
    pub(crate) fn send(&self, message: MpmcMessage<A>, priority: u32) -> Result<(), Disconnected> {
        let waiting = {
            let mut inner = self.0.lock().unwrap();
            if inner.shutdown {
                return Err(Disconnected);
            }

            match inner.waiting_actors.pop_front() {
                Some(actor) => actor,
                None => {
                    let msg = MpmcMessageWithPriority::new(Priority::Valued(priority), message);
                    inner.mpmc_queue.push(msg);
                    return Ok(())
                }
            }
        };

        waiting.lock().fulfill(WakeReason::MpmcMessage(message));

        Ok(())
    }

    pub(crate) fn broadcast(&self, message: BroadcastMessage<A>) -> Result<(), Disconnected> {
        let waiting = {
            let mut inner = self.0.lock().unwrap();

            if inner.shutdown {
                return Err(Disconnected);
            }

            inner.broadcast_queues.retain(|queue| {
                match queue.upgrade() {
                    Some(q) => {
                        q.lock().push(BroadcastMessageWrapper(message.clone()));
                        true
                    },
                    None => false,
                }
            });

            mem::take(&mut inner.waiting_actors)
        };

        for actor in waiting {
            actor.lock().fulfill(WakeReason::BroadcastMessage);
        }

        Ok(())
    }

    pub(crate) fn shutdown(&self) {
        let waiting = {
            let mut inner = self.0.lock().unwrap();
            inner.shutdown = true;
            mem::take(&mut inner.waiting_actors)
        };

        for actor in waiting {
            actor.lock().fulfill(WakeReason::Shutdown);
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
        self.inner.lock().unwrap().broadcast_queues.push(Arc::downgrade(&new_mailbox));

        Receiver {
            inner: self.inner.clone(),
            broadcast_mailbox: new_mailbox,
        }
    }

    pub(crate) async fn receive(&self) -> ActorMessage<A> {
        let waiting = {
            let mut broadcast = self.broadcast_mailbox.lock();
            let broadcast_priority = broadcast.peek().map(|it| it.priority()).unwrap_or_default();

            // In case of a shutdown or max priority message, we can skip the critical section on inner
            if broadcast_priority == Priority::Max {
                return ActorMessage::BroadcastMessage(broadcast.pop().unwrap().0);
            }

            let mut inner = self.inner.lock().unwrap();
            // let t = Instant::now(); // TODO recv timing
            if inner.shutdown {
                return ActorMessage::Shutdown;
            }

            let mpsc = &mut inner.mpmc_queue;
            let mpsc_priority: Priority = mpsc.peek().map(|it| it.priority()).unwrap_or_default();

            match mpsc_priority.cmp(&broadcast_priority) {
                Ordering::Greater => {
                    let m = ActorMessage::MpmcMessage(mpsc.pop().unwrap().val);
                    drop(inner);
                    // eprintln!("Recv: {}ns", t.elapsed().as_nanos());
                    return m;
                }
                Ordering::Less => return ActorMessage::BroadcastMessage(broadcast.pop().unwrap().0),
                Ordering::Equal if mpsc_priority != Priority::Min => return ActorMessage::MpmcMessage(mpsc.pop().unwrap().val),
                Ordering::Equal => {
                    let waiting = Arc::new(Spinlock::new(WaitingActor::default()));
                    inner.waiting_actors.push_back(waiting.clone());
                    waiting
                },
            }
        };


        let waiting = WaitForSend(Some(waiting)).await;
        let wake_reason = waiting.lock().next_message.take();

        match wake_reason.unwrap() {
            WakeReason::MpmcMessage(msg) => ActorMessage::MpmcMessage(msg),
            WakeReason::BroadcastMessage => {
                let mut broadcast = self.broadcast_mailbox.lock();
                ActorMessage::BroadcastMessage(broadcast.pop().unwrap().0)
            }
            WakeReason::Shutdown => ActorMessage::Shutdown,
        }
    }
}

pub(crate) enum ActorMessage<A> {
    MpmcMessage(MpmcMessage<A>),
    BroadcastMessage(BroadcastMessage<A>),
    Shutdown,
}

enum WakeReason<A> {
    MpmcMessage(MpmcMessage<A>),
    // should be fetched from own receiver
    BroadcastMessage,
    Shutdown,
}

pub(crate) trait HasPriority {
    fn priority(&self) -> Priority;
}

impl<A> HasPriority for MpmcMessageWithPriority<A> {
    fn priority(&self) -> Priority {
        self.priority
    }
}

struct MpmcMessageWithPriority<A> {
    priority: Priority,
    val: MpmcMessage<A>,
}

impl<A> MpmcMessageWithPriority<A> {
    fn new(priority: Priority, val: MpmcMessage<A>) -> Self {
        MpmcMessageWithPriority { priority, val }
    }
}

impl<A> PartialEq for MpmcMessageWithPriority<A> {
    fn eq(&self, other: &Self) -> bool {
        self.priority == other.priority // TODO(eq)
    }
}

impl<A> Eq for MpmcMessageWithPriority<A> {}

impl<A> PartialOrd for MpmcMessageWithPriority<A> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl<A> Ord for MpmcMessageWithPriority<A> {
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

struct WaitingActor<A> {
    waker: Option<Waker>,
    next_message: Option<WakeReason<A>>,
}

impl<A> Default for WaitingActor<A> {
    fn default() -> Self {
        WaitingActor { waker: None, next_message: None }
    }
}

impl<A> WaitingActor<A> {
    fn fulfill(&mut self, message: WakeReason<A>) {
        self.next_message = Some(message);

        if let Some(waker) = self.waker.take() {
            waker.wake();
        }
    }
}

struct WaitForSend<A>(Option<Arc<Spinlock<WaitingActor<A>>>>);

impl<A> Future for WaitForSend<A> {
    type Output = Arc<Spinlock<WaitingActor<A>>>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut inner = self.0.as_ref().unwrap().lock();

        match inner.next_message.as_ref() {
            Some(_) => {
                drop(inner);
                Poll::Ready(self.0.take().unwrap())
            },
            None => {
                inner.waker = Some(cx.waker().clone());
                Poll::Pending
            }
        }
    }
}

#[cfg(test)]
mod test {
    use futures_util::FutureExt;
    use crate::prelude::{*, Context};
    use crate::envelope::{BroadcastEnvelopeConcrete, NonReturningEnvelope};
    use super::*;

    struct MyActor;

    #[async_trait]
    impl Actor for MyActor {
        type Stop = ();

        async fn stopped(self) {}
    }

    #[async_trait]
    impl Handler<&'static str> for MyActor {
        type Return = ();

        async fn handle(&mut self, message: &'static str, ctx: &mut Context<Self>) {
            println!("{}", message);
        }
    }

    #[test]
    fn test_priority() {
        assert!(Priority::Min < Priority::Valued(0));
        assert!(Priority::Min < Priority::Max);
        assert!(Priority::Max > Priority::Valued(u32::MAX));
        assert!(Priority::Valued(1) > Priority::Valued(0));
    }

    #[tokio::test]
    async fn test_broadcast() {
        let (tx, rx) = unbounded();
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

    #[test]
    fn test_() {
        let (tx, rx) = unbounded::<MyActor>();
        for _ in 0..1_000 {
            tx.send(Box::new(NonReturningEnvelope::new("Hi")), 1).unwrap();
        }

        pollster::block_on(rx.receive());
    }
}
