//! Adapted from actix `address` module

use futures::channel::mpsc::{self, UnboundedReceiver, UnboundedSender};
use futures::channel::oneshot::{self, Receiver, Sender};
use futures::future::Either;
use futures::TryFutureExt;
use futures::{Future, StreamExt};
use std::boxed::Box;
use std::marker::PhantomData;

pub trait Message: Send + 'static {
    type Result: Send;
}

pub trait Handler<M: Message>: Actor {
    fn handle(&mut self, message: M, ctx: &mut Context<Self>) -> M::Result;
}

pub trait Actor: Send + Unpin + 'static {
    fn started(&mut self, _ctx: &mut Context<Self>) {}
    fn stopping(&mut self, _ctx: &mut Context<Self>) {}
    fn stopped(&mut self) {}

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

pub struct Context<A: Actor + ?Sized> {
    running: bool,
    notifications: Vec<Box<dyn Envelope<Actor = A>>>,
}

impl<A: Actor + ?Sized> Context<A> {
    fn new() -> Self {
        Context {
            running: true,
            notifications: Vec::with_capacity(1),
        }
    }

    pub fn notify<M>(&mut self, notification: M)
    where
        M: Message,
        A: Handler<M>,
    {
        let notification = Box::new(NonReturningEnvelope::new(notification));
        self.notifications.push(notification);
    }

    pub fn stop(&mut self) {
        self.running = false;
    }
}

trait Envelope: Send {
    type Actor: Actor + ?Sized;

    fn handle(&mut self, act: &mut Self::Actor, ctx: &mut Context<Self::Actor>);
}

struct ReturningEnvelope<A: Actor + ?Sized, M: Message> {
    message: Option<M>, // Options so that we can opt.take()
    result_sender: Option<Sender<M::Result>>,
    phantom: PhantomData<A>,
}

impl<A: Actor + ?Sized, M: Message> ReturningEnvelope<A, M> {
    fn new(message: M) -> (Self, Receiver<M::Result>) {
        let (tx, rx) = oneshot::channel();
        let envelope = ReturningEnvelope {
            message: Some(message),
            result_sender: Some(tx),
            phantom: PhantomData,
        };

        (envelope, rx)
    }
}

impl<A: Actor + Handler<M>, M: Message> Envelope for ReturningEnvelope<A, M> {
    type Actor = A;

    fn handle(&mut self, act: &mut Self::Actor, ctx: &mut Context<Self::Actor>) {
        let message_result = act.handle(self.message.take().expect("Message must be Some"), ctx);

        // We don't actually care if the receiver is listening
        let _ = self
            .result_sender
            .take()
            .expect("Sender must be Some")
            .send(message_result);
    }
}

struct NonReturningEnvelope<A: Actor + ?Sized, M: Message> {
    message: Option<M>, // Option so that we can opt.take()
    phantom: PhantomData<A>,
}

impl<A: Actor + ?Sized, M: Message> NonReturningEnvelope<A, M> {
    fn new(message: M) -> Self {
        NonReturningEnvelope {
            message: Some(message),
            phantom: PhantomData,
        }
    }
}

impl<A: Actor + Handler<M> + ?Sized, M: Message> Envelope for NonReturningEnvelope<A, M> {
    type Actor = A;

    fn handle(&mut self, act: &mut Self::Actor, ctx: &mut Context<Self::Actor>) {
        act.handle(self.message.take().expect("Message must be Some"), ctx);
    }
}

#[derive(Clone)]
pub struct Address<A: Actor> {
    sender: UnboundedSender<Box<dyn Envelope<Actor = A>>>,
}

impl<A: Actor> Address<A> {
    pub fn do_send<M>(&self, message: M) -> Result<(), Disconnected>
    where
        M: Message,
        A: Handler<M>,
    {
        let envelope = NonReturningEnvelope::new(message);
        self.sender
            .unbounded_send(Box::new(envelope))
            .map_err(|_| Disconnected)
    }

    pub fn send<M>(&self, message: M) -> impl Future<Output = Result<M::Result, Disconnected>>
    where
        M: Message,
        A: Handler<M>,
    {
        let (envelope, rx) = ReturningEnvelope::new(message);

        let res = self
            .sender
            .unbounded_send(Box::new(envelope))
            .map_err(|_| Disconnected);

        match res {
            Ok(()) => Either::Left(rx.map_err(|_| Disconnected)),
            Err(e) => Either::Right(futures::future::err(e)),
        }
    }
}

/// The actor is no longer running and disconnected
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct Disconnected;

pub struct ActorManager<A: Actor + Unpin + 'static> {
    receiver: UnboundedReceiver<Box<dyn Envelope<Actor = A>>>,
    actor: A,
    ctx: Context<A>,
}

impl<A: Actor + 'static + Unpin> Drop for ActorManager<A> {
    fn drop(&mut self) {
        self.actor.stopping(&mut self.ctx);

        // Handle notifications (messages to self)
        while let Some(mut notif) = self.ctx.notifications.pop() {
            notif.handle(&mut self.actor, &mut self.ctx);
        }

        self.actor.stopped();
    }
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
        let ctx = Context::new();
        let mgr = ActorManager {
            receiver,
            actor,
            ctx,
        };
        let addr = Address { sender };

        (addr, mgr)
    }

    pub async fn manage(mut self) {
        while let Some(mut msg) = self.receiver.next().await {
            msg.handle(&mut self.actor, &mut self.ctx);

            // Check if the context was stopped
            if !self.ctx.running {
                return;
            }

            // Handle notifications (messages to self)
            while let Some(mut notif) = self.ctx.notifications.pop() {
                notif.handle(&mut self.actor, &mut self.ctx);
            }
        }
    }
}
