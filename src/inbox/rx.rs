use std::sync::{atomic, Arc};

use crate::envelope::BroadcastEnvelope;
use crate::inbox::tx::{TxStrong, TxWeak};
use crate::inbox::{BroadcastQueue, Chan, Sender};

pub struct Receiver<A, Rc: RxRefCounter> {
    pub inner: Arc<Chan<A>>,
    pub broadcast_mailbox: Arc<BroadcastQueue<A>>,
    pub rc: Rc,
}

impl<A, Rc: RxRefCounter> Receiver<A, Rc> {
    pub fn next_broadcast_message(&self) -> Option<Arc<dyn BroadcastEnvelope<Actor = A>>> {
        self.inner.pop_broadcast_message(&self.broadcast_mailbox)
    }
}

impl<A> Receiver<A, RxStrong> {
    pub(super) fn new(inner: Arc<Chan<A>>) -> Self {
        let rc = RxStrong(());
        rc.increment(&inner);

        Receiver {
            broadcast_mailbox: inner.new_broadcast_mailbox(),
            inner,
            rc,
        }
    }
}

impl<A, Rc: RxRefCounter> Receiver<A, Rc> {
    pub fn sender(&self) -> Option<Sender<A, TxStrong>> {
        Sender::try_new_strong(self.inner.clone())
    }

    pub fn weak_sender(&self) -> Sender<A, TxWeak> {
        Sender::new_weak(self.inner.clone())
    }
}

impl<A, Rc: RxRefCounter> Clone for Receiver<A, Rc> {
    fn clone(&self) -> Self {
        Receiver {
            inner: self.inner.clone(),
            broadcast_mailbox: self.inner.new_broadcast_mailbox(),
            rc: self.rc.increment(&self.inner),
        }
    }
}

impl<A, Rc: RxRefCounter> Drop for Receiver<A, Rc> {
    fn drop(&mut self) {
        if self.rc.decrement(&self.inner) {
            self.inner.shutdown_waiting_senders()
        }
    }
}

pub trait RxRefCounter: Unpin {
    fn increment<A>(&self, inner: &Chan<A>) -> Self;
    #[must_use = "If decrement returns false, the address must be disconnected"]
    fn decrement<A>(&self, inner: &Chan<A>) -> bool;
}

pub struct RxStrong(());

impl RxRefCounter for RxStrong {
    fn increment<A>(&self, inner: &Chan<A>) -> Self {
        inner.receiver_count.fetch_add(1, atomic::Ordering::Relaxed);
        RxStrong(())
    }

    fn decrement<A>(&self, inner: &Chan<A>) -> bool {
        // Memory orderings copied from Arc::drop
        if inner.receiver_count.fetch_sub(1, atomic::Ordering::Release) != 1 {
            return false;
        }

        atomic::fence(atomic::Ordering::Acquire);
        true
    }
}

pub struct RxWeak(());

impl RxRefCounter for RxWeak {
    fn increment<A>(&self, _inner: &Chan<A>) -> Self {
        RxWeak(())
    }

    fn decrement<A>(&self, _inner: &Chan<A>) -> bool {
        false
    }
}
