use crate::context::ReceiveFuture;
use crate::inbox::rx::RxStrong;
use crate::{inbox, Address, WeakAddress};

/// The mailbox of an actor.
///
/// This is where all messages sent to an actor's address land.
pub struct Mailbox<A>(inbox::Receiver<A, RxStrong>);

impl<A> Clone for Mailbox<A> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<A> Mailbox<A> {
    /// Constructs a new, unbounded mailbox.
    pub fn new(capacity: Option<usize>) -> (Address<A>, Mailbox<A>) {
        let (sender, receiver) = crate::inbox::new(capacity);

        (Address(sender), Mailbox(receiver))
    }

    /// Read the next message from the mailbox.
    pub fn next(&self) -> ReceiveFuture<A> {
        ReceiveFuture(self.0.receive())
    }

    /// Grab an address to this mailbox.
    ///
    /// The returned address is a [`WeakAddress`]. To get a strong address, use [`WeakAddress::upgrade`].
    pub fn address(&self) -> WeakAddress<A> {
        Address(self.0.weak_sender())
    }
}
