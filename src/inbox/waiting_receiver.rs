use std::sync::{Arc, Weak};
use std::task::{Context, Poll, Waker};

use crate::envelope::MessageEnvelope;
use crate::inbox::rx::Receiver;
use crate::inbox::rx::RxRefCounter;
use crate::inbox::ActorMessage;

/// A [`WaitingReceiver`] is handed out by the channel any time [`Chan::try_recv`] is called on an empty mailbox.
///
/// [`WaitingReceiver`] implements [`Future`] which will resolve once a message lands in the mailbox.
pub struct WaitingReceiver<A> {
    state: Arc<spin::Mutex<WaitingState<A>>>,
}

/// A [`FulfillHandle`] is the counter-part to a [`WaitingReceiver`].
///
/// It is stored internally in the channel and used by the channel implementation to notify a
/// [`WaitingReceiver`] once a new message hits the mailbox.
pub struct FulfillHandle<A> {
    state: Weak<spin::Mutex<WaitingState<A>>>,
}

impl<A> FulfillHandle<A> {
    /// Shutdown the connected [`WaitingReceiver`].
    ///
    /// This will wake the corresponding task and notify them that the channel is shutting down.
    pub fn shutdown(&self) {
        let _ = self.fulfill(WakeReason::Shutdown);
    }

    /// Notify the connected [`WaitingReceiver`] about a new broadcast message.
    ///
    /// Broadcast mailboxes are stored outside of the main channel implementation which is why this
    /// messages does not take any arguments.
    pub fn fulfill_message_to_all_actors(&self) -> Result<(), ()> {
        match self.fulfill(WakeReason::MessageToAllActors) {
            Ok(()) => Ok(()),
            Err(WakeReason::MessageToAllActors) => Err(()),
            _ => unreachable!(),
        }
    }

    /// Notify the connected [`WaitingReceiver`] about a new message.
    ///
    /// A new message was sent into the channel and the connected [`WaitingReceiver`] is the chosen
    /// one to handle it, likely because it has been waiting the longest.
    ///
    /// This function will return the message in an `Err` if the [`WaitingReceiver`] has since called
    /// [`cancel`](WaitingReceiver::cancel) and is therefore unable to handle the message.
    pub fn fulfill_message_to_one_actor(
        &self,
        msg: Box<dyn MessageEnvelope<Actor = A>>,
    ) -> Result<(), Box<dyn MessageEnvelope<Actor = A>>> {
        match self.fulfill(WakeReason::MessageToOneActor(msg)) {
            Ok(()) => Ok(()),
            Err(WakeReason::MessageToOneActor(msg)) => Err(msg),
            _ => unreachable!(),
        }
    }

    fn fulfill(&self, reason: WakeReason<A>) -> Result<(), WakeReason<A>> {
        let this = match self.state.upgrade() {
            Some(this) => this,
            None => return Err(reason),
        };

        let mut this = this.lock();

        if this.cancelled {
            return Err(reason);
        }

        match this.wake_reason {
            Some(_) => unreachable!("Waiting receiver was fulfilled but not popped!"),
            None => this.wake_reason = Some(reason),
        }

        if let Some(waker) = this.waker.take() {
            waker.wake();
        }

        Ok(())
    }
}

impl<A> WaitingReceiver<A> {
    pub fn new() -> (WaitingReceiver<A>, FulfillHandle<A>) {
        let state = Arc::new(spin::Mutex::new(WaitingState::default()));

        let handle = FulfillHandle {
            state: Arc::downgrade(&state),
        };
        let receiver = WaitingReceiver { state };

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
        let mut state = self.state.lock();

        state.cancelled = true;

        match state.wake_reason.take()? {
            WakeReason::MessageToOneActor(msg) => Some(msg),
            _ => None,
        }
    }

    pub fn poll<Rc>(
        &self,
        receiver: &Receiver<A, Rc>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<ActorMessage<A>>>
    where
        Rc: RxRefCounter,
    {
        let mut this = self.state.lock();

        let reason = match this.wake_reason.take() {
            Some(reason) => reason,
            None => {
                this.waker = Some(cx.waker().clone());

                return Poll::Pending;
            }
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

struct WaitingState<A> {
    waker: Option<Waker>,
    wake_reason: Option<WakeReason<A>>,
    cancelled: bool,
}

impl<A> Default for WaitingState<A> {
    fn default() -> Self {
        WaitingState {
            waker: None,
            wake_reason: None,
            cancelled: false,
        }
    }
}

enum WakeReason<A> {
    MessageToOneActor(Box<dyn MessageEnvelope<Actor = A>>),
    // should be fetched from own receiver
    MessageToAllActors,
    Shutdown,
}
