use crate::envelope::{NonReturningEnvelope, ReturningEnvelope};
use crate::manager::ManagerMessage;
use crate::*;
use futures::channel::{mpsc::UnboundedSender, oneshot::Receiver};
use std::task::{Context, Poll};
use futures::{Future, Sink};
#[cfg(any(
    doc,
    feature = "with-tokio-0_2",
    feature = "with-async_std-1",
    feature = "with-wasm_bindgen-0_2",
    feature = "with-smol-0_1"
))]
use futures::{Stream, StreamExt};
use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::pin::Pin;
use std::sync::{Arc, Weak};

/// The future returned by a method such as [`AddressExt::send`](trait.AddressExt.html#method.send).
/// It resolves to `Result<M::Result, Disconnected>`.
pub struct MessageResponseFuture<M: Message>(MessageResponseFutureInner<M>);

impl<M: Message> MessageResponseFuture<M> {
    fn result(res: Receiver<M::Result>) -> Self {
        MessageResponseFuture(MessageResponseFutureInner::Result(res))
    }

    fn disconnected() -> Self {
        MessageResponseFuture(MessageResponseFutureInner::Disconnected)
    }
}

enum MessageResponseFutureInner<M: Message> {
    Disconnected,
    Result(Receiver<M::Result>),
}

impl<M: Message> Future for MessageResponseFuture<M> {
    type Output = Result<M::Result, Disconnected>;

    fn poll(self: Pin<&mut Self>, ctx: &mut Context) -> Poll<Self::Output> {
        match &mut self.get_mut().0 {
            MessageResponseFutureInner::Disconnected => Poll::Ready(Err(Disconnected)),
            MessageResponseFutureInner::Result(rx) => {
                let rx = Pin::new(rx);
                rx.poll(ctx).map(|res| res.map_err(|_| Disconnected))
            }
        }
    }
}

/// The actor is no longer running and disconnected from the sending address. For why this could
/// occur, see the [`Actor::stopping`](trait.Actor.html#method.stopping) and
/// [`Actor::stopped`](trait.Actor.html#method.stopped) methods.
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct Disconnected;

impl Display for Disconnected {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str("Actor address disconnected")
    }
}

impl Error for Disconnected {}

/// General trait for any kind of address to an actor, be it strong or weak. This trait contains all
/// functions of the address.
pub trait AddressExt<A: Actor> {
    /// Returns whether the actor referred to by this address is running and accepting messages.
    ///
    /// ```rust
    /// # use xtra::prelude::*;
    /// # use std::time::Duration;
    /// # struct MyActor;
    /// # impl Actor for MyActor {}
    /// # use smol::Timer;
    /// struct Shutdown;
    ///
    /// impl Message for Shutdown {
    ///     type Result = ();
    /// }
    ///
    /// impl SyncHandler<Shutdown> for MyActor {
    ///     fn handle(&mut self, _: Shutdown, ctx: &mut Context<Self>) {
    ///         ctx.stop();
    ///     }
    /// }
    ///
    /// #[smol_potat::main]
    /// async fn main() {
    /// let addr = MyActor.spawn();
    ///     assert!(addr.is_connected());
    ///     addr.send(Shutdown).await;
    /// #   Timer::after(Duration::from_secs(1)).await; // Give it time to shut down
    ///     assert!(!addr.is_connected());
    /// }
    /// ```
    fn is_connected(&self) -> bool;

    /// Sends a [`Message`](trait.Message.html) to the actor, and does not wait for a response.
    /// If this returns `Err(Disconnected)`, then the actor is stopped and not accepting messages.
    /// If this returns `Ok(())`, the will be delivered, but may not be handled in the event that the
    /// actor stops itself (by calling [`Context::stop`](struct.Context.html#method.stop))
    /// before it was handled.
    fn do_send<M>(&self, message: M) -> Result<(), Disconnected>
    where
        M: Message,
        A: Handler<M>;

    /// Sends a [`Message`](trait.Message.html) to the actor, and waits for a response. If this
    /// returns `Err(Disconnected)`, then the actor is stopped and not accepting messages.
    fn send<M>(&self, message: M) -> MessageResponseFuture<M>
    where
        M: Message,
        A: Handler<M>;

