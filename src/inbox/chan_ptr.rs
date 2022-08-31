use std::fmt;
use std::ops::Deref;
use std::sync::{atomic, Arc};

use private::RefCounter as _;

use crate::inbox::Chan;

/// A reference-counted pointer to the channel that is generic over its reference counting policy.
///
/// Apart from [`TxEither`], all reference-counting policies are zero-sized types and the actual channel
/// is stored in an `Arc`, meaning this pointer type is exactly as wide as an `Arc`, i.e. 8 bytes.
///
/// This is possible because the actual reference counting is done within [`Chan`].
pub struct ChanPtr<A, Rc>
where
    Rc: RefCounter,
{
    inner: Arc<Chan<A>>,
    ref_counter: Rc,
}

impl<A> ChanPtr<A, TxStrong> {
    pub fn new(inner: Arc<Chan<A>>) -> Self {
        let policy = TxStrong(()).make_new(inner.as_ref());

        Self {
            ref_counter: policy,
            inner,
        }
    }
}

impl<A> ChanPtr<A, Rx> {
    pub fn new(inner: Arc<Chan<A>>) -> Self {
        let ref_counter = Rx(()).make_new(inner.as_ref());

        Self { ref_counter, inner }
    }

    pub fn try_to_tx_strong(&self) -> Option<ChanPtr<A, TxStrong>> {
        Some(ChanPtr {
            inner: self.inner.clone(),
            ref_counter: TxStrong::try_new(self.inner.as_ref())?,
        })
    }
}

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

impl<A, Rc> ChanPtr<A, Rc>
where
    Rc: RefCounter + Into<TxEither>,
{
    pub fn to_tx_either(&self) -> ChanPtr<A, TxEither> {
        ChanPtr {
            inner: self.inner.clone(),
            ref_counter: self.ref_counter.make_new(&self.inner).into(),
        }
    }
}

impl<A, Rc> Clone for ChanPtr<A, Rc>
where
    Rc: RefCounter,
{
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            ref_counter: self.ref_counter.make_new(self.inner.as_ref()),
        }
    }
}

impl<A, Rc> Drop for ChanPtr<A, Rc>
where
    Rc: RefCounter,
{
    fn drop(&mut self) {
        self.ref_counter.destroy(self.inner.as_ref())
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

impl<A, Rc> fmt::Debug for ChanPtr<A, Rc>
where
    Rc: RefCounter,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use atomic::Ordering::SeqCst;

        let act = std::any::type_name::<A>();
        let rc = std::any::type_name::<Rc>()
            .trim_start_matches(module_path!())
            .trim_start_matches("::");
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

/// A reference counter for the receiving end of the channel or in actor terminology, the mailbox
/// of an actor.
pub struct Rx(());

/// Defines a reference counting policy for a channel pointer.
///
/// # Implementation note
///
/// This trait only exists because we cannot specialise `Drop` impls in Rust, otherwise, we could
/// write:
///
/// - `impl Drop for ChanPtr<A, TxStrong> { }`
/// - `impl Drop for ChanPtr<A, TxEither> { }`
/// - `impl Drop for ChanPtr<A, Rx> { }`
///
/// and call the appropriate functions on `Chan`.
pub trait RefCounter: Send + Sync + 'static + Unpin + private::RefCounter {}

impl RefCounter for TxStrong {}
impl RefCounter for TxWeak {}
impl RefCounter for TxEither {}
impl RefCounter for Rx {}

impl From<TxStrong> for TxEither {
    fn from(strong: TxStrong) -> Self {
        TxEither::Strong(strong)
    }
}

impl From<TxWeak> for TxEither {
    fn from(weak: TxWeak) -> Self {
        TxEither::Weak(weak)
    }
}

/// Private module to ensure [`RefCounter`] is sealed and can neither be implemented on other types
/// nor can a user outside of this crate call functions from [`RefCounter`].
mod private {
    use super::*;

    pub trait RefCounter {
        /// Make a new instance of this reference counting policy.
        fn make_new<A>(&self, chan: &Chan<A>) -> Self;
        /// Destroy an instance of this reference counting policy.
        fn destroy<A>(&self, chan: &Chan<A>);
        /// Whether or not the given policy is strong.
        fn is_strong(&self) -> bool;
    }

    impl RefCounter for TxStrong {
        fn make_new<A>(&self, inner: &Chan<A>) -> Self {
            inner.on_sender_created();

            TxStrong(())
        }

        fn destroy<A>(&self, inner: &Chan<A>) {
            inner.on_sender_dropped();
        }

        fn is_strong(&self) -> bool {
            true
        }
    }

    impl RefCounter for TxWeak {
        fn make_new<A>(&self, _: &Chan<A>) -> Self {
            TxWeak(())
        }

        fn destroy<A>(&self, _: &Chan<A>) {}

        fn is_strong(&self) -> bool {
            false
        }
    }

    impl RefCounter for TxEither {
        fn make_new<A>(&self, chan: &Chan<A>) -> Self {
            match self {
                TxEither::Strong(strong) => TxEither::Strong(strong.make_new(chan)),
                TxEither::Weak(weak) => TxEither::Weak(weak.make_new(chan)),
            }
        }

        fn destroy<A>(&self, chan: &Chan<A>) {
            match self {
                TxEither::Strong(strong) => strong.destroy(chan),
                TxEither::Weak(weak) => weak.destroy(chan),
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
        fn make_new<A>(&self, inner: &Chan<A>) -> Self {
            inner.on_receiver_created();

            Rx(())
        }

        fn destroy<A>(&self, inner: &Chan<A>) {
            inner.on_receiver_dropped();
        }

        fn is_strong(&self) -> bool {
            true
        }
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

        assert_eq!(inner.sender_count.load(atomic::Ordering::SeqCst), 1)
    }

    #[test]
    fn clone_increments_count() {
        let inner = Arc::new(Chan::new(None));

        let ptr1 = ChanPtr::<Foo, TxStrong>::new(inner.clone());
        #[allow(clippy::redundant_clone)]
        let _ptr2 = ptr1.clone();

        assert_eq!(inner.sender_count.load(atomic::Ordering::SeqCst), 2)
    }

    #[test]
    fn dropping_last_reference_calls_on_last_drop() {
        let inner = Arc::new(Chan::new(None));

        let ptr1 = ChanPtr::<Foo, TxStrong>::new(inner.clone());
        std::mem::drop(ptr1);

        assert_eq!(inner.sender_count.load(atomic::Ordering::SeqCst), 0);
    }

    #[test]
    fn can_convert_tx_strong_into_weak() {
        let inner = Arc::new(Chan::new(None));

        let strong_ptr = ChanPtr::<Foo, TxStrong>::new(inner.clone());
        let _weak_ptr = strong_ptr.to_tx_weak();

        assert_eq!(inner.sender_count.load(atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn can_clone_either() {
        let inner = Arc::new(Chan::new(None));

        let strong_ptr = ChanPtr::<Foo, TxStrong>::new(inner.clone());
        let either_ptr_1 = strong_ptr.to_tx_either();
        #[allow(clippy::redundant_clone)]
        let _either_ptr_2 = either_ptr_1.clone();

        assert_eq!(inner.sender_count.load(atomic::Ordering::SeqCst), 3);
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
