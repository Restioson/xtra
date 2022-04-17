//! An address to an actor is a way to send it a message. An address allows an actor to be sent any
//! kind of message that it can receive.

use std::fmt::{self, Display, Formatter};
use std::future::Future;
use std::marker::PhantomData;
use std::mem;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::{cmp::Ordering, error::Error, hash::Hash};

use catty::Receiver;
use flume::r#async::SendFut as ChannelSendFuture;
use flume::Sender;
use futures_core::future::BoxFuture;
use futures_core::Stream;
use futures_util::{future, FutureExt, StreamExt, TryFutureExt};

use crate::envelope::ReturningEnvelope;
use crate::manager::AddressMessage;
use crate::refcount::{Either, RefCounter, Strong, Weak};
use crate::sink::AddressSink;
use crate::{Actor, Handler, KeepRunning, ReceiveAsync, ReceiveSync};

/// The future returned [`Address::send`](struct.Address.html#method.send).
/// It resolves to `Result<M::Result, Disconnected>`.
// This simply wraps the enum in order to hide the implementation details of the inner future
// while still leaving outer future nameable.
#[must_use]
pub struct SendFuture<A: Actor, R, TRecvSyncMarker> {
    inner: SendFutureInner<A, R>,
    phantom: PhantomData<TRecvSyncMarker>,
}

impl<A: Actor, R> SendFuture<A, R, ReceiveSync> {
    fn new(inner: SendFutureInner<A, R>) -> Self {
        Self {
            inner,
            phantom: PhantomData,
        }
    }

    fn disconnected() -> Self {
        Self {
            inner: SendFutureInner::Disconnected,
            phantom: PhantomData,
        }
    }

    /// TODO: docs
    pub fn recv_async(self) -> SendFuture<A, R, ReceiveAsync> {
        SendFuture {
            inner: self.inner,
            phantom: PhantomData,
        }
    }
}

enum SendFutureInner<A: Actor, R> {
    Disconnected,
    Sending(ChannelSendFuture<'static, AddressMessage<A>>, Receiver<R>),
    Receiving(Receiver<R>),
    Done,
}

impl<A: Actor, R> Default for SendFutureInner<A, R> {
    fn default() -> Self {
        SendFutureInner::Disconnected
    }
}

pub(crate) fn poll_rx<T>(rx: &mut Receiver<T>, ctx: &mut Context) -> Poll<Result<T, Disconnected>> {
    rx.poll_unpin(ctx).map_err(|_| Disconnected)
}

impl<A: Actor, R> Future for SendFuture<A, R, ReceiveSync> {
    type Output = Result<R, Disconnected>;

    fn poll(self: Pin<&mut Self>, ctx: &mut Context) -> Poll<Self::Output> {
        let this = self.get_mut();
        let (poll, new) = match mem::take(&mut this.inner) {
            old @ SendFutureInner::Disconnected => (Poll::Ready(Err(Disconnected)), old),
            SendFutureInner::Sending(mut tx, mut rx) => {
                if tx.poll_unpin(ctx).is_ready() {
                    (poll_rx(&mut rx, ctx), SendFutureInner::Receiving(rx))
                } else {
                    (Poll::Pending, SendFutureInner::Sending(tx, rx))
                }
            }
            SendFutureInner::Receiving(mut rx) => {
                (poll_rx(&mut rx, ctx), SendFutureInner::Receiving(rx))
            }
            SendFutureInner::Done => {
                panic!("Polled after completion")
            }
        };

        this.inner = new;
        poll
    }
}

impl<A: Actor, R: Send + 'static> Future for SendFuture<A, R, ReceiveAsync> {
    type Output = BoxFuture<'static, Result<R, Disconnected>>;

    fn poll(self: Pin<&mut Self>, ctx: &mut Context) -> Poll<Self::Output> {
        let this = self.get_mut();
        let (poll, new) = match mem::take(&mut this.inner) {
            old @ SendFutureInner::Disconnected => {
                (Poll::Ready(future::ready(Err(Disconnected)).boxed()), old)
            }
            SendFutureInner::Sending(mut tx, rx) => {
                if tx.poll_unpin(ctx).is_ready() {
                    match tx.poll_unpin(ctx) {
                        Poll::Ready(Ok(())) => (
                            Poll::Ready(rx.map_err(|_| Disconnected).boxed()),
                            SendFutureInner::Done,
                        ),
                        Poll::Ready(Err(_)) => (
                            Poll::Ready(future::ready(Err(Disconnected)).boxed()),
                            SendFutureInner::Done,
                        ),
                        Poll::Pending => (Poll::Pending, SendFutureInner::Sending(tx, rx)),
                    }
                } else {
                    (Poll::Pending, SendFutureInner::Sending(tx, rx))
                }
            }
            SendFutureInner::Receiving(rx) => (
                Poll::Ready(rx.map_err(|_| Disconnected).boxed()),
                SendFutureInner::Done,
            ),
            SendFutureInner::Done => {
                panic!("Polled after completion")
            }
        };

        this.inner = new;
        poll
    }
}

