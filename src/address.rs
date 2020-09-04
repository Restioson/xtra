//! An address to an actor is a way to send it a message. An address allows an actor to be sent any
//! kind of message that it can receive.

use crate::envelope::{NonReturningEnvelope, ReturningEnvelope};
use crate::manager::ManagerMessage;
use crate::*;
use futures::channel::oneshot::Receiver;
use futures::Future;
#[cfg(any(
    doc,
    feature = "with-tokio-0_2",
    feature = "with-async_std-1",
    feature = "with-wasm_bindgen-0_2",
    feature = "with-smol-0_4"
))]
use futures::{Stream, StreamExt, Sink};
use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::pin::Pin;
use std::sync::{Arc, Weak as ArcWeak};
use std::task::{Context, Poll};
use flume::Sender;
use crate::sink::AddressSink;

/// The reference count of a strong address. Strong addresses will prevent the actor from being
/// dropped as long as they live. Read the docs of [`Address`](struct.Address.html) to find
/// out more.
#[derive(Clone)]
pub struct Strong(pub(crate) Arc<()>);

impl Strong {
    pub(crate) fn downgrade(&self) -> Weak {
        Weak(Arc::downgrade(&self.0))
    }
}

/// The reference count of a weak address. Weak addresses will bit prevent the actor from being
/// dropped. Read the docs of [`Address`](struct.Address.html) to find out more.
#[derive(Clone)]
pub struct Weak(pub(crate) ArcWeak<()>);

/// This trait represents the strength of an address's reference counting. It is an internal trait.
/// There are two implementations of this trait: [`Weak`](struct.Weak.html) and
/// [`Strong`](struct.Weak.html). These can be provided as the second type argument to
/// [`Address`](struct.Address.html) in order to change how the address affects the actor's
/// dropping. Read the docs of [`Address`](struct.Address.html) to find out more.
pub trait RefCounter: Clone + Unpin + Send + Sync {
    #[doc(hidden)]
    fn is_last_strong(&self) -> bool;
    #[doc(hidden)]
    fn strong_count(&self) -> usize;

    // These two methods cannot be merged since is_last_strong is always false for Weak. If
    // strong_count were used to implement this, a weak being dropped could think it were a strong.
}

impl RefCounter for Strong {
    fn is_last_strong(&self) -> bool {
        // ActorManager holds one strong address, so if there are 2 strong addresses, this would be
        // the only external one in existence.
        Arc::strong_count(&self.0) == 2
    }

    fn strong_count(&self) -> usize {
        Arc::strong_count(&self.0)
    }
}

impl RefCounter for Weak {
    fn is_last_strong(&self) -> bool {
        false
    }

    fn strong_count(&self) -> usize {
        ArcWeak::strong_count(&self.0)
    }
}

/// The future returned by a method such as [`Address::send`](struct.Address.html#method.send).
/// It resolves to `Result<M::Result, Disconnected>`.
// This simply wraps the enum in order to hide the implementation details of the inner future
// while still leaving outer future nameable.
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

/// An `Address` is a reference to an actor through which [`Message`s](trait.Message.html) can be
/// sent. It can be cloned to create more addresses to the same actor.
/// By default (i.e without specifying the second type parameter, `Rc`, to be [weak](struct.Weak.html)),
/// `Address`es are strong. Therefore, when all `Address`es are dropped, the actor will be stopped.
/// In other words, any existing `Address`es will inhibit the dropping of an actor. If this is
/// undesirable, then a [`WeakAddress`](type.WeakAddress.html) should be used instead. An address
/// is created by calling the [`Actor::create`](trait.Actor.html#method.create) or
/// [`Actor::spawn`](trait.Actor.html#method.spawn) methods.
pub struct Address<A: Actor, Rc: RefCounter = Strong> {
    pub(crate) sender: Sender<ManagerMessage<A>>,
    pub(crate) ref_counter: Rc,
}

/// A `WeakAddress` is a reference to an actor through which [`Message`s](trait.Message.html) can be
/// sent. It can be cloned. Unlike [`Address`](struct.Address.html), a `WeakAddress` will not inhibit
/// the dropping of an actor. It is created by the [`Address::downgrade`](struct.Address.html#method.downgrade)
/// method.
pub type WeakAddress<A> = Address<A, Weak>;

