//! A message channel is a channel through which you can send only one kind of message, but to
//! any actor that can handle it. It is like [`Address`](../address/struct.Address.html), but associated with
//! the message type rather than the actor type.

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use catty::Receiver;
use futures_core::future::BoxFuture;
use futures_core::stream::BoxStream;

use crate::address::{self, Address, Disconnected, WeakAddress};
use crate::envelope::ReturningEnvelope;
use crate::manager::AddressMessage;
use crate::private::Sealed;
use crate::refcount::{RefCounter, Shared, Strong};
use crate::sink::{AddressSink, MessageSink, StrongMessageSink, WeakMessageSink};
use crate::{Handler, KeepRunning, Message};

/// The future returned [`MessageChannel::send`](trait.MessageChannel.html#method.send).
/// It resolves to `Result<M::Result, Disconnected>`.
#[must_use]
pub struct SendFuture<M: Message>(SendFutureInner<M>);

enum SendFutureInner<M: Message> {
    Disconnected,
    Result(Receiver<M::Result>),
}

impl<M: Message> Future for SendFuture<M> {
    type Output = Result<M::Result, Disconnected>;

    fn poll(self: Pin<&mut Self>, ctx: &mut Context) -> Poll<Self::Output> {
        match &mut self.get_mut().0 {
            SendFutureInner::Disconnected => Poll::Ready(Err(Disconnected)),
            SendFutureInner::Result(rx) => address::poll_rx(rx, ctx),
        }
    }
}

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
/// # use xtra::spawn::Smol;
/// # use std::time::Duration;
/// struct WhatsYourName;
///
/// impl Message for WhatsYourName {
///     type Result = &'static str;
/// }
///
/// struct Alice;
/// struct Bob;
///
/// #[async_trait::async_trait]
/// impl Actor for Alice {
///     type Stop = ();
///     async fn stopped(self) -> Self::Stop {
///         println!("Oh no");
///     }
/// }
/// # #[async_trait::async_trait] impl Actor for Bob {type Stop = (); async fn stopped(self) -> Self::Stop {} }
///
/// #[async_trait::async_trait]
/// impl Handler<WhatsYourName> for Alice {
///     async fn handle(&mut self, _: WhatsYourName, _ctx: &mut Context<Self>) -> &'static str {
///         "Alice"
///     }
/// }
///
/// #[async_trait::async_trait]
/// impl Handler<WhatsYourName> for Bob {
///     async fn handle(&mut self, _: WhatsYourName, _ctx: &mut Context<Self>) -> &'static str {
///         "Bob"
///     }
/// }
///
/// fn main() {
/// smol::block_on(async {
///         let channels: [Box<dyn StrongMessageChannel<WhatsYourName>>; 2] = [
///             Box::new(Alice.create(None).spawn(&mut Smol::Global)),
///             Box::new(Bob.create(None).spawn(&mut Smol::Global))
///         ];
///         let name = ["Alice", "Bob"];
///         for (channel, name) in channels.iter().zip(&name) {
///             assert_eq!(*name, channel.send(WhatsYourName).await.unwrap());
///         }
///     })
/// }
/// ```
pub trait MessageChannel<M: Message>: Sealed + Unpin + Send + Sync {
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

    /// Send a [`Message`](../trait.Message.html) to the actor without waiting for a response.
    /// If this returns `Err(Disconnected)`, then the actor is stopped and not accepting messages.
    /// If this returns `Ok(())`, the will be delivered, but may not be handled in the event that the
    /// actor stops itself (by calling [`Context::stop`](../struct.Context.html#method.stop))
    /// before it was handled.
    fn do_send(&self, message: M) -> Result<(), Disconnected>;

