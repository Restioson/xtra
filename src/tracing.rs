use crate::Handler;
use tracing::{Instrument, Span};

/// Instrument a message with `tracing`. This will attach the message handler span to the given
/// `parent` span. If `IS_CHILD` is true, the message handler span will be instrumented with the
///`parent` span. Otherwise, its span will be set as following from the `parent` span.
pub struct Instrumented<M, const IS_CHILD: bool> {
    /// The underlying message whose handler will be instrumented
    pub msg: M,
    /// The parent span
    pub parent: Span,
}

/// Instrument a message as a child of or following from the current span.
pub trait InstrumentedExt: Sized {
    /// Instrument a message as a child of the current span.
    fn instrumented_child(self) -> Instrumented<Self, true>;
    /// Instrument a message as following from the current span.
    fn instrumented_follows_from(self) -> Instrumented<Self, false>;
}

impl<M> InstrumentedExt for M {
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
impl<A, M, const IS_CHILD: bool> Handler<Instrumented<M, IS_CHILD>> for A
where
    A: Handler<M> + Send,
    M: Send + 'static,
{
    type Return = <A as Handler<M>>::Return;

    async fn handle(
        &mut self,
        message: Instrumented<M, IS_CHILD>,
        ctx: &mut crate::Context<Self>,
    ) -> Self::Return {
        if IS_CHILD {
            self.handle(message.msg, ctx)
                .instrument(message.parent)
                .await
        } else {
            let span = Span::current();
            span.follows_from(message.parent);
            self.handle(message.msg, ctx).instrument(span).await
        }
    }
}
