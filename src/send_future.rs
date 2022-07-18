use std::future::Future;
use std::mem;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use futures_core::FusedFuture;
use futures_util::FutureExt;

use crate::envelope::{BroadcastEnvelopeConcrete, ReturningEnvelope};
use crate::inbox::{SentMessage, Spinlock, TrySendFail, WaitingSender};
use crate::refcount::RefCounter;
use crate::send_future::private::SetPriority;
use crate::{inbox, Error, Handler};

pin_project_lite::pin_project! {
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
    pub struct SendFuture<F, TState> {
        #[pin]
        inner: F,
        state: TState,
    }
}

/// Marker type for resolving the [`SendFuture`] to the return value of the [`Handler`](crate::Handler).
pub struct ResolveToHandlerReturn<R> {
    receiver: Receiver<R>,
}

/// Marker type for resolving the [`SendFuture`] to a [`Receiver`] once the message is queued into the actor's mailbox.
///
/// The [`Receiver`] can be used to await the completion of the handler separately.
pub struct ResolveToReceiver<R> {
    receiver: Option<Receiver<R>>,
}

/// Marker type for a [`SendFuture`] that broadcasts messages.
pub struct Broadcast(());

impl<F, R> SendFuture<F, ResolveToHandlerReturn<R>>
where
    F: Future,
{
    /// Split off a [`Receiver`] from this [`SendFuture`].
    ///
    /// Splitting off a [`Receiver`] allows you to await the completion of the [`Handler`](crate::Handler) separately from the queuing of the message into the actor's mailbox.
    ///
    /// Calling this function will change the [`Output`](Future::Output) of this [`Future`] from [`Handler::Return`](crate::Handler::Return) to [`Receiver<Handler::Return>`](Receiver<crate::Handler::Return>).
    pub fn split_receiver(self) -> SendFuture<F, ResolveToReceiver<R>> {
        SendFuture {
            inner: self.inner,
            state: ResolveToReceiver {
                receiver: Some(self.state.receiver),
            },
        }
    }
}

impl<F, TResolve> SendFuture<F, TResolve>
where
    F: SetPriority,
{
    /// Set the priority of a given message. See [`Address`](crate::Address) documentation for more info.
    ///
    /// Panics if this future has already been polled.
    pub fn priority(mut self, new_priority: u32) -> Self {
        self.inner.set_priority(new_priority);

        self
    }
}

impl<A, R, Rc> SendFuture<ActorNamedSending<A, Rc>, ResolveToHandlerReturn<R>>
where
    R: Send + 'static,
    Rc: RefCounter,
{
    /// Construct a [`SendFuture`] that contains the actor's name in its type.
    ///
    /// Compared to [`SendFuture::sending_erased`], this function avoids one allocation.
    pub(crate) fn sending_named<M>(message: M, sender: inbox::Sender<A, Rc>) -> Self
    where
        A: Handler<M, Return = R>,
        M: Send + 'static,
    {
        let (envelope, receiver) = ReturningEnvelope::<A, M, R>::new(message, 0);

        Self {
            inner: ActorNamedSending {
                future: Sending::New {
                    msg: SentMessage::msg_to_one::<M>(Box::new(envelope)),
                    sender,
                },
            },
            state: ResolveToHandlerReturn {
                receiver: Receiver::new(receiver),
            },
        }
    }
}

impl<R> SendFuture<ActorErasedSending, ResolveToHandlerReturn<R>> {
    pub(crate) fn sending_erased<A, M, Rc>(message: M, sender: inbox::Sender<A, Rc>) -> Self
    where
        Rc: RefCounter,
        A: Handler<M, Return = R>,
        M: Send + Sync + 'static + Unpin, // TODO: Check if we can get rid of `Unpin` bound
        R: Send + 'static,
    {
        let (envelope, receiver) = ReturningEnvelope::<A, M, R>::new(message, 0);

        Self {
            inner: ActorErasedSending {
                future: Box::new(Sending::New {
                    msg: SentMessage::msg_to_one::<M>(Box::new(envelope)),
                    sender: sender.clone(),
                }),
            },
            state: ResolveToHandlerReturn {
                receiver: Receiver::new(receiver),
            },
        }
    }
}

