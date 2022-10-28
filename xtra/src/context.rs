use crate::{Actor, Mailbox};

/// `Context` is used to control how the actor is managed and to get the actor's address from inside
/// of a message handler.
pub struct Context<A> {
    pub(crate) running: bool,
    pub(crate) mailbox: Mailbox<A>,
}

impl<A: Actor> Context<A> {
    /// Stop this actor as soon as it has finished processing current message. This means that the
    /// [`Actor::stopped`] method will be called. This will not stop all actors on the address.
    pub fn stop_self(&mut self) {
        self.running = false;
    }

    /// Stop all actors on this address.
    ///
    /// This is equivalent to calling [`Context::stop_self`] on all actors active on this address.
    pub fn stop_all(&mut self) {
        // We only need to shut down if there are still any strong senders left
        if let Some(address) = self.mailbox.address().try_upgrade() {
            address.0.shutdown_all_receivers();
        }
    }

    /// Get a reference to the [`Mailbox`] of this actor.
    pub fn mailbox(&mut self) -> &mut Mailbox<A> {
        &mut self.mailbox
    }
}