/// The actor is no longer running and disconnected from the sending address. For why this could
/// occur, see the [`Actor::stopping`](../trait.Actor.html#method.stopping) and
/// [`Actor::stopped`](../trait.Actor.html#method.stopped) methods.
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct Disconnected;

impl Display for Disconnected {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str("Actor address disconnected")
    }
}

impl Error for Disconnected {}

/// An `Address` is a reference to an actor through which [`Message`s](../trait.Message.html) can be
/// sent. It can be cloned to create more addresses to the same actor.
/// By default (i.e without specifying the second type parameter, `Rc`, to be
/// [weak](../refcount/struct.Weak.html)), `Address`es are strong. Therefore, when all `Address`es
/// are dropped, the actor will be stopped. In other words, any existing `Address`es will inhibit
/// the dropping of an actor. If this is undesirable, then a [`WeakAddress`](type.WeakAddress.html)
/// should be used instead. An address is created by calling the
/// [`Actor::create`](../trait.Actor.html#method.create) or
/// [`Context::run`](../struct.Context.html#method.run) methods, or by cloning another `Address`.
pub struct Address<A, Rc: RefCounter = Strong> {
    pub(crate) sender: Sender<AddressMessage<A>>,
    pub(crate) ref_counter: Rc,
}

/// A `WeakAddress` is a reference to an actor through which [`Message`s](../trait.Message.html) can be
/// sent. It can be cloned. Unlike [`Address`](struct.Address.html), a `WeakAddress` will not inhibit
/// the dropping of an actor. It is created by the [`Address::downgrade`](struct.Address.html#method.downgrade)
/// method.
pub type WeakAddress<A> = Address<A, Weak>;

/// Functions which apply only to strong addresses (the default kind).
impl<A> Address<A, Strong> {
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

/// Functions which apply only to addresses which can either be strong or weak.
impl<A> Address<A, Either> {
    /// Converts this address into a weak address.
    pub fn downgrade(&self) -> WeakAddress<A> {
        WeakAddress {
            sender: self.sender.clone(),
            ref_counter: self.ref_counter.clone().into_weak(),
        }
    }
}

/// Functions which apply to any kind of address, be they strong or weak.
impl<A, Rc: RefCounter> Address<A, Rc> {
    /// Returns whether the actor referred to by this address is running and accepting messages.
    ///
    /// ```rust
    /// # use xtra::prelude::*;
    /// # use xtra::spawn::Smol;
    /// # use std::time::Duration;
    /// # struct MyActor;
    /// # #[async_trait] impl Actor for MyActor {type Stop = (); async fn stopped(self) -> Self::Stop {} }
    /// # use smol::Timer;
    /// struct Shutdown;
    ///
    /// impl Message for Shutdown {
    ///     type Result = ();
    /// }
    ///
    /// #[async_trait]
    /// impl Handler<Shutdown> for MyActor {
    ///     async fn handle(&mut self, _: Shutdown, ctx: &mut Context<Self>) {
    ///         ctx.stop();
    ///     }
    /// }
    ///
    /// smol::block_on(async {
    ///     let addr = MyActor.create(None).spawn(&mut Smol::Global);
    ///     assert!(addr.is_connected());
    ///     addr.send(Shutdown).await;
    ///     Timer::after(Duration::from_secs(1)).await; // Give it time to shut down
    ///     assert!(!addr.is_connected());
    /// })
    /// ```
    pub fn is_connected(&self) -> bool {
        !self.sender.is_disconnected()
    }

    /// Returns the number of messages in the actor's mailbox.
    pub fn len(&self) -> usize {
        self.sender.len()
    }

    /// The total capacity of the actor's mailbox.
    pub fn capacity(&self) -> Option<usize> {
        self.sender.capacity()
    }

    /// Returns whether the actor's mailbox is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Convert this address into a generic address which can be weak or strong.
    pub fn as_either(&self) -> Address<A, Either> {
        Address {
            ref_counter: self.ref_counter.clone().into_either(),
            sender: self.sender.clone(),
        }
    }

