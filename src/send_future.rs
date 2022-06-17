use crate::receiver::Receiver;
use crate::{inbox, Disconnected};
use futures_core::future::BoxFuture;
use futures_core::FusedFuture;
use futures_util::FutureExt;
use std::future::Future;
use std::marker::PhantomData;
use std::mem;
use std::pin::Pin;
use std::task::{Context, Poll};

/// A [`Future`] that represents the state of sending a message to an actor.
///
/// By default, a [`SendFuture`] will resolve to the return value of the handler (see [`Handler::Return`](crate::Handler::Return)).
/// This behaviour can be changed by calling [`split_receiver`](SendFuture::split_receiver).
///
/// A [`SendFuture`] whose [`Receiver`] has been split off will resolve once the message is successfully queued into the actor's mailbox and resolve to the [`Receiver`].
/// The [`Receiver`] itself is a future that will resolve to the return value of the [`Handler`](crate::Handler).
///
/// In case an actor's mailbox is bounded, [`SendFuture`] will yield `Pending` until the message is queued successfully.
/// This allows an actor to exercise backpressure on its users.
#[must_use = "Futures do nothing unless polled"]
pub struct SendFuture<R, F, TResolveMarker> {
    inner: SendFutureInner<R, F>,
    phantom: PhantomData<TResolveMarker>,
}

/// Marker type for resolving the [`SendFuture`] to the return value of the [`Handler`](crate::Handler).
pub enum ResolveToHandlerReturn {}

/// Marker type for resolving the [`SendFuture`] to a [`Receiver`] once the message is queued into the actor's mailbox.
///
/// The [`Receiver`] can be used to await the completion of the handler separately.
pub enum ResolveToReceiver {}

enum SendFutureInner<R, F> {
    Sending(F),
    Receiving(Receiver<R>),
    Done,
}

impl<R, F> SendFuture<R, F, ResolveToHandlerReturn> {
    /// Split off a [`Receiver`] from this [`SendFuture`].
    ///
    /// Splitting off a [`Receiver`] allows you to await the completion of the [`Handler`](crate::Handler) separately from the queuing of the message into the actor's mailbox.
    ///
    /// Calling this function will change the [`Output`](Future::Output) of this [`Future`] from [`Handler::Return`](crate::Handler::Return) to [`Receiver<Handler::Return>`](Receiver<crate::Handler::Return>).
    pub fn split_receiver(self) -> SendFuture<R, F, ResolveToReceiver> {
        SendFuture {
            inner: self.inner,
            phantom: PhantomData,
        }
    }
}

impl<R> SendFuture<R, BoxFuture<'static, Receiver<R>>, ResolveToHandlerReturn> {
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

impl<A, R> SendFuture<R, NameableSending<A, R>, ResolveToHandlerReturn> {
    pub(crate) fn sending_named(
        send_fut: inbox::SendFuture<A>,
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

/// "Sending" state of [`SendFuture`] for cases where the actor type is known and we can there refer to it by name.
pub struct NameableSending<A, R> {
    inner: inbox::SendFuture<A>,
    receiver: Option<Receiver<R>>,
}

impl<A, R> Future for NameableSending<A, R> {
    type Output = Receiver<R>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Receiver<R>> {
        let this = self.get_mut();

        let result = futures_util::ready!(this.inner.poll_unpin(cx));

        match result {
            Ok(()) => Poll::Ready(this.receiver.take().expect("polled after completion")),
            Err(_) => Poll::Ready(Receiver::disconnected()),
        }
    }
}

impl<R, F> Future for SendFuture<R, F, ResolveToHandlerReturn>
where
    F: Future<Output = Receiver<R>> + Unpin,
{
    type Output = Result<R, Disconnected>;

    fn poll(self: Pin<&mut Self>, ctx: &mut Context) -> Poll<Result<R, Disconnected>> {
        let this = self.get_mut();

        match mem::replace(&mut this.inner, SendFutureInner::Done) {
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

impl<R, F> Future for SendFuture<R, F, ResolveToReceiver>
where
    F: Future<Output = Receiver<R>> + Unpin,
{
    type Output = Receiver<R>;

    fn poll(self: Pin<&mut Self>, ctx: &mut Context) -> Poll<Receiver<R>> {
        let this = self.get_mut();

        match mem::replace(&mut this.inner, SendFutureInner::Done) {
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

impl<R, F, TResolveMarker> FusedFuture for SendFuture<R, F, TResolveMarker>
where
    Self: Future,
{
    fn is_terminated(&self) -> bool {
        matches!(self.inner, SendFutureInner::Done)
    }
}
