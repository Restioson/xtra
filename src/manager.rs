use crate::envelope::Envelope;
use crate::{Actor, Address, Context, KeepRunning};
use futures::channel::mpsc::{self, UnboundedReceiver};
use futures::StreamExt;
use std::sync::Arc;

/// A message that can be sent by an [`Address`](struct.Address.html) to the [`ActorManager`](struct.ActorManager.html)
pub(crate) enum ManagerMessage<A: Actor> {
    /// The address sending this is being dropped and is the only external strong address in existence
    /// other than the one held by the [`Context`](struct.Context.html). This notifies the
    /// [`ActorManager`](struct.ActorManager.html) so that it can check if the actor should be
    /// dropped
    LastAddress,
    /// A message being sent to the actor
    Message(Box<dyn Envelope<Actor = A>>),
}

/// A manager for the actor which handles incoming messages and stores the context. Its managing
/// loop can be started with [`ActorManager::manage`](struct.ActorManager.html#method.manage).
pub struct ActorManager<A: Actor> {
    receiver: UnboundedReceiver<ManagerMessage<A>>,
    actor: A,
    ctx: Context<A>,
    ref_counter: Arc<()>,
}

impl<A: Actor> Drop for ActorManager<A> {
    fn drop(&mut self) {
        self.actor.stopped(&mut self.ctx);
    }
}

impl<A: Actor> ActorManager<A> {
    #[cfg(any(feature = "with-tokio-0_2", feature = "with-async_std-1"))]
    pub(crate) fn spawn(actor: A) -> Address<A>
    where
        A: Send,
    {
        let (addr, mgr) = Self::start(actor);

        #[cfg(feature = "with-tokio-0_2")]
        tokio::spawn(mgr.manage());

        #[cfg(feature = "with-async_std-1")]
        async_std::task::spawn(mgr.manage());

        addr
    }

    pub(crate) fn start(actor: A) -> (Address<A>, ActorManager<A>) {
        let (sender, receiver) = mpsc::unbounded();
        let ref_counter = Arc::new(());
        let addr = Address {
            sender,
            ref_counter: ref_counter.clone(),
        };
        let ctx = Context::new(addr.clone());

        let mgr = ActorManager {
            receiver,
            actor,
            ctx,
            ref_counter,
        };

        (addr, mgr)
    }

    /// Starts the manager mainloop. This will start the actor and allow it to respond to messages.
    pub async fn manage(mut self) {
        self.actor.started(&mut self.ctx);

        if !self.ctx.running {
            let keep_running = self.actor.stopping(&mut self.ctx);

            if keep_running == KeepRunning::Yes {
                self.ctx.running = true;
            } else {
                return; // Ok then
            }
        }

        while let Some(msg) = self.receiver.next().await {
            match msg {
                // A new message from an address has arrived, so handle it
                ManagerMessage::Message(msg) => {
                    msg.handle(&mut self.actor, &mut self.ctx).await;

                    // Check if the context was stopped, and if so return, thereby dropping the
                    // manager and calling `stopped` on the actor
                    if !self.ctx.running {
                        let keep_running = self.actor.stopping(&mut self.ctx);

                        if keep_running == KeepRunning::Yes {
                            self.ctx.running = true;
                        } else {
                            return;
                        }
                    }
                }
                // An address in the process of being dropped has realised that it could be the last
                // strong address to the actor, so we need to check if that is still the case, if so
                // stopping the actor
                ManagerMessage::LastAddress => {
                    // strong_count() == 2 because Context and manager both hold a strong arc to
                    // the refcount
                    if Arc::strong_count(&self.ref_counter) == 2 {
                        return;
                    }
                }
            }
        }
    }
}
