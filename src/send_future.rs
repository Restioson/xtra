use crate::manager::AddressMessage;
use crate::receiver::Receiver;
use crate::Disconnected;
use flume::r#async::SendFut;
use futures_core::future::BoxFuture;
use futures_util::FutureExt;
use std::future::Future;
use std::marker::PhantomData;
use std::mem;
use std::pin::Pin;
use std::task::{Context, Poll};

/// A [`Future`] that represents state of sending a message to an actor.
///
/// By default, a `SendFuture` will also wait until the handler has finished executing and resolve to the return value (see [`Handler::Return`](crate::Handler::Return)).
/// This behaviour can be changed by calling [`recv_async`](SendFuture::recv_async).
///
/// When toggled to receive the return value asynchronously, this future will not resolve until the message is successfully queued into the actor's mailbox. If the actors mailbox is bounded, this future will yield until there is space in the mailbox.
#[must_use]
pub struct SendFuture<R, F, TRecvSyncMarker> {
    inner: SendFutureInner<R, F>,
    phantom: PhantomData<TRecvSyncMarker>,
}

/// Marker type for receiving the return value of a [`Handler`](crate::Handler) **synchronously**.
pub enum ReceiveSync {}

/// Marker type for receiving the return value of a [`Handler`](crate::Handler) **asynchronously**.
pub enum ReceiveAsync {}

enum SendFutureInner<R, F> {
    Disconnected,
    Sending(F),
    Receiving(Receiver<R>),
    Done,
}

impl<R, F> SendFuture<R, F, ReceiveSync> {
    pub(crate) fn disconnected() -> Self {
        Self {
            inner: SendFutureInner::Disconnected,
            phantom: PhantomData,
        }
    }

    /// Toggle this future to only queue the message into the actor's mailbox.
    ///
    /// Calling this function will change the [`Output`](Future::Output) of this [`Future`] from [`Handler::Return`](crate::Handler::Return) to [`Receiver<Handler::Return>`](Receiver<crate::Handler::Return>).
    ///
    /// A [`Receiver`] is a [`Future`] itself that can be used to await the execution of the [`Handler`](crate::Handler) separately from the queuing of the message.
    pub fn recv_async(self) -> SendFuture<R, F, ReceiveAsync> {
        SendFuture {
            inner: self.inner,
            phantom: PhantomData,
        }
    }
}

impl<R> SendFuture<R, BoxFuture<'static, Receiver<R>>, ReceiveSync> {
    pub(crate) fn sending_boxed<F>(send_fut: F) -> Self
    where
        F: Future<Output = Receiver<R>> + Send + 'static,
    {
        Self {
            inner: SendFutureInner::Sending(send_fut.boxed()),
            phantom: PhantomData,
        }
    }
}

impl<A, R> SendFuture<R, NameableSending<A, R>, ReceiveSync> {
    pub(crate) fn sending_named(
        send_fut: SendFut<'static, AddressMessage<A>>,
        receiver: catty::Receiver<R>,
    ) -> Self {
        Self {
            inner: SendFutureInner::Sending(NameableSending {
                inner: send_fut,
                receiver: Some(Receiver::new(receiver)),
            }),
            phantom: PhantomData,
        }
    }
}

pub struct NameableSending<A: 'static, R> {
    inner: SendFut<'static, AddressMessage<A>>,
    receiver: Option<Receiver<R>>,
}

impl<A, R> Future for NameableSending<A, R> {
    type Output = Receiver<R>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();

        let result = futures_util::ready!(this.inner.poll_unpin(cx));

        match result {
            Ok(()) => Poll::Ready(this.receiver.take().expect("polled after completion")),
            Err(_) => Poll::Ready(Receiver::disconnected()),
        }
    }
}

impl<R, F> Future for SendFuture<R, F, ReceiveSync>
where
    F: Future<Output = Receiver<R>> + Unpin,
{
    type Output = Result<R, Disconnected>;

    fn poll(self: Pin<&mut Self>, ctx: &mut Context) -> Poll<Self::Output> {
        let this = self.get_mut();

        match mem::replace(&mut this.inner, SendFutureInner::Done) {
            SendFutureInner::Disconnected => {
                this.inner = SendFutureInner::Done;
                Poll::Ready(Err(Disconnected))
            }
            SendFutureInner::Sending(mut send_fut) => match send_fut.poll_unpin(ctx) {
                Poll::Ready(rx) => {
                    this.inner = SendFutureInner::Receiving(rx);
                    this.poll_unpin(ctx)
                }
                Poll::Pending => {
                    this.inner = SendFutureInner::Sending(send_fut);
                    Poll::Pending
                }
            },
            SendFutureInner::Receiving(mut rx) => match rx.poll_unpin(ctx) {
                Poll::Ready(item) => {
                    this.inner = SendFutureInner::Done;
                    Poll::Ready(item)
                }
                Poll::Pending => {
                    this.inner = SendFutureInner::Receiving(rx);
                    Poll::Pending
                }
            },
            SendFutureInner::Done => {
                panic!("Polled after completion")
            }
        }
    }
}

impl<R, F> Future for SendFuture<R, F, ReceiveAsync>
where
    F: Future<Output = Receiver<R>> + Unpin,
{
    type Output = Receiver<R>;

    fn poll(self: Pin<&mut Self>, ctx: &mut Context) -> Poll<Self::Output> {
        let this = self.get_mut();

        match mem::replace(&mut this.inner, SendFutureInner::Done) {
            SendFutureInner::Disconnected => {
                this.inner = SendFutureInner::Done;
                Poll::Ready(Receiver::disconnected())
            }
            SendFutureInner::Sending(mut send_fut) => match send_fut.poll_unpin(ctx) {
                Poll::Ready(rx) => {
                    this.inner = SendFutureInner::Receiving(rx);
                    this.poll_unpin(ctx)
                }
                Poll::Pending => {
                    this.inner = SendFutureInner::Sending(send_fut);
                    Poll::Pending
                }
            },
            SendFutureInner::Receiving(rx) => {
                this.inner = SendFutureInner::Done;
                Poll::Ready(rx)
            }
            SendFutureInner::Done => {
                panic!("Polled after completion")
            }
        }
    }
}
