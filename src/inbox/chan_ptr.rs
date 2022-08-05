use std::fmt;
use std::ops::Deref;
use std::sync::atomic::Ordering;
use std::sync::{atomic, Arc};

use crate::inbox::Chan;

/// A reference-counted pointer to the channel that is generic over its reference counting policy.
///
/// Apart from [`TxEither`], all reference-counting policies are zero-sized types and the actual channel
/// is stored in an `Arc`, meaning this pointer type is exactly as wide as an `Arc`, i.e. 8 bytes.
pub struct ChanPtr<A, Rc>
where
    Rc: RefCounter,
{
    inner: Arc<Chan<A>>,
    ref_counter: Rc,
}

impl<A, Rc> fmt::Debug for ChanPtr<A, Rc>
where
    Rc: RefCounter,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use atomic::Ordering::SeqCst;

        let act = std::any::type_name::<A>();
        let rc = std::any::type_name::<Rc>();
        f.debug_struct(&format!("ChanPtr<{}, {}>", act, rc))
            .field("rx_count", &self.inner.receiver_count.load(SeqCst))
            .field("tx_count", &self.inner.sender_count.load(SeqCst))
            .finish()
    }
}

/// The reference count of a strong address. Strong addresses will prevent the actor from being
/// dropped as long as they live. Read the docs of [`Address`](crate::Address) to find
/// out more.
#[derive(Debug)]
pub struct TxStrong(());

impl TxStrong {
    /// Attempt to construct a new `TxStrong` pointing to the given `inner` if there are existing
    /// strong references to `inner`. This will return `None` if there were 0 strong references to
    /// the inner.
    fn try_new<A>(inner: &Chan<A>) -> Option<TxStrong> {
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

/// The reference count of a weak address. Weak addresses will bit prevent the actor from being
/// dropped. Read the docs of [`Address`](crate::Address) to find out more.
#[derive(Debug)]
pub struct TxWeak(());

/// A reference counter that can be dynamically either strong or weak.
#[derive(Debug)]
pub enum TxEither {
    /// A strong reference counter.
    Strong(TxStrong),
    /// A weak reference counter.
    Weak(TxWeak),
}

pub struct Rx(());

impl<A, Rc> ChanPtr<A, Rc>
where
    Rc: RefCounter,
{
    pub fn is_strong(&self) -> bool {
        self.ref_counter.is_strong()
    }

    pub fn to_tx_weak(&self) -> ChanPtr<A, TxWeak> {
        ChanPtr {
            inner: self.inner.clone(),
            ref_counter: TxWeak(()),
        }
    }

    pub fn inner_ptr(&self) -> *const () {
        Arc::as_ptr(&self.inner) as *const ()
    }
}

impl<A> ChanPtr<A, TxStrong> {
    pub fn new(inner: Arc<Chan<A>>) -> Self {
        let policy = TxStrong(()).increment(inner.as_ref());

        Self {
            ref_counter: policy,
            inner,
        }
    }

    pub fn to_tx_either(&self) -> ChanPtr<A, TxEither> {
        ChanPtr {
            inner: self.inner.clone(),
            ref_counter: TxEither::Strong(self.ref_counter.increment(self.inner.as_ref())),
        }
    }
}

impl<A> ChanPtr<A, TxWeak> {
    pub fn to_tx_either(&self) -> ChanPtr<A, TxEither> {
        ChanPtr {
            inner: self.inner.clone(),
            ref_counter: TxEither::Weak(TxWeak(())),
        }
    }
}

impl<A> ChanPtr<A, Rx> {
    pub fn new(inner: Arc<Chan<A>>) -> Self {
        let policy = Rx(()).increment(inner.as_ref());

        Self {
            ref_counter: policy,
            inner,
        }
    }

    pub fn try_to_tx_strong(&self) -> Option<ChanPtr<A, TxStrong>> {
        Some(ChanPtr {
            inner: self.inner.clone(),
            ref_counter: TxStrong::try_new(self.inner.as_ref())?,
        })
    }
}

impl<A, Rc> Clone for ChanPtr<A, Rc>
where
    Rc: RefCounter,
{
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            ref_counter: self.ref_counter.increment(self.inner.as_ref()),
        }
    }
}

impl<A, Rc> Drop for ChanPtr<A, Rc>
where
    Rc: RefCounter,
{
    fn drop(&mut self) {
        if self.ref_counter.decrement(self.inner.as_ref()) {
            self.ref_counter.on_last_drop(self.inner.as_ref());
        }
    }
}

impl<A, Rc> Deref for ChanPtr<A, Rc>
where
    Rc: RefCounter,
{
    type Target = Chan<A>;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

// TODO: Seal this.
/// todo(docs)
pub trait RefCounter: Send + Sync + 'static + Unpin {
    /// todo(docs)
    fn increment<A>(&self, chan: &Chan<A>) -> Self;
    /// todo(docs)
    fn decrement<A>(&self, chan: &Chan<A>) -> bool;
    /// todo(docs)
    fn on_last_drop<A>(&self, chan: &Chan<A>);
    /// todo(docs)
    fn is_strong(&self) -> bool;
}

