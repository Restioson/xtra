use crate::envelope::{
    AsyncNonReturningEnvelope, AsyncReturningEnvelope, SyncNonReturningEnvelope,
    SyncReturningEnvelope,
};
use crate::manager::ManagerMessage;
use crate::{Actor, AsyncHandler, Handler, Message};
use futures::channel::mpsc::UnboundedSender;
use futures::channel::oneshot::Receiver;
use futures::task::Poll;
use futures::Future;
use std::pin::Pin;
use std::sync::{Arc, Weak};

pub struct MessageResponseFuture<M: Message>(Receiver<M::Result>);

impl<M: Message> Future for MessageResponseFuture<M> {
    type Output = Result<M::Result, Disconnected>;

    fn poll(self: Pin<&mut Self>, ctx: &mut futures::task::Context) -> Poll<Self::Output> {
        let rx = Pin::new(&mut self.get_mut().0);
        rx.poll(ctx).map(|res| res.map_err(|_| Disconnected))
    }
}

/// General trait for any kind of address, be it strong or weak. This trait contains all functions
/// of the address.
#[doc(spotlight)]
pub trait AddressExt<A: Actor> {
    /// Sends a [`Message`](trait.Message.html) that will be handled synchronously to the actor,
    /// and does not wait for a response. If this returns `Err(Disconnected)`, then the actor is stopped
    /// and not accepting messages. If this returns `Ok(())`, the will be delivered, but may
    /// not be handled in the event that the actor stops itself (by calling [`Context::stop`](struct.Context.html#method.stop))
    /// before it was handled.
    fn do_send<M>(&self, message: M) -> Result<(), Disconnected>
    where
        M: Message,
        A: Handler<M> + Send;

    /// Sends a [`Message`](trait.Message.html) that will be handled asynchronously to the actor,
    /// and does not wait for a response. If this returns `Err(Disconnected)`, then the actor is stopped
    /// and not accepting messages. If this returns `Ok(())`, the will be delivered, but may
    /// not be handled in the event that the actor stops itself (by calling [`Context::stop`](struct.Context.html#method.stop))
    /// before it was handled.
    fn do_send_async<M>(&self, message: M) -> Result<(), Disconnected>
    where
        M: Message,
        A: AsyncHandler<M> + Send;

    /// Sends a [`Message`](trait.Message.html) that will be handled asynchronously to the actor,
    /// and waits for a response. If this returns `Err(Disconnected)`, then the actor is stopped
    /// and not accepting messages.
    fn send<M>(&self, message: M) -> MessageResponseFuture<M>
    where
        M: Message,
        A: Handler<M> + Send,
        M::Result: Send;

    /// Sends a [`Message`](trait.Message.html) that will be handled asynchronously to the actor,
    /// and waits for a response. If this returns `Err(Disconnected)`, then the actor is stopped
    /// and not accepting messages.
    fn send_async<M>(&self, message: M) -> MessageResponseFuture<M>
    where
        M: Message,
        A: AsyncHandler<M> + Send,
        for<'a> A::Responder<'a>: Future<Output = M::Result> + Send;
}

/// An `Address` is a reference to an actor through which [`Message`s](trait.Message.html) can be
/// sent. It can be cloned, and when all `Address`es are dropped, the actor will be stopped. Therefore,
/// any existing `Address`es will inhibit the dropping of an actor. If this is undesirable, then
/// the [`WeakAddress`](struct.WeakAddress.html) struct should be used instead. This struct is created
/// by calling the [`Actor::start`](trait.Actor.html#method.start) or  [`Actor::spawn`](trait.Actor.html#method.start)
/// methods.
pub struct Address<A: Actor> {
    pub(crate) sender: UnboundedSender<ManagerMessage<A>>,
    pub(crate) ref_counter: Arc<()>,
}

impl<A: Actor> Address<A> {
    /// Create a weak address to the actor. Unlike with the strong variety of address (this kind),
    /// an actor will not be prevented from being dropped if only weak addresses exist.
    pub fn weak(&self) -> WeakAddress<A> {
        WeakAddress {
            sender: self.sender.clone(),
            ref_counter: Arc::downgrade(&self.ref_counter),
        }
    }
}

impl<A: Actor> AddressExt<A> for Address<A> {
    // To read more about what an envelope is and why we use them, look under `envelope.rs`

    fn do_send<M>(&self, message: M) -> Result<(), Disconnected>
    where
        M: Message,
        A: Handler<M> + Send,
    {
        let envelope = SyncNonReturningEnvelope::new(message);
        self.sender
            .unbounded_send(ManagerMessage::Message(Box::new(envelope)))
            .map_err(|_| Disconnected)
    }

