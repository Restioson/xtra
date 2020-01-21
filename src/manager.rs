use crate::envelope::Envelope;
use crate::{Actor, Address, Context};
use futures::channel::mpsc::{self, UnboundedReceiver};
use futures::StreamExt;

/// A manager for the actor which handles incoming messages and stores the context. Its managing
/// loop can be started with [`ActorManager::manage`](struct.ActorManager.html#method.manage).
pub struct ActorManager<A: Actor> {
    receiver: UnboundedReceiver<Box<dyn Envelope<Actor = A>>>,
    actor: A,
    ctx: Context<A>,
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
        let ctx = Context::new();
        let mgr = ActorManager {
            receiver,
            actor,
            ctx,
        };
        let addr = Address { sender };

        (addr, mgr)
    }

    /// Starts the manager mainloop. This will start the actor and allow it to respond to messages.
    pub async fn manage(mut self) {
        self.actor.started(&mut self.ctx);

        while let Some(msg) = self.receiver.next().await {
            msg.handle(&mut self.actor, &mut self.ctx).await;

            // Check if the context was stopped
            if !self.ctx.running {
                return;
            }
        }
    }
}
