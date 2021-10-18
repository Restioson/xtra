//! Module for the sink equivalents to [`Address`](../address/struct.Address.html) and
//! [`MessageChannel`](../message_channel/trait.MessageChannel.html).

use std::pin::Pin;
use std::task::{Context, Poll};

use flume::r#async::SendSink;
use futures_sink::Sink;
use futures_util::SinkExt;

use crate::address::Disconnected;
use crate::envelope::NonReturningEnvelope;
use crate::manager::AddressMessage;
use crate::private::Sealed;
use crate::refcount::{RefCounter, Strong, Weak};
use crate::{Actor, Handler, Message};

/// An `AddressSink` is the [futures `Sink`](https://docs.rs/futures/0.3/futures/io/struct.Sink.html)
/// returned by [`Address::into_sink`](../address/struct.Address.html#method.into_sink). Similarly to with
/// addresses, the strong variety of `AddressSink` will prevent the actor from being dropped, whereas
/// the [weak variety](struct.AddressSink.html) will not.
pub struct AddressSink<A: 'static, Rc: RefCounter = Strong> {
    pub(crate) sink: SendSink<'static, AddressMessage<A>>,
    pub(crate) ref_counter: Rc,
}

impl<A, Rc: RefCounter> Clone for AddressSink<A, Rc> {
    fn clone(&self) -> Self {
        AddressSink {
            sink: self.sink.clone(),
            ref_counter: self.ref_counter.clone(),
        }
    }
}

/// This variety of `AddressSink` will not prevent the actor from being dropped.
pub type WeakAddressSink<A> = AddressSink<A, Weak>;

impl<A, Rc: RefCounter> AddressSink<A, Rc> {
    /// Returns whether the actor referred to by this address sink is running and accepting messages.
    pub fn is_connected(&self) -> bool {
        self.ref_counter.is_connected()
    }
}

impl<A> AddressSink<A, Strong> {
    /// Create a weak address sink. Unlike with the strong variety of address sink (this kind),
    /// an actor will not be prevented from being dropped if only weak sinks, channels, and
    /// addresses exist.
    pub fn downgrade(&self) -> WeakAddressSink<A> {
        AddressSink {
            sink: self.sink.clone(),
            ref_counter: self.ref_counter.downgrade(),
        }
    }
}

impl<A, Rc: RefCounter> Drop for AddressSink<A, Rc> {
    fn drop(&mut self) {
        // We should notify the ActorManager that there are no more strong Addresses and the actor
        // should be stopped.
        if self.ref_counter.is_last_strong() {
            let _ = pollster::block_on(self.sink.send(AddressMessage::LastAddress));
        }
    }
}

impl<A, Rc: RefCounter, M: Message> Sink<M> for AddressSink<A, Rc>
where
    A: Handler<M>,
{
    type Error = Disconnected;

    fn poll_ready(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.sink)
            .poll_ready(cx)
            .map_err(|_| Disconnected)
    }

    fn start_send(mut self: Pin<&mut Self>, item: M) -> Result<(), Self::Error> {
        let item = AddressMessage::Message(Box::new(NonReturningEnvelope::new(item)));
        Pin::new(&mut self.sink)
            .start_send(item)
            .map_err(|_| Disconnected)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.sink)
            .poll_flush(cx)
            .map_err(|_| Disconnected)
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.sink)
            .poll_close(cx)
            .map_err(|_| Disconnected)
    }
}

/// A `MessageSink` is similar to a [`MessageChannel`](../message_channel/trait.MessageChannel.html),
/// but it is a sink and operates asynchronously.
pub trait MessageSink<M: Message>: Sealed + Sink<M, Error = Disconnected> + Unpin + Send {
    /// Returns whether the actor referred to by this message sink is running and accepting messages.
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

    /// Clones this message sink as a boxed trait object.
    fn clone_message_sink(&self) -> Box<dyn MessageSink<M>>;
}

/// A `WeakMessageSink` is a [`MessageSink`](trait.MessageSink.html) which does not inhibit the actor
/// from being dropped while it exists.
pub trait WeakMessageSink<M: Message>: MessageSink<M> {
    /// Upcasts this weak message sink into a boxed generic
    /// [`MessageSink`](trait.MessageSink.html) trait object
    fn upcast(self) -> Box<dyn MessageSink<M>>;

    /// Upcasts this weak message sink into a reference to the generic
    /// [`MessageSink`](trait.MessageSink.html) trait object
    fn upcast_ref(&self) -> &dyn MessageSink<M>;

    /// Clones this message sink as a boxed trait object.
    fn clone_message_sink(&self) -> Box<dyn WeakMessageSink<M>>;
}

/// A `StrongMessageSink` is a [`MessageSink`](trait.MessageSink.html) which does inhibit the actor
/// from being dropped while it exists.
pub trait StrongMessageSink<M: Message>: MessageSink<M> {
    /// Create a weak message sink. Unlike with the strong variety of message sink (this kind),
    /// an actor will not be prevented from being dropped if only weak sinks, channels, and
    /// addresses exist.
    fn downgrade(self) -> Box<dyn WeakMessageSink<M>>;

    /// Upcasts this strong message sink into a boxed generic
    /// [`MessageSink`](trait.MessageSink.html) trait object
    fn upcast(self) -> Box<dyn MessageSink<M>>;

    /// Upcasts this strong message sink into a reference to the generic
    /// [`MessageSink`](trait.MessageSink.html) trait object
    fn upcast_ref(&self) -> &dyn MessageSink<M>;

    /// Clones this message sink as a boxed trait object.
    fn clone_message_sink(&self) -> Box<dyn StrongMessageSink<M>>;
}

impl<A: Actor, M: Message, Rc: RefCounter> MessageSink<M> for AddressSink<A, Rc>
where
    A: Handler<M>,
{
    fn is_connected(&self) -> bool {
        self.ref_counter.is_connected()
    }

    fn len(&self) -> usize {
        self.sink.len()
    }

    fn capacity(&self) -> Option<usize> {
        self.sink.capacity()
    }

    fn clone_message_sink(&self) -> Box<dyn MessageSink<M>> {
        Box::new(self.clone())
    }
}

impl<A: Actor, M: Message> StrongMessageSink<M> for AddressSink<A, Strong>
where
    A: Handler<M>,
{
    fn downgrade(self) -> Box<dyn WeakMessageSink<M>> {
        Box::new(AddressSink::downgrade(&self))
    }

    fn upcast(self) -> Box<dyn MessageSink<M, Error = Disconnected>> {
        Box::new(self)
    }

    fn upcast_ref(&self) -> &dyn MessageSink<M, Error = Disconnected> {
        self
    }

    fn clone_message_sink(&self) -> Box<dyn StrongMessageSink<M>> {
        Box::new(self.clone())
    }
}

impl<A: Actor, M: Message> WeakMessageSink<M> for AddressSink<A, Weak>
where
    A: Handler<M>,
{
    fn upcast(self) -> Box<dyn MessageSink<M, Error = Disconnected>> {
        Box::new(self)
    }

    fn upcast_ref(&self) -> &dyn MessageSink<M, Error = Disconnected> {
        self
    }

    fn clone_message_sink(&self) -> Box<dyn WeakMessageSink<M>> {
        Box::new(self.clone())
    }
}
