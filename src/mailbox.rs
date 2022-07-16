use crate::inbox::rx::RxStrong;
use crate::inbox::ActorMessage;
use crate::{inbox, Address, WeakAddress};
use futures_core::FusedFuture;
use futures_util::FutureExt;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

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

/// A message sent to a given actor, or a notification that it should shut down.
pub struct Message<A>(pub(crate) ActorMessage<A>);

/// A future which will resolve to the next message to be handled by the actor.
#[must_use = "Futures do nothing unless polled"]
pub struct ReceiveFuture<A>(pub(crate) inbox::rx::ReceiveFuture<A, RxStrong>);

impl<'c, A> ReceiveFuture<A> {
    /// Cancel the receiving future, returning a message if it had been fulfilled with one, but had
    /// not yet been polled after wakeup. Future calls to `Future::poll` will return `Poll::Pending`,
    /// and `FusedFuture::is_terminated` will return `true`.
    ///
    /// This is important to do over `Drop`, as with `Drop` messages may be sent back into the
    /// channel and could be observed as received out of order, if multiple receives are concurrent
    /// on one thread.
    #[must_use = "If dropped, messages could be lost"]
    pub fn cancel(&mut self) -> Option<Message<A>> {
        self.0.cancel().map(Message)
    }
}

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
