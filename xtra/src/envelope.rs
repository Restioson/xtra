use std::marker::PhantomData;
use std::ops::ControlFlow;
use std::sync::Arc;

use catty::{Receiver, Sender};
use futures_core::future::BoxFuture;
use futures_util::FutureExt;

use crate::chan::{HasPriority, MessageToAll, MessageToOne, Priority};
use crate::context::Context;
use crate::instrumentation::{Instrumentation, Span};
use crate::{Actor, Handler, Mailbox};

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
    fn start_span(&mut self);

    /// Handle the message inside of the box by calling the relevant [`Handler::handle`] method,
    /// returning its result over a return channel if applicable. This also takes `Box<Self>` as the
    /// `self` parameter because `Envelope`s always appear as `Box<dyn Envelope<Actor = ...>>`,
    /// and this allows us to consume the envelope.
    fn handle(
        self: Box<Self>,
        act: &mut Self::Actor,
        mailbox: Mailbox<Self::Actor>,
    ) -> (BoxFuture<ControlFlow<(), ()>>, Span);
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

impl<A> HasPriority for MessageToOne<A> {
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

    fn start_span(&mut self) {
        assert!(self.instrumentation.is_parent_none());
        self.instrumentation = Instrumentation::started::<A, M>();
    }

    fn handle(
        self: Box<Self>,
        act: &mut Self::Actor,
        mailbox: Mailbox<Self::Actor>,
    ) -> (BoxFuture<ControlFlow<(), ()>>, Span) {
        let Self {
            message,
            result_sender,
            instrumentation,
            ..
        } = *self;

        let fut = async move {
            let mut ctx = Context {
                running: true,
                mailbox,
            };
            let r = act.handle(message, &mut ctx).await;

            if ctx.running {
                (r, ControlFlow::Continue(()))
            } else {
                (r, ControlFlow::Break(()))
            }
        };

        let (fut, span) = instrumentation.apply::<_>(fut);

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
    fn start_span(&mut self);

    fn handle(
        self: Arc<Self>,
        act: &mut Self::Actor,
        mailbox: Mailbox<Self::Actor>,
    ) -> (BoxFuture<ControlFlow<()>>, Span);
}

impl<A> HasPriority for MessageToAll<A> {
    fn priority(&self) -> Priority {
        self.as_ref().priority()
    }
}

pub struct BroadcastEnvelopeConcrete<A, M> {
    message: M,
    priority: u32,
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

    fn start_span(&mut self) {
        assert!(self.instrumentation.is_parent_none());
        self.instrumentation = Instrumentation::started::<A, M>();
    }

    fn handle(
        self: Arc<Self>,
        act: &mut Self::Actor,
        mailbox: Mailbox<Self::Actor>,
    ) -> (BoxFuture<ControlFlow<(), ()>>, Span) {
        let (msg, instrumentation) = (self.message.clone(), self.instrumentation.clone());
        drop(self); // Drop ASAP to end the message waiting for actor span
        let fut = async move {
            let mut ctx = Context {
                running: true,
                mailbox,
            };
            act.handle(msg, &mut ctx).await;

            if ctx.running {
                ControlFlow::Continue(())
            } else {
                ControlFlow::Break(())
            }
        };
        let (fut, span) = instrumentation.apply::<_>(fut);
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

    pub fn handle() -> (BoxFuture<'static, ControlFlow<()>>, Span) {
        let fut = Box::pin(async { ControlFlow::Break(()) });

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
    fn start_span(&mut self) {}

    fn handle(
        self: Arc<Self>,
        _act: &mut Self::Actor,
        _mailbox: Mailbox<Self::Actor>,
    ) -> (BoxFuture<ControlFlow<()>>, Span) {
        Self::handle()
    }
}
