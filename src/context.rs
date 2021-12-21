use std::fmt;
use std::fmt::{Display, Formatter};
use std::future::Future;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use flume::{Receiver, Sender};
use futures_core::stream::BoxStream;
use futures_util::future::{self, Either};
use futures_util::FutureExt;
use futures_util::StreamExt;

#[cfg(feature = "timing")]
use {futures_timer::Delay, std::time::Duration};

use crate::drop_notice::DropNotifier;
use crate::envelope::{MessageEnvelope, NonReturningEnvelope};
use crate::manager::{AddressMessage, BroadcastMessage};
use crate::refcount::{RefCounter, Strong, Weak};
use crate::{Actor, Address, Handler, KeepRunning, Message};

/// `Context` is used to control how the actor is managed and to get the actor's address from inside
/// of a message handler.
pub struct Context<A> {
    /// Whether the actor is running. It is changed by the `stop` method as a flag to the `ActorManager`
    /// for it to call the `stopping` method on the actor
    running: RunningState,
    /// Channel sender kept by the context to allow for the `Context::address` method to work
    sender: Sender<AddressMessage<A>>,
    /// Broadcast sender kept by the context to allow for the `Context::notify_all` method to work
    broadcaster: barrage::Sender<BroadcastMessage<A>>,
    /// Kept by the context to allow for it to check how many strong addresses exist to the actor
    ref_counter: Weak,
    /// Notifications that must be stored for immediate processing.
    self_notifications: Vec<Box<dyn MessageEnvelope<Actor = A>>>,
    receiver: Receiver<AddressMessage<A>>,
    broadcast_receiver: barrage::SharedReceiver<BroadcastMessage<A>>,
    /// Shared between all contexts on the same address
    shared_drop_notifier: Arc<DropNotifier>,
    /// Activates when this context is dropped. Used in [`Context::notify_interval`] and [`Context::notify_after`]
    /// to shutdown the tasks as soon as the context stops.
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
    /// # use xtra::{Context, Actor};
    /// #
    /// # struct MyActor;
    /// #
    /// # impl MyActor {
    /// #     fn new(_: usize) -> Self {
    /// #         MyActor
    /// #     }
    /// # }
    /// # impl Actor for MyActor {}
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
        let (sender, receiver) = match message_cap {
            None => flume::unbounded(),
            Some(cap) => flume::bounded(cap),
        };
        let (broadcaster, broadcast_rx) = barrage::unbounded();

        let shared_drop_notifier = Arc::new(DropNotifier::new());

        let strong = Strong::new(AtomicBool::new(true), shared_drop_notifier.subscribe());
        let weak = strong.downgrade();

        let addr = Address {
            sender: sender.clone(),
            ref_counter: strong,
        };

