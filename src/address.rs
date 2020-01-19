use crate::envelope::{Envelope, NonReturningEnvelope, SyncReturningEnvelope};
use crate::{Actor, Handler, Message, SyncResponder};
use futures::channel::mpsc::UnboundedSender;
use futures::future::Either;
use futures::{Future, TryFutureExt};

/// An `Address` is a reference to an actor through which [`Message`](struct.Message.html)s can be
/// sent. It can be cloned, and when all `Address`es are dropped, the actor will be stopped. It is
/// created by calling the [`Actor::start`](trait.Actor.html#method.start) or
/// [`Actor::spawn`](trait.Actor.html#method.start) methods.
#[derive(Clone)]
pub struct Address<A: Actor> {
    pub(crate) sender: UnboundedSender<Box<dyn Envelope<Actor = A>>>,
}

impl<A: Actor> Address<A> {
    pub fn do_send<M>(&self, message: M) -> Result<(), Disconnected>
    where
        M: Message,
        A: Handler<M>,
    {
        let envelope = NonReturningEnvelope::new(message);
        self.sender
            .unbounded_send(Box::new(envelope))
            .map_err(|_| Disconnected)
    }

    // TODO async
    pub fn send<M>(&self, message: M) -> impl Future<Output = Result<M::Result, Disconnected>>
    where
        M: Message,
        A: Handler<M>,
        A::Responder: SyncResponder<M> + Send,
    {
        let (envelope, rx) = SyncReturningEnvelope::new(message);

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
