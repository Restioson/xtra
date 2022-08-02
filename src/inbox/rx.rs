use std::sync::Arc;

use crate::envelope::{BroadcastEnvelope, MessageEnvelope};
use crate::inbox::tx::{TxStrong, TxWeak};
use crate::inbox::{ActorMessage, BroadcastQueue, Chan, Sender, WaitingReceiver};
use crate::recv_future::ReceiveFuture;

pub struct Receiver<A> {
    inner: Arc<Chan<A>>,
    broadcast_mailbox: Arc<BroadcastQueue<A>>,
}

impl<A> Receiver<A> {
    pub fn new(inner: Arc<Chan<A>>) -> Self {
        inner.increment_receiver_count();
        let broadcast_mailbox = inner.new_broadcast_mailbox();

        Receiver {
            inner,
            broadcast_mailbox,
        }
    }

    pub fn receive(&self) -> ReceiveFuture<A> {
        self.inner.increment_receiver_count();

        ReceiveFuture::new(Receiver {
            inner: self.inner.clone(),
            broadcast_mailbox: self.broadcast_mailbox.clone(), // It is important to clone the `Arc` here otherwise the future will read from a new broadcast mailbox.
        })
    }

    pub fn pop_broadcast_message(&self) -> Option<Arc<dyn BroadcastEnvelope<Actor=A>>> {
        self.inner
            .chan
            .lock()
            .unwrap()
            .pop_broadcast(self.broadcast_mailbox.as_ref())
    }

    pub fn sender(&self) -> Option<Sender<A, TxStrong>> {
        Sender::try_new_strong(self.inner.clone())
    }

    pub fn weak_sender(&self) -> Sender<A, TxWeak> {
        Sender::new_weak(self.inner.clone())
    }

    pub fn requeue_message(&self, msg: Box<dyn MessageEnvelope<Actor=A>>) {
        self.inner.requeue_message(msg)
    }

    pub(crate) fn try_recv(&self) -> Result<ActorMessage<A>, WaitingReceiver<A>> {
        self.inner.try_recv(self.broadcast_mailbox.as_ref())
    }
}

impl<A> Clone for Receiver<A> {
    fn clone(&self) -> Self {
        self.inner.increment_receiver_count();

        Receiver {
            inner: self.inner.clone(),
            broadcast_mailbox: self.inner.new_broadcast_mailbox(),
        }
    }
}

impl<A> Drop for Receiver<A> {
    fn drop(&mut self) {
        self.inner.decrement_receiver_count()
    }
}
