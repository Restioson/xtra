//! An address to an actor is a way to send it a message. An address allows an actor to be sent any
//! kind of message that it can receive.

use std::cmp::Ordering;
use std::fmt::{self, Debug, Formatter};
use std::future::Future;
use std::hash::{Hash, Hasher};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use event_listener::EventListener;
use futures_sink::Sink;
use futures_util::FutureExt;

use crate::inbox::Chan;
use crate::refcount::{Either, RefCounter, Strong, Weak};
use crate::send_future::ResolveToHandlerReturn;
use crate::{BroadcastFuture, Error, Handler, NameableSending, SendFuture};

/// An [`Address`] is a reference to an actor through which messages can be
/// sent. It can be cloned to create more addresses to the same actor.
/// By default (i.e without specifying the second type parameter, `Rc`, to be
/// [`Weak`], [`Address`]es are strong. Therefore, when all [`Address`]es
/// are dropped, the actor will be stopped. In other words, any existing [`Address`]es will inhibit
/// the dropping of an actor. If this is undesirable, then a [`WeakAddress`]
/// should be used instead. An address is created by calling the
/// [`Actor::create`](crate::Actor::create) or [`Context::run`](crate::Context::run)
/// methods, or by cloning another [`Address`].
///
/// ## Mailboxes
///
/// An actor has three mailboxes, each for a specific kind of message:
/// 1. The default priority, or ordered mailbox.
/// 2. The priority mailbox.
/// 3. The broadcast mailbox.
///
/// The first two mailboxes are shared between all actors on the same address, whilst each actor
/// has its own broadcast mailbox.
///
/// The actor's mailbox capacity applies severally to each mailbox. This means that an actor can
/// have a total of `cap` messages in every mailbox before it is totally full. However, it must only
/// have `cap` in a given mailbox for any sends to that mailbox to be caused to wait for free space
/// and thus exercise backpressure on senders.
///
/// ### Default priority
///
/// The default priority mailbox contains messages which will be handled in order of sending. This is
/// a special property, as the other two mailboxes do not preserve send order!
/// The vast majority of actor messages will probably be sent to this mailbox. They have default
/// priority - a message will be taken from this mailbox next only if the other two mailboxes are empty.
///
/// ### Priority
///
/// The priority mailbox contains messages which will be handled in order of their priority.This
/// can be used to make sure that critical maintenance tasks such as pings are handled as soon as
/// possible. Keep in mind that if this becomes full, attempting to send in a higher priority message
/// than the current highest will still result in waiting for at least one priority message
/// to be handled.
///
/// ### Broadcast
///
/// The broadcast mailbox contains messages which will be handled by every single actor, in order
/// of their priority. All actors must handle a message for it to be removed from the mailbox and
/// the length to decrease. This means that the backpressure provided by [`Address::broadcast`] will
/// wait for the slowest actor.
pub struct Address<A, Rc: RefCounter = Strong> {
    pub(crate) inner: Arc<Chan<A>>,
    pub(crate) rc: Rc,
}

impl<A, Rc: RefCounter> Debug for Address<A, Rc> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let act = std::any::type_name::<A>();
        f.debug_struct(&format!("Address<{}>", act))
            .field("rx_count", &self.inner.receiver_count())
            .field("tx_count", &self.inner.sender_count())
            .field("rc", &self.rc)
            .finish()
    }
}

/// A [`WeakAddress`] is a reference to an actor through which messages can be
/// sent. It can be cloned. Unlike [`Address`], a [`WeakAddress`] will not inhibit
/// the dropping of an actor. It is created by the [`Address::downgrade`]
/// method.
pub type WeakAddress<A> = Address<A, Weak>;

/// Functions which apply only to strong addresses (the default kind).
impl<A> Address<A, Strong> {
    /// Create a weak address to the actor. Unlike with the strong variety of address (this kind),
    /// an actor will not be prevented from being dropped if only weak sinks, channels, and
    /// addresses exist.
    pub fn downgrade(&self) -> WeakAddress<A> {
        Address {
            inner: self.inner.clone(),
            rc: Weak::new(&self.inner),
        }
    }
}

/// Functions which apply only to addresses which can either be strong or weak.
impl<A> Address<A, Either> {
    /// Converts this address into a weak address.
    pub fn downgrade(&self) -> WeakAddress<A> {
        Address {
            inner: self.inner.clone(),
            rc: Weak::new(&self.inner),
        }
    }
}

/// Functions which apply to any kind of address, be they strong or weak.
impl<A, Rc: RefCounter> Address<A, Rc> {
    /// Returns whether the actors referred to by this address are running and accepting messages.
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
    ///         ctx.stop_all();
    ///     }
    /// }
    ///
    /// # #[cfg(feature = "smol")]
    /// smol::block_on(async {
    ///     let addr = MyActor.create(None).spawn(&mut xtra::spawn::Smol::Global);
    ///     assert!(addr.is_connected());
    ///     addr.send(Shutdown).await;
    ///     smol::Timer::after(Duration::from_secs(1)).await; // Give it time to shut down
    ///     assert!(!addr.is_connected());
    /// })
    /// ```
    pub fn is_connected(&self) -> bool {
        self.inner.is_connected()
    }

    /// Returns the number of messages in the actor's mailbox. This will be the sum of broadcast
    /// messages, priority messages, and ordered messages. It can be up to three times the capacity,
    /// as the capacity is for each send type (broadcast, priority, and ordered).
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// The capacity of the actor's mailbox per send type (broadcast, priority, and ordered).
    pub fn capacity(&self) -> Option<usize> {
        self.inner.capacity()
    }

