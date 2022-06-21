//! A message channel is a channel through which you can send only one kind of message, but to
//! any actor that can handle it. It is like [`Address`](../address/struct.Address.html), but associated with
//! the message type rather than the actor type.

use std::fmt::Debug;

use crate::address::{ActorJoinHandle, Address, WeakAddress};
use crate::envelope::ReturningEnvelope;
use crate::inbox::SentMessage;
use crate::receiver::Receiver;
use crate::refcount::{RefCounter, Strong};
use crate::send_future::{ResolveToHandlerReturn, SendFuture};
use crate::{Handler, KeepRunning};
use futures_core::future::BoxFuture;
use futures_core::stream::BoxStream;

/// A message channel is a channel through which you can send only one kind of message, but to
/// any actor that can handle it. It is like [`Address`](../address/struct.Address.html), but associated with
/// the message type rather than the actor type. This trait represents *any kind of message channel*.
/// There are two traits which inherit from it - one for
/// [weak message channels](trait.WeakMessageChannel.html), and one for
/// [strong message channels](trait.StrongMessageChannel.html). Both of these traits may be
/// downcasted to this trait using their respective `downcast` methods. Therefore, this trait is
/// most useful when you want to be generic over both strong and weak message channels. If this is
/// undesireable or not needed, simply use their respective trait objects instead.
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
///     async fn stopped(self) -> Self::Stop {
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
///         let channels: [Box<dyn StrongMessageChannel<WhatsYourName, Return = &'static str>>; 2] = [
///             Box::new(Alice.create(None).spawn(&mut xtra::spawn::Smol::Global)),
///             Box::new(Bob.create(None).spawn(&mut xtra::spawn::Smol::Global))
///         ];
///         let name = ["Alice", "Bob"];
///         for (channel, name) in channels.iter().zip(&name) {
///             assert_eq!(*name, channel.send(WhatsYourName).await.unwrap());
///         }
///     })
/// }
/// ```
pub trait MessageChannel<M>:
    private::IsStrong + private::ToInnerPtr + Unpin + Debug + Send
{
    /// The return value of the handler for `M`.
    type Return: Send + 'static;
    /// Returns whether the actor referred to by this address is running and accepting messages.
    fn is_connected(&self) -> bool;

    /// Returns the number of messages in the actor's mailbox. Note that this does **not**
    /// differentiate between types of messages; it will return the count of all messages in the
    /// actor's mailbox, not only the messages sent by this message channel type.
    fn len(&self) -> usize;

    /// The total capacity of the actor's mailbox. Note that this does **not** differentiate between
    /// types of messages; it will return the total capacity of actor's mailbox, not only the
    /// messages sent by this message channel type
    fn capacity(&self) -> Option<usize>;

    /// Returns whether the actor's mailbox is empty.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Send a message to the actor.
    ///
    /// The actor must implement [`Handler<Message>`] for this to work.
    ///
    /// This function returns a [`Future`](SendFuture) that resolves to the [`Return`](crate::Handler::Return) value of the handler.
    /// The [`SendFuture`] will resolve to [`Err(Disconnected)`] in case the actor is stopped and not accepting messages.
    fn send(
        &self,
        message: M,
    ) -> SendFuture<Self::Return, BoxFuture<'static, Receiver<Self::Return>>, ResolveToHandlerReturn>;

    /// Attaches a stream to this channel such that all messages produced by it are forwarded to the
    /// actor. This could, for instance, be used to forward messages from a socket to the actor
    /// (after the messages have been appropriately `map`ped). This is a convenience method over
    /// explicitly forwarding a stream to this address, spawning that future onto the executor,
    /// and mapping the error away (because disconnects are expected and will simply mean that the
    /// stream is no longer being forwarded).
    ///
    /// **Note:** if this stream's continuation should prevent the actor from being dropped, this
    /// method should be called on [`MessageChannel`](trait.MessageChannel.html). Otherwise, it should be called
    /// on [`WeakMessageChannel`](trait.WeakMessageChannel.html).
    fn attach_stream(self, stream: BoxStream<M>) -> BoxFuture<()>
    where
        Self::Return: Into<KeepRunning> + Send;

    /// Clones this channel as a boxed trait object.
    fn clone_channel(&self) -> Box<dyn MessageChannel<M, Return = Self::Return>>;

    /// Waits until this address becomes disconnected.
    fn join(&self) -> ActorJoinHandle;

    /// Determines whether this and the other message channel address the same actor mailbox.
    fn same_actor(&self, other: &dyn MessageChannel<M, Return = Self::Return>) -> bool;

    /// Determines whether this and the other message channel address the same actor mailbox **and**
    /// they have reference count type equality. This means that this will only return true if
    /// [`MessageChannel::same_actor`] returns true **and** if they both have weak or strong reference
    /// counts. [`Either`](crate::refcount::Either) will compare as whichever reference count type
    /// it wraps.
    fn eq(&self, other: &dyn MessageChannel<M, Return = Self::Return>) -> bool;
}

