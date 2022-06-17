use event_listener::{Event, EventListener};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Weak};
use std::task::Poll;

/// A `DropNotifier` is a simple mechanism that notifies interested parties of its own demise.
/// Once dropped all corresponding `DropNotices` will immediately resolve.
pub struct DropNotifier {
    drop_event: Arc<Event>,
}

impl DropNotifier {
    /// Creates a new `DropNotifier`.
    pub fn new() -> Self {
        Self {
            drop_event: Arc::new(Event::new()),
        }
    }

    /// Issues a new `DropNotice` linked to this `DropNotifier`.
    pub fn subscribe(&self) -> DropNotice {
        DropNotice {
            drop_event: Some(Arc::downgrade(&self.drop_event)),
            listener: None,
        }
    }
}

impl Drop for DropNotifier {
    fn drop(&mut self) {
        self.drop_event.notify(usize::MAX);
    }
}

/// A `DropNotice` is a Future that resolves as soon as the corresponding `DropNotifier` is dropped.
/// For convenience, it can be cloned and easily passed around. All clones are linked to the
/// same `DropNotifier` instance.
pub struct DropNotice {
    drop_event: Option<Weak<Event>>,
    listener: Option<EventListener>,
}

impl DropNotice {
    /// Returns a `DropNotice` that is not linked to a `DropNotifier`. Instead, it resolves
    /// immediately when polled.
    pub fn dropped() -> DropNotice {
        DropNotice {
            drop_event: None,
            listener: None,
        }
    }
}

impl Clone for DropNotice {
    fn clone(&self) -> Self {
        Self {
            drop_event: self.drop_event.clone(),
            listener: None,
        }
    }
}

impl Future for DropNotice {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<()> {
        if self.listener.is_none() {
            let event = match &self.drop_event {
                Some(drop_event) => match drop_event.upgrade() {
                    Some(event) => Some(event),
                    None => {
                        self.drop_event = None;
                        None
                    }
                },
                None => None,
            };

            if event.is_none() {
                return Poll::Ready(());
            }
            self.listener = Some(event.unwrap().listen());
            self.drop_event = None;
        }

        let listener = self.listener.as_mut().unwrap();
        futures_util::pin_mut!(listener);

        match listener.poll(cx) {
            Poll::Ready(()) => {
                self.listener = None;
                self.drop_event = None;
                Poll::Ready(())
            }
            Poll::Pending => Poll::Pending,
        }
    }
}
