use std::fmt;

use super::*;
use crate::inbox::chan_ptr::{ChanPtr, RefCountPolicy, TxEither, TxWeak};
use crate::Actor;

pub struct Sender<A, Rc: RefCountPolicy> {
    pub inner: ChanPtr<A, Rc>,
}

impl<A> Sender<A, TxStrong> {
    pub fn new(inner: Arc<Chan<A>>) -> Self {
        Sender {
            inner: ChanPtr::<A, TxStrong>::new(inner),
        }
    }

    pub fn into_either_rc(self) -> Sender<A, TxEither> {
        Sender {
            inner: self.inner.to_tx_either(),
        }
    }
}

impl<A> Sender<A, TxWeak> {
    pub fn into_either_rc(self) -> Sender<A, TxEither> {
        Sender {
            inner: self.inner.to_tx_either(),
        }
    }
}

impl<Rc: RefCountPolicy, A> Sender<A, Rc> {
    pub fn stop_all_receivers(&self)
    where
        A: Actor,
    {
        self.inner.shutdown_all_receivers()
    }

    pub fn downgrade(&self) -> Sender<A, TxWeak> {
        Sender {
            inner: self.inner.to_tx_weak(),
        }
    }

    pub fn is_strong(&self) -> bool {
        self.inner.is_strong()
    }

    pub fn inner_ptr(&self) -> *const Chan<A> {
        (&self.inner as &Chan<A>) as *const Chan<A>
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

impl<A, Rc: RefCountPolicy> Clone for Sender<A, Rc> {
    fn clone(&self) -> Self {
        Sender {
            inner: self.inner.clone(),
        }
    }
}

impl<A, Rc: RefCountPolicy> fmt::Debug for Sender<A, Rc> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use atomic::Ordering::SeqCst;

        let act = std::any::type_name::<A>();
        let rc = std::any::type_name::<Rc>();
        f.debug_struct(&format!("Sender<{}, {}>", act, rc))
            .field("rx_count", &self.inner.receiver_count.load(SeqCst))
            .field("tx_count", &self.inner.sender_count.load(SeqCst))
            .finish()
    }
}
