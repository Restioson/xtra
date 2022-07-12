use std::fmt::{Debug, Formatter};
use std::future::Future;
use std::mem;
use std::pin::Pin;
use std::sync::{atomic, Arc};
use std::task::{Context, Poll, Waker};

use event_listener::EventListener;
use futures_core::FusedFuture;
use futures_util::FutureExt;

use super::*;
use crate::envelope::Shutdown;
use crate::inbox::tx::private::RefCounterInner;
use crate::send_future::private::SetPriority;
use crate::{Actor, Error};

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

    pub fn try_new_strong(inner: Arc<Chan<A>>) -> Option<Self> {
        let rc = TxStrong::try_new(&inner)?;

        Some(Self { inner, rc })
    }
}

impl<A> Sender<A, TxWeak> {
    pub fn new_weak(inner: Arc<Chan<A>>) -> Self {
        let rc = TxWeak::new(&inner);

        Sender { inner, rc }
    }
}

impl<Rc: TxRefCounter, A> Sender<A, Rc> {
    fn try_send(&self, message: SentMessage<A>) -> Result<(), TrySendFail<A>> {
        self.inner.try_send(message)
    }

    pub fn stop_all_receivers(&self)
    where
        A: Actor,
    {
        self.inner
            .chan
            .lock()
            .unwrap()
            .send_broadcast(MessageToAllActors(Arc::new(Shutdown::new())));
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

    pub fn is_connected(&self) -> bool {
        self.inner.is_connected()
    }

    pub fn capacity(&self) -> Option<usize> {
        self.inner.capacity
    }

    pub fn len(&self) -> usize {
        self.inner.len()
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
            let waiting_rx = {
                let mut inner = match self.inner.chan.lock() {
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
    }
}

impl<A, Rc: TxRefCounter> Debug for Sender<A, Rc> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        use atomic::Ordering::SeqCst;

        let act = std::any::type_name::<A>();
        f.debug_struct(&format!("Sender<{}>", act))
            .field("rx_count", &self.inner.receiver_count.load(SeqCst))
            .field("tx_count", &self.inner.sender_count.load(SeqCst))
            .field("rc", &self.rc)
            .finish()
    }
}

impl<A, Rc: TxRefCounter> SetPriority for SendFuture<A, Rc> {
    fn set_priority(&mut self, priority: u32) {
        match self {
            SendFuture::New {
                msg: SentMessage::ToOneActor(ref mut m),
                ..
            } => m.priority = priority,
            _ => panic!("setting priority after polling is unsupported"),
        }
    }
}

#[must_use = "Futures do nothing unless polled"]
pub enum SendFuture<A, Rc: TxRefCounter> {
    New {
        msg: SentMessage<A>,
        tx: Sender<A, Rc>,
    },
    WaitingToSend(Arc<Spinlock<WaitingSender<A>>>),
    Complete,
}

impl<A, Rc: TxRefCounter> Future for SendFuture<A, Rc> {
    type Output = Result<(), Error>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        let this = self.get_mut();

        loop {
            match mem::replace(this, SendFuture::Complete) {
                SendFuture::New { msg, tx } => match tx.try_send(msg) {
                    Ok(()) => return Poll::Ready(Ok(())),
                    Err(TrySendFail::Disconnected) => return Poll::Ready(Err(Error::Disconnected)),
                    Err(TrySendFail::Full(waiting)) => {
                        // Start waiting. The waiting sender should be immediately polled, in case a
                        // receive operation happened between `try_send` and here, in which case the
                        // WaitingSender would be fulfilled, but not properly woken.
                        *this = SendFuture::WaitingToSend(waiting);
                    }
                },
                SendFuture::WaitingToSend(waiting) => {
                    let poll = { waiting.lock().poll_unpin(cx) }; // Scoped separately to drop mutex guard asap.

                    return match poll {
                        Poll::Ready(result) => Poll::Ready(result),
                        Poll::Pending => {
                            *this = SendFuture::WaitingToSend(waiting);
                            Poll::Pending
                        }
                    };
                }
                SendFuture::Complete => return Poll::Pending,
            }
        }
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

impl<A, Rc: TxRefCounter> FusedFuture for SendFuture<A, Rc> {
    fn is_terminated(&self) -> bool {
        matches!(self, SendFuture::Complete)
    }
}

/// This trait represents the strength of an address's reference counting. It is an internal trait.
/// There are two implementations of this trait: [`Weak`](TxWeak) and [`Strong`](TxStrong). These
/// can be provided as the second type argument to [`Address`](crate::Address) in order to change how the address
/// affects the actor's dropping. Read the docs of [`Address`](crate::Address) to find out more.
pub trait TxRefCounter: RefCounterInner + Unpin + Debug + Send + Sync + 'static {}

impl TxRefCounter for TxStrong {}
impl TxRefCounter for TxWeak {}
impl TxRefCounter for TxEither {}

/// The reference count of a strong address. Strong addresses will prevent the actor from being
/// dropped as long as they live. Read the docs of [`Address`](crate::Address) to find
/// out more.
#[derive(Debug)]
pub struct TxStrong(());

/// The reference count of a weak address. Weak addresses will bit prevent the actor from being
/// dropped. Read the docs of [`Address`](crate::Address) to find out more.
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

impl TxWeak {
    pub(crate) fn new<A>(_inner: &Chan<A>) -> TxWeak {
        TxWeak(())
    }
}

/// A reference counter that can be dynamically either strong or weak.
#[derive(Debug)]
pub enum TxEither {
    /// A strong reference counter.
    Strong(TxStrong),
    /// A weak reference counter.
    Weak(TxWeak),
}

mod private {
    use std::sync::atomic;
    use std::sync::atomic::Ordering;

    use super::{TxEither, TxStrong, TxWeak};
    use crate::inbox::Chan;

    pub trait RefCounterInner {
        /// Increments the reference counter, returning a new reference counter for the same
        /// allocation
        fn increment<A>(&self, inner: &Chan<A>) -> Self;
        /// Decrements the reference counter, returning whether the inner data should be dropped
        #[must_use = "If decrement returns false, the address must be disconnected"]
        fn decrement<A>(&self, inner: &Chan<A>) -> bool;
        /// Converts this reference counter into a dynamic reference counter.
        fn into_either(self) -> TxEither;
        /// Returns if this reference counter is a strong reference counter
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
