//! Adapted from actix `address` module

use futures::FutureExt;
use futures::channel::mpsc::{self, UnboundedReceiver, UnboundedSender};
use futures::channel::oneshot::{self, Sender, Receiver};
use futures::{StreamExt, Future};
use std::boxed::Box;
use std::marker::PhantomData;

pub trait Message: Send + 'static {
    type Result: Send;
}

pub trait Handler<M: Message> {
    fn handle(&mut self, message: M) -> M::Result;
}

pub trait Actor: Send + Unpin + 'static {
    #[cfg(feature = "tokio_spawn")]
    fn spawn(self) -> Address<Self>
    where
        Self: Sized,
    {
        ActorManager::spawn(self)
    }

    fn start(self) -> (Address<Self>, ActorManager<Self>)
    where
        Self: Sized,
    {
        ActorManager::start(self)
    }
}

trait Envelope: Send {
    type Actor: Actor;

    fn handle(&mut self, act: &mut Self::Actor);
}

struct AnEnvelope<A: Actor, M: Message> {
    message: Option<M>, // Options so that we can opt.take()
    result_sender: Option<Sender<M::Result>>,
    phantom: PhantomData<A>,
}

impl<A: Actor, M: Message> AnEnvelope<A, M> {
    fn new(message: M) -> (Self, Receiver<M::Result>) {
        let (tx, rx) = oneshot::channel();
        let envelope = AnEnvelope {
            message: Some(message),
            result_sender: Some(tx),
            phantom: PhantomData,
        };

        (envelope, rx)
    }
}

impl<A: Actor + Handler<M>, M: Message> Envelope for AnEnvelope<A, M> {
    type Actor = A;

    fn handle(&mut self, act: &mut Self::Actor) {
        let message_result = act.handle(self.message.take().expect("Message must be Some"));

        // We don't actually care if the receiver is listening
        let _ = self.result_sender.take().expect("Sender must be Some").send(message_result);
    }
}

pub struct Address<A: Actor> {
    sender: UnboundedSender<Box<dyn Envelope<Actor = A>>>,
}

impl<A: Actor> Address<A> {
    pub fn do_send<M>(&self, message: M)
    where
        M: Message,
        A: Handler<M>,
    {
        let (envelope, _rx) = AnEnvelope::new(message);
        self.sender
            .unbounded_send(Box::new(envelope))
            .expect("Error sending");
    }

    pub fn send<M>(&self, message: M) -> impl Future<Output = M::Result>
        where
            M: Message,
            A: Handler<M>,
    {
        let (envelope, rx) = AnEnvelope::new(message);
        self.sender
            .unbounded_send(Box::new(envelope))
            .expect("Error sending");
        rx.map(|res| res.expect("Receiver must not be cancelled"))
    }
}

pub struct ActorManager<A: Actor + 'static> {
    receiver: UnboundedReceiver<Box<dyn Envelope<Actor = A>>>,
    actor: A,
}

impl<A: Actor + 'static + Unpin> ActorManager<A> {
    #[cfg(feature = "tokio_spawn")]
    fn spawn(actor: A) -> Address<A> {
        let (addr, mgr) = Self::start(actor);
        tokio::spawn(mgr.manage());
        addr
    }

    fn start(actor: A) -> (Address<A>, ActorManager<A>) {
        let (sender, receiver) = mpsc::unbounded();
        let mgr = ActorManager { receiver, actor };
        let addr = Address { sender };

        (addr, mgr)
    }

    pub async fn manage(mut self) {
        while let Some(mut msg) = self.receiver.next().await {
            msg.handle(&mut self.actor);
        }
    }
}
