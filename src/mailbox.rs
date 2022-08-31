use std::sync::Arc;

use crate::inbox::{BroadcastQueue, ChanPtr, Rx, TxStrong, TxWeak};
use crate::recv_future::ReceiveFuture;

pub struct Mailbox<A> {
    inner: ChanPtr<A, Rx>,
    broadcast_mailbox: Arc<BroadcastQueue<A>>,
}

impl<A> Mailbox<A> {
    pub(super) fn new(inner: ChanPtr<A, Rx>) -> Self {
        Mailbox {
            broadcast_mailbox: inner.new_broadcast_mailbox(),
            inner,
        }
    }

    pub fn sender(&self) -> Option<ChanPtr<A, TxStrong>> {
        self.inner.try_to_tx_strong()
    }
}

impl<A> Mailbox<A> {
    pub fn weak_sender(&self) -> ChanPtr<A, TxWeak> {
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
