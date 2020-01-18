//! Adapted from actix `address` module

use crossbeam_channel::{Receiver, Sender, TryRecvError};
use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;
use std::task::{Context, Poll};

pub trait Message: Send {}

pub trait Handler<M: Message> {
    fn handle(&mut self, message: M);
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

impl<A, M> Envelope for AnEnvelope<A, M>
where
    A: Actor,
    A: Handler<M>,
    M: Message,
{
    type Actor = A;

    fn handle(&mut self, act: &mut Self::Actor) {
        act.handle(self.message.take().expect("Message must be Some"))
    }
}

pub trait Actor: Send {
    fn start(self) -> Address<Self>
    where
        Self: Sized,
        Self: Unpin,
        Self: 'static,
    {
        ActorManager::start(self)
    }
}

struct ActorManager<A: Actor + 'static> {
    receiver: Receiver<Box<dyn Envelope<Actor = A>>>,
    actor: A,
}

impl<A: Actor + 'static + Unpin> ActorManager<A> {
    fn start(actor: A) -> Address<A> {
        let (sender, receiver) = crossbeam_channel::unbounded();

        let mgr = ActorManager { receiver, actor };

        tokio::spawn(mgr);

        let addr = Address { sender };

        addr
    }
}

impl<A: Actor + Unpin> Future for ActorManager<A> {
    type Output = ();

    fn poll(self: Pin<&mut Self>, _ctx: &mut Context<'_>) -> Poll<()> {
        match self.receiver.try_recv() {
            Ok(mut msg) => {
                msg.handle(&mut self.get_mut().actor);
                Poll::Pending
            }
            Err(TryRecvError::Empty) => Poll::Pending,
            Err(TryRecvError::Disconnected) => Poll::Ready(()),
        }
    }
}

pub struct Address<A: Actor> {
    sender: Sender<Box<dyn Envelope<Actor = A>>>,
}

impl<A: Actor> Address<A> {
    pub fn send_message<M>(&self, message: M)
    where
        M: Message,
        M: 'static,
        A: Handler<M>,
        A: 'static,
    {
        self.sender
            .send(Box::new(AnEnvelope::new(message)))
            .expect("Error sending");
    }
}
