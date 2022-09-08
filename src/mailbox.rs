use std::sync::Arc;

use crate::chan::{self, BroadcastQueue, Rx, TxStrong, TxWeak};
use crate::recv_future::ReceiveFuture;

pub struct Mailbox<A> {
    inner: chan::Ptr<A, Rx>,
    broadcast_mailbox: Arc<BroadcastQueue<A>>,
}

impl<A> Mailbox<A> {
    pub fn new(inner: chan::Ptr<A, Rx>) -> Self {
        Mailbox {
            broadcast_mailbox: inner.new_broadcast_mailbox(),
            inner,
        }
    }

    pub fn sender(&self) -> Option<chan::Ptr<A, TxStrong>> {
        self.inner.try_to_tx_strong()
    }

    pub fn weak_sender(&self) -> chan::Ptr<A, TxWeak> {
        self.inner.to_tx_weak()
    }

    pub fn receive(&self) -> ReceiveFuture<A> {
        ReceiveFuture::new(self.inner.clone(), self.broadcast_mailbox.clone())
    }
}

impl<A> Clone for Mailbox<A> {
    fn clone(&self) -> Self {
        Mailbox {
            inner: self.inner.clone(),
            broadcast_mailbox: self.inner.new_broadcast_mailbox(),
        }
    }
}
