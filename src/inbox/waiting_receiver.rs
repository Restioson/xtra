use std::task::{Context, Poll};

use futures_util::FutureExt;

use crate::inbox::{ActorMessage, BroadcastQueue, ChanPtr, MessageToOne, Rx};

/// A [`WaitingReceiver`] is handed out by the channel any time [`Chan::try_recv`](crate::inbox::Chan::try_recv) is called on an empty mailbox.
///
/// [`WaitingReceiver`] implements [`Future`](std::future::Future) which will resolve once a message lands in the mailbox.
pub struct WaitingReceiver<A>(catty::Receiver<CtrlMsg<A>>);

/// A [`Handle`] is the counter-part to a [`WaitingReceiver`].
///
/// It is stored internally in the channel and used by the channel implementation to notify a
/// [`WaitingReceiver`] once a new message hits the mailbox.
pub struct Handle<A>(catty::Sender<CtrlMsg<A>>);

impl<A> Handle<A> {
    /// Notify the connected [`WaitingReceiver`] that the channel is shutting down.
    pub fn notify_channel_shutdown(self) {
        let _ = self.0.send(CtrlMsg::Shutdown);
    }

    /// Notify the connected [`WaitingReceiver`] about a new broadcast message.
    ///
    /// Broadcast mailboxes are stored outside of the main channel implementation which is why this
    /// messages does not take any arguments.
    pub fn notify_new_broadcast(self) -> Result<(), ()> {
        self.0.send(CtrlMsg::NewBroadcast).map_err(|_| ())
    }

    /// Notify the connected [`WaitingReceiver`] about a new message.
    ///
    /// A new message was sent into the channel and the connected [`WaitingReceiver`] is the chosen
    /// one to handle it, likely because it has been waiting the longest.
    ///
    /// This function will return the message in an `Err` if the [`WaitingReceiver`] has since called
    /// [`cancel`](WaitingReceiver::cancel) and is therefore unable to handle the message.
    pub fn notify_new_message(self, msg: MessageToOne<A>) -> Result<(), MessageToOne<A>> {
        self.0
            .send(CtrlMsg::NewMessage(msg))
            .map_err(|reason| match reason {
                CtrlMsg::NewMessage(msg) => msg,
                _ => unreachable!(),
            })
    }
}

impl<A> WaitingReceiver<A> {
    pub fn new() -> (WaitingReceiver<A>, Handle<A>) {
        let (sender, receiver) = catty::oneshot();

        (WaitingReceiver(receiver), Handle(sender))
    }

    /// Cancel this [`WaitingReceiver`].
    ///
    /// This may return a message in case the underlying state has been fulfilled with one but this
    /// receiver has not been polled since.
    ///
    /// It is important to call this message over just dropping the [`WaitingReceiver`] as this
    /// message would otherwise be dropped.
    pub fn cancel(&mut self) -> Option<MessageToOne<A>> {
        match self.0.try_recv() {
            Ok(Some(CtrlMsg::NewMessage(msg))) => Some(msg),
            _ => None,
        }
    }

    /// Tell the [`WaitingReceiver`] to make progress.
    ///
    /// In case we have been woken with a reason, we will attempt to produce an [`ActorMessage`].
    /// Wake-ups can be false-positives in the case of a broadcast message which is why this
    /// function returns only [`Option<ActorMessage>`].
    pub fn poll(
        &mut self,
        chan: &ChanPtr<A, Rx>,
        broadcast_mailbox: &BroadcastQueue<A>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<ActorMessage<A>>> {
        let ctrl_msg = match futures_util::ready!(self.0.poll_unpin(cx)) {
            Ok(reason) => reason,
            Err(_) => return Poll::Ready(None), // TODO: Not sure if this is correct.
        };

        let actor_message = match ctrl_msg {
            CtrlMsg::NewMessage(msg) => ActorMessage::ToOneActor(msg),
            CtrlMsg::Shutdown => ActorMessage::Shutdown,
            CtrlMsg::NewBroadcast => match chan.next_broadcast_message(broadcast_mailbox) {
                Some(msg) => ActorMessage::ToAllActors(msg),
                None => return Poll::Ready(None),
            },
        };

        Poll::Ready(Some(actor_message))
    }
}

enum CtrlMsg<A> {
    NewMessage(MessageToOne<A>),
    NewBroadcast,
    Shutdown,
}