    /// Attaches a stream to this actor such that all messages produced by it are forwarded to the
    /// actor. This could, for instance, be used to forward messages from a socket to the actor
    /// (after the messages have been appropriately `map`ped). This is a convenience method over
    /// explicitly forwarding a stream to this address, spawning that future onto the executor,
    /// and mapping the error away (because disconnects are expected and will simply mean that the
    /// stream is no longer being forwarded).
    ///
    /// **Note:** if this stream's continuation should prevent the actor from being dropped, this
    /// method should be called on [`Address`](struct.Address.html). Otherwise, it should be called
    /// on [`WeakAddress`](struct.WeakAddress.html).
    #[cfg(any(
        doc,
        feature = "with-tokio-0_2",
        feature = "with-async_std-1",
        feature = "with-wasm_bindgen-0_2",
        feature = "with-smol-0_1"
    ))]
    #[cfg_attr(doc, doc(cfg(feature = "with-tokio-0_2")))]
    #[cfg_attr(doc, doc(cfg(feature = "with-async_std-1")))]
    #[cfg_attr(doc, doc(cfg(feature = "with-wasm_bindgen-0_2")))]
    #[cfg_attr(doc, doc(cfg(feature = "with-smol-0_1")))]
    fn attach_stream<S, M, K>(self, mut stream: S)
    where
        K: Into<KeepRunning> + Send,
        M: Message<Result = K>,
        A: Handler<M>,
        S: Stream<Item = M> + Send + Unpin + 'static,
        Self: Sized + Send + Sink<M, Error = Disconnected> + 'static,
    {
        let fut = async move {
            while let Some(m) = stream.next().await {
                let res = self.send(m); // Bound to make it Sync
                if !matches!(res.await.map(Into::into), Ok(KeepRunning::Yes)) {
                    break;
                }
            }
        };

        tokio::spawn(fut);
    }
}

/// An `Address` is a reference to an actor through which [`Message`s](trait.Message.html) can be
/// sent. It can be cloned, and when all `Address`es are dropped, the actor will be stopped. Therefore,
/// any existing `Address`es will inhibit the dropping of an actor. If this is undesirable, then
/// the [`WeakAddress`](struct.WeakAddress.html) struct should be used instead. This struct is created
/// by calling the [`Actor::create`](trait.Actor.html#method.create) or  [`Actor::spawn`](trait.Actor.html#method.spawn)
/// methods.
pub struct Address<A: Actor> {
    pub(crate) sender: UnboundedSender<ManagerMessage<A>>,
    pub(crate) ref_counter: Arc<()>,
}

impl<A: Actor> Address<A> {
    /// Create a weak address to the actor. Unlike with the strong variety of address (this kind),
    /// an actor will not be prevented from being dropped if only weak addresses exist.
    pub fn downgrade(&self) -> WeakAddress<A> {
        WeakAddress {
            sender: self.sender.clone(),
            ref_counter: Arc::downgrade(&self.ref_counter),
        }
    }

    /// Converts this address into a weak address to the actor. Unlike with the strong variety of
    /// address (this kind), an actor will not be prevented from being dropped if only weak addresses
    /// exist.
    pub fn into_downgraded(self) -> WeakAddress<A> {
        self.downgrade()
    }

    /// Gets a message channel to the actor. Like an address, a message channel allows messages
    /// to be sent to an actor. Unlike an address, rather than allowing you to send any kind of
    /// message to one kind of actor, a message channel allows you to send one kind of message to
    /// any kind of actor. This method involves a clone of the address, and as such is slightly more
    /// expensive than [`Address::into_channel`](struct.Address.html#method.into_channel).
    pub fn channel<M: Message>(&self) -> MessageChannel<M>
    where
        A: Handler<M>,
    {
        self.clone().into_channel()
    }

