use std::fmt;
use std::fmt::{Display, Formatter};
use std::future::Future;
use std::ops::ControlFlow;
use std::sync::Arc;

use flume::{Receiver, Sender};
use futures_util::future::{self, Either};
use futures_util::FutureExt;

#[cfg(feature = "timing")]
use {futures_timer::Delay, std::time::Duration};

use crate::drop_notice::DropNotifier;
use crate::envelope::{MessageEnvelope, NonReturningEnvelope};
use crate::manager::{AddressMessage, BroadcastMessage};
use crate::refcount::{RefCounter, Strong, Weak};
use crate::{Actor, Address, Handler};

/// `Context` is used to control how the actor is managed and to get the actor's address from inside
/// of a message handler. Keep in mind that if a free-floating `Context` (i.e not running an actor via
/// [`Context::run`] or [`Context::attach`]) exists, **it will prevent the actor's channel from being
/// closed**, as more actors that could still then be added to the address, so closing early, while
/// maybe intuitive, would be subtly wrong.
pub struct Context<A> {
    /// Whether the actor is running. It is changed by the `stop` method as a flag to the `ActorManager`
    /// for it to call the `stopped` method on the actor
    running: bool,
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
        let (sender, receiver) = match message_cap {
            None => flume::unbounded(),
            Some(cap) => flume::bounded(cap),
        };
        let (broadcaster, broadcast_rx) = barrage::unbounded();

        let shared_drop_notifier = Arc::new(DropNotifier::new());

        let strong = Strong::new(shared_drop_notifier.subscribe());
        let weak = strong.downgrade();

        let addr = Address {
            sink: sender.clone().into_sink(),
            ref_counter: strong,
        };

