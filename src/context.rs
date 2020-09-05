use crate::envelope::{MessageEnvelope, NonReturningEnvelope};
use crate::manager::{ContinueManageLoop, ManagerMessage};
use crate::{Actor, Address, Handler, KeepRunning, Message, ActorManager};
use futures::future::{self, Either, Future};
#[cfg(any(
    doc,
    feature = "with-tokio-0_2",
    feature = "with-async_std-1",
    feature = "with-wasm_bindgen-0_2",
    feature = "with-smol-0_4"
))]
use std::time::Duration;
use flume::{Receiver, Sender};
use crate::address::{RefCounter, Weak};

/// `Context` is used to control how the actor is managed and to get the actor's address from inside
/// of a message handler.
pub struct Context<A: Actor> {
    /// Whether the actor is running. It is changed by the `stop` method as a flag to the `ActorManager`
    /// for it to call the `stopping` method on the actor
    pub(crate) running: bool,
    /// Channel sender kept by the context to allow for the `Context::address` method to work
    sender: Sender<ManagerMessage<A>>,
    /// Kept by the context to allow for it to tcheck how many strong addresses exist to the actor
    ref_counter: Weak,
    /// Notifications that must be stored for immediate processing.
    pub(crate) immediate_notifications: Vec<Box<dyn MessageEnvelope<Actor = A>>>,
    pub(crate) receiver: Receiver<ManagerMessage<A>>,
}

impl<A: Actor> Context<A> {
    pub(crate) fn new(
        sender: Sender<ManagerMessage<A>>,
        ref_counter: Weak,
        receiver: Receiver<ManagerMessage<A>>,
    ) -> Self {
        Context {
            running: true,
            sender,
            ref_counter,
            immediate_notifications: Vec::new(),
            receiver,
        }
    }

    // TODO spawn_actor

    /// Creates another actor of the same type listening to the same address as this actor is.
    /// They will operate in a message-stealing fashion, with no message handled by two actors.
    /// See [`Actor::create_multiple`](trait.Actor.html#method.create_multiple) for more info.
    pub fn create_actor(&mut self, actor: A) -> ActorManager<A> {
        ActorManager::start(
            actor,
            self.sender.clone(),
            self.receiver.clone(),
            self.ref_counter.clone()
        )
    }

    /// Stop the actor as soon as it has finished processing current message. This will mean that the
    /// [`Actor::stopping`](trait.Actor.html#method.stopping) method will be called.
    /// If that returns [`KeepRunning::No`](enum.KeepRunning.html#variant.No), any subsequent attempts
    /// to send messages to this actor will return the [`Disconnected`](struct.Disconnected.html) error.
    pub fn stop(&mut self) {
        self.running = false;
    }

    /// Get an address to the current actor if the actor is still running.
    // TODO check this still works when we want it to
    pub fn address(&self) -> Result<Address<A>, ActorShutdown> {
        if self.running {
            Ok(Address {
                sender: self.sender.clone(),
                ref_counter: self.ref_counter.upgrade().ok_or(ActorShutdown)?,
            })
        } else {
            Err(ActorShutdown)
        }
    }

    /// Check if the Context is still set to running, returning whether to continue the manage loop
    pub(crate) async fn check_running(&mut self, actor: &mut A) -> bool {
        // Check if the context was stopped, and if so return, thereby dropping the
        // manager and calling `stopped` on the actor
        if !self.running {
            let keep_running = actor.stopping(self).await;

            if keep_running == KeepRunning::Yes {
                self.running = true;
            } else {
                return false;
            }
        }

        true
    }

    /// Handles a single immediate notification, returning whether to continue the manage loop
    async fn handle_immediate_notification(&mut self, actor: &mut A) -> Option<bool> {
        if let Some(notification) = self.immediate_notifications.pop() {
            notification.handle(actor, self).await;
            return Some(self.check_running(actor).await);
        }
        None
    }

