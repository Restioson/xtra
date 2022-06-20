use std::marker::PhantomData;

use catty::{Receiver, Sender};
use futures_core::future::BoxFuture;
use futures_util::FutureExt;

use crate::context::Context;
use crate::{Actor, Handler};

/// A message envelope is a struct that encapsulates a message and its return channel sender (if applicable).
/// Firstly, this allows us to be generic over returning and non-returning messages (as all use the
/// same `handle` method and return the same pinned & boxed future), but almost more importantly it
/// allows us to erase the type of the message when this is in dyn Trait format, thereby being able to
/// use only one channel to send all the kinds of messages that the actor can receives. This does,
/// however, induce a bit of allocation (as envelopes have to be boxed).
pub trait MessageEnvelope: Send {
    /// The type of actor that this envelope carries a message for
    type Actor;

    /// Handle the message inside of the box by calling the relevant `AsyncHandler::handle` or
    /// `Handler::handle` method, returning its result over a return channel if applicable. The
    /// reason that this returns a future is so that we can propagate any `Handler` responder
    /// futures upwards and `.await` on them in the manager loop. This also takes `Box<Self>` as the
    /// `self` parameter because `Envelope`s always appear as `Box<dyn Envelope<Actor = ...>>`,
    /// and this allows us to consume the envelope, meaning that we don't have to waste *precious
    /// CPU cycles* on useless option checks.
    ///
    /// # Doesn't the return type induce *Unnecessary Boxing* for synchronous handlers?
    /// To save on boxing for non-asynchronously handled message envelopes, we *could* return some
    /// enum like:
    ///
    /// ```not_a_test
    /// enum Return<'a> {
    ///     Fut(BoxFuture<'a, ()>),
    ///     Noop,
    /// }
    /// ```
    ///
    /// But this is actually about 10% *slower* for `do_send`. I don't know why. Maybe it's something
    /// to do with branch (mis)prediction or compiler optimisation. If you think that you can get
    /// it to be faster, then feel free to open a PR with benchmark results attached to prove it.
    fn handle<'a>(
        self: Box<Self>,
        act: &'a mut Self::Actor,
        ctx: &'a mut Context<Self::Actor>,
    ) -> BoxFuture<'a, ()>;
}

/// An envelope that returns a result from a message. Constructed by the `AddressExt::do_send` method.
pub struct ReturningEnvelope<A, M, R> {
    message: M,
    result_sender: Sender<R>,
    phantom: PhantomData<fn() -> A>,
}

impl<A: Actor, M, R: Send + 'static> ReturningEnvelope<A, M, R> {
    pub fn new(message: M) -> (Self, Receiver<R>) {
        let (tx, rx) = catty::oneshot();
        let envelope = ReturningEnvelope {
            message,
            result_sender: tx,
            phantom: PhantomData,
        };

        (envelope, rx)
    }
}

impl<A: Handler<M, Return = R>, M: Send + 'static, R: Send + 'static> MessageEnvelope
    for ReturningEnvelope<A, M, R>
{
    type Actor = A;

    fn handle<'a>(
        self: Box<Self>,
        act: &'a mut Self::Actor,
        ctx: &'a mut Context<Self::Actor>,
    ) -> BoxFuture<'a, ()> {
        let Self {
            message,
            result_sender,
            ..
        } = *self;
        Box::pin(act.handle(message, ctx).map(move |r| {
            // We don't actually care if the receiver is listening
            let _ = result_sender.send(r);
        }))
    }
}

/// Special-case envelope for handlers which return `()`.
pub struct UnitReturnEnvelope<A, M> {
    message: M,
    phantom: PhantomData<fn() -> A>,
}

impl<A: Actor, M> UnitReturnEnvelope<A, M> {
    pub fn new(message: M) -> Self {
        UnitReturnEnvelope {
            message,
            phantom: PhantomData,
        }
    }
}

impl<A, M: Clone> Clone for UnitReturnEnvelope<A, M> {
    fn clone(&self) -> Self {
        Self {
            message: self.message.clone(),
            phantom: PhantomData::default(),
        }
    }
}

impl<A, M> MessageEnvelope for UnitReturnEnvelope<A, M>
where
    A: Handler<M, Return = ()>,
    M: Send + 'static,
{
    type Actor = A;

    fn handle<'a>(
        self: Box<Self>,
        act: &'a mut Self::Actor,
        ctx: &'a mut Context<Self::Actor>,
    ) -> BoxFuture<'a, ()> {
        Box::pin(act.handle(self.message, ctx))
    }
}

/// A [`BroadcastEnvelope`] is a [`MessageEnvelope`] which can be cloned and thus used several times.
pub trait BroadcastEnvelope: MessageEnvelope + Send + Sync {
    fn clone(&self) -> Box<dyn BroadcastEnvelope<Actor = Self::Actor>>;
}

impl<A, M> BroadcastEnvelope for UnitReturnEnvelope<A, M>
where
    M: Clone + Send + Sync + 'static,
    A: Handler<M, Return = ()>,
{
    fn clone(&self) -> Box<dyn BroadcastEnvelope<Actor = Self::Actor>> {
        Box::new(<UnitReturnEnvelope<A, M> as Clone>::clone(self))
    }
}
