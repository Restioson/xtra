use crate::address::MessageResponseFuture;
use crate::*;
use futures::channel::oneshot::{self, Receiver, Sender};
#[cfg(feature = "nightly")]
use futures::future;
use futures::{Future, FutureExt, Sink};
use std::marker::PhantomData;
use std::pin::Pin;

/// The type of future returned by `Envelope::handle`
type Fut<'a> = Pin<Box<dyn Future<Output = ()> + Send + 'a>>;

/// A message envelope is a struct that encapsulates a message and its return channel sender (if applicable).
/// Firstly, this allows us to be generic over returning and non-returning messages (as all use the
/// same `handle` method and return the same pinned & boxed future), but almost more importantly it
/// allows us to erase the type of the message when this is in dyn Trait format, thereby being able to
/// use only one channel to send all the kinds of messages that the actor can receives. This does,
/// however, induce a bit of allocation (as envelopes have to be boxed).
pub(crate) trait MessageEnvelope: Send {
    /// The type of actor that this envelope carries a message for
    type Actor: Actor;

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

/// An envelope that returns a result from a message. Constructed by the `AddressExt::do_send` method.
pub(crate) struct ReturningEnvelope<A: Actor + Send, M: Message> {
    message: M,
    result_sender: Sender<M::Result>,
    phantom: PhantomData<A>,
}

impl<A: Actor + Send, M: Message> ReturningEnvelope<A, M> {
    pub(crate) fn new(message: M) -> (Self, Receiver<M::Result>) {
        let (tx, rx) = oneshot::channel();
        let envelope = ReturningEnvelope {
            message,
            result_sender: tx,
            phantom: PhantomData,
        };

        (envelope, rx)
    }
}

#[cfg(not(feature = "nightly"))]
impl<A: Handler<M> + Send, M: Message> MessageEnvelope for ReturningEnvelope<A, M> {
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

#[cfg(feature = "nightly")]
impl<A: Handler<M> + Send, M: Message> MessageEnvelope for ReturningEnvelope<A, M>
where
    for<'a> A::Responder<'a>: Future<Output = M::Result>,
{
    type Actor = A;

    default fn handle<'a>(
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

#[cfg(feature = "nightly")]
impl<A, M> MessageEnvelope for ReturningEnvelope<A, M>
where
    A: Handler<M> + SyncHandler<M> + Send,
    M: Message,
    M::Result: Send,
{
    fn handle<'a>(
        self: Box<Self>,
        act: &'a mut Self::Actor,
        ctx: &'a mut Context<Self::Actor>,
    ) -> Fut<'a> {
        let message_result = SyncHandler::handle(act, self.message, ctx);

        // We don't actually care if the receiver is listening
        let _ = self.result_sender.send(message_result);

        Box::pin(future::ready(()))
    }
}

/// An envelope that does not return a result from a message. Constructed  by the `AddressExt::do_send`
/// method.
pub(crate) struct NonReturningEnvelope<A: Actor, M: Message> {
    message: M,
    phantom: PhantomData<A>,
}

impl<A: Actor, M: Message> NonReturningEnvelope<A, M> {
    pub(crate) fn new(message: M) -> Self {
        NonReturningEnvelope {
            message,
            phantom: PhantomData,
        }
    }
}

#[cfg(not(feature = "nightly"))]
impl<A: Handler<M> + Send, M: Message> MessageEnvelope for NonReturningEnvelope<A, M> {
    type Actor = A;

    fn handle<'a>(
        self: Box<Self>,
        act: &'a mut Self::Actor,
        ctx: &'a mut Context<Self::Actor>,
    ) -> Fut<'a> {
        Box::pin(act.handle(self.message, ctx).map(|_| ()))
    }
}

#[cfg(feature = "nightly")]
impl<A: Handler<M> + Send, M: Message> MessageEnvelope for NonReturningEnvelope<A, M>
where
    for<'a> A::Responder<'a>: Future<Output = M::Result>,
{
    type Actor = A;

    default fn handle<'a>(
        self: Box<Self>,
        act: &'a mut Self::Actor,
        ctx: &'a mut Context<Self::Actor>,
    ) -> Fut<'a> {
        Box::pin(act.handle(self.message, ctx).map(|_| ()))
    }
}

#[cfg(feature = "nightly")]
impl<A, M> MessageEnvelope for NonReturningEnvelope<A, M>
where
    A: Handler<M> + SyncHandler<M> + Send,
    M: Message,
{
    fn handle<'a>(
        self: Box<Self>,
        act: &'a mut Self::Actor,
        ctx: &'a mut Context<Self::Actor>,
    ) -> Fut<'a> {
        SyncHandler::handle(act, self.message, ctx);
        Box::pin(future::ready(()))
    }
}

/// Similar to `MessageEnvelope`, but used to erase the type of the actor instead of the channel.
/// This is used in `message_channel.rs`. All of its methods map to an equivalent method in
/// `Address` or `AddressExt`
pub(crate) trait AddressEnvelope<M: Message>:
    Sink<M, Error = Disconnected> + Unpin + Send
{
    fn is_connected(&self) -> bool;
    fn do_send(&self, message: M) -> Result<(), Disconnected>;
    fn send(&self, message: M) -> MessageResponseFuture<M>;

    /// It is an error for this method to be called on an already weak address
    fn downgrade(&self) -> Box<dyn AddressEnvelope<M>>;
}

impl<A, M> AddressEnvelope<M> for Address<A>
where
    A: Handler<M> + Send,
    M: Message,
{
    fn is_connected(&self) -> bool {
        AddressExt::is_connected(self)
    }

    fn do_send(&self, message: M) -> Result<(), Disconnected> {
        AddressExt::do_send(self, message)
    }

    fn send(&self, message: M) -> MessageResponseFuture<M> {
        AddressExt::send(self, message)
    }

    fn downgrade(&self) -> Box<dyn AddressEnvelope<M>> {
        Box::new(Address::downgrade(self))
    }
}

impl<A, M> AddressEnvelope<M> for WeakAddress<A>
where
    A: Handler<M> + Send,
    M: Message,
{
    fn is_connected(&self) -> bool {
        AddressExt::is_connected(self)
    }

    fn do_send(&self, message: M) -> Result<(), Disconnected> {
        AddressExt::do_send(self, message)
    }

    fn send(&self, message: M) -> MessageResponseFuture<M> {
        AddressExt::send(self, message)
    }

    fn downgrade(&self) -> Box<dyn AddressEnvelope<M>> {
        unimplemented!()
    }
}
