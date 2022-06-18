//! An address to an actor is a way to send it a message. An address allows an actor to be sent any
//! kind of message that it can receive.

use std::cmp::Ordering;
use crate::envelope::ReturningEnvelope;
use crate::refcount::{Either, RefCounter, Strong, Weak};
use crate::send_future::ResolveToHandlerReturn;
use crate::{inbox, Handler, KeepRunning, NameableSending, SendFuture};
use event_listener::EventListener;
use futures_core::Stream;
use futures_util::{future, FutureExt, StreamExt};
use std::error::Error;
use std::fmt::{self, Debug, Display, Formatter};
use std::future::Future;
use std::hash::{Hash, Hasher};
use std::pin::Pin;
use std::task::{Context, Poll};

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
pub struct Address<A: 'static, Rc: RefCounter = Strong>(pub(crate) inbox::Sender<A, Rc>);

impl<A, Rc: RefCounter> Debug for Address<A, Rc> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Address").field(&self.0).finish()
    }
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
        Address(self.0.downgrade())
    }
}

/// Functions which apply only to addresses which can either be strong or weak.
impl<A> Address<A, Either> {
    /// Converts this address into a weak address.
    pub fn downgrade(&self) -> WeakAddress<A> {
        Address(self.0.downgrade())
    }
}

/// Functions which apply to any kind of address, be they strong or weak.
impl<A, Rc: RefCounter> Address<A, Rc> {
    /// Returns whether the actor referred to by this address is running and accepting messages.
    ///
    /// ```rust
    /// # use xtra::prelude::*;
    /// # use std::time::Duration;
    /// # struct MyActor;
    /// # #[async_trait] impl Actor for MyActor {type Stop = (); async fn stopped(self) -> Self::Stop {} }
    /// struct Shutdown;
    ///
    /// #[async_trait]
    /// impl Handler<Shutdown> for MyActor {
    ///     type Return = ();
    ///
    ///     async fn handle(&mut self, _: Shutdown, ctx: &mut Context<Self>) {
    ///         ctx.stop();
    ///     }
    /// }
    ///
    /// # #[cfg(feature = "with-smol-1")]
    /// smol::block_on(async {
    ///     let addr = MyActor.create(None).spawn(&mut xtra::spawn::Smol::Global);
    ///     assert!(addr.is_connected());
    ///     addr.send(Shutdown).await;
    ///     smol::Timer::after(Duration::from_secs(1)).await; // Give it time to shut down
    ///     assert!(!addr.is_connected());
    /// })
    /// ```
    pub fn is_connected(&self) -> bool {
        self.0.is_connected()
    }

    /// Returns the number of messages in the actor's mailbox.
    pub fn len(&self) -> usize {
        todo!("Len")
        //self.sink.len()
    }

    /// The total capacity of the actor's mailbox.
    pub fn capacity(&self) -> Option<usize> {
        self.0.capacity()
    }

    /// Returns whether the actor's mailbox is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Convert this address into a generic address which can be weak or strong.
    pub fn as_either(&self) -> Address<A, Either> {
        Address(self.0.clone().into_either_rc())
    }

    /// Send a message to the actor.
    ///
    /// The actor must implement [`Handler<Message>`] for this to work.
    ///
    /// This function returns a [`Future`](SendFuture) that resolves to the [`Return`](crate::Handler::Return) value of the handler.
    /// The [`SendFuture`] will resolve to [`Err(Disconnected)`] in case the actor is stopped and not accepting messages.
    #[allow(clippy::type_complexity)]
    pub fn send<M>(
        &self,
        message: M,
    ) -> SendFuture<
        <A as Handler<M>>::Return,
        NameableSending<A, <A as Handler<M>>::Return, Rc>,
        ResolveToHandlerReturn,
    >
    where
        A: Handler<M>,
        M: Send + 'static,
    {
        let (envelope, rx) = ReturningEnvelope::<A, M, <A as Handler<M>>::Return>::new(message);
        let tx = self.0.send(Box::new(envelope));
        SendFuture::sending_named(tx, rx)
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
        let mut stopped = self.join();
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

    /// Waits until this address becomes disconnected.
    pub fn join(&self) -> ActorJoinHandle {
        ActorJoinHandle(self.0.disconnect_notice())
    }

    /// Returns true if this address and the other address point to the same actor. This is
    /// distinct from the implementation of `PartialEq` as it ignores reference count type, which
    /// must be the same for `PartialEq` to return `true`.
    pub fn same_actor<Rc2: RefCounter>(&self, other: &Address<A, Rc2>) -> bool {
        self.0.inner_ptr() == other.0.inner_ptr()
    }
}

/// A future which will complete when the corresponding actor stops and its address becomes
/// disconnected.
#[must_use = "Futures do nothing unless polled"]
pub struct ActorJoinHandle(Option<EventListener>);

impl Future for ActorJoinHandle {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match self.0.take() {
            Some(mut listener) => match listener.poll_unpin(cx) {
                Poll::Ready(()) => Poll::Ready(()),
                Poll::Pending => {
                    self.0 = Some(listener);
                    Poll::Pending
                }
            },
            None => Poll::Ready(()),
        }
    }
}

// Required because #[derive] adds an A: Clone bound
impl<A, Rc: RefCounter> Clone for Address<A, Rc> {
    fn clone(&self) -> Self {
        Address(self.0.clone())
    }
}

/// Determines whether this and the other message channel address the same actor mailbox **and**
/// they have reference count type equality. This means that this will only return true if
/// [`MessageChannel::same_actor`] returns true **and** if they both have weak or strong reference
/// counts. [`Either`](crate::refcount::Either) will compare as whichever reference count type
/// it wraps.
impl<A, Rc: RefCounter, Rc2: RefCounter> PartialEq<Address<A, Rc2>> for Address<A, Rc> {
    fn eq(&self, other: &Address<A, Rc2>) -> bool {
        (self.same_actor(other)) && (self.0.is_strong() == other.0.is_strong())
    }
}

impl<A, Rc: RefCounter> Eq for Address<A, Rc> {}

/// TODO(doc)
impl<A, Rc: RefCounter, Rc2: RefCounter> PartialOrd<Address<A, Rc2>> for Address<A, Rc> {
    fn partial_cmp(&self, other: &Address<A, Rc2>) -> Option<Ordering> {
        Some(match self.0.inner_ptr().cmp(&other.0.inner_ptr()) {
            Ordering::Equal => self.0.is_strong().cmp(&other.0.is_strong()),
            ord => ord,
        })
    }
}

impl<A, Rc: RefCounter> Ord for Address<A, Rc> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.inner_ptr().cmp(&other.0.inner_ptr())
    }
}

impl<A, Rc: RefCounter> Hash for Address<A, Rc> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write_usize(self.0.inner_ptr() as *const _ as usize);
        state.write_u8(self.0.is_strong() as u8);
        state.finish();
    }
}
