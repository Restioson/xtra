use std::future::Future;
use std::mem;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};

use crate::Error;

pub enum WaitingSender<M> {
    Active { waker: Option<Waker>, message: M },
    Delivered,
    Closed,
}

impl<M> WaitingSender<M> {
    pub fn new(message: M) -> Arc<spin::Mutex<Self>> {
        let sender = WaitingSender::Active {
            waker: None,
            message,
        };
        Arc::new(spin::Mutex::new(sender))
    }

    pub fn peek(&self) -> &M {
        match self {
            WaitingSender::Active { message, .. } => message,
            _ => panic!("WaitingSender should have message"),
        }
    }

    pub fn fulfill(&mut self) -> M {
        match mem::replace(self, Self::Delivered) {
            WaitingSender::Active { mut waker, message } => {
                if let Some(waker) = waker.take() {
                    waker.wake();
                }

                message
            }
            WaitingSender::Delivered | WaitingSender::Closed => {
                panic!("WaitingSender is already fulfilled or closed")
            }
        }
    }

    /// Mark this [`WaitingSender`] as closed.
    ///
    /// Should be called when the last [`Receiver`](crate::inbox::Receiver) goes away.
    pub fn set_closed(&mut self) {
        if let WaitingSender::Active {
            waker: Some(waker), ..
        } = mem::replace(self, Self::Closed)
        {
            waker.wake();
        }
    }
}

impl<A> Future for WaitingSender<A>
where
    A: Unpin,
{
    type Output = Result<(), Error>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();

        match this {
            WaitingSender::Active { waker, .. } => {
                *waker = Some(cx.waker().clone());
                Poll::Pending
            }
            WaitingSender::Delivered => Poll::Ready(Ok(())),
            WaitingSender::Closed => Poll::Ready(Err(Error::Disconnected)),
        }
    }
}
