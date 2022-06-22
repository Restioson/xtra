//! The mailbox is where all messages on an actor's address get sent to.

use crate::inbox::rx::{ReceiveFuture as InboxReceiveFuture, RxStrong};
use crate::inbox::ActorMessage;
use crate::{inbox, ActorShutdown, Address, WeakAddress};
use futures_core::FusedFuture;
use futures_util::FutureExt;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

/// The mailbox of an actor. All messages sent and broadcast to this actor will be received from
/// its mailbox.
pub struct Mailbox<A>(pub(crate) inbox::Receiver<A, RxStrong>);

impl<A> Mailbox<A> {
    /// Get for the next message from the actor's mailbox.
    pub fn next(&self) -> ReceiveFuture<A> {
        ReceiveFuture(self.0.receive())
    }

    /// Get an address to the current actor if there are still external addresses to the actor.
    pub fn address(&self) -> Result<Address<A>, ActorShutdown> {
        self.0.sender().ok_or(ActorShutdown).map(Address)
    }

    /// Get a weak address to the current actor.
    pub fn weak_address(&self) -> WeakAddress<A> {
        Address(self.0.weak_sender())
    }

    pub(crate) fn cloned_new_broadcast_mailbox(&self) -> Self {
        Mailbox(self.0.cloned_new_broadcast_mailbox())
    }
}

/// A message sent to a given actor, or a notification that it should shut down.
pub struct Message<A>(pub(crate) ActorMessage<A>);

/// A future which will resolve to the next message to be handled by the actor.
#[must_use = "Futures do nothing unless polled"]
pub struct ReceiveFuture<A>(InboxReceiveFuture<A, RxStrong>);

impl<A> ReceiveFuture<A> {
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