impl<A, Rc> SendFuture<ActorNamedSending<A, Rc>, Broadcast>
where
    Rc: RefCounter,
{
    pub(crate) fn broadcast_named<M>(msg: M, sender: inbox::Sender<A, Rc>) -> Self
    where
        A: Handler<M, Return = ()>,
        M: Clone + Send + Sync + 'static + Unpin,
    {
        let envelope = BroadcastEnvelopeConcrete::new(msg, 0);

        Self {
            inner: ActorNamedSending {
                future: Sending::New {
                    msg: SentMessage::msg_to_all::<M>(Arc::new(envelope)),
                    sender: sender.clone(),
                },
            },
            state: Broadcast(()),
        }
    }
}

pin_project_lite::pin_project! {
    /// "Sending" state of [`SendFuture`] for cases where the actor type is named.
    pub struct ActorNamedSending<A, Rc: RefCounter> {
        #[pin]
        future: Sending<A, Rc>,
    }
}

pin_project_lite::pin_project! {
    /// "Sending" state of [`SendFuture`] for cases where the actor type is erased.
    pub struct ActorErasedSending {
        #[pin]
        future: Box<dyn private::ErasedSending>,
    }
}

enum Sending<A, Rc: RefCounter> {
    New {
        msg: SentMessage<A>,
        sender: inbox::Sender<A, Rc>,
    },
    WaitingToSend {
        waiting: Arc<Spinlock<WaitingSender<A>>>,
    },
    Done,
}

impl<A, Rc> Future for Sending<A, Rc>
where
    Rc: RefCounter,
{
    type Output = Result<(), Error>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();

        loop {
            match mem::replace(this, Sending::Done) {
                Sending::New { msg, sender } => match sender.inner.try_send(msg) {
                    Ok(()) => return Poll::Ready(Ok(())),
                    Err(TrySendFail::Disconnected) => return Poll::Ready(Err(Error::Disconnected)),
                    Err(TrySendFail::Full(waiting)) => {
                        *this = Sending::WaitingToSend { waiting };
                    }
                },
                Sending::WaitingToSend { waiting } => {
                    let poll = { waiting.lock().poll_unpin(cx) }; // Scoped separately to drop mutex guard asap.

                    return match poll {
                        Poll::Ready(Ok(())) => return Poll::Ready(Ok(())),
                        Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                        Poll::Pending => {
                            *this = Sending::WaitingToSend { waiting };
                            Poll::Pending
                        }
                    };
                }
                Sending::Done => {
                    panic!("Polled after completion")
                }
            }
        }
    }
}

impl<A, Rc> FusedFuture for Sending<A, Rc>
where
    Self: Future,
    Rc: RefCounter,
{
    fn is_terminated(&self) -> bool {
        matches!(self, Sending::Done)
    }
}

impl<A, Rc: RefCounter> Future for ActorNamedSending<A, Rc> {
    type Output = Result<(), Error>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.project().future.poll(cx)
    }
}

impl<A, Rc> FusedFuture for ActorNamedSending<A, Rc>
where
    Self: Future,
    Rc: RefCounter,
{
    fn is_terminated(&self) -> bool {
        self.future.is_terminated()
    }
}

impl Future for ActorErasedSending {
    type Output = Result<(), Error>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.project().future.poll(cx)
    }
}

impl FusedFuture for ActorErasedSending {
    fn is_terminated(&self) -> bool {
        self.future.is_terminated()
    }
}

