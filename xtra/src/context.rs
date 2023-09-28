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
    /// This bypasses the message queue, so it will always be handled as soon as possible by all actors.
    /// It will not wait for other messages to be enqueued if the queue is full.
    /// In other words, it will not wait for an actor which is lagging behind on broadcast messages
    /// to catch up before other actors can receive the shutdown message.
    /// Therefore, each actor is guaranteed to shut down as its next action immediately after it
    /// finishes processing its current message, or as soon as its task is woken if it is currently idle.
    ///
    /// This is similar to calling [`Context::stop_self`] on all actors active on this address, but
    /// a broadcast message that would cause [`Context::stop_self`] to be called may have to wait
    /// for other broadcast messages, during which time other messages may be handled by actors (i.e
    /// the shutdown may be delayed by a lagging actor).
    pub fn stop_all(&self) {
        // We only need to shut down if there are still any strong senders left
        if let Some(address) = self.mailbox.address().try_upgrade() {
            address.0.shutdown_all_receivers();
        }
    }

    /// Get a reference to the [`Mailbox`] of this actor.
    pub fn mailbox(&self) -> &Mailbox<A> {
        &self.mailbox
    }
}
