use std::fmt;

use super::*;
use crate::inbox::tx::private::RefCounterInner;
use crate::Actor;

pub struct Sender<A, Rc: TxRefCounter> {
    pub inner: Arc<Chan<A>>,
    rc: Rc,
}

impl<A> Sender<A, TxStrong> {
    pub fn new(inner: Arc<Chan<A>>) -> Self {
        let rc = TxStrong(());
        rc.make_new(&inner);

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
    pub fn stop_all_receivers(&self)
    where
        A: Actor,
    {
        self.inner.shutdown_all_receivers()
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
            rc: self.rc.make_new(&self.inner).into_either(),
        }
    }

    pub fn is_connected(&self) -> bool {
        self.inner.is_connected()
    }

    pub fn capacity(&self) -> Option<usize> {
        self.inner.capacity()
    }

    pub fn len(&self) -> usize {
        self.inner.len()
    }

    pub fn disconnect_notice(&self) -> Option<EventListener> {
        self.inner.disconnect_listener()
    }
}

impl<A, Rc: TxRefCounter> Clone for Sender<A, Rc> {
    fn clone(&self) -> Self {
        Sender {
            inner: self.inner.clone(),
            rc: self.rc.make_new(&self.inner),
        }
    }
}

impl<A, Rc: TxRefCounter> Drop for Sender<A, Rc> {
    fn drop(&mut self) {
        self.rc.on_drop(&self.inner);
    }
}

impl<A, Rc: TxRefCounter> fmt::Debug for Sender<A, Rc> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use atomic::Ordering::SeqCst;

        let act = std::any::type_name::<A>();
        f.debug_struct(&format!("Sender<{}>", act))
            .field("rx_count", &self.inner.receiver_count.load(SeqCst))
            .field("tx_count", &self.inner.sender_count.load(SeqCst))
            .field("rc", &self.rc)
            .finish()
    }
}

/// This trait represents the strength of an address's reference counting. It is an internal trait.
/// There are two implementations of this trait: [`Weak`](TxWeak) and [`Strong`](TxStrong). These
/// can be provided as the second type argument to [`Address`](crate::Address) in order to change how the address
/// affects the actor's dropping. Read the docs of [`Address`](crate::Address) to find out more.
pub trait TxRefCounter: RefCounterInner + Unpin + fmt::Debug + Send + Sync + 'static {}

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
    use super::{TxEither, TxStrong, TxWeak};
    use crate::inbox::Chan;

    pub trait RefCounterInner {
        /// Creates a new instance of the reference counter.
        fn make_new<A>(&self, inner: &Chan<A>) -> Self;
        /// Callback to be invoked when the reference counter is dropped.
        fn on_drop<A>(&self, inner: &Chan<A>);
        /// Converts this reference counter into a dynamic reference counter.
        fn into_either(self) -> TxEither;
        /// Returns if this reference counter is a strong reference counter
        fn is_strong(&self) -> bool;
    }

    impl RefCounterInner for TxStrong {
        fn make_new<A>(&self, inner: &Chan<A>) -> Self {
            inner.on_sender_created();

            TxStrong(())
        }

        fn on_drop<A>(&self, inner: &Chan<A>) {
            inner.on_sender_dropped();
        }

        fn into_either(self) -> TxEither {
            TxEither::Strong(self)
        }

        fn is_strong(&self) -> bool {
            true
        }
    }

    impl RefCounterInner for TxWeak {
        fn make_new<A>(&self, _inner: &Chan<A>) -> Self {
            // A weak being cloned does not affect the strong count
            TxWeak(())
        }

        fn on_drop<A>(&self, _inner: &Chan<A>) {
            // A weak being dropped can never result in the inner data being dropped, as this
            // depends on strongs alone
        }

        fn into_either(self) -> TxEither {
            TxEither::Weak(self)
        }
        fn is_strong(&self) -> bool {
            false
        }
    }

    impl RefCounterInner for TxEither {
        fn make_new<A>(&self, inner: &Chan<A>) -> Self {
            match self {
                TxEither::Strong(strong) => TxEither::Strong(strong.make_new(inner)),
                TxEither::Weak(weak) => TxEither::Weak(weak.new(inner)),
            }
        }

        fn on_drop<A>(&self, inner: &Chan<A>) {
            match self {
                TxEither::Strong(strong) => strong.on_drop(inner),
                TxEither::Weak(weak) => weak.on_drop(inner),
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
