use crate::envelope::{Envelope, EnvelopeHandleResult};
use crate::{Actor, Address, Context, KeepRunning};
use futures::channel::mpsc::{self, UnboundedReceiver};
use futures::StreamExt;

pub struct ActorManager<A: Actor> {
    receiver: UnboundedReceiver<Box<dyn Envelope<Actor = A>>>,
    //    sender: UnboundedSender<Box<dyn Envelope<Actor = A>>>, // TODO
    actor: A,
    ctx: Context<A>,
}

impl<A: Actor> Drop for ActorManager<A> {
    fn drop(&mut self) {
        self.actor.stopped(&mut self.ctx);
    }
}

impl<A: Actor> ActorManager<A> {
    pub(crate) fn spawn(actor: A) -> Address<A> {
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

    pub async fn manage(mut self) {
        self.actor.started(&mut self.ctx);

        while let Some(msg) = self.receiver.next().await {
            if let EnvelopeHandleResult::Fut(f) = msg.handle(&mut self.actor, &mut self.ctx) {
                f.await;
            };

            // Check if the context was stopped
            if !self.ctx.running {
                return;
            }

            // Handle notifications (messages to self)
            while let Some(notif) = self.ctx.notifications.pop() {
                if let EnvelopeHandleResult::Fut(f) = notif.handle(&mut self.actor, &mut self.ctx) {
                    f.await;
                }
            }
        }

        // TODO
        if self.actor.stopping(&mut self.ctx) == KeepRunning::Yes {
            // Handle notifications (messages to self)
            while let Some(notif) = self.ctx.notifications.pop() {
                if let EnvelopeHandleResult::Fut(f) = notif.handle(&mut self.actor, &mut self.ctx) {
                    f.await;
                }
            }
        }
    }
}
