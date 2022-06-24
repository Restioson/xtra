use std::future::Future;
use std::marker::PhantomData;
use std::sync::Arc;

use catty::{Receiver, Sender};
use futures_core::future::BoxFuture;
use futures_util::FutureExt;

use crate::context::Context;
use crate::inbox::HasPriority;
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

#[derive(Clone)]
struct Instrumentation {
    #[cfg(feature = "instrumentation")]
    parent: tracing::Span,
    #[cfg(feature = "instrumentation")]
    _waiting_for_actor: tracing::Span,
}

impl Instrumentation {
    fn new<A, M>() -> Self {
        #[cfg(feature = "instrumentation")]
        {
            let parent = tracing::debug_span!(
                parent: tracing::Span::current(),
                "xtra_actor_request",
                actor = std::any::type_name::<A>(),
                message = std::any::type_name::<M>(),
            );

            let waiting_for_actor = tracing::debug_span!(
                parent: &parent,
                "xtra_message_waiting_for_actor",
                actor = std::any::type_name::<A>(),
                message = std::any::type_name::<M>(),
            );

            Instrumentation {
                parent,
                _waiting_for_actor: waiting_for_actor,
            }
        }

        #[cfg(not(feature = "instrumentation"))]
        Instrumentation {}
    }

    fn apply<A, M, F>(self, fut: F) -> impl Future<Output = F::Output>
    where
        F: Future,
    {
        #[cfg(feature = "instrumentation")]
        {
            let executing = tracing::debug_span!(
                parent: &self.parent,
                "xtra_message_handler",
                actor = std::any::type_name::<A>(),
                message = std::any::type_name::<M>(),
            );

            tracing::Instrument::instrument(fut, executing)
        }

        #[cfg(not(feature = "instrumentation"))]
        fut
    }
}

/// An envelope that returns a result from a message. Constructed by the `AddressExt::do_send` method.
pub struct ReturningEnvelope<A, M, R> {
    message: M,
    result_sender: Sender<R>,
    phantom: PhantomData<fn() -> A>,
    instrumentation: Instrumentation,
}

impl<A, M, R: Send + 'static> ReturningEnvelope<A, M, R> {
    pub fn new(message: M) -> (Self, Receiver<R>) {
        let (tx, rx) = catty::oneshot();
        let envelope = ReturningEnvelope {
            message,
            result_sender: tx,
            phantom: PhantomData,
            instrumentation: Instrumentation::new::<A, M>(),
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
            instrumentation,
            ..
        } = *self;

        Box::pin(
            instrumentation
                .apply::<A, M, _>(act.handle(message, ctx))
                .map(move |r| {
                    // We don't actually care if the receiver is listening
                    let _ = result_sender.send(r);
                }),
        )
    }
}

/// An envelope that does not return a result from a message. Constructed  by the `AddressExt::do_send`
/// method.
pub struct NonReturningEnvelope<A, M> {
    message: M,
    phantom: PhantomData<fn() -> A>,
    instrumentation: Instrumentation,
}

impl<A: Actor, M> NonReturningEnvelope<A, M> {
    pub fn new(message: M) -> Self {
        NonReturningEnvelope {
            message,
            phantom: PhantomData,
            instrumentation: Instrumentation::new::<A, M>(),
        }
    }
}

impl<A: Handler<M>, M: Send + 'static> MessageEnvelope for NonReturningEnvelope<A, M> {
    type Actor = A;

    fn handle<'a>(
        self: Box<Self>,
        act: &'a mut Self::Actor,
        ctx: &'a mut Context<Self::Actor>,
    ) -> BoxFuture<'a, ()> {
        let Self {
            message,
            instrumentation,
            ..
        } = *self;

        Box::pin(
            instrumentation
                .apply::<A, M, _>(act.handle(message, ctx))
                .map(|_| ()),
        )
    }
}

/// Like MessageEnvelope, but with an Arc instead of Box
pub trait BroadcastEnvelope: HasPriority + Send + Sync {
    type Actor;

    fn handle<'a>(
        self: Arc<Self>,
        act: &'a mut Self::Actor,
        ctx: &'a mut Context<Self::Actor>,
    ) -> BoxFuture<'a, ()>;
}

pub struct BroadcastEnvelopeConcrete<A, M> {
    message: M,
    priority: u32,
    phantom: PhantomData<fn() -> A>,
    instrumentation: Instrumentation,
}

impl<A: Actor, M> BroadcastEnvelopeConcrete<A, M> {
    pub fn new(message: M, priority: u32) -> Self {
        BroadcastEnvelopeConcrete {
            message,
            priority,
            phantom: PhantomData,
            instrumentation: Instrumentation::new::<A, M>(),
        }
    }
}

impl<A: Handler<M>, M> BroadcastEnvelope for BroadcastEnvelopeConcrete<A, M>
where
    A: Handler<M, Return = ()>,
    M: Clone + Send + Sync + 'static,
{
    type Actor = A;

    fn handle<'a>(
        self: Arc<Self>,
        act: &'a mut Self::Actor,
        ctx: &'a mut Context<Self::Actor>,
    ) -> BoxFuture<'a, ()> {
        let (msg, instrumentation) = (self.message.clone(), self.instrumentation.clone());
        drop(self); // Drop ASAP to end the message waiting for actor span
        Box::pin(instrumentation.apply::<A, M, _>(act.handle(msg, ctx)))
    }
}

impl<A, M> HasPriority for BroadcastEnvelopeConcrete<A, M> {
    fn priority(&self) -> u32 {
        self.priority
    }
}