    /// Returns whether the actor's mailbox is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Convert this address into a generic address which can be weak or strong.
    pub fn as_either(&self) -> Address<A, Either> {
        Address {
            inner: self.inner.clone(),
            rc: self.rc.increment(&self.inner).into_either(),
        }
    }

    /// Send a message to the actor. The message will, by default, have a priority of 0 and be sent
    /// into the ordered queue. This can be configured through [`SendFuture::priority`].
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
        NameableSending<A, <A as Handler<M>>::Return>,
        ResolveToHandlerReturn,
    >
    where
        M: Send + 'static,
        A: Handler<M>,
    {
        SendFuture::sending_named(message, self.inner.clone())
    }

    /// Send a message to all actors on this address.
    ///
    /// For details, please see the documentation on [`BroadcastFuture`].
    pub fn broadcast<M>(&self, msg: M) -> BroadcastFuture<A, M>
    where
        M: Clone + Sync + Send + 'static,
        A: Handler<M, Return = ()>,
    {
        BroadcastFuture::new(msg, self.inner.clone())
    }

    /// Waits until this address becomes disconnected. Note that if this is called on a strong
    /// address, it will only ever trigger if the actor calls [`Context::stop_self`](crate::Context::stop_self),
    /// as the address would prevent the actor being dropped due to too few strong addresses.
    pub fn join(&self) -> ActorJoinHandle {
        ActorJoinHandle(self.inner.disconnect_listener())
    }

    /// Returns true if this address and the other address point to the same actor. This is
    /// distinct from the implementation of `PartialEq` as it ignores reference count type, which
    /// must be the same for `PartialEq` to return `true`.
    pub fn same_actor<Rc2: RefCounter>(&self, other: &Address<A, Rc2>) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }

    /// Converts this address into a sink that can be used to send messages to the actor. These
    /// messages will have default priority and will be handled in send order.
    ///
    /// When converting an [`Address`] into a [`Sink`], it is important to think about the address'
    /// reference counts. By default [`Address`]es are [`Strong`]. The [`Sink`] wraps the [`Address`]
    /// for its entire lifetime and will thus inherit the reference count type.
    ///
    /// If you are going to use [`Address::into_sink`] in combination with things like
    /// [`Stream::forward`](futures_util::stream::StreamExt::forward), bear in mind that a
    /// strong [`Address`] will keep the actor alive for as long as that
    /// [`Stream`](futures_util::stream::Stream) is being polled. Depending on your usecase, you
    /// may want to use a [`WeakAddress`] instead.
    ///
    /// Because [`Sink`]s do not return anything, this function is only available for messages with
    /// a [`Handler`] implementation that sets [`Return`](Handler::Return) to `()`.
    pub fn into_sink<M>(self) -> impl Sink<M, Error = Error>
    where
        A: Handler<M, Return = ()>,
        M: Send + 'static,
    {
        futures_util::sink::unfold((), move |(), message| self.send(message))
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
        Address {
            inner: self.inner.clone(),
            rc: self.rc.increment(&self.inner),
        }
    }
}

/// Determines whether this and the other address point to the same actor mailbox **and**
/// they have reference count type equality. This means that this will only return true if
/// [`Address::same_actor`] returns true **and** if they both have weak or strong reference
/// counts. [`Either`](crate::refcount::Either) will compare as whichever reference count type
/// it wraps.
impl<A, Rc: RefCounter, Rc2: RefCounter> PartialEq<Address<A, Rc2>> for Address<A, Rc> {
    fn eq(&self, other: &Address<A, Rc2>) -> bool {
        (self.same_actor(other)) && (self.rc.is_strong() == other.rc.is_strong())
    }
}

impl<A, Rc: RefCounter> Eq for Address<A, Rc> {}

/// Compare this address to another. This comparison has little semantic meaning, and is intended
/// to be used for comparison-based indexing only (e.g to allow addresses to be keys in binary search
/// trees). The pointer of the actors' mailboxes will be compared, and if they are equal, a strong
/// address will compare as greater than a weak one.
impl<A, Rc: RefCounter, Rc2: RefCounter> PartialOrd<Address<A, Rc2>> for Address<A, Rc> {
    fn partial_cmp(&self, other: &Address<A, Rc2>) -> Option<Ordering> {
        Some(
            match Arc::as_ptr(&self.inner).cmp(&Arc::as_ptr(&other.inner)) {
                Ordering::Equal => self.rc.is_strong().cmp(&other.rc.is_strong()),
                ord => ord,
            },
        )
    }
}

impl<A, Rc: RefCounter> Ord for Address<A, Rc> {
    fn cmp(&self, other: &Self) -> Ordering {
        Arc::as_ptr(&self.inner).cmp(&Arc::as_ptr(&other.inner))
    }
}

impl<A, Rc: RefCounter> Hash for Address<A, Rc> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write_usize(Arc::as_ptr(&self.inner) as usize);
        state.write_u8(self.rc.is_strong() as u8);
        state.finish();
    }
}

impl<A, Rc: RefCounter> Drop for Address<A, Rc> {
    fn drop(&mut self) {
        if self.rc.decrement(&self.inner) {
            self.inner.shutdown_waiting_receivers()
        }
    }
}
