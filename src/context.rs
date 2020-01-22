use crate::{Actor, Address};

/// `Context` is used to signal things to the [`ActorManager`](struct.ActorManager.html)'s
/// management loop. Currently, it can be used to stop the actor ([`Context::stop`](struct.Context.html#method.stop)).
pub struct Context<A: Actor> {
    /// Whether the actor is running. It is changed by the `stop` method as a flag to the `ActorManager`
    /// to calling the `stopping` method on the actor
    pub(crate) running: bool,
    /// The address kept by the context to
    address: Address<A>,
}

impl<A: Actor> Context<A> {
    pub(crate) fn new(address: Address<A>) -> Self {
        Context {
            running: true,
            address,
        }
    }

    /// Stop the actor as soon as it has finished processing current message. This will mean that the
    /// [`Actor::stopping`](trait.Actor.html#method.stopping) method will be called.
    /// If that returns [`KeepRunning::No`](enum.KeepRunning.html#variant.No), any subsequent attempts
    /// to send messages to this actor will return the [`Disconnected`](struct.Disconnected.html) error.
    pub fn stop(&mut self) {
        self.running = false;
    }

    /// Get an address to the current actor if the actor is still running.
    pub fn address(&self) -> Option<Address<A>> {
        if self.running {
            Some(self.address.clone())
        } else {
            None
        }
    }
}
