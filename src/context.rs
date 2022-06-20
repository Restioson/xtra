use crate::envelope::BroadcastEnvelopeConcrete;
use crate::inbox::{rx::RxStrong, ActorMessage};
use crate::{inbox, Actor, Address, Handler};
use futures_util::future::{self, Either};
use std::fmt;
use std::fmt::{Display, Formatter};
use std::future::Future;
use std::ops::ControlFlow;
use std::sync::Arc;
#[cfg(feature = "timing")]
use {futures_timer::Delay, std::time::Duration};

/// `Context` is used to control how the actor is managed and to get the actor's address from inside
/// of a message handler. Keep in mind that if a free-floating `Context` (i.e not running an actor via
/// [`Context::run`] or [`Context::attach`]) exists, **it will prevent the actor's channel from being
/// closed**, as more actors that could still then be added to the address, so closing early, while
/// maybe intuitive, would be subtly wrong.
pub struct Context<A> {
    /// Whether the actor is running. It is changed by the `stop` method as a flag to the `ActorManager`
    /// for it to call the `stopping` method on the actor
    running: bool,
    receiver: inbox::Receiver<A, RxStrong>,
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
    /// # async { // This does not actually run because there is nothing to assert
    /// let (addr, mut ctx) = Context::new(Some(32));
    /// for n in 0..3 {
    ///     smol::spawn(ctx.attach(MyActor::new(n))).detach();
    /// }
    /// ctx.run(MyActor::new(4)).await;
    /// # };
    ///
    /// ```
    pub fn new(message_cap: Option<usize>) -> (Address<A>, Self) {
        let (tx, rx) = inbox::new(message_cap);

        let context = Context {
            running: true,
            receiver: rx,
        };

        (Address(tx), context)
    }

