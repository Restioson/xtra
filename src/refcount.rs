use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak as ArcWeak};

/// The reference count of a strong address. Strong addresses will prevent the actor from being
/// dropped as long as they live. Read the docs of [`Address`](../address/struct.Address.html) to find
/// out more.
#[derive(Clone)]
// TODO AtomicBool for disconnected when forcibly stopped ?
pub struct Strong(pub(crate) Arc<AtomicBool>);

impl Strong {
    pub(crate) fn downgrade(&self) -> Weak {
        Weak(Arc::downgrade(&self.0))
    }
}

/// The reference count of a weak address. Weak addresses will bit prevent the actor from being
/// dropped. Read the docs of [`Address`](../address/struct.Address.html) to find out more.
#[derive(Clone)]
pub struct Weak(pub(crate) ArcWeak<AtomicBool>);

impl Weak {
    pub(crate) fn upgrade(&self) -> Option<Strong> {
        ArcWeak::upgrade(&self.0).map(Strong)
    }
}

/// A reference counter that can be dynamically either strong or weak.
#[derive(Clone)]
pub enum Either {
    /// A strong reference counter.
    Strong(Strong),
    /// A weak reference counter.
    Weak(Weak),
}

impl Either {
    pub(crate) fn into_weak(self) -> Weak {
        match self {
            Either::Strong(strong) => strong.downgrade(),
            Either::Weak(weak) => weak,
        }
    }
}

/// This trait represents the strength of an address's reference counting. It is an internal trait.
/// There are two implementations of this trait: [`Weak`](struct.Weak.html) and
/// [`Strong`](struct.Weak.html). These can be provided as the second type argument to
/// [`Address`](../address/struct.Address.html) in order to change how the address affects the actor's
/// dropping. Read the docs of [`Address`](../address/struct.Address.html) to find out more.
pub trait RefCounter: Clone + Unpin + Send + Sync + 'static {
    #[doc(hidden)]
    fn is_connected(&self) -> bool;
    #[doc(hidden)]
    fn is_last_strong(&self) -> bool;
    #[doc(hidden)]
    fn strong_count(&self) -> usize;

    // These above two methods cannot be merged since is_last_strong is always false for Weak. If
    // strong_count were used to implement this, a weak being dropped could think it were a strong.

    #[doc(hidden)]
    fn into_either(self) -> Either;

    #[doc(hidden)]
    fn as_ptr(&self) -> *const AtomicBool;
}

impl RefCounter for Strong {
    fn is_connected(&self) -> bool {
        self.strong_count() > 0 && self.0.load(Ordering::Acquire)
    }

    fn is_last_strong(&self) -> bool {
        Arc::strong_count(&self.0) == 1
    }

    fn strong_count(&self) -> usize {
        Arc::strong_count(&self.0)
    }

    fn into_either(self) -> Either {
        Either::Strong(self)
    }

    fn as_ptr(&self) -> *const AtomicBool {
        Arc::as_ptr(&self.0)
    }
}

impl RefCounter for Weak {
    fn is_connected(&self) -> bool {
        let running = self
            .0
            .upgrade()
            .map(|b| b.load(Ordering::Acquire))
            .unwrap_or(false);

        self.strong_count() > 0 && running
    }

    fn is_last_strong(&self) -> bool {
        false
    }

    fn strong_count(&self) -> usize {
        ArcWeak::strong_count(&self.0)
    }

    fn into_either(self) -> Either {
        Either::Weak(self)
    }

    fn as_ptr(&self) -> *const AtomicBool {
        ArcWeak::as_ptr(&self.0)
    }
}

impl RefCounter for Either {
    fn is_connected(&self) -> bool {
        match self {
            Either::Strong(strong) => strong.is_connected(),
            Either::Weak(weak) => weak.is_connected(),
        }
    }

    fn is_last_strong(&self) -> bool {
        match self {
            Either::Strong(strong) => strong.is_last_strong(),
            Either::Weak(weak) => weak.is_last_strong(),
        }
    }

    fn strong_count(&self) -> usize {
        match self {
            Either::Strong(strong) => strong.strong_count(),
            Either::Weak(weak) => weak.strong_count(),
        }
    }

    fn into_either(self) -> Either {
        self
    }

    fn as_ptr(&self) -> *const AtomicBool {
        match self {
            Either::Strong(s) => s.as_ptr(),
            Either::Weak(s) => s.as_ptr(),
        }
    }
}
