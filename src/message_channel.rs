//! A message channel is a channel through which you can send only one kind of message, but to
//! any actor that can handle it. It is like [`Address`], but associated with
//! the message type rather than the actor type.

use std::fmt::Debug;

use futures_core::future::BoxFuture;
use futures_core::stream::BoxStream;

use crate::address::{ActorJoinHandle, Address};
use crate::envelope::ReturningEnvelope;
use crate::inbox::{PriorityMessageToOne, SentMessage};
use crate::refcount::{Either, RefCounter, Strong, Weak};
use crate::send_future::{ActorErasedSending, ResolveToHandlerReturn, SendFuture};
use crate::{Handler, KeepRunning};

/// A message channel is a channel through which you can send only one kind of message, but to
/// any actor that can handle it. It is like [`Address`], but associated with
/// the message type rather than the actor type.
///
/// # Example
///
/// ```rust
/// # use xtra::prelude::*;
/// # use smol::Timer;
/// # use std::time::Duration;
/// struct WhatsYourName;
///
/// struct Alice;
/// struct Bob;
///
/// #[async_trait]
/// impl Actor for Alice {
///     type Stop = ();
///     async fn stopped(self) {
///         println!("Oh no");
///     }
/// }
/// # #[async_trait] impl Actor for Bob {type Stop = (); async fn stopped(self) -> Self::Stop {} }
///
/// #[async_trait]
/// impl Handler<WhatsYourName> for Alice {
///     type Return = &'static str;
///
///     async fn handle(&mut self, _: WhatsYourName, _ctx: &mut Context<Self>) -> Self::Return {
///         "Alice"
///     }
/// }
///
/// #[async_trait]
/// impl Handler<WhatsYourName> for Bob {
///     type Return = &'static str;
///
///     async fn handle(&mut self, _: WhatsYourName, _ctx: &mut Context<Self>) -> Self::Return {
///         "Bob"
///     }
/// }
///
/// fn main() {
/// # #[cfg(feature = "with-smol-1")]
/// smol::block_on(async {
///         let  alice = Alice.create(None).spawn(&mut xtra::spawn::Smol::Global);
///         let  bob = Bob.create(None).spawn(&mut xtra::spawn::Smol::Global);
///
///         let channels: [MessageChannel<WhatsYourName, &'static str>; 2] = [
///             MessageChannel::new(&alice),
///             MessageChannel::new(&bob)
///         ];
///         let name = ["Alice", "Bob"];
///
///         for (channel, name) in channels.iter().zip(&name) {
///             assert_eq!(*name, channel.send(WhatsYourName).await.unwrap());
///         }
///     })
/// }
/// ```
pub struct MessageChannel<M, R, Rc = Strong> {
    inner: Box<dyn MessageChannelTrait<M, Rc, Return = R> + Send + Sync + 'static>,
}

impl<M, Rc, R> MessageChannel<M, R, Rc>
where
    M: Send + 'static,
    R: Send + 'static,
{
    /// TODO(docs)
    pub fn new<A>(address: &Address<A, Rc>) -> Self
    where
        A: Handler<M, Return = R>,
        Rc: RefCounter,
    {
        Self {
            inner: Box::new(address.clone()),
        }
    }

    /// Returns whether the actor referred to by this address is running and accepting messages.
    pub fn is_connected(&self) -> bool {
        self.inner.is_connected()
    }

    /// Returns the number of messages in the actor's mailbox. Note that this does **not**
    /// differentiate between types of messages; it will return the count of all messages in the
    /// actor's mailbox, not only the messages sent by this message channel type.
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// The total capacity of the actor's mailbox. Note that this does **not** differentiate between
    /// types of messages; it will return the total capacity of actor's mailbox, not only the
    /// messages sent by this message channel type
    pub fn capacity(&self) -> Option<usize> {
        self.inner.capacity()
    }

    /// Returns whether the actor's mailbox is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Send a message to the actor.
    ///
    /// The actor must implement [`Handler<Message>`] for this to work.
    ///
    /// This function returns a [`Future`](SendFuture) that resolves to the [`Return`](crate::Handler::Return) value of the handler.
    /// The [`SendFuture`] will resolve to [`Err(Disconnected)`] in case the actor is stopped and not accepting messages.
    pub fn send(&self, message: M) -> SendFuture<R, ActorErasedSending<R>, ResolveToHandlerReturn> {
        self.inner.send(message)
    }

    /// Attaches a stream to this channel such that all messages produced by it are forwarded to the
    /// actor. This could, for instance, be used to forward messages from a socket to the actor
    /// (after the messages have been appropriately `map`ped). This is a convenience method over
    /// explicitly forwarding a stream to this address, spawning that future onto the executor,
    /// and mapping the error away (because disconnects are expected and will simply mean that the
    /// stream is no longer being forwarded).
    pub fn attach_stream(self, stream: BoxStream<M>) -> BoxFuture<()>
    where
        R: Into<KeepRunning> + Send,
    {
        todo!()
    }

    /// Waits until this [`MessageChannel`] becomes disconnected.
    pub fn join(&self) -> ActorJoinHandle {
        self.inner.join()
    }

    /// Determines whether this and the other message channel address the same actor mailbox.
    pub fn same_actor<Rc2>(&self, other: &MessageChannel<M, R, Rc2>) -> bool
    where
        Rc2: Send + 'static,
    {
        self.inner.to_inner_ptr() == other.inner.to_inner_ptr()
    }
}

