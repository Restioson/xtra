use std::future::Future;
use std::mem;
use std::pin::Pin;
use std::task::{Context, Poll};

use futures_util::future::MapErr;
use futures_util::{FutureExt, TryFutureExt};

use crate::Disconnected;

/// A [`Future`] that resolves to the [`Return`](crate::Handler::Return) value of a [`Handler`](crate::Handler).
///
/// In case the actor becomes disconnected during the execution of the handler, this future will resolve to [`Disconnected`].
#[must_use = "Futures do nothing unless polled"]
pub struct Receiver<R> {
    inner: Inner<R>,
}

impl<R> Receiver<R> {
    pub(crate) fn disconnected() -> Self {
        Self {
            inner: Inner::Disconnected,
        }
    }

    pub(crate) fn new(receiver: catty::Receiver<R>) -> Self {
        Self {
            inner: Inner::Receiving(receiver.map_err(|_| Disconnected)),
        }
    }
}

enum Inner<R> {
    Disconnected,
    Receiving(MapErr<catty::Receiver<R>, fn(catty::Disconnected) -> Disconnected>),
    Done,
}

impl<R> Future for Receiver<R> {
    type Output = Result<R, Disconnected>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();

        match mem::replace(&mut this.inner, Inner::Done) {
            Inner::Disconnected => {
                this.inner = Inner::Done;
                Poll::Ready(Err(Disconnected))
            }
            Inner::Receiving(mut rx) => match rx.poll_unpin(cx) {
                Poll::Ready(item) => {
                    this.inner = Inner::Done;
                    Poll::Ready(item)
                }
                Poll::Pending => {
                    this.inner = Inner::Receiving(rx);
                    Poll::Pending
                }
            },
            Inner::Done => panic!("Polled after completion"),
        }
    }
}