/// A message channel is a channel through which you can send only one kind of message, but to
/// any actor that can handle it. It is like [`Address`](../address/struct.Address.html), but associated with
/// the message type rather than the actor type. Any existing `MessageChannel`s will prevent the
/// dropping of the actor. If this is undesirable, then the [`WeakMessageChannel`](trait.WeakMessageChannel.html)
/// struct should be used instead. A `StrongMessageChannel` trait object is created by casting a
/// strong [`Address`](../address/struct.Address.html).
pub trait StrongMessageChannel<M>: MessageChannel<M> {
    /// Create a weak message channel. Unlike with the strong variety of message channel (this kind),
    /// an actor will not be prevented from being dropped if only weak sinks, channels, and
    /// addresses exist.
    fn downgrade(&self) -> Box<dyn WeakMessageChannel<M, Return = Self::Return>>;

    /// Upcasts this strong message channel into a boxed generic
    /// [`MessageChannel`](trait.MessageChannel.html) trait object
    fn upcast(self) -> Box<dyn MessageChannel<M, Return = Self::Return>>;

    /// Upcasts this strong message channel into a reference to the generic
    /// [`MessageChannel`](trait.MessageChannel.html) trait object
    fn upcast_ref(&self) -> &dyn MessageChannel<M, Return = Self::Return>;

    /// Clones this channel as a boxed trait object.
    fn clone_channel(&self) -> Box<dyn StrongMessageChannel<M, Return = Self::Return>>;
}

/// A message channel is a channel through which you can send only one kind of message, but to
/// any actor that can handle it. It is like [`Address`](../address/struct.Address.html), but associated with
/// the message type rather than the actor type. Any existing `WeakMessageChannel`s will *not* prevent the
/// dropping of the actor. If this is undesirable, then  [`StrongMessageChannel`](trait.StrongMessageChannel.html)
/// should be used instead. A `WeakMessageChannel` trait object is created by calling
/// [`StrongMessageChannel::downgrade`](trait.StrongMessageChannel.html#method.downgrade) or by
/// casting a [`WeakAddress`](../address/type.WeakAddress.html).
pub trait WeakMessageChannel<M>: MessageChannel<M> {
    /// Upcasts this weak message channel into a boxed generic
    /// [`MessageChannel`](trait.MessageChannel.html) trait object
    fn upcast(self) -> Box<dyn MessageChannel<M, Return = Self::Return>>;

    /// Upcasts this weak message channel into a reference to the generic
    /// [`MessageChannel`](trait.MessageChannel.html) trait object
    fn upcast_ref(&self) -> &dyn MessageChannel<M, Return = Self::Return>;

    /// Clones this channel as a boxed trait object.
    fn clone_channel(&self) -> Box<dyn WeakMessageChannel<M, Return = Self::Return>>;
}

