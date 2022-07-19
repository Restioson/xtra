use std::task::{Context, Poll};

use crate::envelope::MessageEnvelope;
use crate::inbox::rx::Receiver;
use crate::inbox::rx::RxRefCounter;
use crate::inbox::ActorMessage;

/// A [`WaitingReceiver`] is handed out by the channel any time [`Chan::try_recv`](crate::inbox::Chan::try_recv) is called on an empty mailbox.
///
/// [`WaitingReceiver`] implements [`Future`](std::future::Future) which will resolve once a message lands in the mailbox.
pub struct WaitingReceiver<A>(catty::Receiver<WakeReason<A>>);

/// A [`FulfillHandle`] is the counter-part to a [`WaitingReceiver`].
///
/// It is stored internally in the channel and used by the channel implementation to notify a
/// [`WaitingReceiver`] once a new message hits the mailbox.
pub struct FulfillHandle<A>(catty::Sender<WakeReason<A>>);

impl<A> FulfillHandle<A> {
    /// Shutdown the connected [`WaitingReceiver`].
    ///
    /// This will wake the corresponding task and notify them that the channel is shutting down.
    pub fn shutdown(self) {
        let _ = self.0.send(WakeReason::Shutdown);
    }

    /// Notify the connected [`WaitingReceiver`] about a new broadcast message.
    ///
    /// Broadcast mailboxes are stored outside of the main channel implementation which is why this
    /// messages does not take any arguments.
    pub fn fulfill_message_to_all_actors(self) -> Result<(), ()> {
        self.0.send(WakeReason::MessageToAllActors).map_err(|_| ())
    }

    /// Notify the connected [`WaitingReceiver`] about a new message.
    ///
    /// A new message was sent into the channel and the connected [`WaitingReceiver`] is the chosen
    /// one to handle it, likely because it has been waiting the longest.
    ///
    /// This function will return the message in an `Err` if the [`WaitingReceiver`] has since called
    /// [`cancel`](WaitingReceiver::cancel) and is therefore unable to handle the message.
    pub fn fulfill_message_to_one_actor(
        self,
        msg: Box<dyn MessageEnvelope<Actor = A>>,
    ) -> Result<(), Box<dyn MessageEnvelope<Actor = A>>> {
        self.0
            .send(WakeReason::MessageToOneActor(msg))
            .map_err(|reason| match reason {
                WakeReason::MessageToOneActor(msg) => msg,
                _ => unreachable!(),
            })
    }
}

impl<A> WaitingReceiver<A> {
    pub fn new() -> (WaitingReceiver<A>, FulfillHandle<A>) {
        let (sender, receiver) = catty::oneshot();

        let handle = FulfillHandle(sender);
        let receiver = WaitingReceiver(receiver);

        (receiver, handle)
    }

    /// Cancel this [`WaitingReceiver`].
    ///
    /// This may return a message in case the underlying state has been fulfilled with one but this
    /// receiver has not been polled since.
    ///
    /// It is important to call this message over just dropping the [`WaitingReceiver`] as this
    /// message would otherwise be dropped.
    pub fn cancel(&self) -> Option<Box<dyn MessageEnvelope<Actor = A>>> {
        match self.0.try_recv() {
            Ok(Some(WakeReason::MessageToOneActor(msg))) => Some(msg),
            _ => None,
        }
    }

    /// Tell the [`WaitingReceiver`] to make progress.
    ///
    /// In case we have been woken with a reason, we will attempt to produce an [`ActorMessage`].
    /// Wake-ups can be false-positives in the case of a broadcast message which is why this
    /// function returns only [`Option<ActorMessage>`].
    pub fn poll<Rc>(
        &self,
        receiver: &Receiver<A, Rc>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<ActorMessage<A>>>
    where
        Rc: RxRefCounter,
    {
        let reason = match futures_util::ready!(self.inner.poll_recv(cx)) {
            Ok(reason) => reason,
            Err(_) => return Poll::Ready(None), // TODO: Not sure if this is correct.
        };

        let actor_message = match reason {
            WakeReason::MessageToOneActor(msg) => ActorMessage::ToOneActor(msg),
            WakeReason::Shutdown => ActorMessage::Shutdown,
            WakeReason::MessageToAllActors => match receiver.next_broadcast_message() {
                Some(msg) => ActorMessage::ToAllActors(msg),
                None => return Poll::Ready(None),
            },
        };

        Poll::Ready(Some(actor_message))
    }
}

enum WakeReason<A> {
    MessageToOneActor(Box<dyn MessageEnvelope<Actor = A>>),
    // should be fetched from own receiver
    MessageToAllActors,
    Shutdown,
}
