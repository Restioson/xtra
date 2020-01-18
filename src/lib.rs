//! Adapted from actix `address` module

use std::marker::PhantomData;
use std::boxed::Box;
use futures::channel::mpsc::{self, UnboundedReceiver, UnboundedSender};
use futures::StreamExt;

pub trait Message: Send + 'static {}

pub trait Handler<M: Message> {
    fn handle(&mut self, message: M);
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
    message: Option<M>, // Option so that we can opt.take()
    phantom: PhantomData<A>,
}

impl<A: Actor, M: Message> AnEnvelope<A, M> {
    fn new(message: M) -> Self {
        AnEnvelope {
            message: Some(message),
            phantom: PhantomData,
        }
    }
}

impl<A: Actor + Handler<M>, M: Message> Envelope for AnEnvelope<A, M> {
    type Actor = A;

    fn handle(&mut self, act: &mut Self::Actor) {
        act.handle(self.message.take().expect("Message must be Some"));
    }
}

pub struct Address<A: Actor> {
    sender: UnboundedSender<Box<dyn Envelope<Actor = A>>>,
}

impl<A: Actor> Address<A> {
    pub fn send_message<M>(&self, message: M)
    where
        M: Message,
        A: Handler<M>,
    {
        self.sender
            .unbounded_send(Box::new(AnEnvelope::new(message)))
            .expect("Error sending");
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
        loop {
            match self.receiver.next().await {
                Some(mut msg) => msg.handle(&mut self.actor),
                None => break
            }
        }
    }
}
