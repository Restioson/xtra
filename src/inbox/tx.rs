use super::*;
use crate::inbox::tx::private::RefCounterInner;
use crate::Disconnected;
use event_listener::EventListener;
use futures_core::FusedFuture;
use futures_util::FutureExt;
use std::fmt::{Debug, Formatter};
use std::future::Future;
use std::mem;
use std::pin::Pin;
use std::sync::{atomic, Arc};
use std::task::{Context, Poll, Waker};

pub struct Sender<A, Rc: TxRefCounter> {
    pub(super) inner: Arc<Chan<A>>,
    pub(super) rc: Rc,
}

impl<A> Sender<A, TxStrong> {
    pub fn new(inner: Arc<Chan<A>>) -> Self {
        let rc = TxStrong(());
        rc.increment(&inner);

        Sender { inner, rc }
    }
}

impl<Rc: TxRefCounter, A> Sender<A, Rc> {
    fn try_send(&self, message: SentMessage<A>) -> Result<(), TrySendFail<A>> {
        let mut inner = self.inner.chan.lock().unwrap();

        if self.inner.is_shutdown() {
            return Err(TrySendFail::Disconnected);
        }

        match message {
            SentMessage::ToAllActors(m) if !self.inner.is_full(inner.broadcast_tail) => {
                inner.send_broadcast(MessageToAllActors(m));
                Ok(())
            }
            SentMessage::ToAllActors(m) => {
                // Is full
                let waiting = WaitingSender::new(SentMessage::ToAllActors(m));
                inner.waiting_senders.push_back(Arc::downgrade(&waiting));
                Err(TrySendFail::Full(waiting))
            }
            msg => match inner.try_fulfill_receiver(msg.into()) {
                Ok(()) => Ok(()),
                Err(WakeReason::MessageToOneActor(m))
                    if m.priority == Priority::default()
                        && !self.inner.is_full(inner.ordered_queue.len()) =>
                {
                    inner.ordered_queue.push_back(m.val);
                    Ok(())
                }
                Err(WakeReason::MessageToOneActor(m))
                    if m.priority != Priority::default()
                        && !self.inner.is_full(inner.priority_queue.len()) =>
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
            },
        }
    }

    pub fn send(&self, message: SentMessage<A>) -> SendFuture<A, Rc> {
        SendFuture::new(message, self.clone())
    }

    pub fn downgrade(&self) -> Sender<A, TxWeak> {
        Sender {
            inner: self.inner.clone(),
            rc: TxWeak(()),
        }
    }

    pub fn is_strong(&self) -> bool {
        self.rc.is_strong()
    }

    pub fn inner_ptr(&self) -> *const Chan<A> {
        (&self.inner as &Chan<A>) as *const Chan<A>
    }

    pub fn into_either_rc(self) -> Sender<A, TxEither> {
        Sender {
            inner: self.inner.clone(),
            rc: self.rc.increment(&self.inner).into_either(),
        }
    }

    pub fn shutdown_and_drain(&self) {
        self.inner.shutdown_and_drain();
    }

    pub fn is_connected(&self) -> bool {
        !self.inner.is_shutdown()
    }

    pub fn capacity(&self) -> Option<usize> {
        self.inner.capacity
    }

    pub fn len(&self) -> usize {
        let inner = self.inner.chan.lock().unwrap();
        inner.broadcast_tail + inner.ordered_queue.len() + inner.priority_queue.len()
    }

    pub fn disconnect_notice(&self) -> Option<EventListener> {
        // Listener is created before checking connectivity to avoid the following race scenario:
        //
        // 1. is_connected returns true
        // 2. on_shutdown is notified
        // 3. listener is registered
        //
        // The listener would never be woken in this scenario, as the notification preceded its
        // creation.
        let listener = self.inner.on_shutdown.listen();

        if self.is_connected() {
            Some(listener)
        } else {
            None
        }
    }
}

impl<A, Rc: TxRefCounter> Clone for Sender<A, Rc> {
    fn clone(&self) -> Self {
        Sender {
            inner: self.inner.clone(),
            rc: self.rc.increment(&self.inner),
        }
    }
}

