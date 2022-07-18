use std::future::Future;
use std::marker::PhantomData;
use std::ops::ControlFlow;
use std::sync::Arc;

use catty::{Receiver, Sender};
use futures_core::future::BoxFuture;
use futures_util::FutureExt;

use crate::context::Context;
use crate::inbox::{HasPriority, Priority};
use crate::{Actor, Handler};

/// A message envelope is a struct that encapsulates a message and its return channel sender (if applicable).
/// Firstly, this allows us to be generic over returning and non-returning messages (as all use the
/// same `handle` method and return the same pinned & boxed future), but almost more importantly it
/// allows us to erase the type of the message when this is in dyn Trait format, thereby being able to
/// use only one channel to send all the kinds of messages that the actor can receives. This does,
/// however, induce a bit of allocation (as envelopes have to be boxed).
pub trait MessageEnvelope: HasPriority + Send {
    /// The type of actor that this envelope carries a message for
    type Actor;

    fn set_priority(&mut self, new_priority: u32);

    /// Starts the instrumentation of this message request. This will create the request span.
    fn start_span(&mut self, msg_name: &'static str);

    /// Handle the message inside of the box by calling the relevant [`Handler::handle`] method,
    /// returning its result over a return channel if applicable. This also takes `Box<Self>` as the
    /// `self` parameter because `Envelope`s always appear as `Box<dyn Envelope<Actor = ...>>`,
    /// and this allows us to consume the envelope.
    fn handle<'a>(
        self: Box<Self>,
        act: &'a mut Self::Actor,
        ctx: &'a mut Context<Self::Actor>,
    ) -> (BoxFuture<'a, ControlFlow<()>>, Span);
}

#[cfg_attr(not(feature = "instrumentation"), allow(dead_code))]
#[derive(Clone)]
struct Instrumentation {
    parent: Span,
    _waiting_for_actor: Span,
}

#[derive(Clone)]
pub struct Span(#[cfg(feature = "instrumentation")] pub tracing::Span);

impl Span {
    pub fn in_scope<R>(&self, f: impl FnOnce() -> R) -> R {
        #[cfg(feature = "instrumentation")]
        let r = self.0.in_scope(f);

        #[cfg(not(feature = "instrumentation"))]
        let r = f();

        r
    }

    fn none() -> Span {
        #[cfg(feature = "instrumentation")]
        let span = Span(tracing::Span::none());

        #[cfg(not(feature = "instrumentation"))]
        let span = Span();

        span
    }

    fn is_none(&self) -> bool {
        #[cfg(feature = "instrumentation")]
        let none = self.0.is_none();

        #[cfg(not(feature = "instrumentation"))]
        let none = true;

        none
    }
}

impl Instrumentation {
    fn empty() -> Self {
        Instrumentation {
            parent: Span::none(),
            _waiting_for_actor: Span::none(),
        }
    }

    #[cfg_attr(not(feature = "instrumentation"), allow(unused_variables))]
    fn started<A>(message: &'static str) -> Self {
        #[cfg(feature = "instrumentation")]
        {
            let parent = Span(tracing::debug_span!(
                "xtra_actor_request",
                actor = std::any::type_name::<A>(),
                %message,
            ));

            let _waiting_for_actor = Span(tracing::debug_span!(
                parent: &parent.0,
                "xtra_message_waiting_for_actor",
                actor = std::any::type_name::<A>(),
                %message,
            ));

            Instrumentation {
                parent,
                _waiting_for_actor,
            }
        }

        #[cfg(not(feature = "instrumentation"))]
        Instrumentation::empty()
    }

    fn apply<A, M, F>(self, fut: F) -> (impl Future<Output = F::Output>, Span)
    where
        F: Future,
    {
        #[cfg(feature = "instrumentation")]
        {
            let executing = tracing::debug_span!(
                parent: &self.parent.0,
                "xtra_message_handler",
                actor = std::any::type_name::<A>(),
                message = std::any::type_name::<M>(),
                interrupted = tracing::field::Empty,
            );

            (
                tracing::Instrument::instrument(fut, executing.clone()),
                Span(executing),
            )
        }

        #[cfg(not(feature = "instrumentation"))]
        (fut, Span())
    }
}

/// An envelope that returns a result from a message. Constructed by the `AddressExt::do_send` method.
pub struct ReturningEnvelope<A, M, R> {
    message: M,
    result_sender: Sender<R>,
    priority: u32,
    phantom: PhantomData<for<'a> fn(&'a A)>,
    instrumentation: Instrumentation,
}

impl<A, M, R: Send + 'static> ReturningEnvelope<A, M, R> {
    pub fn new(message: M, priority: u32) -> (Self, Receiver<R>) {
        let (tx, rx) = catty::oneshot();
        let envelope = ReturningEnvelope {
            message,
            result_sender: tx,
            priority,
            phantom: PhantomData,
            instrumentation: Instrumentation::empty(),
        };

        (envelope, rx)
    }
}

impl<A, M, R> HasPriority for ReturningEnvelope<A, M, R> {
    fn priority(&self) -> Priority {
        Priority::Valued(self.priority)
    }
}

