use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use futures_core::FusedFuture;
use futures_util::FutureExt;

use crate::inbox;
use crate::inbox::rx::RxStrong;
use crate::inbox::ActorMessage;

/// A future which will resolve to the next message to be handled by the actor.
///
/// # Cancellation safety
///
/// This future is cancellation-safe in that no messages will ever be lost, even if this future is
/// dropped half-way through. However, reinserting the message into the mailbox may mess with the
/// ordering of messages and they may be handled by the actor out of order.
///
/// If the order in which your actors process messages is not important to you, you can consider this
/// future to be fully cancellation-safe.
///
/// If you wish to maintain message ordering, you can use [`FutureExt::now_or_never`] to do a final
/// poll on the future. [`ReceiveFuture`] is guaranteed to complete in a single poll if it has
/// remaining work to do.
#[must_use = "Futures do nothing unless polled"]
pub struct ReceiveFuture<A>(pub(crate) inbox::rx::ReceiveFuture<A, RxStrong>);

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

/// A message sent to a given actor, or a notification that it should shut down.
pub struct Message<A>(pub(crate) ActorMessage<A>);