impl<A, Rc: TxRefCounter> Drop for Sender<A, Rc> {
    fn drop(&mut self) {
        if self.rc.decrement(&self.inner) {
            self.inner.shutdown();
        }
    }
}

impl<A, Rc: TxRefCounter> Debug for Sender<A, Rc> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        use atomic::Ordering::SeqCst;

        let act = std::any::type_name::<A>();
        f.debug_struct(&format!("Sender<{}>", act))
            .field("shutdown", &self.inner.shutdown.load(SeqCst))
            .field("rx_count", &self.inner.receiver_count.load(SeqCst))
            .field("tx_count", &self.inner.sender_count.load(SeqCst))
            .field("rc", &self.rc)
            .finish()
    }
}

#[must_use = "Futures do nothing unless polled"]
pub struct SendFuture<A, Rc: TxRefCounter> {
    pub tx: Sender<A, Rc>,
    inner: SendFutureInner<A>,
}

impl<A, Rc: TxRefCounter> SendFuture<A, Rc> {
    fn new(msg: SentMessage<A>, tx: Sender<A, Rc>) -> Self {
        SendFuture {
            tx,
            inner: SendFutureInner::New(msg),
        }
    }

    pub fn empty(tx: Sender<A, Rc>) -> Self {
        SendFuture {
            tx,
            inner: SendFutureInner::Complete,
        }
    }
}

enum SendFutureInner<A> {
    New(SentMessage<A>),
    WaitingToSend(Arc<Spinlock<WaitingSender<A>>>),
    Complete,
}

impl<A, Rc: TxRefCounter> Future for SendFuture<A, Rc> {
    type Output = Result<(), Disconnected>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Disconnected>> {
        match mem::replace(&mut self.inner, SendFutureInner::Complete) {
            SendFutureInner::New(msg) => match self.tx.try_send(msg) {
                Ok(()) => Poll::Ready(Ok(())),
                Err(TrySendFail::Disconnected) => Poll::Ready(Err(Disconnected)),
                Err(TrySendFail::Full(waiting)) => {
                    // Start waiting. The waiting sender should be immediately polled, in case a
                    // receive operation happened between `try_send` and here, in which case the
                    // WaitingSender would be fulfilled, but not properly woken.
                    self.inner = SendFutureInner::WaitingToSend(waiting);
                    self.poll_unpin(cx)
                }
            },
            SendFutureInner::WaitingToSend(waiting) => {
                {
                    let mut inner = waiting.lock();

                    match inner.message {
                        Some(_) => inner.waker = Some(cx.waker().clone()), // The message has not yet been taken
                        None => return Poll::Ready(Ok(())),
                    }
                }

                self.inner = SendFutureInner::WaitingToSend(waiting);
                Poll::Pending
            }
            SendFutureInner::Complete => Poll::Pending,
        }
    }
}

pub struct WaitingSender<A> {
    waker: Option<Waker>,
    message: Option<SentMessage<A>>,
}

impl<A> WaitingSender<A> {
    pub fn new(message: SentMessage<A>) -> Arc<Spinlock<Self>> {
        let sender = WaitingSender {
            waker: None,
            message: Some(message),
        };
        Arc::new(Spinlock::new(sender))
    }

    pub fn peek(&self) -> &SentMessage<A> {
        self.message
            .as_ref()
            .expect("WaitingSender should have message")
    }

    pub fn fulfill(&mut self) -> SentMessage<A> {
        if let Some(waker) = self.waker.take() {
            waker.wake();
        }

        self.message
            .take()
            .expect("WaitingSender should have message")
    }
}

impl<A, Rc: TxRefCounter> FusedFuture for SendFuture<A, Rc> {
    fn is_terminated(&self) -> bool {
        matches!(self.inner, SendFutureInner::Complete)
    }
}

/// TODO(doc)
pub trait TxRefCounter: RefCounterInner + Unpin + Debug + Send + Sync + 'static {}

impl TxRefCounter for TxStrong {}
impl TxRefCounter for TxWeak {}
impl TxRefCounter for TxEither {}

/// TODO(doc)
#[derive(Debug)]
pub struct TxStrong(());

/// TODO(doc)
#[derive(Debug)]
pub struct TxWeak(());

