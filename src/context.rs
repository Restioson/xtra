use crate::envelope::{MessageEnvelope, NonReturningEnvelope};
use crate::manager::{ManagerMessage, ContinueManageLoop};
use crate::{Actor, Address, Handler, Message, WeakAddress, KeepRunning};
use futures::channel::mpsc::UnboundedReceiver;
#[cfg(any(
    doc,
    feature = "with-tokio-0_2",
    feature = "with-async_std-1",
    feature = "with-wasm_bindgen-0_2",
    feature = "with-smol-0_1"
))]
use {crate::AddressExt, std::time::Duration};
use futures::future::{self, Future, Either};
use std::sync::Arc;
use futures::StreamExt;

/// `Context` is used to signal things to the [`ActorManager`](struct.ActorManager.html)'s
/// management loop or to get the actor's address from inside of a message handler.
pub struct Context<A: Actor> {
    /// Whether the actor is running. It is changed by the `stop` method as a flag to the `ActorManager`
    /// for it to call the `stopping` method on the actor
    pub(crate) running: bool,
    /// The address kept by the context to allow for the `Context::address` method to work.
    address: WeakAddress<A>,
    /// Notifications that must be stored for immediate processing.
    pub(crate) immediate_notifications: Vec<Box<dyn MessageEnvelope<Actor = A>>>,
    pub(crate) receiver: UnboundedReceiver<ManagerMessage<A>>,
    /// The reference counter of the actor. This tells us how many external strong addresses
    /// (and weak addresses, but we don't care about those) exist to the actor.
    ref_counter: Arc<()>,
}

impl<A: Actor> Context<A> {
    pub(crate) fn new(
        address: WeakAddress<A>,
        receiver: UnboundedReceiver<ManagerMessage<A>>,
        ref_counter: Arc<()>,
    ) -> Self {
        Context {
            running: true,
            address,
            immediate_notifications: Vec::new(),
            receiver,
            ref_counter,
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
            let strong = Address {
                sender: self.address.sender.clone(),
                ref_counter: self.address.ref_counter.upgrade().unwrap(),
            };

            Some(strong)
        } else {
            None
        }
    }

    /// Check if the Context is still set to running, returning whether to return from the manage
    /// loop or not
    pub(crate) fn check_running(&mut self, actor: &mut A) -> bool {
        // Check if the context was stopped, and if so return, thereby dropping the
        // manager and calling `stopped` on the actor
        if !self.running {
            let keep_running = actor.stopping(self);

            if keep_running == KeepRunning::Yes {
                self.running = true;
            } else {
                return false;
            }
        }

        true
    }

    /// Handle all immediate notifications, returning whether to return from the manage loop or not
    async fn handle_immediate_notifications(&mut self, actor: &mut A) -> bool {
        while let Some(notification) = self.immediate_notifications.pop() {
            notification.handle(actor, self).await;
            if !self.check_running(actor) {
                return false;
            }
        }

        true
    }

    /// Handle a message, returning whether to exit from the manage loop or not
    pub(crate) async fn handle_message(
        &mut self,
        msg: ManagerMessage<A>,
        actor: &mut A,
    ) -> ContinueManageLoop {
        match msg {
            // A new message from an address or a notification has arrived, so handle it
            ManagerMessage::Message(msg) | ManagerMessage::LateNotification(msg) => {
                msg.handle(actor, self).await;
                if !self.check_running(actor) {
                    return ContinueManageLoop::ExitImmediately;
                }
                if !self.handle_immediate_notifications(actor).await {
                    return ContinueManageLoop::ExitImmediately;
                }
            }
            // An address in the process of being dropped has realised that it could be the last
            // strong address to the actor, so we need to check if that is still the case, if so
            // stopping the actor
            ManagerMessage::LastAddress => {
                // strong_count() == 1 manager holds a strong arc to the refcount
                if Arc::strong_count(&self.ref_counter) == 1 {
                    self.stop();
                    return ContinueManageLoop::ProcessNotifications;
                }
            }
        }
        ContinueManageLoop::Yes
    }

    /// TODO doc
    pub async fn select<F, R>(&mut self, act: &mut A, mut fut: F) -> R
        where F: Future<Output = R> + Unpin,
    {
        let mut next_msg = self.receiver.next();
        loop {
            match future::select(fut, next_msg).await {
                Either::Left((res, _)) => break res,
                Either::Right((manager_message, unfinished_fut)) => {
                    match manager_message {
                        Some(msg) => {
                            self.handle_message(msg, act).await;
                        },
                        None => self.stop(),
                    }
                    next_msg = self.receiver.next();
                    fut = unfinished_fut;
                }
            }
        }
    }