/// Functions which apply only to strong addresses (the default kind).
impl<A: Actor> Address<A, Strong> {
    /// Create a weak address to the actor. Unlike with the strong variety of address (this kind),
    /// an actor will not be prevented from being dropped if only weak sinks, channels, and
    /// addresses exist.
    pub fn downgrade(&self) -> WeakAddress<A> {
        WeakAddress {
            sender: self.sender.clone(),
            ref_counter: self.ref_counter.downgrade(),
        }
    }
}

/// Functions which apply to any kind of address, be they strong or weak.
impl<A: Actor, Rc: RefCounter> Address<A, Rc> {
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
    /// #[async_trait::async_trait]
    /// impl Handler<Shutdown> for MyActor {
    ///     async fn handle(&mut self, _: Shutdown, ctx: &mut Context<Self>) {
    ///         ctx.stop();
    ///     }
    /// }
    ///
    /// smol::block_on(async {
    ///     let addr = MyActor.spawn();
    ///     assert!(addr.is_connected());
    ///     addr.send(Shutdown).await;
    ///     Timer::after(Duration::from_secs(1)).await; // Give it time to shut down
    ///     assert!(!addr.is_connected());
    /// })
    /// ```
    pub fn is_connected(&self) -> bool {
        // Check that there are external strong addresses and the actor isn't stopped.
        // strong_count() == 2 because Context holds a strong arc to the refcount
        self.ref_counter.strong_count() > 1 && !self.sender.is_disconnected()
    }

    /// Sends a [`Message`](trait.Message.html) to the actor, and does not wait for a response.
    /// If this returns `Err(Disconnected)`, then the actor is stopped and not accepting messages.
    /// If this returns `Ok(())`, the will be delivered, but may not be handled in the event that the
    /// actor stops itself (by calling [`Context::stop`](struct.Context.html#method.stop))
    /// before it was handled.
    pub fn do_send<M>(&self, message: M) -> Result<(), Disconnected>
    where
        M: Message,
        A: Handler<M>,
    {
        if self.is_connected() {
            // To read more about what an envelope is and why we use them, look under `envelope.rs`
            let envelope = NonReturningEnvelope::<A, M>::new(message);
            self.sender
                .send(ManagerMessage::Message(Box::new(envelope)))
                .map_err(|_| Disconnected)
        } else {
            Err(Disconnected)
        }
    }

    /// Sends a [`Message`](trait.Message.html) to the actor, and waits for a response. If this
    /// returns `Err(Disconnected)`, then the actor is stopped and not accepting messages.
    pub fn send<M>(&self, message: M) -> MessageResponseFuture<M>
    where
        M: Message,
        A: Handler<M>,
    {
        if self.is_connected() {
            let (envelope, rx) = ReturningEnvelope::<A, M>::new(message);
            let _ = self
                .sender
                .send(ManagerMessage::Message(Box::new(envelope)));
            MessageResponseFuture::result(rx)
        } else {
            MessageResponseFuture::disconnected()
        }
    }

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
        feature = "with-smol-0_4"
    ))]
    #[cfg_attr(docsrs, doc(cfg(feature = "with-tokio-0_2")))]
    #[cfg_attr(docsrs, doc(cfg(feature = "with-async_std-1")))]
    #[cfg_attr(docsrs, doc(cfg(feature = "with-wasm_bindgen-0_2")))]
    #[cfg_attr(docsrs, doc(cfg(feature = "with-smol-0_3")))]
    pub fn attach_stream<S, M, K>(self, mut stream: S)
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

        crate::spawn(fut);
    }

    /// Converts this address into a [futures `Sink`](https://docs.rs/futures/0.3/futures/io/struct.Sink.html).
    pub fn into_sink(self) -> AddressSink<A, Rc> {
        AddressSink {
            sink: self.sender.clone().into_sink(),
            ref_counter: self.ref_counter.clone(),
        }
    }
}

// Required because #[derive] adds an A: Clone bound
impl<A: Actor, Rc: RefCounter> Clone for Address<A, Rc> {
    fn clone(&self) -> Self {
        Address {
            sender: self.sender.clone(),
            ref_counter: self.ref_counter.clone(),
        }
    }
}

// Drop impls cannot be specialised, so a little bit of fanagling is used in the RefCounter impl
impl<A: Actor, Rc: RefCounter> Drop for Address<A, Rc> {
    fn drop(&mut self) {
        // We should notify the ActorManager that there are no more strong Addresses and the actor
        // should be stopped.
        if self.ref_counter.is_last_strong() {
            let _ = self.sender.send(ManagerMessage::LastAddress);
        }
    }
}