    /// Send a [`Message`](../trait.Message.html) to the actor and asynchronously wait for a response. If this
    /// returns `Err(Disconnected)`, then the actor is stopped and not accepting messages. This,
    /// unlike [`Address::send`](../address/struct.Address.html#method.send) will block if the actor's mailbox
    /// is full. If this is undesired, consider using a [`MessageSink`](../sink/trait.MessageSink.html).
    fn send(&self, message: M) -> SendFuture<M>;

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
        M::Result: Into<KeepRunning> + Send;

    /// Clones this channel as a boxed trait object.
    fn clone_channel(&self) -> Box<dyn MessageChannel<M>>;

    /// Use this message channel as [a futures `Sink`](https://docs.rs/futures/0.3/futures/io/struct.Sink.html)
    /// and asynchronously send messages through it.
    fn sink(&self) -> Box<dyn MessageSink<M>>;

    /// Determines whether this and the other message channel address the same actor mailbox.
    fn eq(&self, other: &dyn MessageChannel<M>) -> bool;

    /// This is an internal method and should never be called manually.
    #[doc(hidden)]
    fn _ref_counter_eq(&self, other: *const Shared) -> bool;
}

/// A message channel is a channel through which you can send only one kind of message, but to
/// any actor that can handle it. It is like [`Address`](../address/struct.Address.html), but associated with
/// the message type rather than the actor type. Any existing `MessageChannel`s will prevent the
/// dropping of the actor. If this is undesirable, then the [`WeakMessageChannel`](trait.WeakMessageChannel.html)
/// struct should be used instead. A `StrongMessageChannel` trait object is created by casting a
/// strong [`Address`](../address/struct.Address.html).
pub trait StrongMessageChannel<M: Message>: MessageChannel<M> {
    /// Create a weak message channel. Unlike with the strong variety of message channel (this kind),
    /// an actor will not be prevented from being dropped if only weak sinks, channels, and
    /// addresses exist.
    fn downgrade(&self) -> Box<dyn WeakMessageChannel<M>>;

    /// Upcasts this strong message channel into a boxed generic
    /// [`MessageChannel`](trait.MessageChannel.html) trait object
    fn upcast(self) -> Box<dyn MessageChannel<M>>;

    /// Upcasts this strong message channel into a reference to the generic
    /// [`MessageChannel`](trait.MessageChannel.html) trait object
    fn upcast_ref(&self) -> &dyn MessageChannel<M>;

    /// Clones this channel as a boxed trait object.
    fn clone_channel(&self) -> Box<dyn StrongMessageChannel<M>>;

    /// Use this message channel as [a futures `Sink`](https://docs.rs/futures/0.3/futures/io/struct.Sink.html)
    /// and asynchronously send messages through it.
    fn sink(&self) -> Box<dyn StrongMessageSink<M>>;
}

/// A message channel is a channel through which you can send only one kind of message, but to
/// any actor that can handle it. It is like [`Address`](../address/struct.Address.html), but associated with
/// the message type rather than the actor type. Any existing `WeakMessageChannel`s will *not* prevent the
/// dropping of the actor. If this is undesirable, then  [`StrongMessageChannel`](trait.StrongMessageChannel.html)
/// should be used instead. A `WeakMessageChannel` trait object is created by calling
/// [`StrongMessageChannel::downgrade`](trait.StrongMessageChannel.html#method.downgrade) or by
/// casting a [`WeakAddress`](../address/type.WeakAddress.html).
pub trait WeakMessageChannel<M: Message>: MessageChannel<M> {
    /// Upcasts this weak message channel into a boxed generic
    /// [`MessageChannel`](trait.MessageChannel.html) trait object
    fn upcast(self) -> Box<dyn MessageChannel<M>>;

    /// Upcasts this weak message channel into a reference to the generic
    /// [`MessageChannel`](trait.MessageChannel.html) trait object
    fn upcast_ref(&self) -> &dyn MessageChannel<M>;

    /// Clones this channel as a boxed trait object.
    fn clone_channel(&self) -> Box<dyn WeakMessageChannel<M>>;

