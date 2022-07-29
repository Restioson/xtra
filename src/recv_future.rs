use std::future::Future;
use std::mem;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use futures_core::FusedFuture;
use futures_util::FutureExt;

use crate::inbox::rx::RxStrong;
use crate::inbox::{ActorMessage, BroadcastQueue, Chan, Receiver, WaitingReceiver};

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
pub struct ReceiveFuture<A>(Receiving<A>);

/// A message sent to a given actor, or a notification that it should shut down.
pub struct Message<A>(pub(crate) ActorMessage<A>);

impl<A> ReceiveFuture<A> {
    pub(crate) fn new(
        chan: Arc<Chan<A>>,
        broadcast_mailbox: Arc<BroadcastQueue<A>>,
        rc: RxStrong,
    ) -> Self {
        Self(Receiving::New(Receiver {
            inner: chan,
            broadcast_mailbox,
            rc,
        }))
    }
}

impl<A> Future for ReceiveFuture<A> {
    type Output = Message<A>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.0.poll_unpin(cx).map(Message)
    }
}

/// Module-private type modelling the actual state machine of receiving a message.
///
/// This type only exists because the variants of an enum are public and we would leak
/// implementation details like the variant names into the public API.
///
/// This future is the counterpart to [`Sending`](crate::send_future::Sending).
enum Receiving<A> {
    New(Receiver<A, RxStrong>),
    WaitingToReceive(Waiting<A>),
    Done,
}

/// Dedicated "waiting" state for the [`ReceiveFuture`].
///
/// This type encapsulates the waiting for a notification from the channel about a new message that
/// can be received. This notification may arrive in the [`WaitingReceiver`] before we poll it again.
///
/// To avoid losing a message, this type implements [`Drop`] and re-queues the message into the
/// mailbox in such a scenario.
struct Waiting<A> {
    channel_receiver: Receiver<A, RxStrong>,
    waiting_receiver: WaitingReceiver<A>,
}

impl<A> Future for Waiting<A> {
    type Output = Result<ActorMessage<A>, Receiver<A, RxStrong>>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();

        let result =
            match futures_util::ready!(this.waiting_receiver.poll(&this.channel_receiver, cx)) {
                None => Err(this.channel_receiver.clone()), // TODO: Optimise this clone with an `Option` where we call `take`?
                Some(msg) => Ok(msg),
            };

        Poll::Ready(result)
    }
}

impl<A> Drop for Waiting<A> {
    fn drop(&mut self) {
        if let Some(msg) = self.waiting_receiver.cancel() {
            self.channel_receiver.inner.requeue_message(msg);
        }
    }
}

impl<A> Future for Receiving<A> {
    type Output = ActorMessage<A>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<ActorMessage<A>> {
        let this = self.get_mut();

        loop {
            match mem::replace(this, Receiving::Done) {
                Receiving::New(rx) => match rx.inner.try_recv(rx.broadcast_mailbox.as_ref()) {
                    Ok(message) => return Poll::Ready(message),
                    Err(waiting) => {
                        *this = Receiving::WaitingToReceive(Waiting {
                            channel_receiver: rx,
                            waiting_receiver: waiting,
                        });
                    }
                },
                Receiving::WaitingToReceive(mut waiting) => match waiting.poll_unpin(cx) {
                    Poll::Ready(Ok(msg)) => return Poll::Ready(msg),
                    Poll::Ready(Err(rx)) => {
                        // False positive wake up, try receive again.
                        *this = Receiving::New(rx);
                    }
                    Poll::Pending => {
                        *this = Receiving::WaitingToReceive(waiting);
                        return Poll::Pending;
                    }
                },
                Receiving::Done => panic!("polled after completion"),
            }
        }
    }
}

impl<A> FusedFuture for Receiving<A> {
    fn is_terminated(&self) -> bool {
        matches!(self, Receiving::Done)
    }
}

impl<A> FusedFuture for ReceiveFuture<A> {
    fn is_terminated(&self) -> bool {
        self.0.is_terminated()
    }
}
