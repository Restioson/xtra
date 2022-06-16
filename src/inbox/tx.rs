use super::*;
use crate::Disconnected;
use futures_core::FusedFuture;
use futures_sink::Sink;
use futures_util::FutureExt;
use std::future::Future;
use std::mem;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};

pub(crate) struct Sender<A>(pub(super) Chan<A>);

impl<A> Clone for Sender<A> {
    fn clone(&self) -> Self {
        Sender(self.0.clone())
    }
}

impl<A> Sender<A> {
    pub(crate) fn send(&self, message: StolenMessage<A>) -> SendFuture<A> {
        SendFuture::new(message, self.0.clone())
    }

    pub(crate) fn send_priority(
        &self,
        message: StolenMessage<A>,
        priority: i32,
    ) -> Result<(), Disconnected> {
        let waiting = {
            let mut inner = self.0.lock().unwrap();
            if inner.shutdown {
                return Err(Disconnected);
            }

            match inner.pop_receiver() {
                Some(actor) => actor,
                None => {
                    let msg = StolenMessageWithPriority::new(Priority::Valued(priority), message);
                    inner.priority_queue.push(msg);
                    return Ok(());
                }
            }
        };

        waiting.lock().fulfill(WakeReason::StolenMessage(message));

        Ok(())
    }

    pub(crate) fn broadcast(&self, message: BroadcastMessage<A>) -> Result<(), Disconnected> {
        let waiting_receivers = {
            let mut inner = self.0.lock().unwrap();

            if inner.shutdown {
                return Err(Disconnected);
            }

            inner
                .broadcast_queues
                .retain(|queue| match queue.upgrade() {
                    Some(q) => {
                        q.lock().push(BroadcastMessageWrapper(message.clone()));
                        true
                    }
                    None => false, // The corresponding receiver has been dropped - remove it
                });

            mem::take(&mut inner.waiting_receivers)
        };

        for rx in waiting_receivers.into_iter().flat_map(|w| w.upgrade()) {
            rx.lock().fulfill(WakeReason::BroadcastMessage);
        }

        Ok(())
    }

    pub(crate) fn into_sink(self) -> SendSink<A> {
        SendSink {
            inner: self.0,
            future: SendFuture(SendFutureInner::Complete),
        }
    }

    pub(crate) fn shutdown(&self) {
        let waiting_receivers = {
            let mut inner = self.0.lock().unwrap();
            inner.shutdown = true;
            mem::take(&mut inner.waiting_receivers)
        };

        for rx in waiting_receivers.into_iter().flat_map(|w| w.upgrade()) {
            rx.lock().fulfill(WakeReason::BroadcastMessage);
        }
    }

    pub(crate) fn is_connected(&self) -> bool {
        !self.0.lock().unwrap().shutdown
    }
}

pub(crate) struct SendFuture<A>(SendFutureInner<A>);

impl<A> SendFuture<A> {
    fn new(msg: StolenMessage<A>, inner: Chan<A>) -> Self {
        SendFuture(SendFutureInner::New { msg, inner })
    }
}

enum SendFutureInner<A> {
    New {
        msg: StolenMessage<A>,
        inner: Chan<A>,
    },
    WaitingToSend(Arc<Spinlock<WaitingSender<A>>>),
    Complete,
}

impl<A> Default for SendFutureInner<A> {
    fn default() -> Self {
        SendFutureInner::Complete
    }
}

impl<A> Future for SendFuture<A> {
    type Output = Result<(), Disconnected>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.0 = match mem::take(&mut self.0) {
            SendFutureInner::New { msg, inner } => match inner.lock().unwrap().try_send(msg) {
                Ok(()) => return Poll::Ready(Ok(())),
                Err(TrySendFail::Disconnected) => return Poll::Ready(Err(Disconnected)),
                Err(TrySendFail::Full(waiting)) => SendFutureInner::WaitingToSend(waiting),
            },
            SendFutureInner::WaitingToSend(waiting) => {
                {
                    let mut inner = waiting.lock();

                    match inner.message {
                        Some(_) => inner.waker = Some(cx.waker().clone()), // The message has not yet been taken
                        None => return Poll::Ready(Ok(())),
                    }
                }

                SendFutureInner::WaitingToSend(waiting)
            }
            SendFutureInner::Complete => SendFutureInner::Complete,
        };

        Poll::Pending
    }
}

pub(crate) struct WaitingSender<A> {
    waker: Option<Waker>,
    message: Option<StolenMessage<A>>,
}

impl<A> WaitingSender<A> {
    pub(crate) fn new(message: StolenMessage<A>) -> Arc<Spinlock<Self>> {
        let sender = WaitingSender {
            waker: None,
            message: Some(message),
        };
        Arc::new(Spinlock::new(sender))
    }

    pub(crate) fn fulfill(&mut self) -> Option<StolenMessage<A>> {
        if let Some(waker) = self.waker.take() {
            waker.wake();
        }

        self.message.take()
    }
}

impl<A> FusedFuture for SendFuture<A> {
    fn is_terminated(&self) -> bool {
        matches!(self.0, SendFutureInner::Complete)
    }
}

pub(crate) struct SendSink<A> {
    inner: Chan<A>,
    future: SendFuture<A>,
}

impl<A> Sink<StolenMessage<A>> for SendSink<A> {
    type Error = Disconnected;

    fn poll_ready(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        if let Poll::Ready(Err(Disconnected)) = self.future.poll_unpin(cx) {
            Poll::Ready(Err(Disconnected))
        } else if self.future.is_terminated() {
            Poll::Ready(Ok(())) // TODO check disconnected
        } else {
            Poll::Pending
        }
    }

    fn start_send(mut self: Pin<&mut Self>, msg: StolenMessage<A>) -> Result<(), Self::Error> {
        self.future = SendFuture::new(msg, self.inner.clone());
        Ok(())
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.poll_ready(cx)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.poll_flush(cx)
    }
}