    fn do_send_async<M>(&self, message: M) -> Result<(), Disconnected>
    where
        M: Message,
        A: AsyncHandler<M> + Send,
    {
        let envelope = AsyncNonReturningEnvelope::new(message);
        self.sender
            .unbounded_send(ManagerMessage::Message(Box::new(envelope)))
            .map_err(|_| Disconnected)
    }

    fn send<M>(&self, message: M) -> MessageResponseFuture<M>
    where
        M: Message,
        A: Handler<M> + Send,
        M::Result: Send,
    {
        let (envelope, rx) = SyncReturningEnvelope::new(message);
        let _ = self
            .sender
            .unbounded_send(ManagerMessage::Message(Box::new(envelope)));
        MessageResponseFuture(rx)
    }

    fn send_async<M>(&self, message: M) -> MessageResponseFuture<M>
    where
        M: Message,
        A: AsyncHandler<M> + Send,
        for<'a> A::Responder<'a>: Future<Output = M::Result> + Send,
    {
        let (envelope, rx) = AsyncReturningEnvelope::new(message);
        let _ = self
            .sender
            .unbounded_send(ManagerMessage::Message(Box::new(envelope)));
        MessageResponseFuture(rx)
    }
}

// Required because #[derive] adds an A: Clone bound
impl<A: Actor> Clone for Address<A> {
    fn clone(&self) -> Self {
        Address {
            sender: self.sender.clone(),
            ref_counter: self.ref_counter.clone(),
        }
    }
}

impl<A: Actor> Drop for Address<A> {
    fn drop(&mut self) {
        // Context holds one strong address (for `Context::address`) and so does ActorManager, so if
        // there are 3 strong addresses, this would be the only external one in existence. Therefore, we
        // should notify the ActorManager that there are potentially no more strong Addresses and the
        // actor should be stopped.
        if Arc::strong_count(&self.ref_counter) == 3 {
            let _ = self.sender.unbounded_send(ManagerMessage::LastAddress);
        }
    }
}

/// A `WeakAddress` is a reference to an actor through which [`Message`s](trait.Message.html) can be
/// sent. It can be cloned. Unlike [`Address`](struct.Address.html), a `WeakAddress` will not inhibit
/// the dropping of an actor. It is created by the [`Address::weak`](struct.Address.html) method.
pub struct WeakAddress<A: Actor> {
    pub(crate) sender: UnboundedSender<ManagerMessage<A>>,
    ref_counter: Weak<()>,
}

impl<A: Actor> AddressExt<A> for WeakAddress<A> {
    fn do_send<M>(&self, message: M) -> Result<(), Disconnected>
    where
        M: Message,
        A: Handler<M> + Send,
    {
        let envelope = SyncNonReturningEnvelope::new(message);
        self.sender
            .unbounded_send(ManagerMessage::Message(Box::new(envelope)))
            .map_err(|_| Disconnected)
    }

    fn do_send_async<M>(&self, message: M) -> Result<(), Disconnected>
    where
        M: Message,
        A: AsyncHandler<M> + Send,
    {
        let envelope = AsyncNonReturningEnvelope::new(message);
        self.sender
            .unbounded_send(ManagerMessage::Message(Box::new(envelope)))
            .map_err(|_| Disconnected)
    }

    fn send<M>(&self, message: M) -> MessageResponseFuture<M>
    where
        M: Message,
        A: Handler<M> + Send,
        M::Result: Send,
    {
        let (envelope, rx) = SyncReturningEnvelope::new(message);
        let _ = self
            .sender
            .unbounded_send(ManagerMessage::Message(Box::new(envelope)));
        MessageResponseFuture(rx)
    }

    fn send_async<M>(&self, message: M) -> MessageResponseFuture<M>
    where
        M: Message,
        A: AsyncHandler<M> + Send,
        for<'a> A::Responder<'a>: Future<Output = M::Result> + Send,
    {
        let (envelope, rx) = AsyncReturningEnvelope::new(message);
        let _ = self
            .sender
            .unbounded_send(ManagerMessage::Message(Box::new(envelope)));
        MessageResponseFuture(rx)
    }
}

// Required because #[derive] adds an A: Clone bound
impl<A: Actor> Clone for WeakAddress<A> {
    fn clone(&self) -> Self {
        WeakAddress {
            sender: self.sender.clone(),
            ref_counter: self.ref_counter.clone(),
        }
    }
}

/// The actor is no longer running and disconnected from the sending address. For why this could
/// occur, see the [`Actor::stopping`](trait.Actor.html#method.stopping) and
/// [`Actor::stopped`](trait.Actor.html#method.stopped) methods.
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct Disconnected;