    /// Send a [`Message`](../trait.Message.html) to the actor and asynchronously wait for a response. If this
    /// returns `Err(Disconnected)`, then the actor is stopped and not accepting messages. Like most
    /// futures, this must be polled to actually send the message.
    pub fn send<M>(&self, message: M) -> SendFuture<A, <A as Handler<M>>::Return, ReceiveSync>
    where
        M: Send + 'static,
        A: Handler<M>,
    {
        if self.is_connected() {
            let (envelope, rx) = ReturningEnvelope::<A, M, <A as Handler<M>>::Return>::new(message);
            let tx = self
                .sender
                .clone()
                .into_send_async(AddressMessage::Message(Box::new(envelope)));
            SendFuture::new(SendFutureInner::Sending(tx, rx))
        } else {
            SendFuture::disconnected()
        }
    }

    /// Attaches a stream to this actor such that all messages produced by it are forwarded to the
    /// actor. This could, for instance, be used to forward messages from a socket to the actor
    /// (after the messages have been appropriately `map`ped). This is a convenience method over
    /// explicitly forwarding a stream to this address and checking when to stop forwarding.
    ///
    /// Often, this should be spawned onto an executor to run in the background. **Do not await this
    /// inside of an actor** - this will cause it to await forever and never receive any messages.
    ///
    /// **Note:** if this stream's continuation should prevent the actor from being dropped, this
    /// method should be called on [`Address`](struct.Address.html). Otherwise, it should be called
    /// on [`WeakAddress`](type.WeakAddress.html).
    pub async fn attach_stream<S, M, K>(self, stream: S)
    where
        K: Into<KeepRunning> + Send,
        M: Send + 'static,
        A: Handler<M, Return = K>,
        S: Stream<Item = M> + Send,
    {
        let mut stopped = self.ref_counter.disconnect_notice();
        futures_util::pin_mut!(stream);

        loop {
            if let future::Either::Left((Some(m), _)) =
                future::select(&mut stream.next(), &mut stopped).await
            {
                let res = self.send(m); // Bound to make it Sync
                if matches!(res.await.map(Into::into), Ok(KeepRunning::Yes)) {
                    continue;
                }
            }
            break;
        }
    }

    /// Converts this address into a [futures `Sink`](https://docs.rs/futures/0.3/futures/io/struct.Sink.html).
    pub fn into_sink(self) -> AddressSink<A, Rc> {
        AddressSink {
            sink: self.sender.clone().into_sink(),
            ref_counter: self.ref_counter.clone(),
        }
    }

    /// Waits until this address becomes disconnected.
    pub fn join(&self) -> impl Future<Output = ()> + Send + Unpin {
        self.ref_counter.disconnect_notice()
    }
}

// Required because #[derive] adds an A: Clone bound
impl<A, Rc: RefCounter> Clone for Address<A, Rc> {
    fn clone(&self) -> Self {
        Address {
            sender: self.sender.clone(),
            ref_counter: self.ref_counter.clone(),
        }
    }
}

// Drop impls cannot be specialised, so a little bit of fanagling is used in the RefCounter impl
impl<A, Rc: RefCounter> Drop for Address<A, Rc> {
    fn drop(&mut self) {
        // We should notify the ActorManager that there are no more strong Addresses and the actor
        // should be stopped.
        if self.ref_counter.is_last_strong() {
            let _ = self.sender.send(AddressMessage::LastAddress);
        }
    }
}

// Pointer identity for Address equality/comparison
impl<A, Rc: RefCounter, Rc2: RefCounter> PartialEq<Address<A, Rc2>> for Address<A, Rc> {
    fn eq(&self, other: &Address<A, Rc2>) -> bool {
        PartialEq::eq(&self.ref_counter.as_ptr(), &other.ref_counter.as_ptr())
    }
}

impl<A, Rc: RefCounter> Eq for Address<A, Rc> {}

impl<A, Rc: RefCounter, Rc2: RefCounter> PartialOrd<Address<A, Rc2>> for Address<A, Rc> {
    fn partial_cmp(&self, other: &Address<A, Rc2>) -> Option<Ordering> {
        PartialOrd::partial_cmp(&self.ref_counter.as_ptr(), &other.ref_counter.as_ptr())
    }
}
impl<A, Rc: RefCounter> Ord for Address<A, Rc> {
    fn cmp(&self, other: &Self) -> Ordering {
        Ord::cmp(&self.ref_counter.as_ptr(), &other.ref_counter.as_ptr())
    }
}
impl<A, Rc: RefCounter> Hash for Address<A, Rc> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        Hash::hash(&self.ref_counter.as_ptr(), state)
    }
}
