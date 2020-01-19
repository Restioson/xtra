use crate::envelope::{Envelope, NonReturningEnvelope};
use crate::{Actor, Handler, Message};
use std::marker::PhantomData;

/// `Context` is used to signal things to the [`ActorManager`](struct.ActorManager.html)'s
/// management loop. It can be used to stop the actor ([`Context::stop`](struct.Context.html#method.stop)) or
/// queue a message to be processed by the actor immediately after it has finished processing the
/// current message ([`Context::notify`](struct.Context.html#method.notify)).
pub struct Context<A: Actor + ?Sized> {
    pub(crate) running: bool,
    phantom: PhantomData<A>,
    // TODO
}

impl<A: Actor + ?Sized> Context<A> {
    pub(crate) fn new() -> Self {
        Context {
            running: true,
            phantom: PhantomData,
//            notifications: Vec::new(),
        }
    }

//    /// Send a message to this actor to be processed immediately after it has finished processing
//    /// the current message. An unlimited number of notifications can be queued at one time (but,
//    /// as always, they will only be processed one-by-one). Be aware that this does allocate space
//    /// in a [`Vec`](https://doc.rust-lang.org/std/vec/struct.Vec.html).
//    pub fn notify<M>(&mut self, notification: M)
//    where
//        M: Message,
//        A: Handler<'a, M>,
//    {
//        let notification = Box::new(NonReturningEnvelope::new(notification));
//        self.notifications.push(notification);
//    }

    /// Stop the actor as soon as it has finished processing current message. This will mean that it
    /// will be dropped, and [`Actor::stopping`](trait.Actor.html#method.stopping) and then
    /// [`Actor::stopped`](trait.Actor.html#method.stopped) will be called. Any subsequent attempts
    /// to send messages to this actor will return the [`Disconnected`](struct.Disconnected.html)
    /// error.
    pub fn stop(&mut self) {
        self.running = false;
    }
}
