use std::future::Future;
use std::mem;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use futures_core::FusedFuture;
use futures_util::FutureExt;

use crate::chan::{self, ActorMessage, BroadcastQueue, Rx, WaitingReceiver};

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
pub struct Message<A> {
    pub(crate) inner: ActorMessage<A>,

    pub(crate) channel: chan::Ptr<A, Rx>,
    pub(crate) broadcast_mailbox: Arc<BroadcastQueue<A>>,
}

impl<A> ReceiveFuture<A> {
    pub(crate) fn new(
        channel: chan::Ptr<A, Rx>,
        broadcast_mailbox: Arc<BroadcastQueue<A>>,
    ) -> Self {
        Self(Receiving::New {
            channel,
            broadcast_mailbox,
        })
    }
}

impl<A> Future for ReceiveFuture<A> {
    type Output = Message<A>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.0.poll_unpin(cx)
    }
}

/// Module-private type modelling the actual state machine of receiving a message.
///
/// This type only exists because the variants of an enum are public and we would leak
/// implementation details like the variant names into the public API.
#[must_use = "Futures do nothing unless polled"]
enum Receiving<A> {
    New {
        channel: chan::Ptr<A, Rx>,
        broadcast_mailbox: Arc<BroadcastQueue<A>>,
    },
    Waiting(Waiting<A>),
    Done,
}

/// Dedicated "waiting" state for the [`ReceiveFuture`].
///
/// This type encapsulates the waiting for a notification from the channel about a new message that
/// can be received. This notification may arrive in the [`WaitingReceiver`] before we poll it again.
///
/// To avoid losing a message, this type implements [`Drop`] and re-queues the message into the
/// mailbox in such a scenario.
#[must_use = "Futures do nothing unless polled"]
pub struct Waiting<A> {
    channel: Option<chan::Ptr<A, Rx>>,
    broadcast_mailbox: Option<Arc<BroadcastQueue<A>>>,
    waiting_receiver: WaitingReceiver<A>,
}

impl<A> Future for Waiting<A> {
    type Output = Result<
        (ActorMessage<A>, chan::Ptr<A, Rx>, Arc<BroadcastQueue<A>>),
        (chan::Ptr<A, Rx>, Arc<BroadcastQueue<A>>),
    >;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();

        let maybe_message = futures_util::ready!(this.waiting_receiver.poll(
            &this
                .channel
                .as_mut()
                .expect("to not be polled after completion"),
            &this
                .broadcast_mailbox
                .as_mut()
                .expect("to not be polled after completion"),
            cx
        ));

        let channel = this
            .channel
            .take()
            .expect("to not be polled after completion");
        let mailbox = this
            .broadcast_mailbox
            .take()
            .expect("to not be polled after completion");

        let result = match maybe_message {
            None => Err((channel, mailbox)),
            Some(msg) => Ok((msg, channel, mailbox)),
        };

        Poll::Ready(result)
    }
}

impl<A> Drop for Waiting<A> {
    fn drop(&mut self) {
        if let Some(msg) = self.waiting_receiver.cancel() {
            self.channel
                .as_mut()
                .expect("to not have message on drop but channel is gone")
                .requeue_message(msg);
        }
    }
}

impl<A> Future for Receiving<A> {
    type Output = Message<A>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Message<A>> {
        let this = self.get_mut();

        loop {
            match mem::replace(this, Receiving::Done) {
                Receiving::New {
                    channel,
                    broadcast_mailbox,
                } => match channel.try_recv(broadcast_mailbox.as_ref()) {
                    Ok(inner) => {
                        return Poll::Ready(Message {
                            inner,
                            channel,
                            broadcast_mailbox,
                        })
                    }
                    Err(waiting) => {
                        *this = Receiving::Waiting(Waiting {
                            channel: Some(channel),
                            broadcast_mailbox: Some(broadcast_mailbox),
                            waiting_receiver: waiting,
                        });
                    }
                },
                Receiving::Waiting(mut inner) => match inner.poll_unpin(cx) {
                    Poll::Ready(Ok((msg, channel, broadcast_mailbox))) => {
                        return Poll::Ready(Message {
                            inner: msg,
                            channel,
                            broadcast_mailbox,
                        })
                    }
                    Poll::Ready(Err((channel, broadcast_mailbox))) => {
                        // False positive wake up, try receive again.
                        *this = Receiving::New {
                            channel,
                            broadcast_mailbox,
                        };
                    }
                    Poll::Pending => {
                        *this = Receiving::Waiting(inner);
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
