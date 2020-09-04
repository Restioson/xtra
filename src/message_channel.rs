//! A message channel is a channel through which you can send only one kind of message, but to
//! any actor that can handle it. It is like [`Address`](../struct.Address.html), but associated with
//! the message type rather than the actor type.

use crate::address::{MessageResponseFuture, Strong, RefCounter};
use crate::*;
#[cfg(any(
    doc,
    feature = "with-tokio-0_2",
    feature = "with-async_std-1",
    feature = "with-wasm_bindgen-0_2",
    feature = "with-smol-0_4"
))]
use futures::{FutureExt, Stream, StreamExt, Sink};

/// A message channel is a channel through which you can send only one kind of message, but to
/// any actor that can handle it. It is like [`Address`](../struct.Address.html), but associated with
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
/// impl Message for WhatsYourName {
///     type Result = &'static str;
/// }
///
/// struct Alice;
/// struct Bob;
///
/// impl Actor for Alice {}
/// impl Actor for Bob {}
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
///     smol::block_on(async {
///         let channels: [Box<dyn MessageChannel<WhatsYourName>>; 2] = [
///             Box::new(Alice.spawn()).upcast(),
///             Box::new(Bob.spawn()).upcast()
///         ];
///         let name = ["Alice", "Bob"];
///         for (channel, name) in channels.iter().zip(&name) {
///             assert_eq!(*name, channel.send(WhatsYourName).await.unwrap());
///         }
///     })
/// }
/// ```
pub trait MessageChannel<M: Message>: Unpin + Send + Sync {
    /// Returns whether the actor referred to by this address is running and accepting messages.
    fn is_connected(&self) -> bool;

    /// Sends a [`Message`](trait.Message.html) to the actor, and does not wait for a response.
    /// If this returns `Err(Disconnected)`, then the actor is stopped and not accepting messages.
    /// If this returns `Ok(())`, the will be delivered, but may not be handled in the event that the
    /// actor stops itself (by calling [`Context::stop`](struct.Context.html#method.stop))
    /// before it was handled.
    fn do_send(&self, message: M) -> Result<(), Disconnected>;

    /// Sends a [`Message`](trait.Message.html) to the actor, and waits for a response. If this
    /// returns `Err(Disconnected)`, then the actor is stopped and not accepting messages.
    fn send(&self, message: M) -> MessageResponseFuture<M>;

    /// Attaches a stream to this channel such that all messages produced by it are forwarded to the
    /// actor. This could, for instance, be used to forward messages from a socket to the actor
    /// (after the messages have been appropriately `map`ped). This is a convenience method over
    /// explicitly forwarding a stream to this address, spawning that future onto the executor,
    /// and mapping the error away (because disconnects are expected and will simply mean that the
    /// stream is no longer being forwarded).
    ///
    /// **Note:** if this stream's continuation should prevent the actor from being dropped, this
    /// method should be called on [`MessageChannel`](struct.MessageChannel.html). Otherwise, it should be called
    /// on [`WeakMessageChannel`](struct.WeakMessageChannel.html).
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
    #[cfg_attr(docsrs, doc(cfg(feature = "with-smol-0_4")))]
    fn attach_stream<S>(self, stream: S)
        where
            S: Stream<Item = M> + Send + Unpin + 'static,
            M::Result: Into<KeepRunning> + Send,
            Self: Sized + Send + Sink<M, Error = Disconnected> + 'static,
    {
        let fut = stream.map(|i| Ok(i)).forward(self).map(|_| ());
        crate::spawn(fut);
    }
}

/// A message channel is a channel through which you can send only one kind of message, but to
/// any actor that can handle it. It is like [`Address`](struct.Address.html), but associated with
/// the message type rather than the actor type. Any existing `MessageChannel`s will prevent the
/// dropping of the actor. If this is undesirable, then the [`WeakMessageChannel`](struct.WeakMessageChannel.html)
/// struct should be used instead. A `StrongMessageChannel` trait object is created by casting a
/// strong [`Address`](struct.Address.html).
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
}

/// A message channel is a channel through which you can send only one kind of message, but to
/// any actor that can handle it. It is like [`Address`](struct.Address.html), but associated with
/// the message type rather than the actor type. Any existing `WeakMessageChannel`s will *not* prevent the
/// dropping of the actor. If this is undesirable, then  [`StrongMessageChannel`](trait.StrongMessageChannel.html)
/// should be used instead. A `WeakMessageChannel` trait object is created by calling
/// [`StrongMessageChannel::downgrade`](trait.StrongMessageChannel.html#method.downgrade) or by
/// casting a [`WeakAddress`](struct.WeakAddress.html).
pub trait WeakMessageChannel<M: Message>: MessageChannel<M> {
    /// Upcasts this weak message channel into a boxed generic
    /// [`MessageChannel`](trait.MessageChannel.html) trait object
    fn upcast(self) -> Box<dyn MessageChannel<M>>;

    /// Upcasts this weak message channel into a reference to the generic
    /// [`MessageChannel`](trait.MessageChannel.html) trait object
    fn upcast_ref(&self) -> &dyn MessageChannel<M>;
}

impl<A, M: Message, Rc: RefCounter> MessageChannel<M> for Address<A, Rc>
    where A: Handler<M>,
{
    fn is_connected(&self) -> bool {
        self.is_connected()
    }

    fn do_send(&self, message: M) -> Result<(), Disconnected> {
        self.do_send(message)
    }

    fn send(&self, message: M) -> MessageResponseFuture<M> {
        self.send(message)
    }
}

impl<A, M: Message> StrongMessageChannel<M> for Address<A, Strong>
    where A: Handler<M>
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
}

impl<A, M: Message> WeakMessageChannel<M> for WeakAddress<A> where A: Handler<M> {
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
}