    /// Converts this address into a message channel to the actor. Like an address, a message channel
    /// allows messages to be sent to an actor. Unlike an address, rather than allowing you to send
    /// any kind of message to one kind of actor, a message channel allows you to send one kind of
    /// message to any kind of actor. This method does not clone the address, and as such is
    /// slightly less expensive than [`Address::channel`](struct.Address.html#method.channel).
    pub fn into_channel<M: Message>(self) -> MessageChannel<M>
    where
        A: Handler<M>,
    {
        MessageChannel {
            address: Box::new(self),
        }
    }
}

impl<A, M> Into<MessageChannel<M>> for Address<A>
where
    A: Handler<M>,
    M: Message,
{
    fn into(self) -> MessageChannel<M> {
        self.into_channel()
    }
}

impl<A: Actor> AddressExt<A> for Address<A> {
    fn is_connected(&self) -> bool {
        !self.sender.is_closed()
    }

    fn do_send<M>(&self, message: M) -> Result<(), Disconnected>
    where
        M: Message,
        A: Handler<M>,
    {
        // To read more about what an envelope is and why we use them, look under `envelope.rs`
        let envelope = NonReturningEnvelope::<A, M>::new(message);
        self.sender
            .unbounded_send(ManagerMessage::Message(Box::new(envelope)))
            .map_err(|_| Disconnected)
    }

    fn send<M>(&self, message: M) -> MessageResponseFuture<M>
    where
        M: Message,
        A: Handler<M>,
    {
        let (envelope, rx) = ReturningEnvelope::<A, M>::new(message);
        let _ = self
            .sender
            .unbounded_send(ManagerMessage::Message(Box::new(envelope)));
        MessageResponseFuture::result(rx)
    }
}

impl<M, A> Sink<M> for Address<A>
where
    M: Message,
    A: Handler<M>,
{
    type Error = Disconnected;

    fn poll_ready(self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        if self.is_connected() {
            self.sender.poll_ready(ctx).map_err(|_| Disconnected)
        } else {
            Poll::Ready(Err(Disconnected))
        }
    }

    fn start_send(self: Pin<&mut Self>, message: M) -> Result<(), Self::Error> {
        if self.is_connected() {
            let envelope = NonReturningEnvelope::<A, M>::new(message);
            let msg = ManagerMessage::Message(Box::new(envelope));
            Pin::new(&mut self.get_mut().sender)
                .start_send(msg)
                .map_err(|_| Disconnected)
        } else {
            Err(Disconnected)
        }
    }

    fn poll_flush(self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.get_mut().sender)
            .poll_flush(ctx)
            .map_err(|_| Disconnected)
    }

    fn poll_close(self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.get_mut().sender)
            .poll_close(ctx)
            .map_err(|_| Disconnected)
    }
}

// Required because #[derive] adds an A: Clone bound
impl<A: Actor> Clone for Address<A> {
    fn clone(&self) -> Self {
        Address {
            sender: self.sender.clone(),
            ref_counter: self.ref_counter.clone(),
        }
    }
}

impl<A: Actor> Drop for Address<A> {
    fn drop(&mut self) {
        // ActorManager holds one strong address, so if there are 2 strong addresses, this would be
        // the only external one in existence. Therefore, we should notify the ActorManager that
        // there are potentially no more strong Addresses and the actor should be stopped.
        if Arc::strong_count(&self.ref_counter) == 2 {
            let _ = self.sender.unbounded_send(ManagerMessage::LastAddress);
        }
    }
}

/// A `WeakAddress` is a reference to an actor through which [`Message`s](trait.Message.html) can be
/// sent. It can be cloned. Unlike [`Address`](struct.Address.html), a `WeakAddress` will not inhibit
/// the dropping of an actor. It is created by the [`Address::downgrade`](struct.Address.html#method.downgrade)
/// or [`Address::into_downgraded`](struct.Address.html#method.into_downgraded) methods.
pub struct WeakAddress<A: Actor> {
    pub(crate) sender: UnboundedSender<ManagerMessage<A>>,
    pub(crate) ref_counter: Weak<()>,
}

