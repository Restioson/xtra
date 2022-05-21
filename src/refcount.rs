use std::fmt::{Debug, Formatter};
use crate::drop_notice;
use crate::drop_notice::DropNotice;
use std::fmt::{Debug, Formatter};
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
pub struct Strong {
    shared: Arc<Shared>,
    lock: Arc<RwLock<()>>,
}

impl Debug for Strong {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Strong")
            .field("connected", &self.is_connected())
            .field("strong_count", &self.strong_count())
            .field("weak_count", &self.weak_count())
            .finish()
    }
}

#[doc(hidden)]
pub struct Shared {
    connected: AtomicBool,
    drop_notice: DropNotice,
}

impl Strong {
    pub(crate) fn new(connected: AtomicBool, drop_notice: DropNotice) -> Self {
        Self {
            shared: Arc::new(Shared {
                connected,
                drop_notice,
            }),
            lock: Arc::new(RwLock::new(())),
        }
    }

    fn weak_count(&self) -> usize {
        let _lock = self.lock.read().unwrap();
        Arc::weak_count(&self.shared)
    }

    pub(crate) fn downgrade(&self) -> Weak {
        Weak {
            shared: Arc::downgrade(&self.shared),
            lock: self.lock.clone(),
        }
    }

    pub(crate) fn mark_disconnected(&self) {
        self.shared.connected.store(false, Ordering::Release)
    }
}

/// The reference count of a weak address. Weak addresses will bit prevent the actor from being
/// dropped. Read the docs of [`Address`](../address/struct.Address.html) to find out more.
#[derive(Clone)]
pub struct Weak {
    shared: ArcWeak<Shared>,
    lock: Arc<RwLock<()>>,
}

impl Debug for Weak {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Weak")
            .field("connected", &self.is_connected())
            .field("strong_count", &self.strong_count())
            .field("weak_count", &self.weak_count())
            .finish()
    }
}

impl Weak {
    pub(crate) fn upgrade(&self) -> Option<Strong> {
        ArcWeak::upgrade(&self.shared).map(|shared| Strong {
            shared,
            lock: self.lock.clone(),
        })
    }

    fn weak_count(&self) -> usize {
        let _lock = self.lock.read().unwrap();
        ArcWeak::weak_count(&self.shared)
    }
}

/// A reference counter that can be dynamically either strong or weak.
#[derive(Clone, Debug)]
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
pub trait RefCounter: Sealed + Clone + Unpin + Debug + Send + Sync + 'static {
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
    fn as_ptr(&self) -> *const Shared;

    /// Returns a `DropNotice` that resolves once this address becomes disconnected.
    fn disconnect_notice(&self) -> DropNotice;
}

impl RefCounter for Strong {
    fn is_connected(&self) -> bool {
        self.strong_count() > 0 && self.shared.connected.load(Ordering::Acquire)
    }

    fn is_last_strong(&self) -> bool {
        let _lock = self.lock.read().unwrap();
        Arc::strong_count(&self.shared) == 1
    }

    fn strong_count(&self) -> usize {
        let _lock = self.lock.read().unwrap();
        Arc::strong_count(&self.shared)
    }

    fn into_either(self) -> Either {
        Either::Strong(self)
    }

    fn as_ptr(&self) -> *const Shared {
        Arc::as_ptr(&self.shared)
    }

    fn disconnect_notice(&self) -> DropNotice {
        self.shared.drop_notice.clone()
    }
}

impl RefCounter for Weak {
    fn is_connected(&self) -> bool {
        let lock = self.lock.write().unwrap();

        let running = self
            .shared
            .upgrade()
            .map(|shared| {
                let connected = shared.connected.load(Ordering::Acquire);
                drop(shared);
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
        let _lock = self.lock.read().unwrap();
        ArcWeak::strong_count(&self.shared)
    }

    fn into_either(self) -> Either {
        Either::Weak(self)
    }

    fn as_ptr(&self) -> *const Shared {
        ArcWeak::as_ptr(&self.shared)
    }

    fn disconnect_notice(&self) -> DropNotice {
        let _lock = self.lock.write().unwrap();

        match self.shared.upgrade() {
            Some(shared) => {
                let drop_notice = shared.drop_notice.clone();
                drop(shared);
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

    fn as_ptr(&self) -> *const Shared {
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
