use crate::envelope::BroadcastEnvelopeConcrete;
use crate::inbox::tx::TxRefCounter;
use crate::inbox::{SendFuture, SentMessage};
use crate::{inbox, Disconnected, Handler};
use futures_core::FusedFuture;
use futures_util::FutureExt;
use std::future::Future;
use std::mem;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

/// TODO
#[must_use = "Futures do nothing unless polled"]
pub struct BroadcastFuture<A, M, Rc: TxRefCounter> {
    inner: Inner<A, M, Rc>,
}

impl<A, M, Rc> BroadcastFuture<A, M, Rc>
where
    Rc: TxRefCounter,
{
    /// TODO
    pub fn new(message: M, sender: inbox::Sender<A, Rc>) -> Self {
        Self {
            inner: Inner::Initial {
                message,
                sender,
                priority: None,
            },
        }
    }

    /// TODO
    pub fn priority(self, priority: i32) -> Self {
        match self.inner {
            Inner::Initial {
                message, sender, ..
            } => Self {
                inner: Inner::Initial {
                    message,
                    sender,
                    priority: Some(priority),
                },
            },
            _ => panic!("setting priority after polling is unsupported"),
        }
    }
}

enum Inner<A, M, Rc: TxRefCounter> {
    Initial {
        message: M,
        sender: inbox::Sender<A, Rc>,
        priority: Option<i32>,
    },
    Sending(SendFuture<A, Rc>),
    Done,
}

impl<A, M, Rc> Future for BroadcastFuture<A, M, Rc>
where
    Rc: TxRefCounter,
    M: Clone + Send + Sync + 'static + Unpin,
    A: Handler<M, Return = ()>,
{
    type Output = Result<(), Disconnected>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();

        match mem::replace(&mut this.inner, Inner::Done) {
            Inner::Initial {
                message,
                sender,
                priority,
            } => {
                let envelope =
                    BroadcastEnvelopeConcrete::<A, M>::new(message, priority.unwrap_or(0));
                this.inner =
                    Inner::Sending(sender.send(SentMessage::ToAllActors(Arc::new(envelope))));
                this.poll_unpin(cx)
            }
            Inner::Sending(mut send_fut) => match send_fut.poll_unpin(cx) {
                Poll::Ready(result) => Poll::Ready(result),
                Poll::Pending => {
                    this.inner = Inner::Sending(send_fut);
                    Poll::Pending
                }
            },
            Inner::Done => {
                panic!("Polled after completion")
            }
        }
    }
}

impl<A, M, Rc> FusedFuture for BroadcastFuture<A, M, Rc>
where
    Rc: TxRefCounter,
    Self: Future,
{
    fn is_terminated(&self) -> bool {
        matches!(self.inner, Inner::Done)
    }
}