impl<A, R, M, Rc: RefCounter> MessageChannel<M> for Address<A, Rc>
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
    ) -> SendFuture<R, BoxFuture<'static, Receiver<R>>, ResolveToHandlerReturn> {
        let (envelope, rx) = ReturningEnvelope::<A, M, R>::new(message);
        let sending = self.0.send(SentMessage::Ordered(Box::new(envelope)));

        #[allow(clippy::async_yields_async)] // We only want to await the sending.
        SendFuture::sending_boxed(async move {
            match sending.await {
                Ok(()) => Receiver::new(rx),
                Err(_) => Receiver::disconnected(),
            }
        })
    }

    fn attach_stream(self, stream: BoxStream<M>) -> BoxFuture<()>
    where
        R: Into<KeepRunning> + Send,
    {
        Box::pin(Address::attach_stream(self, stream))
    }

    fn clone_channel(&self) -> Box<dyn MessageChannel<M, Return = Self::Return>> {
        Box::new(self.clone())
    }

    fn join(&self) -> ActorJoinHandle {
        self.join()
    }

    fn same_actor(&self, other: &dyn MessageChannel<M, Return = Self::Return>) -> bool {
        use private::ToInnerPtr;

        self.to_inner_ptr() == other.to_inner_ptr()
    }

    fn eq(&self, other: &dyn MessageChannel<M, Return = Self::Return>) -> bool {
        use private::IsStrong;

        MessageChannel::same_actor(self, other) && (self.is_strong() == other.is_strong())
    }
}

/// Contains crate-private traits to allow implementations of [`MessageChannel::same_actor`] and [`MessageChannel::eq`].
///
/// Both of these functions only operate on type-erased [`MessageChannel`]s and thus cannot access the underlying [`Address`].
mod private {
    use super::*;

    pub trait IsStrong {
        fn is_strong(&self) -> bool;
    }

    pub trait ToInnerPtr {
        fn to_inner_ptr(&self) -> *const ();
    }

    impl<A, Rc: RefCounter> IsStrong for Address<A, Rc> {
        fn is_strong(&self) -> bool {
            self.0.is_strong()
        }
    }

    impl<A, Rc: RefCounter> ToInnerPtr for Address<A, Rc> {
        fn to_inner_ptr(&self) -> *const () {
            self.0.inner_ptr() as *const ()
        }
    }
}

impl<A, M> StrongMessageChannel<M> for Address<A, Strong>
where
    A: Handler<M>,
    M: Send + 'static,
{
    fn downgrade(&self) -> Box<dyn WeakMessageChannel<M, Return = Self::Return>> {
        Box::new(self.downgrade())
    }

    /// Upcasts this strong message channel into a boxed generic
    /// [`MessageChannel`](trait.MessageChannel.html) trait object
    fn upcast(self) -> Box<dyn MessageChannel<M, Return = Self::Return>> {
        Box::new(self)
    }

    /// Upcasts this strong message channel into a reference to the generic
    /// [`MessageChannel`](trait.MessageChannel.html) trait object
    fn upcast_ref(&self) -> &dyn MessageChannel<M, Return = Self::Return> {
        self
    }

    fn clone_channel(&self) -> Box<dyn StrongMessageChannel<M, Return = Self::Return>> {
        Box::new(self.clone())
    }
}

impl<A, M> WeakMessageChannel<M> for WeakAddress<A>
where
    A: Handler<M>,
    M: Send + 'static,
{
    /// Upcasts this weak message channel into a boxed generic
    /// [`MessageChannel`](trait.MessageChannel.html) trait object
    fn upcast(self) -> Box<dyn MessageChannel<M, Return = Self::Return>> {
        Box::new(self)
    }

    /// Upcasts this weak message channel into a reference to the generic
    /// [`MessageChannel`](trait.MessageChannel.html) trait object
    fn upcast_ref(&self) -> &dyn MessageChannel<M, Return = Self::Return> {
        self
    }

    fn clone_channel(&self) -> Box<dyn WeakMessageChannel<M, Return = Self::Return>> {
        Box::new(self.clone())
    }
}
