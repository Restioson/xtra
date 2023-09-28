use std::future::Future;

pub use tracing::Span;

#[derive(Clone)]
pub struct Instrumentation {
    pub parent: Span,
    _waiting_for_actor: Span,
}

impl Instrumentation {
    pub fn empty() -> Self {
        Instrumentation {
            parent: Span::none(),
            _waiting_for_actor: Span::none(),
        }
    }

    pub fn started<A, M>() -> Self {
        let parent = tracing::debug_span!(
            "xtra_actor_request",
            actor_type = %std::any::type_name::<A>(),
            message_type = %std::any::type_name::<M>(),
        )
        .or_current();

        let _waiting_for_actor =
            tracing::debug_span!(parent: &parent, "xtra_message_waiting_for_actor",).or_current();

        Instrumentation {
            parent,
            _waiting_for_actor,
        }
    }

    pub fn is_parent_none(&self) -> bool {
        self.parent.is_none()
    }

    pub fn apply<F>(self, fut: F) -> (impl Future<Output = F::Output>, Span)
    where
        F: Future,
    {
        let executing = self.parent.in_scope(|| {
            tracing::debug_span!("xtra_message_handler", interrupted = tracing::field::Empty)
                .or_current()
        });

        (
            tracing::Instrument::instrument(fut, executing.clone()),
            executing,
        )
    }
}
