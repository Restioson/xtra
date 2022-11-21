use std::sync::Arc;

use crate::chan::{self, BroadcastQueue, Rx};
use crate::recv_future::ReceiveFuture;
use crate::{Address, WeakAddress};

/// A [`Mailbox`] is the counter-part to an [`Address`].
///
/// Messages sent into an [`Address`] will be received in an actor's [`Mailbox`].
/// Think of [`Address`] and [`Mailbox`] as an MPMC channel.
pub struct Mailbox<A> {
    inner: chan::Ptr<A, Rx>,
    broadcast_mailbox: Arc<BroadcastQueue<A>>,
}

impl<A> Mailbox<A> {
    /// Creates a new [`Mailbox`] with the given capacity.
    pub fn bounded(capacity: usize) -> (Address<A>, Mailbox<A>) {
        let (sender, receiver) = chan::new(Some(capacity));

        let address = Address(sender);
        let mailbox = Mailbox {
            broadcast_mailbox: receiver.new_broadcast_mailbox(),
            inner: receiver,
        };

        (address, mailbox)
    }

    /// Creates a new, unbounded [`Mailbox`].
    ///
    /// Unbounded mailboxes will not perform an back-pressure and can result in potentially unbounded memory growth. Use with care.
    pub fn unbounded() -> (Address<A>, Mailbox<A>) {
        let (sender, receiver) = crate::chan::new(None);

        let address = Address(sender);
        let mailbox = Mailbox {
            broadcast_mailbox: receiver.new_broadcast_mailbox(),
            inner: receiver,
        };

        (address, mailbox)
    }

    /// Obtain a [`WeakAddress`] to this [`Mailbox`].
    ///
    /// Obtaining a [`WeakAddress`] is always successful even if there are no more strong addresses
    /// around. Use [`WeakAddress::try_upgrade`] to get a strong address.
    pub fn address(&self) -> WeakAddress<A> {
        Address(self.inner.to_tx_weak())
    }

    /// Take the next message out of the [`Mailbox`].
    pub fn next(&self) -> ReceiveFuture<A> {
        ReceiveFuture::new(self.inner.clone(), self.broadcast_mailbox.clone())
    }

    pub(crate) fn from_parts(
        chan: chan::Ptr<A, Rx>,
        broadcast_mailbox: Arc<BroadcastQueue<A>>,
    ) -> Self {
        Self {
            inner: chan,
            broadcast_mailbox,
        }
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
