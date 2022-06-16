use futures_util::future::{self, Either};
use std::fmt;
use std::fmt::{Display, Formatter};
use std::future::Future;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

#[cfg(feature = "timing")]
use {futures_timer::Delay, std::time::Duration};

use crate::drop_notice::DropNotifier;
use crate::envelope::BroadcastEnvelopeConcrete;
use crate::inbox::ActorMessage;
use crate::manager::ContinueManageLoop;
use crate::refcount::{Strong, Weak};
use crate::{inbox, Actor, Address, Handler, KeepRunning};

/// `Context` is used to control how the actor is managed and to get the actor's address from inside
/// of a message handler.
pub struct Context<A> {
    /// Whether the actor is running. It is changed by the `stop` method as a flag to the `ActorManager`
    /// for it to call the `stopping` method on the actor
    running: RunningState,
    /// Kept by the context to allow for it to check how many strong addresses exist to the actor
    ref_counter: Weak,
    receiver: inbox::Receiver<A>,
    /// Shared between all contexts on the same address
    shared_drop_notifier: Arc<DropNotifier>,
    /// Activates when this context is dropped. Used in [`Context::notify_interval`] and [`Context::notify_after`]
    /// to shutdown the tasks as soon as the context stops.
    #[cfg_attr(not(feature = "timing"), allow(dead_code))]
    drop_notifier: DropNotifier,
}

#[derive(Eq, PartialEq, Copy, Clone)]
enum RunningState {
    Running,
    Stopping,
    Stopped,
}

impl<A: Actor> Context<A> {
    /// Creates a new actor context with a given mailbox capacity, returning an address to the actor
    /// and the context. This can be used as a builder to add more actors to an address before
    /// any have started.
    ///
    /// # Example
    ///
    /// ```rust
    /// # use xtra::prelude::*;
    /// #
    /// # struct MyActor;
    /// #
    /// # impl MyActor {
    /// #     fn new(_: usize) -> Self {
    /// #         MyActor
    /// #     }
    /// # }
    /// # #[async_trait] impl Actor for MyActor {type Stop = (); async fn stopped(self) -> Self::Stop {} }
    /// # async {
    /// let (addr, mut ctx) = Context::new(Some(32));
    /// for n in 0..3 {
    ///     smol::spawn(ctx.attach(MyActor::new(n))).detach();
    /// }
    /// ctx.run(MyActor::new(4)).await;
    /// # };
    ///
    /// ```
    pub fn new(message_cap: Option<usize>) -> (Address<A>, Self) {
        let shared_drop_notifier = Arc::new(DropNotifier::new());

        let strong = Strong::new(AtomicBool::new(true), shared_drop_notifier.subscribe());
        let weak = strong.downgrade();

        let (sender, rx) = inbox::new(message_cap);

        let addr = Address {
            sender,
            ref_counter: strong,
        };

        let context = Context {
            running: RunningState::Running,
            ref_counter: weak,
            receiver: rx,
            shared_drop_notifier,
            drop_notifier: DropNotifier::new(),
        };
        (addr, context)
    }

    /// Attaches an actor of the same type listening to the same address as this actor is.
    /// They will operate in a message-stealing fashion, with no message handled by two actors.
    pub fn attach(&mut self, actor: A) -> impl Future<Output = A::Stop> {
        let ctx = Context {
            running: RunningState::Running,
            ref_counter: self.ref_counter.clone(),
            receiver: self.receiver.cloned_new_broadcast_mailbox(),
            shared_drop_notifier: self.shared_drop_notifier.clone(),
            drop_notifier: DropNotifier::new(),
        };
        ctx.run(actor)
    }

    /// Stop the actor as soon as it has finished processing current message. This will mean that the
    /// [`Actor::stopping`](trait.Actor.html#method.stopping) method will be called.
    pub fn stop(&mut self) {
        self.running = RunningState::Stopping;
    }

    /// Get an address to the current actor if there are still external addresses to the actor.
    pub fn address(&self) -> Result<Address<A>, ActorShutdown> {
        Ok(Address {
            sender: self.receiver.sender(),
            ref_counter: self.ref_counter.upgrade().ok_or(ActorShutdown)?,
        })
    }

    /// Stop all actors on this address
    fn stop_all(&mut self) {
        if let Some(strong) = self.ref_counter.upgrade() {
            strong.mark_disconnected();
        }

        self.receiver.sender().shutdown();
    }

    /// Check if the Context is still set to running, returning whether to continue the manage loop
    async fn check_running(&mut self, actor: &mut A) -> bool {
        // Check if the context was stopped, and if so return, thereby dropping the
        // manager and calling `stopped` on the actor
        match self.running {
            RunningState::Running => true,
            RunningState::Stopping => {
                let keep_running = actor.stopping(self).await;

                match keep_running {
                    KeepRunning::Yes => {
                        self.running = RunningState::Running;
                        true
                    }
                    KeepRunning::StopSelf => {
                        self.running = RunningState::Stopped;
                        false
                    }
                    KeepRunning::StopAll => {
                        self.stop_all();
                        self.running = RunningState::Stopped;
                        false
                    }
                }
            }
            RunningState::Stopped => false,
        }
    }