impl<A: Actor> WeakAddress<A> {
    /// Gets a message channel to the actor. Like an address, a message channel allows messages
    /// to be sent to an actor. Unlike an address, rather than allowing you to send any kind of
    /// message to one kind of actor, a message channel allows you to send one kind of message to
    /// any kind of actor.  This method involves a clone of the address, and as such is slightly more
    /// expensive than [`WeakAddress::into_channel`](struct.WeakAddress.html#method.into_channel).
    pub fn channel<M: Message>(&self) -> WeakMessageChannel<M>
    where
        A: Handler<M>,
    {
        self.clone().into_channel()
    }

    /// Converts this address into a message channel to the actor. Like an address, a message channel
    /// allows messages to be sent to an actor. Unlike an address, rather than allowing you to send
    /// any kind of message to one kind of actor, a message channel allows you to send one kind of
    /// message to any kind of actor. This method does not clone the address, and as such is
    /// slightly less expensive than [`WeakAddress::channel`](struct.WeakAddress.html#method.channel).
    pub fn into_channel<M: Message>(self) -> WeakMessageChannel<M>
    where
        A: Handler<M>,
    {
        WeakMessageChannel {
            address: Box::new(self),
        }
    }
}

impl<A, M> Into<WeakMessageChannel<M>> for WeakAddress<A>
where
    A: Handler<M>,
    M: Message,
{
    fn into(self) -> WeakMessageChannel<M> {
        self.into_channel()
    }
}

impl<A: Actor> AddressExt<A> for WeakAddress<A> {
    fn is_connected(&self) -> bool {
        // Check that there are external strong addresses. If there are none, the actor is
        // disconnected and our message would interrupt its dropping. strong_count() == 2 because
        // Context and manager both hold a strong arc to the refcount
        self.ref_counter.strong_count() > 1 && !self.sender.is_closed()
    }

    fn do_send<M>(&self, message: M) -> Result<(), Disconnected>
    where
        M: Message,
        A: Handler<M>,
    {
        if self.is_connected() {
            // To read more about what an envelope is and why we use them, look under `envelope.rs`
            let envelope = NonReturningEnvelope::<A, M>::new(message);
            self.sender
                .unbounded_send(ManagerMessage::Message(Box::new(envelope)))
                .map_err(|_| Disconnected)
        } else {
            Err(Disconnected)
        }
    }

    fn send<M>(&self, message: M) -> MessageResponseFuture<M>
    where
        M: Message,
        A: Handler<M>,
    {
        if self.is_connected() {
            let (envelope, rx) = ReturningEnvelope::<A, M>::new(message);
            let _ = self
                .sender
                .unbounded_send(ManagerMessage::Message(Box::new(envelope)));
            MessageResponseFuture::result(rx)
        } else {
            MessageResponseFuture::disconnected()
        }
    }
}

impl<M, A> Sink<M> for WeakAddress<A>
where
    M: Message,
    A: Handler<M>,
{
    type Error = Disconnected;

    fn poll_ready(self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        if self.is_connected() {
            self.sender.poll_ready(ctx).map_err(|_| Disconnected)
        } else {
            Poll::Ready(Err(Disconnected))
        }
    }

    fn start_send(self: Pin<&mut Self>, message: M) -> Result<(), Self::Error> {
        if self.is_connected() {
            let envelope = NonReturningEnvelope::<A, M>::new(message);
            let msg = ManagerMessage::Message(Box::new(envelope));
            Pin::new(&mut self.get_mut().sender)
                .start_send(msg)
                .map_err(|_| Disconnected)
        } else {
            Err(Disconnected)
        }
    }

    fn poll_flush(self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        if self.is_connected() {
            Pin::new(&mut self.get_mut().sender)
                .poll_flush(ctx)
                .map_err(|_| Disconnected)
        } else {
            Poll::Ready(Err(Disconnected))
        }
    }

    fn poll_close(self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        if self.is_connected() {
            Pin::new(&mut self.get_mut().sender)
                .poll_close(ctx)
                .map_err(|_| Disconnected)
        } else {
            Poll::Ready(Err(Disconnected))
        }
    }
}

// Required because #[derive] adds an A: Clone bound
impl<A: Actor> Clone for WeakAddress<A> {
    fn clone(&self) -> Self {
        WeakAddress {
            sender: self.sender.clone(),
            ref_counter: self.ref_counter.clone(),
        }
    }
}