impl<R, F> Future for SendFuture<F, ResolveToReceiver<R>>
where
    F: Future<Output = Result<(), Error>> + FusedFuture,
{
    type Output = Result<Receiver<R>, Error>;

    fn poll(self: Pin<&mut Self>, ctx: &mut Context) -> Poll<Self::Output> {
        let this = self.project();

        loop {
            if !this.inner.is_terminated() {
                futures_util::ready!(this.inner.poll(ctx))?;
            }

            return match this.state.receiver.take() {
                None => Poll::Pending,
                Some(receiver) => Poll::Ready(Ok(receiver)),
            };
        }
    }
}

impl<R, F> Future for SendFuture<F, ResolveToHandlerReturn<R>>
where
    F: Future<Output = Result<(), Error>> + FusedFuture,
{
    type Output = Result<R, Error>;

    fn poll(self: Pin<&mut Self>, ctx: &mut Context) -> Poll<Self::Output> {
        let this = self.project();

        loop {
            if !this.inner.is_terminated() {
                futures_util::ready!(this.inner.poll(ctx))?;
            }

            let r = futures_util::ready!(this.state.receiver.poll_unpin(ctx))?;

            return Poll::Ready(Ok(r));
        }
    }
}
impl<F> Future for SendFuture<F, Broadcast>
where
    F: Future<Output = Result<(), Error>> + Unpin,
{
    type Output = Result<(), Error>;

    fn poll(self: Pin<&mut Self>, ctx: &mut Context) -> Poll<Self::Output> {
        self.project().inner.poll(ctx)
    }
}

pin_project_lite::pin_project! {
    /// A [`Future`] that resolves to the [`Return`](crate::Handler::Return) value of a [`Handler`](crate::Handler).
    ///
    /// In case the actor becomes disconnected during the execution of the handler, this future will resolve to [`Error::Disconnected`].
    #[must_use = "Futures do nothing unless polled"]
    pub struct Receiver<R> {
        #[pin]
        inner: catty::Receiver<R>,
    }
}

impl<R> Receiver<R> {
    pub(crate) fn new(receiver: catty::Receiver<R>) -> Self {
        Self { inner: receiver }
    }
}

impl<R> Future for Receiver<R> {
    type Output = Result<R, Error>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.project()
            .inner
            .poll(cx)
            .map_err(|_| Error::Interrupted)
    }
}

mod private {
    use super::*;
    use crate::inbox::MessageKind;

    pub trait SetPriority {
        fn set_priority(&mut self, priority: u32);
    }

    impl<A, Rc> SetPriority for Sending<A, Rc>
    where
        Rc: RefCounter,
    {
        fn set_priority(&mut self, new_priority: u32) {
            match self {
                Sending::New {
                    msg:
                        SentMessage {
                            msg: MessageKind::ToOneActor(msg),
                            ..
                        },
                    ..
                } => msg.set_priority(new_priority),
                Sending::New {
                    msg:
                        SentMessage {
                            msg: MessageKind::ToAllActors(msg),
                            ..
                        },
                    ..
                } => Arc::get_mut(msg)
                    .expect("envelope is not cloned until here")
                    .set_priority(new_priority),
                _ => panic!("Cannot set priority after first poll"),
            }
        }
    }

    impl<A, Rc> SetPriority for ActorNamedSending<A, Rc>
    where
        Rc: RefCounter,
    {
        fn set_priority(&mut self, priority: u32) {
            self.future.set_priority(priority)
        }
    }

    impl SetPriority for ActorErasedSending {
        fn set_priority(&mut self, priority: u32) {
            self.future.set_priority(priority)
        }
    }

    /// Helper trait because Rust does not allow to `+` non-auto traits in trait objects.
    pub trait ErasedSending:
        Future<Output = Result<(), Error>> + FusedFuture + SetPriority + Send + 'static + Unpin
    {
    }

    impl<F> ErasedSending for F where
        F: Future<Output = Result<(), Error>> + FusedFuture + SetPriority + Send + 'static + Unpin
    {
    }
}
