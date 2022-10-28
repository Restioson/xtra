use std::future::Future;

#[derive(Clone)]
pub struct Instrumentation {}

#[derive(Clone)]
pub struct Span(());

impl Span {
    pub fn in_scope<R>(&self, f: impl FnOnce() -> R) -> R {
        f()
    }

    pub fn none() -> Span {
        Span(())
    }

    pub fn is_none(&self) -> bool {
        true
    }
}

impl Instrumentation {
    pub fn empty() -> Self {
        Instrumentation {}
    }

    pub fn started<A, M>() -> Self {
        Self::empty()
    }

    pub fn is_parent_none(&self) -> bool {
        true
    }

    pub fn apply<A, M, F>(self, fut: F) -> (impl Future<Output = F::Output>, Span)
    where
        F: Future,
    {
        (fut, Span(()))
    }
}