/// Determines whether this and the other message channel address the same actor mailbox **and**
/// they have reference count type equality. This means that this will only return true if
/// [`MessageChannel::same_actor`] returns true **and** if they both have weak or strong reference
/// counts. [`Either`](crate::refcount::Either) will compare as whichever reference count type
/// it wraps.
impl<M, R, Rc> PartialEq for MessageChannel<M, R, Rc> {
    fn eq(&self, other: &Self) -> bool {
        todo!()
    }
}

impl<M, R, Rc> Clone for MessageChannel<M, R, Rc>
where
    R: Send + 'static,
{
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone_channel(),
        }
    }
}

impl<M, R> MessageChannel<M, R, Strong> {
    /// TODO(docs)
    pub fn downgrade(&self) -> MessageChannel<M, R, Weak> {
        todo!()
    }
}

impl<M, R> MessageChannel<M, R, Either> {
    /// TODO(docs)
    pub fn downgrade(&self) -> MessageChannel<M, R, Weak> {
        todo!()
    }
}

trait MessageChannelTrait<M, Rc> {
    type Return: Send + 'static;

    fn is_connected(&self) -> bool;

    fn len(&self) -> usize;

    fn capacity(&self) -> Option<usize>;

    fn send(
        &self,
        message: M,
    ) -> SendFuture<Self::Return, ActorErasedSending<Self::Return>, ResolveToHandlerReturn>;

    fn attach_stream(self, stream: BoxStream<M>) -> BoxFuture<()>
    where
        Self::Return: Into<KeepRunning> + Send;

    fn clone_channel(
        &self,
    ) -> Box<dyn MessageChannelTrait<M, Rc, Return = Self::Return> + Send + Sync + 'static>;

    fn join(&self) -> ActorJoinHandle;

    fn to_inner_ptr(&self) -> *const ();
}

impl<A, R, M, Rc: RefCounter> MessageChannelTrait<M, Rc> for Address<A, Rc>
where
    A: Handler<M, Return = R>,
    M: Send + 'static,
    R: Send + 'static,
{
    type Return = R;

    fn is_connected(&self) -> bool {
        self.is_connected()
    }

    fn len(&self) -> usize {
        self.len()
    }

    fn capacity(&self) -> Option<usize> {
        self.capacity()
    }

    fn send(
        &self,
        message: M,
    ) -> SendFuture<R, ActorErasedSending<Self::Return>, ResolveToHandlerReturn> {
        let (envelope, rx) = ReturningEnvelope::<A, M, R>::new(message);
        let msg = PriorityMessageToOne::new(0, Box::new(envelope));
        let sending = self.0.send(SentMessage::ToOneActor(msg));

        #[allow(clippy::async_yields_async)] // We only want to await the sending.
        SendFuture::sending_erased(sending, rx)
    }

    fn attach_stream(self, stream: BoxStream<M>) -> BoxFuture<()>
    where
        R: Into<KeepRunning> + Send,
    {
        Box::pin(Address::attach_stream(self, stream))
    }

    fn clone_channel(
        &self,
    ) -> Box<dyn MessageChannelTrait<M, Rc, Return = Self::Return> + Send + Sync + 'static> {
        Box::new(self.clone())
    }

    fn join(&self) -> ActorJoinHandle {
        self.join()
    }

    fn to_inner_ptr(&self) -> *const () {
        self.0.inner_ptr() as *const ()
    }
}
