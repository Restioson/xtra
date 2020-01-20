use crate::Actor;
use std::marker::PhantomData;

/// `Context` is used to signal things to the [`ActorManager`](struct.ActorManager.html)'s
/// management loop. It can be used to stop the actor ([`Context::stop`](struct.Context.html#method.stop)) or
/// queue a message to be processed by the actor immediately after it has finished processing the
/// current message ([`Context::notify`](struct.Context.html#method.notify)).
pub struct Context<A: Actor + ?Sized> {
    pub(crate) running: bool,
    phantom: PhantomData<A>, // TODO(weak_address)
}

impl<A: Actor + ?Sized> Context<A> {
    pub(crate) fn new() -> Self {
        Context {
            running: true,
            phantom: PhantomData,
        }
    }

    /// Stop the actor as soon as it has finished processing current message. This will mean that it
    /// will be dropped, and [`Actor::stopping`](trait.Actor.html#method.stopping) and then
    /// [`Actor::stopped`](trait.Actor.html#method.stopped) will be called. Any subsequent attempts
    /// to send messages to this actor will return the [`Disconnected`](struct.Disconnected.html)
    /// error.
    pub fn stop(&mut self) {
        self.running = false;
    }
}