impl<A> HasPriority for Box<dyn MessageEnvelope<Actor = A>> {
    fn priority(&self) -> Priority {
        self.as_ref().priority()
    }
}

impl<A, M, R> MessageEnvelope for ReturningEnvelope<A, M, R>
where
    A: Handler<M, Return = R>,
    M: Send + 'static,
    R: Send + 'static,
{
    type Actor = A;

    fn set_priority(&mut self, new_priority: u32) {
        self.priority = new_priority;
    }

    fn start_span(&mut self, msg_name: &'static str) {
        assert!(self.instrumentation.parent.is_none());
        self.instrumentation = Instrumentation::started::<A>(msg_name);
    }

    fn handle<'a>(
        self: Box<Self>,
        act: &'a mut Self::Actor,
        ctx: &'a mut Context<Self::Actor>,
    ) -> (BoxFuture<'a, ControlFlow<()>>, Span) {
        let Self {
            message,
            result_sender,
            instrumentation,
            ..
        } = *self;

        let fut = async move { (act.handle(message, ctx).await, ctx.flow()) };

        let (fut, span) = instrumentation.apply::<A, M, _>(fut);

        let fut = Box::pin(fut.map(move |(r, flow)| {
            // We don't actually care if the receiver is listening
            let _ = result_sender.send(r);
            flow
        }));

        (fut, span)
    }
}

/// Like MessageEnvelope, but with an Arc instead of Box
pub trait BroadcastEnvelope: HasPriority + Send + Sync {
    type Actor;

    fn set_priority(&mut self, new_priority: u32);

    /// Starts the instrumentation of this message request, if this arc is unique. This will create
    /// the request span
    fn start_span(&mut self, msg_name: &'static str);

    fn handle<'a>(
        self: Arc<Self>,
        act: &'a mut Self::Actor,
        ctx: &'a mut Context<Self::Actor>,
    ) -> (BoxFuture<'a, ControlFlow<()>>, Span);
}

impl<A> HasPriority for Arc<dyn BroadcastEnvelope<Actor = A>> {
    fn priority(&self) -> Priority {
        self.as_ref().priority()
    }
}

pub struct BroadcastEnvelopeConcrete<A, M> {
    message: M,
    pub priority: u32,
    phantom: PhantomData<for<'a> fn(&'a A)>,
    instrumentation: Instrumentation,
}

impl<A: Actor, M> BroadcastEnvelopeConcrete<A, M> {
    pub fn new(message: M, priority: u32) -> Self {
        BroadcastEnvelopeConcrete {
            message,
            priority,
            phantom: PhantomData,
            instrumentation: Instrumentation::empty(),
        }
    }
}

impl<A, M> BroadcastEnvelope for BroadcastEnvelopeConcrete<A, M>
where
    A: Handler<M, Return = ()>,
    M: Clone + Send + Sync + 'static,
{
    type Actor = A;

    fn set_priority(&mut self, new_priority: u32) {
        self.priority = new_priority;
    }

    fn start_span(&mut self, msg_name: &'static str) {
        assert!(self.instrumentation.parent.is_none());
        self.instrumentation = Instrumentation::started::<A>(msg_name);
    }

    fn handle<'a>(
        self: Arc<Self>,
        act: &'a mut Self::Actor,
        ctx: &'a mut Context<Self::Actor>,
    ) -> (BoxFuture<'a, ControlFlow<()>>, Span) {
        let (msg, instrumentation) = (self.message.clone(), self.instrumentation.clone());
        drop(self); // Drop ASAP to end the message waiting for actor span
        let fut = async move {
            act.handle(msg, ctx).await;
            ctx.flow()
        };
        let (fut, span) = instrumentation.apply::<A, M, _>(fut);
        (Box::pin(fut), span)
    }
}

impl<A, M> HasPriority for BroadcastEnvelopeConcrete<A, M> {
    fn priority(&self) -> Priority {
        Priority::Valued(self.priority)
    }
}

#[derive(Copy, Clone, Default)]
pub struct Shutdown<A>(PhantomData<for<'a> fn(&'a A)>);

impl<A> Shutdown<A> {
    pub fn new() -> Self {
        Shutdown(PhantomData)
    }

    pub fn handle(ctx: &mut Context<A>) -> (BoxFuture<ControlFlow<()>>, Span) {
        let fut = Box::pin(async {
            ctx.running = false;
            ControlFlow::Break(())
        });

        (fut, Span::none())
    }
}

impl<A> HasPriority for Shutdown<A> {
    fn priority(&self) -> Priority {
        Priority::Shutdown
    }
}

impl<A> BroadcastEnvelope for Shutdown<A>
where
    A: Actor,
{
    type Actor = A;

    fn set_priority(&mut self, _: u32) {}

    // This message is not instrumented
    fn start_span(&mut self, _msg_name: &'static str) {}

    fn handle<'a>(
        self: Arc<Self>,
        _act: &'a mut Self::Actor,
        ctx: &'a mut Context<Self::Actor>,
    ) -> (BoxFuture<'a, ControlFlow<()>>, Span) {
        Self::handle(ctx)
    }
}
