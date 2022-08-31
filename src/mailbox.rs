use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use futures_core::FusedFuture;
use futures_util::FutureExt;

use crate::inbox::ActorMessage;
use crate::{inbox, Address, WeakAddress};

/// The mailbox of an actor.
///
/// This is where all messages sent to an actor's address land.
pub struct Mailbox<A>(inbox::Receiver<A>);

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
    /// The returned address is a [`WeakAddress`]. To get a strong address, use [`WeakAddress::try_upgrade`].
    pub fn address(&self) -> WeakAddress<A> {
        Address(self.0.weak_sender())
    }
}

/// A message sent to a given actor, or a notification that it should shut down.
pub struct Message<A>(pub(crate) ActorMessage<A>);

/// A future which will resolve to the next message to be handled by the actor.
#[must_use = "Futures do nothing unless polled"]
pub struct ReceiveFuture<A>(pub(crate) inbox::rx::ReceiveFuture<A>);

impl<A> Future for ReceiveFuture<A> {
    type Output = Message<A>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.0.poll_unpin(cx).map(Message)
    }
}

impl<A> FusedFuture for ReceiveFuture<A> {
    fn is_terminated(&self) -> bool {
        self.0.is_terminated()
    }
}
