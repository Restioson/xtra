use crate::Disconnected;
use futures_util::future::MapErr;
use futures_util::{FutureExt, TryFutureExt};
use std::future::Future;
use std::mem;
use std::pin::Pin;
use std::task::{Context, Poll};

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
                return Poll::Ready(Err(Disconnected));
            }
            Inner::Receiving(mut rx) => match rx.poll_unpin(cx) {
                Poll::Ready(item) => {
                    this.inner = Inner::Done;
                    return Poll::Ready(item);
                }
                Poll::Pending => {
                    this.inner = Inner::Receiving(rx);
                    return Poll::Pending;
                }
            },
            Inner::Done => panic!("Polled after completion"),
        }
    }
}