impl TxStrong {
    /// Attempt to construct a new `TxStrong` pointing to the given `inner` if there are existing
    /// strong references to `inner`. This will return `None` if there were 0 strong references to
    /// the inner.
    pub(crate) fn try_new<A>(inner: &Chan<A>) -> Option<TxStrong> {
        // All code taken from Weak::upgrade in std
        use std::sync::atomic::Ordering::*;

        // Relaxed load because any write of 0 that we can observe leaves the field in a permanently
        // zero state (so a "stale" read of 0 is fine), and any other value is confirmed via the
        // CAS below.
        let mut n = inner.sender_count.load(Relaxed);

        loop {
            if n == 0 {
                return None;
            }

            // Relaxed is fine for the failure case because we don't have any expectations about the new state.
            // Acquire is necessary for the success case to synchronise with `Arc::new_cyclic`, when the inner
            // value can be initialized after `Weak` references have already been created. In that case, we
            // expect to observe the fully initialized value.
            match inner
                .sender_count
                .compare_exchange_weak(n, n + 1, Acquire, Relaxed)
            {
                Ok(_) => return Some(TxStrong(())), // 0-case checked above
                Err(old) => n = old,
            }
        }
    }
}

/// TODO(doc)
#[derive(Debug)]
pub enum TxEither {
    /// TODO(doc)
    Strong(TxStrong),
    /// TODO(doc)
    Weak(TxWeak),
}

mod private {
    use super::{TxEither, TxStrong, TxWeak};
    use crate::inbox::Chan;
    use std::sync::atomic;
    use std::sync::atomic::Ordering;

    pub trait RefCounterInner {
        /// Increments the reference counter, returning a new reference counter for the same
        /// allocation
        fn increment<A>(&self, inner: &Chan<A>) -> Self;
        /// Decrements the reference counter, returning whether the inner data should be dropped
        #[must_use = "If decrement returns false, the address must be disconnected"]
        fn decrement<A>(&self, inner: &Chan<A>) -> bool;
        /// TODO(doc)
        fn into_either(self) -> TxEither;
        /// TODO(doc)
        fn is_strong(&self) -> bool;
    }

    impl RefCounterInner for TxStrong {
        fn increment<A>(&self, inner: &Chan<A>) -> Self {
            // Memory orderings copied from Arc::clone
            inner.sender_count.fetch_add(1, Ordering::Relaxed);
            TxStrong(())
        }

        fn decrement<A>(&self, inner: &Chan<A>) -> bool {
            // Memory orderings copied from Arc::drop
            if inner.sender_count.fetch_sub(1, Ordering::Release) != 1 {
                return false;
            }

            atomic::fence(Ordering::Acquire);
            true
        }

        fn into_either(self) -> TxEither {
            TxEither::Strong(self)
        }

        fn is_strong(&self) -> bool {
            true
        }
    }

    impl RefCounterInner for TxWeak {
        fn increment<A>(&self, _inner: &Chan<A>) -> Self {
            // A weak being cloned does not affect the strong count
            TxWeak(())
        }

        fn decrement<A>(&self, _inner: &Chan<A>) -> bool {
            // A weak being dropped can never result in the inner data being dropped, as this
            // depends on strongs alone
            false
        }

        fn into_either(self) -> TxEither {
            TxEither::Weak(self)
        }
        fn is_strong(&self) -> bool {
            false
        }
    }

    impl RefCounterInner for TxEither {
        fn increment<A>(&self, inner: &Chan<A>) -> Self {
            match self {
                TxEither::Strong(strong) => TxEither::Strong(strong.increment(inner)),
                TxEither::Weak(weak) => TxEither::Weak(weak.increment(inner)),
            }
        }

        fn decrement<A>(&self, inner: &Chan<A>) -> bool {
            match self {
                TxEither::Strong(strong) => strong.decrement(inner),
                TxEither::Weak(weak) => weak.decrement(inner),
            }
        }

        fn into_either(self) -> TxEither {
            self
        }

        fn is_strong(&self) -> bool {
            match self {
                TxEither::Strong(_) => true,
                TxEither::Weak(_) => false,
            }
        }
    }
}
