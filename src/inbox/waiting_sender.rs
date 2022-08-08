use std::future::Future;
use std::mem;
use std::pin::Pin;
use std::sync::{Arc, Weak};
use std::task::{Context, Poll, Waker};

use crate::inbox::{HasPriority, Priority};
use crate::Error;

pub struct WaitingSender<M>(Arc<spin::Mutex<Inner<M>>>);

pub struct FulFillHandle<M>(Weak<spin::Mutex<Inner<M>>>);

impl<M> WaitingSender<M> {
    pub fn new(msg: M) -> (FulFillHandle<M>, WaitingSender<M>) {
        let inner = Arc::new(spin::Mutex::new(Inner::new(msg)));

        (FulFillHandle(Arc::downgrade(&inner)), WaitingSender(inner))
    }
}

impl<M> FulFillHandle<M> {
    pub fn is_active(&self) -> bool {
        Weak::strong_count(&self.0) > 0
    }

    pub fn fulfill(&self) -> Option<M> {
        let inner = self.0.upgrade()?;
        let mut this = inner.lock();

        Some(match mem::replace(&mut *this, Inner::Delivered) {
            Inner::Active { mut waker, message } => {
                if let Some(waker) = waker.take() {
                    waker.wake();
                }

                message
            }
            Inner::Delivered | Inner::Closed => {
                panic!("WaitingSender is already fulfilled or closed")
            }
        })
    }

    /// Mark this [`WaitingSender`] as closed.
    ///
    /// Should be called when the last [`Receiver`](crate::inbox::Receiver) goes away.
    fn set_closed(&self) {
        if let Some(inner) = self.0.upgrade() {
            let mut this = inner.lock();

            match &*this {
                Inner::Active { waker, .. } => {
                    if let Some(waker) = waker {
                        waker.wake_by_ref();
                    }
                    *this = Inner::Closed;
                }
                Inner::Delivered => {}
                Inner::Closed => {}
            }
        }
    }
}

impl<M> Drop for FulFillHandle<M> {
    fn drop(&mut self) {
        self.set_closed();
    }
}

impl<M> FulFillHandle<M>
where
    M: HasPriority,
{
    pub fn priority(&self) -> Option<Priority> {
        let inner = self.0.upgrade()?;
        let this = inner.lock();

        match &*this {
            Inner::Active { message, .. } => Some(message.priority()),
            Inner::Closed | Inner::Delivered => None,
        }
    }
}

impl<A> Future for WaitingSender<A>
where
    A: Unpin,
{
    type Output = Result<(), Error>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut this = self.get_mut().0.lock();

        match &mut *this {
            Inner::Active { waker, .. } => {
                *waker = Some(cx.waker().clone());
                Poll::Pending
            }
            Inner::Delivered => Poll::Ready(Ok(())),
            Inner::Closed => Poll::Ready(Err(Error::Disconnected)),
        }
    }
}

enum Inner<M> {
    Active { waker: Option<Waker>, message: M },
    Delivered,
    Closed,
}

impl<M> Inner<M> {
    fn new(message: M) -> Self {
        Inner::Active {
            waker: None,
            message,
        }
    }
}