    /// Handle all immediate notifications, returning whether to continue the manage loop
    async fn handle_immediate_notifications(&mut self, actor: &mut A) -> bool {
        while let Some(continue_running) = self.handle_immediate_notification(actor).await {
            if !continue_running {
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
                if !self.check_running(actor).await {
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
                if self.ref_counter.strong_count() == 0 {
                    self.stop();
                    return ContinueManageLoop::ProcessNotifications;
                }
            }
        }
        ContinueManageLoop::Yes
    }

    /// Yields to the manager to handle one message.
    pub async fn yield_once(&mut self, act: &mut A) {
        if let Some(keep_running) = self.handle_immediate_notification(act).await {
            if !keep_running {
                self.stop();
            }
            return;
        }

        match self.receiver.recv_async().await {
            Ok(msg) => {
                self.handle_message(msg, act).await;
            }
            Err(_) => self.stop(),
        }
    }

    /// Handle any incoming messages for the actor while running a given future.
    ///
    /// # Example
    ///
    #[cfg_attr(docsrs, doc("```"))]
    #[cfg_attr(docsrs, doc(include = "../examples/interleaved_messages.rs"))]
    #[cfg_attr(docsrs, doc("```"))]
    pub async fn handle_while<F, R>(&mut self, act: &mut A, mut fut: F) -> R
    where
        F: Future<Output = R> + Unpin,
    {
        if !self.handle_immediate_notifications(act).await {
            self.stop();
        }

        loop {
            let recv = self.receiver.recv_async();
            let (msg, unfinished) = match future::select(fut, recv).await {
                Either::Left((res, _)) => break res,
                Either::Right(tuple) => tuple,
            };

            match msg {
                Ok(msg) => {
                    self.handle_message(msg, act).await;
                },
                Err(_) => self.stop(),
            }
            fut = unfinished;
        }
    }

    /// Notify this actor with a message that is handled synchronously before any other messages
    /// from the general queue are processed (therefore, immediately). If multiple
    /// `notify_immediately` messages are queued, they will still be processed in the order that they
    /// are queued (i.e the immediate priority is only over other messages).
    pub fn notify_immediately<M>(&mut self, msg: M)
    where
        M: Message,
        A: Handler<M>,
    {
        let envelope = Box::new(NonReturningEnvelope::<A, M>::new(msg));
        self.immediate_notifications.push(envelope);
    }

    /// Notify this actor with a message that is handled after any other messages from the general
    /// queue are processed. This is almost equivalent to calling send on
    /// [`Context::address()`](struct.Context.html#method.address), but will never fail to send
    /// the message.
    pub fn notify_later<M>(&mut self, msg: M)
    where
        M: Message,
        A: Handler<M>,
    {
        let envelope = NonReturningEnvelope::<A, M>::new(msg);
        let _ = self
            .sender
            .send(ManagerMessage::LateNotification(Box::new(envelope)));
    }

    /// Notify the actor with a synchronously handled message every interval until it is stopped
    /// (either directly with [`Context::stop`](struct.Context.html#method.stop), or for a lack of
    /// strong [`Address`es](struct.Address.html)). This does not take priority over other messages.
    #[cfg(any(
        doc,
        feature = "with-tokio-0_2",
        feature = "with-async_std-1",
        feature = "with-wasm_bindgen-0_2",
        feature = "with-smol-0_4"
    ))]
    #[cfg_attr(docsrs, doc(cfg(feature = "with-tokio-0_2")))]
    #[cfg_attr(docsrs, doc(cfg(feature = "with-async_std-1")))]
    #[cfg_attr(docsrs, doc(cfg(feature = "with-wasm_bindgen-0_2")))]
    #[cfg_attr(docsrs, doc(cfg(feature = "with-smol-0_4")))]
    pub fn notify_interval<F, M>(
        &mut self,
        duration: Duration,
        constructor: F
    ) -> Result<(), ActorShutdown>
    where
        F: Send + 'static + Fn() -> M,
        M: Message,
        A: Handler<M>,
    {
        let addr = self.address()?;

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

        #[cfg(feature = "with-smol-0_4")]
        {
            use smol::Timer;
            smol::spawn(async move {
                loop {
                    Timer::after(duration.clone()).await;
                    if let Err(_) = addr.do_send(constructor()) {
                        break;
                    }
                }
            })
            .detach();
        }

        Ok(())
    }

    /// Notify the actor with a synchronously handled message after a certain duration has elapsed.
    /// This does not take priority over other messages.
    #[cfg(any(
        doc,
        feature = "with-tokio-0_2",
        feature = "with-async_std-1",
        feature = "with-wasm_bindgen-0_2",
        feature = "with-smol-0_4"
    ))]
    #[cfg_attr(docsrs, doc(cfg(feature = "with-tokio-0_2")))]
    #[cfg_attr(docsrs, doc(cfg(feature = "with-async_std-1")))]
    #[cfg_attr(docsrs, doc(cfg(feature = "with-wasm_bindgen-0_2")))]
    #[cfg_attr(docsrs, doc(cfg(feature = "with-smol-0_4")))]
    pub fn notify_after<M>(&mut self, duration: Duration, notification: M) -> Result<(), ActorShutdown>
    where
        M: Message,
        A: Handler<M>,
    {
        let addr = self.address()?;

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

        #[cfg(feature = "with-smol-0_4")]
        {
            use smol::Timer;
            smol::spawn(async move {
                Timer::after(duration.clone()).await;
                let _ = addr.do_send(notification);
            })
            .detach();
        }

        Ok(())
    }
}

/// The operation failed because the actor is being shut down
pub struct ActorShutdown;