    /// Use this message channel as [a futures `Sink`](https://docs.rs/futures/0.3/futures/io/struct.Sink.html)
    /// and asynchronously send messages through it.
    fn sink(&self) -> Box<dyn WeakMessageSink<M>>;
}

impl<A, M: Message, Rc: RefCounter> MessageChannel<M> for Address<A, Rc>
where
    A: Handler<M>,
{
    fn is_connected(&self) -> bool {
        self.is_connected()
    }

    fn len(&self) -> usize {
        self.len()
    }

    fn capacity(&self) -> Option<usize> {
        self.capacity()
    }

    fn do_send(&self, message: M) -> Result<(), Disconnected> {
        self.do_send(message)
    }

    fn send(&self, message: M) -> SendFuture<M> {
        if self.is_connected() {
            let (envelope, rx) = ReturningEnvelope::<A, M>::new(message);
            let _ = self
                .sender
                .send(AddressMessage::Message(Box::new(envelope)));
            SendFuture(SendFutureInner::Result(rx))
        } else {
            SendFuture(SendFutureInner::Disconnected)
        }
    }

    fn attach_stream(self, stream: BoxStream<M>) -> BoxFuture<()>
    where
        M::Result: Into<KeepRunning> + Send,
    {
        Box::pin(self.attach_stream(stream))
    }

    fn clone_channel(&self) -> Box<dyn MessageChannel<M>> {
        Box::new(self.clone())
    }

    fn sink(&self) -> Box<dyn MessageSink<M>> {
        Box::new(AddressSink {
            sink: self.sender.clone().into_sink(),
            ref_counter: self.ref_counter.clone(),
        })
    }

    fn eq(&self, other: &dyn MessageChannel<M>) -> bool {
        other._ref_counter_eq(self.ref_counter.as_ptr())
    }

    fn _ref_counter_eq(&self, other: *const Shared) -> bool {
        self.ref_counter.as_ptr() == other
    }
}

impl<A, M: Message> StrongMessageChannel<M> for Address<A, Strong>
where
    A: Handler<M>,
{
    fn downgrade(&self) -> Box<dyn WeakMessageChannel<M>> {
        Box::new(self.downgrade())
    }

    /// Upcasts this strong message channel into a boxed generic
    /// [`MessageChannel`](trait.MessageChannel.html) trait object
    fn upcast(self) -> Box<dyn MessageChannel<M>> {
        Box::new(self)
    }

    /// Upcasts this strong message channel into a reference to the generic
    /// [`MessageChannel`](trait.MessageChannel.html) trait object
    fn upcast_ref(&self) -> &dyn MessageChannel<M> {
        self
    }

    fn clone_channel(&self) -> Box<dyn StrongMessageChannel<M>> {
        Box::new(self.clone())
    }

    fn sink(&self) -> Box<dyn StrongMessageSink<M>> {
        Box::new(AddressSink {
            sink: self.sender.clone().into_sink(),
            ref_counter: self.ref_counter.clone(),
        })
    }
}

impl<A, M: Message> WeakMessageChannel<M> for WeakAddress<A>
where
    A: Handler<M>,
{
    /// Upcasts this weak message channel into a boxed generic
    /// [`MessageChannel`](trait.MessageChannel.html) trait object
    fn upcast(self) -> Box<dyn MessageChannel<M>> {
        Box::new(self)
    }

    /// Upcasts this weak message channel into a reference to the generic
    /// [`MessageChannel`](trait.MessageChannel.html) trait object
    fn upcast_ref(&self) -> &dyn MessageChannel<M> {
        self
    }

    fn clone_channel(&self) -> Box<dyn WeakMessageChannel<M>> {
        Box::new(self.clone())
    }

    fn sink(&self) -> Box<dyn WeakMessageSink<M>> {
        Box::new(AddressSink {
            sink: self.sender.clone().into_sink(),
            ref_counter: self.ref_counter.clone(),
        })
    }
}