impl RefCounter for TxStrong {
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

    fn on_last_drop<A>(&self, chan: &Chan<A>) {
        chan.shutdown_waiting_receivers();
    }

    fn is_strong(&self) -> bool {
        true
    }
}

impl RefCounter for TxWeak {
    fn increment<A>(&self, _: &Chan<A>) -> Self {
        TxWeak(())
    }

    fn decrement<A>(&self, _: &Chan<A>) -> bool {
        false
    }

    fn on_last_drop<A>(&self, _: &Chan<A>) {}

    fn is_strong(&self) -> bool {
        false
    }
}

impl RefCounter for TxEither {
    fn increment<A>(&self, chan: &Chan<A>) -> Self {
        match self {
            TxEither::Strong(strong) => TxEither::Strong(strong.increment(chan)),
            TxEither::Weak(weak) => TxEither::Weak(weak.increment(chan)),
        }
    }

    fn decrement<A>(&self, chan: &Chan<A>) -> bool {
        match self {
            TxEither::Strong(strong) => strong.decrement(chan),
            TxEither::Weak(weak) => weak.decrement(chan),
        }
    }

    fn on_last_drop<A>(&self, chan: &Chan<A>) {
        match self {
            TxEither::Strong(strong) => strong.on_last_drop(chan),
            TxEither::Weak(weak) => weak.on_last_drop(chan),
        }
    }

    fn is_strong(&self) -> bool {
        match self {
            TxEither::Strong(_) => true,
            TxEither::Weak(_) => false,
        }
    }
}

impl RefCounter for Rx {
    fn increment<A>(&self, inner: &Chan<A>) -> Self {
        inner.receiver_count.fetch_add(1, atomic::Ordering::Relaxed);
        Rx(())
    }

    fn decrement<A>(&self, inner: &Chan<A>) -> bool {
        // Memory orderings copied from Arc::drop
        if inner.receiver_count.fetch_sub(1, atomic::Ordering::Release) != 1 {
            return false;
        }

        atomic::fence(atomic::Ordering::Acquire);
        true
    }

    fn on_last_drop<A>(&self, chan: &Chan<A>) {
        chan.shutdown_waiting_senders()
    }

    fn is_strong(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use std::mem::size_of;

    use super::*;

    #[test]
    fn size_of_ptr() {
        assert_eq!(size_of::<ChanPtr<Foo, TxStrong>>(), 8);
        assert_eq!(size_of::<ChanPtr<Foo, TxWeak>>(), 8);
        assert_eq!(size_of::<ChanPtr<Foo, Rx>>(), 8);
        assert_eq!(size_of::<ChanPtr<Foo, TxEither>>(), 16);
    }

    #[test]
    fn starts_with_rc_count_one() {
        let inner = Arc::new(Chan::new(None));

        let _ptr1 = ChanPtr::<Foo, TxStrong>::new(inner.clone());

        assert_eq!(inner.sender_count.load(Ordering::SeqCst), 1)
    }

    #[test]
    fn clone_increments_count() {
        let inner = Arc::new(Chan::new(None));

        let ptr1 = ChanPtr::<Foo, TxStrong>::new(inner.clone());
        #[allow(clippy::redundant_clone)]
        let _ptr2 = ptr1.clone();

        assert_eq!(inner.sender_count.load(Ordering::SeqCst), 2)
    }

    #[test]
    fn dropping_last_reference_calls_on_last_drop() {
        let inner = Arc::new(Chan::new(None));

        let ptr1 = ChanPtr::<Foo, TxStrong>::new(inner.clone());
        std::mem::drop(ptr1);

        assert_eq!(inner.sender_count.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn can_convert_tx_strong_into_weak() {
        let inner = Arc::new(Chan::new(None));

        let strong_ptr = ChanPtr::<Foo, TxStrong>::new(inner.clone());
        let _weak_ptr = strong_ptr.to_tx_weak();

        assert_eq!(inner.sender_count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn can_clone_either() {
        let inner = Arc::new(Chan::new(None));

        let strong_ptr = ChanPtr::<Foo, TxStrong>::new(inner.clone());
        let either_ptr_1 = strong_ptr.to_tx_either();
        #[allow(clippy::redundant_clone)]
        let _either_ptr_2 = either_ptr_1.clone();

        assert_eq!(inner.sender_count.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn either_is_strong() {
        let inner = Arc::new(Chan::new(None));

        let strong_ptr = ChanPtr::<Foo, TxStrong>::new(inner);
        let either_ptr = strong_ptr.to_tx_either();

        assert!(either_ptr.is_strong());
    }

    struct Foo;

    #[async_trait::async_trait]
    impl crate::Actor for Foo {
        type Stop = ();

        async fn stopped(self) -> Self::Stop {}
    }
}