        let context = Context {
            running: true,
            sender,
            broadcaster,
            ref_counter: weak,
            self_notifications: Vec::new(),
            receiver,
            broadcast_receiver: broadcast_rx.into_shared(),
            shared_drop_notifier,
        };
        (addr, context)
    }

    /// Attaches an actor of the same type listening to the same address as this actor is.
    /// They will operate in a message-stealing fashion, with no message handled by two actors.
    pub fn attach(&mut self, actor: A) -> impl Future<Output = A::Stop> {
        // Give the new context a new mailbox on the same broadcast channel, and then make this
        // receiver into a shared receiver.
        let broadcast_receiver = self.broadcast_receiver.clone().upgrade().into_shared();

        let ctx = Context {
            running: true,
            sender: self.sender.clone(),
            broadcaster: self.broadcaster.clone(),
            ref_counter: self.ref_counter.clone(),
            self_notifications: Vec::new(),
            receiver: self.receiver.clone(),
            broadcast_receiver,
            shared_drop_notifier: self.shared_drop_notifier.clone(),
        };
        ctx.run(actor)
    }

    /// Stop this actor as soon as it has finished processing current message. This means that the
    /// [`Actor::stopped`] method will be called.
    pub fn stop_self(&mut self) {
        self.running = false;
    }

    /// Stop all actors on this address. This is similar to [`Context::stop_self`] but it will stop
    /// all actors on this address.
    pub fn stop_all(&mut self) {
        assert!(self.broadcaster.send(BroadcastMessage::Shutdown).is_ok());
        self.receiver.drain();
    }

    /// Get an address to the current actor if there are still external addresses to the actor.
    pub fn address(&self) -> Result<Address<A>, ActorShutdown> {
        Ok(Address {
            sink: self.sender.clone().into_sink(),
            ref_counter: self.ref_counter.upgrade().ok_or(ActorShutdown)?,
        })
    }

    /// Handles a single self notification
    async fn handle_self_notification(&mut self, actor: &mut A) {
        if let Some(notification) = self.self_notifications.pop() {
            notification.handle(actor, self).await;
        }
    }

    /// Handle all self notifications, or until the actor is stopped
    async fn handle_self_notifications(&mut self, actor: &mut A) {
        while self.running && !self.self_notifications.is_empty() {
            self.handle_self_notification(actor).await;
        }
    }

    /// Run the given actor's main loop, handling incoming messages to its mailbox.
    pub async fn run(mut self, mut actor: A) -> A::Stop {
        actor.started(&mut self).await;

        // Idk why anyone would do this, but we have to check that they didn't already stop the actor
        // in the started method, otherwise it would kinda be a bug
        if !self.running {
            return actor.stopped().await;
        }

        // Listen for any messages for the ActorManager
        let addr_rx = self.receiver.clone();
        let broadcast_rx = self.broadcast_receiver.clone();

        let mut addr_recv = addr_rx.recv_async();
        let mut broadcast_recv = broadcast_rx.recv_async();

        loop {
            let next = future::select(addr_recv, broadcast_recv).await;

            let msg = match next {
                Either::Left((res, other)) => {
                    broadcast_recv = other;
                    addr_recv = addr_rx.recv_async();
                    Either::Right(res.unwrap())
                }
                Either::Right((res, other)) => {
                    addr_recv = other;
                    broadcast_recv = broadcast_rx.recv_async();
                    Either::Left(res.unwrap())
                }
            };

            // To avoid broadcast starvation, try receive a broadcast here
            if let Ok(Some(broadcast)) = broadcast_rx.try_recv() {
                match self.tick(Either::Left(broadcast), &mut actor).await {
                    ControlFlow::Continue(()) => {}
                    ControlFlow::Break(()) => {
                        return actor.stopped().await;
                    }
                }
            }

            match self.tick(msg, &mut actor).await {
                ControlFlow::Continue(()) => {}
                ControlFlow::Break(()) => {
                    return actor.stopped().await;
                }
            }
        }
    }

    /// Handle a message and immediate notifications, returning whether to exit from the manage loop
    /// or not.
    async fn tick(
        &mut self,
        msg: Either<BroadcastMessage<A>, AddressMessage<A>>,
        actor: &mut A,
    ) -> ControlFlow<()> {
        match msg {
            Either::Left(BroadcastMessage::Message(msg)) => msg.handle(actor, self).await,
            Either::Left(BroadcastMessage::Shutdown) => {
                self.running = false;
                return ControlFlow::Break(());
            }
            Either::Right(AddressMessage::Message(msg)) => {
                msg.handle(actor, self).await;
            }
            Either::Right(AddressMessage::LastAddress) => {
                if self.ref_counter.strong_count() == 0 {
                    self.stop_all();
                    self.running = false;
                    return ControlFlow::Break(());
                }
            }
        }

        if !self.running {
            return ControlFlow::Break(());
        }

        self.handle_self_notifications(actor).await;

        if !self.running {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    }

    /// This is a combinator to avoid holding !Sync references across await points
    fn recv_once(
        &self,
    ) -> impl Future<Output = Either<BroadcastMessage<A>, AddressMessage<A>>> + '_ {
        future::select(
            self.broadcast_receiver.recv_async(),
            self.receiver.recv_async(),
        )
        .map(|next| match next {
            Either::Left((res, _)) => Either::Left(res.unwrap()),
            Either::Right((res, _)) => Either::Right(res.unwrap()),
        })
    }

    /// Yields to the manager to handle one message.
    pub async fn yield_once(&mut self, act: &mut A) -> ControlFlow<()> {
        if self.running {
            self.handle_self_notification(act).await;
        }

        if self.running {
            self.tick(self.recv_once().await, act).await?;
        }

        if self.running {
            ControlFlow::Continue(())
        } else {
            ControlFlow::Break(())
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
        self.handle_self_notifications(actor).await;

        futures_util::pin_mut!(fut);

        let addr_rx = self.receiver.clone();
        let broadcast_rx = self.broadcast_receiver.clone();
        let mut addr_recv = addr_rx.recv_async();
        let mut broadcast_recv = broadcast_rx.recv_async();

        loop {
            let (next_msg, unfinished) = {
                let next_msg = future::select(addr_recv, broadcast_recv);
                futures_util::pin_mut!(next_msg);
                match future::select(fut, next_msg).await {
                    Either::Left((future_res, _)) => break future_res,
                    Either::Right(tuple) => tuple,
                }
            };

            let msg = match next_msg {
                Either::Left((res, other)) => {
                    broadcast_recv = other;
                    addr_recv = addr_rx.recv_async();
                    Either::Right(res.unwrap())
                }
                Either::Right((res, other)) => {
                    addr_recv = other;
                    broadcast_recv = broadcast_rx.recv_async();
                    Either::Left(res.unwrap())
                }
            };

            // To avoid broadcast starvation, try receive a broadcast here
            if let Ok(Some(broadcast)) = broadcast_rx.try_recv() {
                self.tick(Either::Left(broadcast), actor).await;
            }

            self.tick(msg, actor).await;
            fut = unfinished;
        }
    }

    /// Notify this actor with a message that is handled before any other messages from the general
    /// queue are processed (therefore, immediately). If multiple `notify` messages are queued,
    /// they will still be processed in the order that they are queued (i.e the immediate priority
    /// is only over other messages).
    pub fn notify<M>(&mut self, msg: M)
    where
        M: Send + 'static,
        A: Handler<M>,
    {
        let envelope = Box::new(NonReturningEnvelope::<A, M>::new(msg));
        self.self_notifications.push(envelope);
    }

    /// Notify all actors on this address with a given message, in a broadcast fashion. The message
    /// will be received once by all actors. Note that currently there is no message cap on the
    /// broadcast channel (it is unbounded).
    pub fn notify_all<M>(&mut self, msg: M)
    where
        M: Clone + Sync + Send + 'static,
        A: Handler<M>,
    {
        let envelope = NonReturningEnvelope::<A, M>::new(msg);
        let _ = self
            .broadcaster
            .send(BroadcastMessage::Message(Box::new(envelope)));
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
        let mut stopped = self.shared_drop_notifier.subscribe();

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
        let mut stopped = self.shared_drop_notifier.subscribe();

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