        let context = Context {
            running: RunningState::Running,
            sender,
            broadcaster,
            ref_counter: weak,
            self_notifications: Vec::new(),
            receiver,
            broadcast_receiver: broadcast_rx.into_shared(),
            shared_drop_notifier,
            drop_notifier: DropNotifier::new(),
        };
        (addr, context)
    }

    /// Attaches an actor of the same type listening to the same address as this actor is.
    /// They will operate in a message-stealing fashion, with no message handled by two actors.
    pub fn attach(&mut self, actor: A) -> impl Future<Output = ()> {
        // Give the new context a new mailbox on the same broadcast channel, and then make this
        // receiver into a shared receiver.
        let broadcast_receiver = self.broadcast_receiver.clone().upgrade().into_shared();

        let ctx = Context {
            running: RunningState::Running,
            sender: self.sender.clone(),
            broadcaster: self.broadcaster.clone(),
            ref_counter: self.ref_counter.clone(),
            self_notifications: Vec::new(),
            receiver: self.receiver.clone(),
            broadcast_receiver,
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
            sender: self.sender.clone(),
            ref_counter: self.ref_counter.upgrade().ok_or(ActorShutdown)?,
        })
    }

    /// Stop all actors on this address
    fn stop_all(&mut self) {
        if let Some(strong) = self.ref_counter.upgrade() {
            strong.mark_disconnected();
        }

        assert!(self.broadcaster.send(BroadcastMessage::Shutdown).is_ok());
        self.receiver.drain();
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

    /// Handles a single self notification, returning whether to continue the manage loop
    async fn handle_self_notification(&mut self, actor: &mut A) -> Option<bool> {
        if let Some(notification) = self.self_notifications.pop() {
            notification.handle(actor, self).await;
            return Some(self.check_running(actor).await);
        }
        None
    }

    /// Handle all self notifications, returning whether to continue the manage loop
    async fn handle_self_notifications(&mut self, actor: &mut A) -> bool {
        while let Some(continue_running) = self.handle_self_notification(actor).await {
            if !continue_running {
                return false;
            }
        }

        true
    }

    /// Run the given actor's main loop, handling incoming messages to its mailbox.
    pub async fn run(mut self, mut actor: A) {
        actor.started(&mut self).await;

        // Idk why anyone would do this, but we have to check that they didn't do ctx.stop()
        // in the started method, otherwise it would kinda be a bug
        if !self.check_running(&mut actor).await {
            actor.stopped().await;
            return;
        }

        // Similar to above
        if let Some(BroadcastMessage::Shutdown) = self.broadcast_receiver.try_recv().unwrap() {
            actor.stopped().await;
            return;
        }

        let mut inbox = self.inbox();

        while let Some(msg) = inbox.next().await {
            match self.tick(msg, &mut actor).await {
                ContinueManageLoop::Yes => {}
                ContinueManageLoop::ExitImmediately => break,
            }
        }

        actor.stopped().await;
    }

    /// Handle a message and immediate notifications, returning whether to exit from the manage loop
    /// or not.
    async fn tick(&mut self, msg: InboxMessage<A>, actor: &mut A) -> ContinueManageLoop {
        match msg.inner {
            Either::Left(BroadcastMessage::Message(msg)) => msg.handle(actor, self).await,
            Either::Right(AddressMessage::Message(msg)) => msg.handle(actor, self).await,
            Either::Left(BroadcastMessage::Shutdown) => {
                self.running = RunningState::Stopped;
            }
            Either::Right(AddressMessage::LastAddress) => {
                if self.ref_counter.strong_count() == 0 {
                    self.stop_all();
                    self.running = RunningState::Stopped;
                }
            }
        }

        if !self.check_running(actor).await {
            return ContinueManageLoop::ExitImmediately;
        }
        if !self.handle_self_notifications(actor).await {
            return ContinueManageLoop::ExitImmediately;
        }

        ContinueManageLoop::Yes
    }

    /// Yields to the manager to handle one message.
    pub async fn yield_once(&mut self, act: &mut A) {
        if let Some(keep_running) = self.handle_self_notification(act).await {
            if !keep_running {
                self.stop();
            }
            return;
        }

        if let Some(msg) = self.inbox().next().await {
            self.tick(msg, act).await;
        }
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
        if !self.handle_self_notifications(actor).await {
            self.stop();
        }

        futures_util::pin_mut!(fut);

        let mut fut = fut.fuse();
        let mut inbox = self.inbox();

        loop {
            let inbox_msg = inbox.next().fuse();
            futures_util::pin_mut!(inbox_msg);

            futures_util::select! {
                msg = inbox_msg => {
                    if let Some(msg) = msg {
                        self.tick(msg, actor).await;
                    }

                    // On `None`, the actor's inbox is closed (shutdown?) but our `fut` hasn't resolved yet. Essentially, we now just wait for `fut` to complete.
                }
                result = fut => {
                    return result
                }
            }
        }
    }

    /// Notify this actor with a message that is handled before any other messages
    /// from the general queue are processed (therefore, immediately). If multiple
    /// `notify` messages are queued, they will still be processed in the order that they
    /// are queued (i.e the immediate priority is only over other messages).
    pub fn notify<M>(&mut self, msg: M)
    where
        M: Message,
        A: Handler<M>,
    {
        let envelope = Box::new(NonReturningEnvelope::<A, M>::new(msg));
        self.self_notifications.push(envelope);
    }

    /// Notify all actors on this address actor with a message that is handled after any other
    /// messages from the general queue are processed.
    pub fn notify_all<M>(&mut self, msg: M)
    where
        M: Message + Clone + Sync,
        A: Handler<M>,
    {
        let envelope = NonReturningEnvelope::<A, M>::new(msg);
        let _ = self
            .broadcaster
            .send(BroadcastMessage::Message(Box::new(envelope)));
    }

    /// Notify the actor with a synchronously handled message every interval until it is stopped
    /// (either directly with [`Context::stop`](struct.Context.html#method.stop), or for a lack of
    /// strong [`Address`es](address/struct.Address.html)). This does not take priority over other messages.
    #[cfg(feature = "timing")]
    pub fn notify_interval<F, M>(
        &mut self,
        duration: Duration,
        constructor: F,
    ) -> Result<impl Future<Output = ()>, ActorShutdown>
    where
        F: Send + 'static + Fn() -> M,
        M: Message,
        A: Handler<M>,
    {
        let addr = self.address()?.downgrade();
        let mut stopped = self.drop_notifier.subscribe();

        let fut = async move {
            loop {
                let delay = Delay::new(duration);
                match future::select(delay, &mut stopped).await {
                    Either::Left(_) => {
                        if addr.do_send(constructor()).is_err() {
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

    /// Notify the actor with a synchronously handled message after a certain duration has elapsed.
    /// This does not take priority over other messages.
    pub fn notify_after<M>(
        &mut self,
        duration: Duration,
        notification: M,
    ) -> Result<impl Future<Output = ()>, ActorShutdown>
    where
        M: Message,
        A: Handler<M>,
    {
        let addr = self.address()?.downgrade();
        let mut stopped = self.drop_notifier.subscribe();

        let fut = async move {
            let delay = Delay::new(duration);
            match future::select(delay, &mut stopped).await {
                Either::Left(_) => {
                    let _ = addr.do_send(notification);
                }
                Either::Right(_) => {
                    // Context stopped before the end of the delay was reached
                }
            }
        };

        Ok(fut)
    }

    fn inbox(&self) -> Inbox<A> {
        Inbox::new(self.receiver.clone(), self.broadcast_receiver.clone())
    }
}

struct Inbox<A> {
    inner: BoxStream<'static, Either<BroadcastMessage<A>, AddressMessage<A>>>,
}

impl<A> Inbox<A>
where
    A: 'static,
{
    fn new(
        address_receiver: Receiver<AddressMessage<A>>,
        broadcast_receiver: barrage::SharedReceiver<BroadcastMessage<A>>,
    ) -> Self {
        let broadcast_stream =
            futures_util::stream::unfold(broadcast_receiver, |broadcast_receiver| async move {
                match broadcast_receiver.recv_async().await {
                    Ok(msg) => Some((msg, broadcast_receiver)),
                    Err(_) => None,
                }
            })
            .map(Either::Left)
            .boxed();

        let address_stream =
            futures_util::stream::unfold(address_receiver, |address_receiver| async move {
                match address_receiver.recv_async().await {
                    Ok(msg) => Some((msg, address_receiver)),
                    Err(_) => None,
                }
            })
            .map(Either::Right)
            .boxed();

        Self {
            inner: futures_util::stream_select!(broadcast_stream, address_stream).boxed(),
        }
    }

    pub async fn next(&mut self) -> Option<InboxMessage<A>> {
        let inner = self.inner.next().await?;

        Some(InboxMessage { inner })
    }
}

struct InboxMessage<A> {
    inner: Either<BroadcastMessage<A>, AddressMessage<A>>,
}

/// If and how to continue the manage loop
#[derive(PartialEq, Eq, Debug, Copy, Clone)]
enum ContinueManageLoop {
    Yes,
    ExitImmediately,
}

/// The operation failed because the actor is being shut down
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ActorShutdown;

impl Display for ActorShutdown {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str("Actor is shutting down")
    }
}
