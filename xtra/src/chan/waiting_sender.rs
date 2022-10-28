use std::future::Future;
use std::mem;
use std::pin::Pin;
use std::sync::{Arc, Weak};
use std::task::{Context, Poll, Waker};

use crate::chan::{HasPriority, Priority};
use crate::Error;

#[must_use = "Futures do nothing unless polled"]
pub struct WaitingSender<M>(Arc<spin::Mutex<Inner<M>>>);

pub struct Handle<M>(Weak<spin::Mutex<Inner<M>>>);

impl<M> WaitingSender<M> {
    pub fn new(msg: M) -> (Handle<M>, WaitingSender<M>) {
        let inner = Arc::new(spin::Mutex::new(Inner::new(msg)));

        (Handle(Arc::downgrade(&inner)), WaitingSender(inner))
    }
}

impl<M> Handle<M> {
    pub fn is_active(&self) -> bool {
        Weak::strong_count(&self.0) > 0
    }

    /// Take the message out of the paired [`WaitingSender`].
    ///
    /// This may return `None` in case the [`WaitingSender`] is no longer active, has already been closed or the message was already taken.
    pub fn take_message(self) -> Option<M> {
        let inner = self.0.upgrade()?;
        let mut this = inner.lock();

        match mem::replace(&mut *this, Inner::Delivered) {
            Inner::Active { mut waker, message } => {
                if let Some(waker) = waker.take() {
                    waker.wake();
                }

                Some(message)
            }
            Inner::Delivered | Inner::Closed => None,
        }
    }
}

impl<M> Drop for Handle<M> {
    fn drop(&mut self) {
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
        };
    }
}

impl<M> Handle<M>
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

impl<A> Future for WaitingSender<A> {
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
