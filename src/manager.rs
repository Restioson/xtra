use crate::envelope::MessageEnvelope;
use crate::{Actor, Address, Context};
use std::sync::Arc;
use crate::address::Strong;

/// A message that can be sent by an [`Address`](struct.Address.html) to the [`ActorManager`](struct.ActorManager.html)
pub(crate) enum ManagerMessage<A: Actor> {
    /// The address sending this is being dropped and is the only external strong address in existence
    /// other than the one held by the [`Context`](struct.Context.html). This notifies the
    /// [`ActorManager`](struct.ActorManager.html) so that it can check if the actor should be
    /// dropped
    LastAddress,
    /// A message being sent to the actor. To read about envelopes and why we use them, check out
    /// `envelope.rs`
    Message(Box<dyn MessageEnvelope<Actor = A>>),
    /// A notification queued with `Context::notify_later`
    LateNotification(Box<dyn MessageEnvelope<Actor = A>>),
}

/// If and how to continue the manage loop
#[derive(PartialEq, Eq, Debug, Copy, Clone)]
pub(crate) enum ContinueManageLoop {
    Yes,
    ExitImmediately,
    ProcessNotifications,
}

/// A manager for the actor which handles incoming messages and stores the context. Its managing
/// loop can be started with [`ActorManager::manage`](struct.ActorManager.html#method.manage).
pub struct ActorManager<A: Actor> {
    actor: A,
    ctx: Context<A>,
}

impl<A: Actor> ActorManager<A> {
    /// Return the actor and its address in ready-to-run the actor by returning its address and
    /// its manager. The `ActorManager::manage` future has to be executed for the actor to actually
    /// start.
    pub(crate) fn start(actor: A, message_cap: Option<usize>) -> (Address<A>, ActorManager<A>) {
        let (sender, receiver) = match message_cap {
            None => flume::unbounded(),
            Some(cap) => flume::bounded(cap),
        };

        let ref_counter = Strong(Arc::new(()));
        let addr = Address { sender, ref_counter };
        let ctx = Context::new(addr.clone(), receiver);

        let mgr = ActorManager { actor, ctx };

        (addr, mgr)
    }

    /// Starts the manager loop. This will start the actor and allow it to respond to messages.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use xtra::prelude::*;
    /// struct MyActor;
    /// impl Actor for MyActor {}
    ///
    /// smol::block_on(async {
    ///     let (addr, mgr) = MyActor.create(None);
    ///     smol::spawn(mgr.manage()).detach(); // Actually spawn the actor onto an executor
    /// });
    /// ```
    pub async fn manage(mut self) {
        self.actor.started(&mut self.ctx).await;

        // Idk why anyone would do this, but we have to check that they didn't do ctx.stop() in the
        // started method, otherwise it would kinda be a bug
        if !self.ctx.check_running(&mut self.actor).await {
            self.actor.stopped(&mut self.ctx).await;
            return;
        }

        // Listen for any messages for the ActorManager
        while let Ok(msg) = self.ctx.receiver.recv_async().await {
            match self.ctx.handle_message(msg, &mut self.actor).await {
                ContinueManageLoop::Yes => {}
                ContinueManageLoop::ProcessNotifications => break,
                ContinueManageLoop::ExitImmediately => {
                    self.actor.stopped(&mut self.ctx).await;
                    return;
                },
            }
        }

        // Handle any last late notifications that were sent after the last strong address was dropped
        // We can't .await, because that would mean that we are awaiting forever! So, instead, we do
        // `next_message` and check if the result is `Ok`. Because we know that any late notifications
        // sent from the context must be fully send by now due to it being marked as stopped (so
        // that no other addresses can be created and sending concurrently), we can make the inference
        // that if `next_message` returns `Err`, there are no more late notifications to handle.
        while let Ok(msg) = self.ctx.receiver.try_recv() {
            let res = self.ctx.handle_message(msg, &mut self.actor).await;
            if res == ContinueManageLoop::ExitImmediately {
                break;
            }
        }

        self.actor.stopped(&mut self.ctx).await;
    }
}