    /// Run the given actor's main loop, handling incoming messages to its mailbox.
    pub async fn run(mut self, mut actor: A) -> A::Stop {
        actor.started(&mut self).await;

        // Idk why anyone would do this, but we have to check that they didn't do ctx.stop()
        // in the started method, otherwise it would kinda be a bug
        if !self.check_running(&mut actor).await {
            self.stop_all();
            return actor.stopped().await;
        }

        loop {
            match self.tick(self.receiver.receive().await, &mut actor).await {
                ContinueManageLoop::Yes => {}
                ContinueManageLoop::ExitImmediately => {
                    return actor.stopped().await;
                }
            }
        }
    }

    /// Handle a message and immediate notifications, returning whether to exit from the manage loop
    /// or not.
    async fn tick(&mut self, msg: ActorMessage<A>, actor: &mut A) -> ContinueManageLoop {
        match msg {
            ActorMessage::BroadcastMessage(msg) => msg.handle(actor, self).await,
            ActorMessage::Shutdown => {
                self.running = RunningState::Stopped;
                return ContinueManageLoop::ExitImmediately;
            }
            ActorMessage::StolenMessage(msg) => msg.handle(actor, self).await,
        }

        if !self.check_running(actor).await {
            return ContinueManageLoop::ExitImmediately;
        }

        ContinueManageLoop::Yes
    }

    /// Yields to the manager to handle one message.
    pub async fn yield_once(&mut self, act: &mut A) {
        self.tick(self.receiver.receive().await, act).await;
    }

    /// Handle any incoming messages for the actor while running a given future.
    ///
    /// # Example
    ///
    #[cfg_attr(docsrs, doc("```"))]
    #[cfg_attr(docsrs, doc(include = "../examples/interleaved_messages.rs"))]
    #[cfg_attr(docsrs, doc("```"))]
    pub async fn handle_while<F, R>(&mut self, actor: &mut A, fut: F) -> R
    where
        F: Future<Output = R>,
    {
        futures_util::pin_mut!(fut);

        // TODO try simplify this
        let rx = self.receiver.cloned_same_broadcast_mailbox();

        loop {
            let (msg, unfinished) = {
                let next_msg = rx.receive();
                futures_util::pin_mut!(next_msg);
                match future::select(fut, next_msg).await {
                    Either::Left((future_res, _)) => break future_res,
                    Either::Right(tuple) => tuple,
                }
            };

            self.tick(msg, actor).await;
            fut = unfinished;
        }
    }

    /// Notify all actors on this address with a given message, in a broadcast fashion. The message
    /// will be received once by all actors. Note that currently there is no message cap on the
    /// broadcast channel (it is unbounded).
    pub fn notify_all<M>(&mut self, msg: M)
    where
        M: Clone + Sync + Send + 'static,
        A: Handler<M>,
    {
        let envelope = BroadcastEnvelopeConcrete::<A, M>::new(msg, 1);
        let _ = self
            .receiver
            .sender() // TODO inefficient
            .broadcast(Arc::new(envelope));
    }

    /// Notify the actor with a message every interval until it is stopped (either directly with
    /// [`Context::stop`](struct.Context.html#method.stop), or for a lack of strong
    /// [`Address`es](address/struct.Address.html)). This does not take priority over other messages.
    ///
    /// This function is subject to back-pressure by the actor's mailbox. Thus, if the mailbox is full
    /// the loop will wait until a slot is available. It is therefore not guaranteed that a message
    /// will be delivered at exactly `duration` intervals.
    #[cfg(feature = "timing")]
    pub fn notify_interval<F, M>(
        &mut self,
        duration: Duration,
        constructor: F,
    ) -> Result<impl Future<Output = ()>, ActorShutdown>
    where
        F: Send + 'static + Fn() -> M,
        M: Send + 'static,
        A: Handler<M>,
    {
        let addr = self.address()?.downgrade();
        let mut stopped = self.drop_notifier.subscribe();

        let fut = async move {
            loop {
                let delay = Delay::new(duration);
                match future::select(delay, &mut stopped).await {
                    Either::Left(_) => {
                        if addr.send(constructor()).await.is_err() {
                            break;
                        }
                    }
                    Either::Right(_) => {
                        // Context stopped before the end of the delay was reached
                        break;
                    }
                }
            }
        };

        Ok(fut)
    }

    /// Notify the actor with a message after a certain duration has elapsed. This does not take
    /// priority over other messages.
    ///
    /// This function is subject to back-pressure by the actor's mailbox. If the mailbox is full once
    /// the timer expires, the future will continue to block until the message is delivered.
    #[cfg(feature = "timing")]
    pub fn notify_after<M>(
        &mut self,
        duration: Duration,
        notification: M,
    ) -> Result<impl Future<Output = ()>, ActorShutdown>
    where
        M: Send + 'static,
        A: Handler<M>,
    {
        let addr = self.address()?.downgrade();
        let mut stopped = self.drop_notifier.subscribe();

        let fut = async move {
            let delay = Delay::new(duration);
            match future::select(delay, &mut stopped).await {
                Either::Left(_) => {
                    let _ = addr.send(notification).await;
                }
                Either::Right(_) => {
                    // Context stopped before the end of the delay was reached
                }
            }
        };

        Ok(fut)
    }
}

/// The operation failed because the actor is being shut down
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ActorShutdown;

impl Display for ActorShutdown {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str("Actor is shutting down")
    }
}
