use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::Poll;
use event_listener::{Event, EventListener};

/// A `DropNotifier` is a simple mechanism that notifies interested parties of its own demise.
/// Once dropped all corresponding `DropNotices` will immediately resolve.
pub struct DropNotifier {
    drop_event: Arc<Event>
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
            drop_event: Arc::downgrade(&self.drop_event),
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
    drop_event: std::sync::Weak<Event>,
    listener: Option<EventListener>,
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

    fn poll(mut self: Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<Self::Output> {
        let event = {
            match self.drop_event.clone().upgrade() {
                Some(event) =>  Some(event),
                None => None,
            }
        };

        if event.is_none() {
            self.listener = None;
            return Poll::Ready(());
        }

        if self.listener.is_none() {
            let listener = event.unwrap().listen();
            self.listener = Some(listener);
        } else {
            drop(event);
        }

        let listener = self.listener.as_mut().unwrap();
        futures_util::pin_mut!(listener);
        listener.poll(cx)
    }
}