    /// Notify this actor with a message that is handled synchronously before any other messages
    /// from the general queue are processed (therefore, immediately). If multiple
    /// `notify_immediately` messages are queued, they will still be processed in the order that they
    /// are queued (i.e the immediate priority is only over other messages).
    pub fn notify_immediately<M>(&mut self, msg: M)
    where
        M: Message,
        A: Handler<M> + Send,
    {
        let envelope = Box::new(NonReturningEnvelope::<A, M>::new(msg));
        self.immediate_notifications.push(envelope);
    }

    /// Notify this actor with a message that is handled synchronously after any other messages
    /// from the general queue are processed.
    pub fn notify_later<M>(&mut self, msg: M)
    where
        M: Message,
        A: Handler<M> + Send,
    {
        let envelope = NonReturningEnvelope::<A, M>::new(msg);
        let _ = self
            .address
            .sender
            .unbounded_send(ManagerMessage::LateNotification(Box::new(envelope)));
    }

    /// Notify the actor with a synchronously handled message every interval until it is stopped
    /// (either directly with [`Context::stop`](struct.Context.html#method.stop), or for a lack of
    /// strong [`Address`es](struct.Address.html)). This does not take priority over other messages.
    #[cfg(any(
        doc,
        feature = "with-tokio-0_2",
        feature = "with-async_std-1",
        feature = "with-wasm_bindgen-0_2",
        feature = "with-smol-0_1"
    ))]
    #[cfg_attr(nightly, doc(cfg(feature = "with-tokio-0_2")))]
    #[cfg_attr(nightly, doc(cfg(feature = "with-async_std-1")))]
    #[cfg_attr(nightly, doc(cfg(feature = "with-wasm_bindgen-0_2")))]
    #[cfg_attr(nightly, doc(cfg(feature = "with-smol-0_1")))]
    pub fn notify_interval<F, M>(&mut self, duration: Duration, constructor: F)
    where
        F: Send + 'static + Fn() -> M,
        M: Message,
        A: Handler<M> + Send,
    {
        let addr = self.address.clone();

        #[cfg(feature = "with-tokio-0_2")]
        tokio::spawn(async move {
            let mut timer = tokio::time::interval(duration);
            loop {
                timer.tick().await;
                if let Err(_) = addr.do_send(constructor()) {
                    break;
                }
            }
        });

        #[cfg(feature = "with-async_std-1")]
        {
            use async_std::prelude::FutureExt;
            async_std::task::spawn(async move {
                loop {
                    futures::future::ready(()).delay(duration.clone()).await;
                    if let Err(_) = addr.do_send(constructor()) {
                        break;
                    }
                }
            });
        }

        #[cfg(feature = "with-wasm_bindgen-0_2")]
        {
            use futures_timer::Delay;
            wasm_bindgen_futures::spawn_local(async move {
                loop {
                    Delay::new(duration.clone()).await;
                    if let Err(_) = addr.do_send(constructor()) {
                        break;
                    }
                }
            })
        }

        #[cfg(feature = "with-smol-0_1")]
        {
            use smol::Timer;
            smol::Task::spawn(async move {
                loop {
                    Timer::after(duration.clone()).await;
                    if let Err(_) = addr.do_send(constructor()) {
                        break;
                    }
                }
            }).detach();
        }
    }

    /// Notify the actor with a synchronously handled message after a certain duration has elapsed.
    /// This does not take priority over other messages.
    #[cfg(any(
        doc,
        feature = "with-tokio-0_2",
        feature = "with-async_std-1",
        feature = "with-wasm_bindgen-0_2",
        feature = "with-smol-0_1"
    ))]
    #[cfg_attr(nightly, doc(cfg(feature = "with-tokio-0_2")))]
    #[cfg_attr(nightly, doc(cfg(feature = "with-async_std-1")))]
    #[cfg_attr(nightly, doc(cfg(feature = "with-wasm_bindgen-0_2")))]
    #[cfg_attr(nightly, doc(cfg(feature = "with-smol-0_1")))]
    pub fn notify_after<M>(&mut self, duration: Duration, notification: M)
    where
        M: Message,
        A: Handler<M> + Send,
    {
        let addr = self.address.clone();

        #[cfg(feature = "with-tokio-0_2")]
        tokio::spawn(async move {
            tokio::time::delay_for(duration).await;
            let _ = addr.do_send(notification);
        });

        #[cfg(feature = "with-async_std-1")]
        {
            use async_std::prelude::FutureExt;
            async_std::task::spawn(async move {
                futures::future::ready(()).delay(duration.clone()).await;
                let _ = addr.do_send(notification);
            });
        }

        #[cfg(feature = "with-wasm_bindgen-0_2")]
        {
            use futures_timer::Delay;
            wasm_bindgen_futures::spawn_local(async move {
                Delay::new(duration.clone()).await;
                let _ = addr.do_send(notification);
            })
        }

        #[cfg(feature = "with-smol-0_1")]
        {
            use smol::Timer;
            smol::Task::spawn(async move {
                Timer::after(duration.clone()).await;
                let _ = addr.do_send(notification);
            }).detach();
        }
    }
}
