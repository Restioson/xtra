use crate::address::{ActorJoinHandle, Address};
use crate::refcount::RefCounter;
use futures_util::FutureExt;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

pin_project_lite::pin_project! {
    /// A task that is scoped to the lifecycle of an actor. This means that when the associated
    /// actor stops, the task will stop too. This future will either complete when the inner future
    /// completes, or when the actor is dropped, whichever comes first. If the inner future completes
    /// successfully, `Some(result)` will be returned, else `None` will be returned if the actor
    /// is stopped before it could be polled to completion.
    #[must_use = "Futures do nothing unless polled"]
    pub struct ScopedTask<F: ?Sized> {
        join_handle: ActorJoinHandle,
        #[pin]
        fut: F,
    }
}

impl<F> Future for ScopedTask<F>
where
    F: Future,
{
    type Output = Option<F::Output>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();

        match this.fut.poll(cx) {
            Poll::Ready(v) => Poll::Ready(Some(v)),
            Poll::Pending => match this.join_handle.poll_unpin(cx) {
                Poll::Ready(()) => Poll::Ready(None),
                Poll::Pending => Poll::Pending,
            },
        }
    }
}

/// Extension trait to allow a future to be conveniently converted to a [`ScopedTask`], which will
/// end as soon as the actor is stopped, or when it completes (whichever comes first).
pub trait ActorScopedExt {
    /// Convert this future to a [`ScopedTask`].
    fn scoped<A, Rc>(self, address: &Address<A, Rc>) -> ScopedTask<Self>
    where
        Rc: RefCounter;
}

impl<F> ActorScopedExt for F
where
    F: Future,
{
    fn scoped<A, Rc>(self, address: &Address<A, Rc>) -> ScopedTask<Self>
    where
        Rc: RefCounter,
    {
        ScopedTask {
            join_handle: address.join(),
            fut: self,
        }
    }
}
