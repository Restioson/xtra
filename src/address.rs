use crate::envelope::{
    AsyncNonReturningEnvelope, AsyncReturningEnvelope, Envelope, SyncNonReturningEnvelope,
    SyncReturningEnvelope,
};
use crate::{Actor, AsyncHandler, Handler, Message};
use futures::channel::mpsc::UnboundedSender;
use futures::future::Either;
use futures::{Future, TryFutureExt};

/// An `Address` is a reference to an actor through which [`Message`s](trait.Message.html) can be
/// sent. It can be cloned, and when all `Address`es are dropped, the actor will be stopped. It is
/// created by calling the [`Actor::start`](trait.Actor.html#method.start) or
/// [`Actor::spawn`](trait.Actor.html#method.start) methods.
#[derive(Clone)]
pub struct Address<A: Actor> {
    pub(crate) sender: UnboundedSender<Box<dyn Envelope<Actor = A>>>,
}

impl<A: Actor> Address<A> {
    /// Sends a [`Message`](trait.Message.html) that will be handled synchronously to the actor,
    /// and does not wait for a response. If this returns `Err(Disconnected)`, then the actor is stopped
    /// and not accepting messages. If this returns `Ok(())`, the will be delivered, but may
    /// not be handled in the event that the actor stops itself (by calling [`Context::stop`](struct.Context.html#method.stop))
    /// before it was handled.
    pub fn do_send<M>(&self, message: M) -> Result<(), Disconnected>
    where
        M: Message,
        A: Handler<M> + Send,
    {
        let envelope = SyncNonReturningEnvelope::new(message);
        self.sender
            .unbounded_send(Box::new(envelope))
            .map_err(|_| Disconnected)
    }

    /// Sends a [`Message`](trait.Message.html) that will be handled asynchronously to the actor,
    /// and does not wait for a response. If this returns `Err(Disconnected)`, then the actor is stopped
    /// and not accepting messages. If this returns `Ok(())`, the will be delivered, but may
    /// not be handled in the event that the actor stops itself (by calling [`Context::stop`](struct.Context.html#method.stop))
    /// before it was handled.
    pub fn do_send_async<M>(&self, message: M) -> Result<(), Disconnected>
    where
        M: Message,
        A: AsyncHandler<M> + Send,
    {
        let envelope = AsyncNonReturningEnvelope::new(message);
        self.sender
            .unbounded_send(Box::new(envelope))
            .map_err(|_| Disconnected)
    }

    /// Sends a [`Message`](trait.Message.html) that will be handled asynchronously to the actor,
    /// and waits for a response. If this returns `Err(Disconnected)`, then the actor is stopped
    /// and not accepting messages.
    pub fn send<M>(&self, message: M) -> impl Future<Output = Result<M::Result, Disconnected>>
    where
        M: Message,
        A: Handler<M> + Send,
        M::Result: Send,
    {
        let t = SyncReturningEnvelope::new(message);
        let envelope: SyncReturningEnvelope<A, M> = t.0;
        let rx = t.1;

        let res = self
            .sender
            .unbounded_send(Box::new(envelope))
            .map_err(|_| Disconnected);

        match res {
            Ok(()) => Either::Left(rx.map_err(|_| Disconnected)),
            Err(e) => Either::Right(futures::future::err(e)),
        }
    }

    /// Sends a [`Message`](trait.Message.html) that will be handled asynchronously to the actor,
    /// and waits for a response. If this returns `Err(Disconnected)`, then the actor is stopped
    /// and not accepting messages.
    pub fn send_async<M>(&self, message: M) -> impl Future<Output = Result<M::Result, Disconnected>>
    where
        M: Message,
        A: AsyncHandler<M> + Send,
        for<'a> A::Responder<'a>: Future<Output = M::Result> + Send,
    {
        let t = AsyncReturningEnvelope::new(message);
        let envelope: AsyncReturningEnvelope<A, M> = t.0;
        let rx = t.1;

        let res = self
            .sender
            .unbounded_send(Box::new(envelope))
            .map_err(|_| Disconnected);

        match res {
            Ok(()) => Either::Left(rx.map_err(|_| Disconnected)),
            Err(e) => Either::Right(futures::future::err(e)),
        }
    }
}

/// The actor is no longer running and disconnected
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct Disconnected;
