use crate::{Actor, AsyncHandler, Context, Handler, Message};
use futures::channel::oneshot::{self, Receiver, Sender};
use futures::{future, Future, FutureExt};
use std::marker::PhantomData;
use std::pin::Pin;

/// The type of future returned by `Envelope::handle`
type Fut<'a> = Pin<Box<dyn Future<Output = ()> + Send + 'a>>;

/// An envelope is a struct that encapsulates a message and its return channel sender (if applicable).
/// Firstly, this allows us to be generic over returning and non-returning messages (as all use the
/// same `handle` method and return the same pinned & boxed future), but almost more importantly it
/// allows us to erase the type of the message when this is in dyn Trait format, thereby being able to
/// use only one channel to send all the kinds of messages that the actor can receives. This does,
/// however, induce a bit of allocation (as envelopes have to be boxed).
pub(crate) trait Envelope: Send {
    /// The type of actor that this envelope carries a message for
    type Actor: Actor;

    /// Handle the message inside of the box by calling the relevant `Handler::handle` or
    /// `AsyncHandler::handle` method, returning its result over a return channel if applicable. The
    /// reason that this returns a future is so that we can propagate any `AsyncHandler` responder
    /// futures upwards and `.await` on them in the manager loop. This also takes `Box<Self>` as the
    /// `self` parameter because `Envelope`s always appear as `Box<dyn Envelope<Actor = ...>>`,
    /// and this allows us to consume the envelope, meaning that we don't have to waste *precious
    /// CPU cycles* on useless option checks.
    ///
    /// # Doesn't the return type induce *Unnecessary Boxing* for synchronous handlers?
    /// To save on boxing for non-asynchronously handled message envelopes, we *could* return some
    /// enum like:
    ///
    /// ```ignore
    /// enum Return<'a> {
    ///     Fut(Fut<'a>),
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
    ) -> Fut<'a>;
}

/// An envelope that returns a result from a synchronously handled message. Constructed
/// by the `AddressExt::do_send` method.
pub(crate) struct SyncReturningEnvelope<A: Actor + Send, M: Message> {
    message: M,
    result_sender: Sender<M::Result>,
    phantom: PhantomData<A>,
}

impl<A: Actor + Send, M: Message> SyncReturningEnvelope<A, M> {
    pub(crate) fn new(message: M) -> (Self, Receiver<M::Result>) {
        let (tx, rx) = oneshot::channel();
        let envelope = SyncReturningEnvelope {
            message,
            result_sender: tx,
            phantom: PhantomData,
        };

        (envelope, rx)
    }
}

impl<A, M: Message> Envelope for SyncReturningEnvelope<A, M>
where
    A: Handler<M> + Send,
    M: Message,
    M::Result: Send,
{
    type Actor = A;

    fn handle<'a>(
        self: Box<Self>,
        act: &'a mut Self::Actor,
        ctx: &'a mut Context<Self::Actor>,
    ) -> Fut<'a> {
        let message_result = act.handle(self.message, ctx);

        // We don't actually care if the receiver is listening
        let _ = self.result_sender.send(message_result);

        Box::pin(future::ready(()))
    }
}

/// An envelope that returns a result from an asynchronously handled message. Constructed
/// by the `AddressExt::send` method.
pub(crate) struct AsyncReturningEnvelope<A: Actor, M: Message> {
    message: M,
    result_sender: Sender<M::Result>,
    phantom: PhantomData<A>,
}

impl<A: Actor, M: Message> AsyncReturningEnvelope<A, M> {
    pub(crate) fn new(message: M) -> (Self, Receiver<M::Result>) {
        let (tx, rx) = oneshot::channel();
        let envelope = AsyncReturningEnvelope {
            message,
            result_sender: tx,
            phantom: PhantomData,
        };

        (envelope, rx)
    }
}

impl<M, A> Envelope for AsyncReturningEnvelope<A, M>
where
    A: AsyncHandler<M> + Send,
    for<'a> A::Responder<'a>: Future<Output = M::Result>,
    M: Message,
{
    type Actor = A;

    fn handle<'a>(
        self: Box<Self>,
        act: &'a mut Self::Actor,
        ctx: &'a mut Context<Self::Actor>,
    ) -> Fut<'a> {
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

/// An envelope that does not return a result from a synchronously handled message. Constructed
/// by the `AddressExt::do_send` method.
pub(crate) struct SyncNonReturningEnvelope<A: Actor, M: Message> {
    message: M,
    phantom: PhantomData<A>,
}

impl<A: Actor, M: Message> SyncNonReturningEnvelope<A, M> {
    pub(crate) fn new(message: M) -> Self {
        SyncNonReturningEnvelope {
            message,
            phantom: PhantomData,
        }
    }
}

impl<A: Handler<M> + Send, M: Message> Envelope for SyncNonReturningEnvelope<A, M> {
    type Actor = A;

    fn handle<'a>(
        self: Box<Self>,
        act: &'a mut Self::Actor,
        ctx: &'a mut Context<Self::Actor>,
    ) -> Fut<'a> {
        act.handle(self.message, ctx);
        Box::pin(future::ready(()))
    }
}

/// An envelope that does not return a result from an asynchronously handled message. Constructed
/// by the `AddressExt::do_send_async` method.
pub(crate) struct AsyncNonReturningEnvelope<A: Actor, M: Message> {
    message: M,
    phantom: PhantomData<A>,
}

impl<A: Actor, M: Message> AsyncNonReturningEnvelope<A, M> {
    pub(crate) fn new(message: M) -> Self {
        AsyncNonReturningEnvelope {
            message,
            phantom: PhantomData,
        }
    }
}

impl<A, M> Envelope for AsyncNonReturningEnvelope<A, M>
where
    A: AsyncHandler<M> + Send,
    for<'a> A::Responder<'a>: Future<Output = M::Result>,
    M: Message,
{
    type Actor = A;

    fn handle<'a>(
        self: Box<Self>,
        act: &'a mut Self::Actor,
        ctx: &'a mut Context<Self::Actor>,
    ) -> Fut<'a> {
        Box::pin(act.handle(self.message, ctx).map(|_| ()))
    }
}
