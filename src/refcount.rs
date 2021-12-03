use crate::drop_notice;
use crate::drop_notice::DropNotice;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock, Weak as ArcWeak};

use crate::private::Sealed;

/// The reference count of a strong address. Strong addresses will prevent the actor from being
/// dropped as long as they live. Read the docs of [`Address`](../address/struct.Address.html) to find
/// out more.
#[derive(Clone)]
// TODO AtomicBool for disconnected when forcibly stopped ?
// The RwLock is there to prevent exposing a temporarily inconsistent strong_count caused by brief
// Arc::upgrade calls in some `Weak` functions below. If exposed, it could lead to a race condition
// that can prevent an Actor from being stopped.
pub struct Strong(pub(crate) Arc<(AtomicBool, DropNotice)>, Arc<RwLock<()>>);

impl Strong {
    pub(crate) fn new(connected: AtomicBool, drop_notice: DropNotice) -> Self {
        Self {
            0: Arc::new((connected, drop_notice)),
            1: Arc::new(RwLock::new(())),
        }
    }

    pub(crate) fn downgrade(&self) -> Weak {
        Weak(Arc::downgrade(&self.0), self.1.clone())
    }
}

/// The reference count of a weak address. Weak addresses will bit prevent the actor from being
/// dropped. Read the docs of [`Address`](../address/struct.Address.html) to find out more.
#[derive(Clone)]
pub struct Weak(
    pub(crate) ArcWeak<(AtomicBool, DropNotice)>,
    Arc<RwLock<()>>,
);

impl Weak {
    pub(crate) fn upgrade(&self) -> Option<Strong> {
        ArcWeak::upgrade(&self.0).map(|b| Strong(b, self.1.clone()))
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
pub trait RefCounter: Sealed + Clone + Unpin + Send + Sync + 'static {
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
    fn as_ptr(&self) -> *const (AtomicBool, DropNotice);

    /// Returns a `DropNotice` that resolves once this address becomes disconnected.
    fn disconnect_notice(&self) -> DropNotice;
}

impl RefCounter for Strong {
    fn is_connected(&self) -> bool {
        self.strong_count() > 0 && self.0 .0.load(Ordering::Acquire)
    }

    fn is_last_strong(&self) -> bool {
        let _lock = self.1.read().unwrap();
        Arc::strong_count(&self.0) == 1
    }

    fn strong_count(&self) -> usize {
        let _lock = self.1.read().unwrap();
        Arc::strong_count(&self.0)
    }

    fn into_either(self) -> Either {
        Either::Strong(self)
    }

    fn as_ptr(&self) -> *const (AtomicBool, DropNotice) {
        Arc::as_ptr(&self.0)
    }

    fn disconnect_notice(&self) -> DropNotice {
        self.0 .1.clone()
    }
}

impl RefCounter for Weak {
    fn is_connected(&self) -> bool {
        let lock = self.1.write().unwrap();

        let running = self
            .0
            .upgrade()
            .map(|b| {
                let connected = b.0.load(Ordering::Acquire);
                drop(b);
                connected
            })
            .unwrap_or(false);

        drop(lock);
        self.strong_count() > 0 && running
    }

    fn is_last_strong(&self) -> bool {
        false
    }

    fn strong_count(&self) -> usize {
        let _lock = self.1.read().unwrap();
        ArcWeak::strong_count(&self.0)
    }

    fn into_either(self) -> Either {
        Either::Weak(self)
    }

    fn as_ptr(&self) -> *const (AtomicBool, DropNotice) {
        ArcWeak::as_ptr(&self.0)
    }

    fn disconnect_notice(&self) -> DropNotice {
        let _lock = self.1.write().unwrap();

        match self.0.upgrade() {
            Some(b) => {
                let drop_notice = b.1.clone();
                drop(b);
                drop_notice
            }
            None => drop_notice::dropped(),
        }
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

    fn as_ptr(&self) -> *const (AtomicBool, DropNotice) {
        match self {
            Either::Strong(s) => s.as_ptr(),
            Either::Weak(s) => s.as_ptr(),
        }
    }

    fn disconnect_notice(&self) -> DropNotice {
        match self {
            Either::Strong(s) => s.disconnect_notice(),
            Either::Weak(s) => s.disconnect_notice(),
        }
    }
}
