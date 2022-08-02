use std::sync::Arc;

use crate::inbox::tx::{TxStrong, TxWeak};
use crate::inbox::{BroadcastQueue, Chan, Sender};

pub struct Receiver<A> {
    pub inner: Arc<Chan<A>>,
    pub broadcast_mailbox: Arc<BroadcastQueue<A>>,
}

impl<A> Receiver<A> {
    pub fn new(inner: Arc<Chan<A>>) -> Self {
        let new_broadcast_mailbox = inner.new_broadcast_mailbox();

        Receiver::with_broadcast_mailbox(inner, new_broadcast_mailbox)
    }

    pub fn with_broadcast_mailbox(
        inner: Arc<Chan<A>>,
        broadcast_mailbox: Arc<BroadcastQueue<A>>,
    ) -> Self {
        inner.increment_receiver_count();

        Receiver {
            inner,
            broadcast_mailbox,
        }
    }
}

impl<A> Receiver<A> {
    pub fn sender(&self) -> Option<Sender<A, TxStrong>> {
        Sender::try_new_strong(self.inner.clone())
    }

    pub fn weak_sender(&self) -> Sender<A, TxWeak> {
        Sender::new_weak(self.inner.clone())
    }
}

impl<A> Clone for Receiver<A> {
    fn clone(&self) -> Self {
        self.inner.increment_receiver_count();

        Receiver {
            inner: self.inner.clone(),
            broadcast_mailbox: self.inner.new_broadcast_mailbox(),
        }
    }
}

impl<A> Drop for Receiver<A> {
    fn drop(&mut self) {
        self.inner.decrement_receiver_count()
    }
}
