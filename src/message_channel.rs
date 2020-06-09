use crate::address::MessageResponseFuture;
use crate::envelope::AddressEnvelope;
use crate::{Disconnected, Message};
use futures::task::{Context, Poll};
use futures::Sink;
#[cfg(any(
    doc,
    feature = "with-tokio-0_2",
    feature = "with-async_std-1",
    feature = "with-wasm_bindgen-0_2",
    feature = "with-smol-0_1"
))]
use futures::{FutureExt, Stream, StreamExt};
use std::pin::Pin;

/// General trait for any kind of channel of messages, be it strong or weak. This trait contains all
/// functions of the channel.
pub trait MessageChannelExt<M: Message> {
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
        feature = "with-smol-0_1"
    ))]
    #[cfg_attr(nightly, doc(cfg(feature = "with-tokio-0_2")))]
    #[cfg_attr(nightly, doc(cfg(feature = "with-async_std-1")))]
    #[cfg_attr(nightly, doc(cfg(feature = "with-wasm_bindgen-0_2")))]
    #[cfg_attr(nightly, doc(cfg(feature = "with-smol-0_1")))]
    fn attach_stream<S>(self, stream: S)
    where
        S: Stream<Item = M> + Send + Unpin + 'static,
        Self: Sized + Send + Sink<M, Error = Disconnected> + 'static;
}

/// A message channel is a channel through which you can send only one kind of message, but to
/// any actor that can handle it. It is like [`Address`](struct.Address.html), but associated with
/// the message type rather than the actor type. Any existing `MessageChannel`s will prevent the
/// dropping of the actor. If this is undesirable, then the [`WeakMessageChannel`](struct.WeakMessageChannel.html)
/// struct should be used instead. This struct is created by calling
/// [`Address::channel`](struct.Address.html#method.channel),
/// [`Address::into_channel`](struct.Address.html#method.into_channel), or the similar methods on
/// [`WeakAddress`](struct.WeakAddress.html).
pub struct MessageChannel<M: Message> {
    pub(crate) address: Box<dyn AddressEnvelope<M>>,
}

impl<M: Message> MessageChannel<M> {
    /// Create a weak message channel to the actor. Unlike with the strong variety of channel (this kind),
    /// an actor will not be prevented from being dropped if only weak channels exist.
    pub fn downgrade(&self) -> WeakMessageChannel<M> {
        WeakMessageChannel {
            address: self.address.downgrade(),
        }
    }

    /// Converts this message channel into a weak channel to the actor. Unlike with the strong variety of
    /// channel (this kind), an actor will not be prevented from being dropped if only weak channels
    /// exist.
    pub fn into_downgraded(self) -> WeakMessageChannel<M> {
        self.downgrade()
    }
}

impl<M: Message> MessageChannelExt<M> for MessageChannel<M> {
    fn is_connected(&self) -> bool {
        self.address.is_connected()
    }

    fn do_send(&self, message: M) -> Result<(), Disconnected> {
        self.address.do_send(message)
    }

    fn send(&self, message: M) -> MessageResponseFuture<M> {
        self.address.send(message)
    }

    #[cfg(any(
        doc,
        feature = "with-tokio-0_2",
        feature = "with-async_std-1",
        feature = "with-wasm_bindgen-0_2",
        feature = "with-smol-0_1"
    ))]
    fn attach_stream<S>(self, stream: S)
    where
        S: Stream<Item = M> + Send + Unpin + 'static,
        Self: Sized + Send + Sink<M, Error = Disconnected> + 'static,
    {
        let fut = stream.map(|i| Ok(i)).forward(self).map(|_| ());

        #[cfg(feature = "with-async_std-1")]
        async_std::task::spawn(fut);

        #[cfg(feature = "with-tokio-0_2")]
        tokio::spawn(fut);

        #[cfg(feature = "with-wasm_bindgen-0_2")]
        wasm_bindgen_futures::spawn_local(fut);

        #[cfg(feature = "with-smol-0_1")]
        smol::Task::spawn(fut).detach();
    }
}

impl<M: Message> Sink<M> for MessageChannel<M> {
    type Error = Disconnected;

    fn poll_ready(self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.get_mut().address).poll_ready(ctx)
    }

    fn start_send(self: Pin<&mut Self>, message: M) -> Result<(), Self::Error> {
        Pin::new(&mut self.get_mut().address).start_send(message)
    }

    fn poll_flush(self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.get_mut().address).poll_flush(ctx)
    }

    fn poll_close(self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.get_mut().address).poll_close(ctx)
    }
}

/// A message channel is a channel through which you can send only one kind of message, but to
/// any actor that can handle it. It is like [`Address`](struct.Address.html), but associated with
/// the message type rather than the actor type. Any existing `WeakMessageChannel`s will *not* prevent the
/// dropping of the actor. If this is undesirable, then the [`MessageChannel`](struct.MessageChannel.html)
/// struct should be used instead. This struct is created by calling
/// [`MessageChannel::downgrade`](struct.MessageChannel.html#method.downgrade)
/// [`MessageChannel::into_downgraded`](struct.MessageChannel.html#method.into_downgraded),
/// [`WeakAddress::channel`](struct.WeakAddress.html#method.channel),
/// or [`WeakAddress::into_channel`](struct.WeakAddress.html#method.into_channel).
pub struct WeakMessageChannel<M: Message> {
    pub(crate) address: Box<dyn AddressEnvelope<M>>,
}

impl<M: Message> MessageChannelExt<M> for WeakMessageChannel<M> {
    fn is_connected(&self) -> bool {
        self.address.is_connected()
    }

    fn do_send(&self, message: M) -> Result<(), Disconnected> {
        self.address.do_send(message)
    }

    fn send(&self, message: M) -> MessageResponseFuture<M> {
        self.address.send(message)
    }

    #[cfg(any(
        doc,
        feature = "with-tokio-0_2",
        feature = "with-async_std-1",
        feature = "with-wasm_bindgen-0_2",
        feature = "with-smol-0_1"
    ))]
    fn attach_stream<S>(self, stream: S)
    where
        S: Stream<Item = M> + Send + Unpin + 'static,
        Self: Sized + Send + Sink<M, Error = Disconnected> + 'static,
    {
        let fut = stream.map(|i| Ok(i)).forward(self).map(|_| ());

        #[cfg(feature = "with-async_std-1")]
        async_std::task::spawn(fut);

        #[cfg(feature = "with-tokio-0_2")]
        tokio::spawn(fut);

        #[cfg(feature = "with-wasm_bindgen-0_2")]
        wasm_bindgen_futures::spawn_local(fut);

        #[cfg(feature = "with-smol-0_1")]
        smol::Task::spawn(fut).detach();
    }
}

impl<M: Message> Sink<M> for WeakMessageChannel<M> {
    type Error = Disconnected;

    fn poll_ready(self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.get_mut().address).poll_ready(ctx)
    }

    fn start_send(self: Pin<&mut Self>, message: M) -> Result<(), Self::Error> {
        Pin::new(&mut self.get_mut().address).start_send(message)
    }

    fn poll_flush(self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.get_mut().address).poll_flush(ctx)
    }

    fn poll_close(self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.get_mut().address).poll_close(ctx)
    }
}