    /// Attaches an actor of the same type listening to the same address as this actor is.
    /// They will operate in a message-stealing fashion, with no message handled by two actors.
    pub fn attach(&mut self, actor: A) -> impl Future<Output = A::Stop> {
        let ctx = Context {
            running: true,
            receiver: self.receiver.deep_clone(),
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
        // We only need to shut down if there are still any strong senders left
        if let Some(sender) = self.receiver.sender() {
            sender.shutdown_and_drain();
        }
    }

    /// Get an address to the current actor if there are still external addresses to the actor.
    pub fn address(&self) -> Result<Address<A>, ActorShutdown> {
        self.receiver.sender().ok_or(ActorShutdown).map(Address)
    }

    /// Run the given actor's main loop, handling incoming messages to its mailbox.
    pub async fn run(mut self, mut actor: A) -> A::Stop {
        actor.started(&mut self).await;

        // Idk why anyone would do this, but we have to check that they didn't already stop the actor
        // in the started method, otherwise it would kinda be a bug
        if !self.running {
            return actor.stopped().await;
        }

        loop {
            match self.tick(self.receiver.receive().await, &mut actor).await {
                ControlFlow::Continue(()) => {}
                ControlFlow::Break(()) => {
                    return actor.stopped().await;
                }
            }
        }
    }

    /// Handle a message and immediate notifications, returning whether to exit from the manage loop
    /// or not.
    async fn tick(&mut self, msg: ActorMessage<A>, actor: &mut A) -> ControlFlow<()> {
        match msg {
            ActorMessage::ToAllActors(msg) => msg.handle(actor, self).await,
            ActorMessage::Shutdown => {
                self.running = false;
                return ControlFlow::Break(());
            }
            ActorMessage::ToOneActor(msg) => msg.handle(actor, self).await,
        }

        if !self.running {
            return ControlFlow::Break(());
        }

        ControlFlow::Continue(())
    }

    /// Yields to the manager to handle one message.
    pub async fn yield_once(&mut self, act: &mut A) {
        self.tick(self.receiver.receive().await, act).await;
    }

    /// Joins on a future by handling all incoming messages whilst polling it. The future will
    /// always be polled to completion, even if the actor is stopped. If the actor is stopped,
    /// handling of messages will cease, and only the future will be polled. It is somewhat
    /// analagous to [`futures::join`](https://docs.rs/futures/latest/futures/macro.join.html),
    /// but it will not wait for the incoming stream of messages from addresses to end before
    /// returning - it will return as soon as the provided future does.
    ///
    /// ## Example
    ///
    /// ```rust
    /// # use std::time::Duration;
    /// use futures_util::FutureExt;
    /// # use xtra::prelude::*;
    /// # use smol::future;
    /// # struct MyActor;
    /// # #[async_trait] impl Actor for MyActor { type Stop = (); async fn stopped(self) {} }
    ///
    /// struct Stop;
    /// struct Joining;
    ///
    /// #[async_trait]
    /// impl Handler<Stop> for MyActor {
    ///     type Return = ();
    ///
    ///     async fn handle(&mut self, _msg: Stop, ctx: &mut Context<Self>) {
    ///         ctx.stop_self();
    ///     }
    /// }
    ///
    /// #[async_trait]
    /// impl Handler<Joining> for MyActor {
    ///     type Return = bool;
    ///
    ///     async fn handle(&mut self, _msg: Joining, ctx: &mut Context<Self>) -> bool {
    ///         let addr = ctx.address().unwrap();
    ///         let join = ctx.join(self, future::ready::<()>(()));
    ///         let _ = addr.send(Stop).split_receiver().await;
    ///
    ///         // Actor is stopping, but the join should still evaluate correctly
    ///         join.now_or_never().is_some()
    ///     }
    /// }
    ///
    /// # #[cfg(feature = "with-smol-1")]
    /// # smol::block_on(async {
    /// let addr = MyActor.create(None).spawn(&mut xtra::spawn::Smol::Global);
    /// assert!(addr.is_connected());
    /// assert_eq!(addr.send(Joining).await, Ok(true)); // Assert that the join did evaluate the future
    /// # })
    #[cfg_attr(docsrs, doc("```"))]
    #[cfg_attr(docsrs, doc(include = "../examples/interleaved_messages.rs"))]
    #[cfg_attr(docsrs, doc("```"))]
    pub async fn join<F, R>(&mut self, actor: &mut A, fut: F) -> R
    where
        F: Future<Output = R>,
    {
        futures_util::pin_mut!(fut);
        match self.select(actor, fut).await {
            Either::Left(res) => res,
            Either::Right(fut) => fut.await,
        }
    }

    /// Handle any incoming messages for the actor while running a given future. This is similar to
    /// [`Context::join`], but will exit if the actor is stopped, returning the future. Returns
    /// `Ok` with the result of the future if it was successfully completed, or `Err` with the
    /// future if the actor was stopped before it could complete. It is analagous to
    /// [`futures::select`](https://docs.rs/futures/latest/futures/macro.select.html).
    ///
    /// ## Example
    ///
    /// ```rust
    /// # use std::time::Duration;
    /// use futures_util::future::Either;
    /// # use xtra::prelude::*;
    /// # use smol::future;
    /// # struct MyActor;
    /// # #[async_trait] impl Actor for MyActor { type Stop = (); async fn stopped(self) {} }
    ///
    /// struct Stop;
    /// struct Selecting;
    ///
    /// #[async_trait]
    /// impl Handler<Stop> for MyActor {
    ///     type Return = ();
    ///
    ///     async fn handle(&mut self, _msg: Stop, ctx: &mut Context<Self>) {
    ///         ctx.stop_self();
    ///     }
    /// }
    ///
    /// #[async_trait]
    /// impl Handler<Selecting> for MyActor {
    ///     type Return = bool;
    ///
    ///     async fn handle(&mut self, _msg: Selecting, ctx: &mut Context<Self>) -> bool {
    ///         // Actor is still running, so this will return Either::Left
    ///         match ctx.select(self, future::ready(1)).await {
    ///             Either::Left(ans) => println!("Answer is: {}", ans),
    ///             Either::Right(_) => panic!("How did we get here?"),
    ///         };
    ///
    ///         let addr = ctx.address().unwrap();
    ///         let select = ctx.select(self, future::pending::<()>());
    ///         let _ = addr.send(Stop).split_receiver().await;
    ///
    ///         // Actor is stopping, so this will return Err, even though the future will
    ///         // usually never complete.
    ///         matches!(select.await, Either::Right(_))
    ///     }
    /// }
    ///
    /// # #[cfg(feature = "with-smol-1")]
    /// # smol::block_on(async {
    /// let addr = MyActor.create(None).spawn(&mut xtra::spawn::Smol::Global);
    /// assert!(addr.is_connected());
    /// assert_eq!(addr.send(Selecting).await, Ok(true)); // Assert that the select did end early
    /// # })
    ///
    /// ```
    pub async fn select<F, R>(&mut self, actor: &mut A, mut fut: F) -> Either<R, F>
    where
        F: Future<Output = R> + Unpin,
    {
        while self.running {
            let (msg, unfinished) = {
                let mut next_msg = self.receiver.receive();
                match future::select(fut, &mut next_msg).await {
                    Either::Left((future_res, _)) => {
                        // TODO(?) should this be here? Preserves ordering but may increase time for
                        // this future to return
                        if let Some(msg) = next_msg.cancel() {
                            self.tick(msg, actor).await;
                        }

                        return Either::Left(future_res);
                    }
                    Either::Right(tuple) => tuple,
                }
            };

            self.tick(msg, actor).await;
            fut = unfinished;
        }

        Either::Right(fut)
    }

    /// Notify all actors on this address with a given message, in a broadcast fashion. The message
    /// will be received once by all actors. Note that currently there is no message cap on the
    /// broadcast channel (it is unbounded).
    pub fn notify_all<M>(&mut self, msg: M) -> Result<(), ActorShutdown>
    where
        M: Clone + Sync + Send + 'static,
        A: Handler<M, Return = ()>,
    {
        let envelope = BroadcastEnvelopeConcrete::<A, M>::new(msg);
        self.receiver
            .sender()
            .ok_or(ActorShutdown)?
            .broadcast(Arc::new(envelope), 1)
            .map_err(|_| ActorShutdown)
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
        let mut stopped = addr.join();

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
        let mut stopped = addr.join();

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
