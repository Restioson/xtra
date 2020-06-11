use crate::envelope::MessageEnvelope;
use crate::{Actor, Address, Context, WeakAddress};
use futures::channel::mpsc;
use futures::StreamExt;
use std::sync::Arc;

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

impl<A: Actor> Drop for ActorManager<A> {
    fn drop(&mut self) {
        self.actor.stopped(&mut self.ctx);
    }
}

impl<A: Actor> ActorManager<A> {
    /// Spawn the manager future onto the tokio or async-std executor
    #[cfg(any(
        feature = "with-tokio-0_2",
        feature = "with-async_std-1",
        feature = "with-wasm_bindgen-0_2",
        feature = "with-smol-0_1"
    ))]
    #[cfg_attr(nightly, doc(cfg(feature = "with-tokio-0_2")))]
    #[cfg_attr(nightly, doc(cfg(feature = "with-async_std-1")))]
    #[cfg_attr(nightly, doc(cfg(feature = "with-wasm_bindgen-0_2")))]
    #[cfg_attr(nightly, doc(cfg(feature = "with-smol-0_1")))]
    pub(crate) fn spawn(actor: A) -> Address<A>
    where
        A: Send,
    {
        let (addr, mgr) = Self::start(actor);

        #[cfg(feature = "with-tokio-0_2")]
        tokio::spawn(mgr.manage());

        #[cfg(feature = "with-async_std-1")]
        async_std::task::spawn(mgr.manage());

        #[cfg(feature = "with-wasm_bindgen-0_2")]
        wasm_bindgen_futures::spawn_local(mgr.manage());

        #[cfg(feature = "with-smol-0_1")]
        smol::Task::spawn(mgr.manage()).detach();

        addr
    }

    /// Return the actor and its address in ready-to-run the actor by returning its address and
    /// its manager. The `ActorManager::manage` future has to be executed for the actor to actually
    /// start.
    pub(crate) fn start(actor: A) -> (Address<A>, ActorManager<A>) {
        let (sender, receiver) = mpsc::unbounded();
        let ref_counter = Arc::new(());
        let addr = WeakAddress {
            sender: sender.clone(),
            ref_counter: Arc::downgrade(&ref_counter),
        };
        let ctx = Context::new(addr, receiver, ref_counter.clone());

        let mgr = ActorManager { actor, ctx };

        let addr = Address {
            sender,
            ref_counter,
        };

        (addr, mgr)
    }

    /// Starts the manager loop. This will start the actor and allow it to respond to messages.
    pub async fn manage(mut self) {
        self.actor.started(&mut self.ctx);

        // Idk why anyone would do this, but we have to check that they didn't do ctx.stop() in the
        // started method, otherwise it would kinda be a bug
        if !self.ctx.check_running(&mut self.actor) {
            return;
        }

        // Listen for any messages for the ActorManager
        while let Some(msg) = self.ctx.receiver.next().await {
            match self.ctx.handle_message(msg, &mut self.actor).await {
                ContinueManageLoop::Yes => {}
                ContinueManageLoop::ProcessNotifications => break,
                ContinueManageLoop::ExitImmediately => return,
            }
        }

        // Handle any last late notifications that were sent after the last strong address was dropped
        // We can't .await, because that would mean that we are awaiting forever! So, instead, we do
        // `next_message` and check if the result is `Ok`. Because we know that any late notifications
        // sent from the context must be fully send by now due to it being marked as stopped (so
        // that no other addresses can be created and sending concurrently), we can make the inference
        // that if `next_message` returns `Err`, there are no more late notifications to handle.
        while let Ok(Some(msg)) = self.ctx.receiver.try_next() {
            let res = self.ctx.handle_message(msg, &mut self.actor).await;
            if res == ContinueManageLoop::ExitImmediately {
                break;
            }
        }
    }
}
