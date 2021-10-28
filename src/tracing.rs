use tracing::{Instrument, Span};
use crate::{Handler, Message};

/// Instrument a message with `tracing`. This will attach the message handler span to the given
/// `parent` span. If `IS_CHILD` is true, the message handler span will be instrumented with the
///`parent` span. Otherwise, its span will be set as following from the `parent` span.
pub struct Instrumented<M: Message, const IS_CHILD: bool> {
    /// The underlying message whose handler will be instrumented
    pub msg: M,
    /// The parent span
    pub parent: Span,
}

impl<M: Message, const IS_CHILD: bool> Message for Instrumented<M, IS_CHILD> {
    type Result = M::Result;
}

/// Instrument a message as a child of or following from the current span.
pub trait InstrumentedExt: Sized + Message {
    /// Instrument a message as a child of the current span.
    fn instrumented_child(self) -> Instrumented<Self, true>;
    /// Instrument a message as following from the current span.
    fn instrumented_follows_from(self) -> Instrumented<Self, false>;
}

impl<M: Message> InstrumentedExt for M {
    fn instrumented_child(self) -> Instrumented<Self, true> {
        Instrumented {
            msg: self,
            parent: Span::current(),
        }
    }

    fn instrumented_follows_from(self) -> Instrumented<Self, false> {
        Instrumented {
            msg: self,
            parent: Span::current(),
        }
    }
}

#[async_trait::async_trait]
impl<A: Handler<M>, M: Message, const IS_CHILD: bool> Handler<Instrumented<M, IS_CHILD>> for A {
    async fn handle(
        &mut self,
        message: Instrumented<M, IS_CHILD>,
        ctx: &mut crate::Context<Self>
    ) -> M::Result {
        if IS_CHILD {
            self.handle(message.msg, ctx).instrument(message.parent).await
        } else {
            Span::current().follows_from(message.parent);
            self.handle(message.msg, ctx).await
        }
    }
}
