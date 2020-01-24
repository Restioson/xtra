use crate::envelope::{Envelope, NonReturningEnvelope};
use crate::manager::ManagerMessage;
use crate::{Actor, Address, Message, Handler};
#[cfg(any(doc, feature = "with-tokio-0_2", feature = "with-async_std-1"))]
use {crate::AddressExt, std::time::Duration};

/// `Context` is used to signal things to the [`ActorManager`](struct.ActorManager.html)'s
/// management loop. Currently, it can be used to stop the actor ([`Context::stop`](struct.Context.html#method.stop)).
pub struct Context<A: Actor> {
    /// Whether the actor is running. It is changed by the `stop` method as a flag to the `ActorManager`
    /// for it to call the `stopping` method on the actor
    pub(crate) running: bool,
    /// The address kept by the context to allow for the `Context::address` method to work.
    address: Address<A>,
    /// Notifications that must be stored for immediate processing.
    pub(crate) immediate_notifications: Vec<Box<dyn Envelope<Actor = A>>>,
}

impl<A: Actor> Context<A> {
    pub(crate) fn new(address: Address<A>) -> Self {
        Context {
            running: true,
            address,
            immediate_notifications: Vec::with_capacity(1),
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
            Some(self.address.clone())
        } else {
            None
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
    #[doc(cfg(feature = "with-tokio-0_2"))]
    #[doc(cfg(feature = "with-async_std-1"))]
    #[cfg(any(doc, feature = "with-tokio-0_2", feature = "with-async_std-1"))]
    pub fn notify_interval<F, M>(&mut self, duration: Duration, constructor: F)
    where
        F: Send + 'static + Fn() -> M,
        M: Message,
        A: Handler<M> + Send,
    {
        let addr = self.address.downgrade();

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
    }

    /// Notify the actor with a synchronously handled message after a certain duration has elapsed.
    /// This does not take priority over other messages.
    #[doc(cfg(feature = "with-async_std-1"))]
    #[cfg(any(doc, feature = "with-tokio-0_2", feature = "with-async_std-1"))]
    pub fn notify_after<M>(&mut self, duration: Duration, notification: M)
    where
        M: Message,
        A: Handler<M> + Send,
    {
        let addr = self.address.downgrade();

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
    }
}
