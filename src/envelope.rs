use crate::{Actor, AsyncHandler, Context, Handler, Message};
use futures::channel::oneshot::{self, Receiver, Sender};
use futures::{future, Future, FutureExt};
use std::marker::PhantomData;
use std::pin::Pin;

type Fut<'a> = Pin<Box<dyn Future<Output = ()> + Send + 'a>>;

pub(crate) trait Envelope: Send {
    type Actor: Actor;

    // We could return some enum like:
    //
    // enum Return<'a> {
    //     Fut(Fut<'a>),
    //     Noop,
    // }
    //
    // But this is actually about 10% *slower* for `do_send`. I don't know why. Maybe something to
    // do with branch [mis]prediction or compiler optimisation
    fn handle<'a>(
        self: Box<Self>,
        act: &'a mut Self::Actor,
        ctx: &'a mut Context<Self::Actor>,
    ) -> Fut<'a>;
}

pub(crate) struct SyncReturningEnvelope<A: Actor + Send, M: Message> {
    message: M,
    result_sender: Sender<M::Result>,
    phantom: PhantomData<A>,
}

impl<A: Actor + Send, M: Message> SyncReturningEnvelope<A, M> {
    pub(crate) fn new(message: M) -> (Self, Receiver<M::Result>) {
        let (tx, rx) = oneshot::channel();
        let envelope = SyncReturningEnvelope {
            message,
            result_sender: tx,
            phantom: PhantomData,
        };

        (envelope, rx)
    }
}

impl<A, M: Message> Envelope for SyncReturningEnvelope<A, M>
where
    A: Handler<M> + Send,
    M: Message,
    M::Result: Send,
{
    type Actor = A;

    fn handle<'a>(
        self: Box<Self>,
        act: &'a mut Self::Actor,
        ctx: &'a mut Context<Self::Actor>,
    ) -> Fut<'a> {
        let message_result = act.handle(self.message, ctx);

        // We don't actually care if the receiver is listening
        let _ = self.result_sender.send(message_result);

        Box::pin(future::ready(()))
    }
}

pub(crate) struct AsyncReturningEnvelope<A: Actor, M: Message> {
    message: M,
    result_sender: Sender<M::Result>,
    phantom: PhantomData<A>,
}

impl<A: Actor, M: Message> AsyncReturningEnvelope<A, M> {
    pub(crate) fn new(message: M) -> (Self, Receiver<M::Result>) {
        let (tx, rx) = oneshot::channel();
        let envelope = AsyncReturningEnvelope {
            message,
            result_sender: tx,
            phantom: PhantomData,
        };

        (envelope, rx)
    }
}

impl<M, A> Envelope for AsyncReturningEnvelope<A, M>
where
    A: AsyncHandler<M> + Send,
    for<'a> A::Responder<'a>: Future<Output = M::Result>,
    M: Message,
{
    type Actor = A;

    fn handle<'a>(
        self: Box<Self>,
        act: &'a mut Self::Actor,
        ctx: &'a mut Context<Self::Actor>,
    ) -> Fut<'a> {
        let Self {
            message,
            result_sender,
            ..
        } = *self;
        Box::pin(act.handle(message, ctx).map(move |r| {
            // We don't actually care if the receiver is listening
            let _ = result_sender.send(r);
        }))
    }
}

pub(crate) struct SyncNonReturningEnvelope<A: Actor, M: Message> {
    message: M,
    phantom: PhantomData<A>,
}

impl<A: Actor, M: Message> SyncNonReturningEnvelope<A, M> {
    pub(crate) fn new(message: M) -> Self {
        SyncNonReturningEnvelope {
            message,
            phantom: PhantomData,
        }
    }
}

impl<A: Handler<M> + Send, M: Message> Envelope for SyncNonReturningEnvelope<A, M> {
    type Actor = A;

    fn handle<'a>(
        self: Box<Self>,
        act: &'a mut Self::Actor,
        ctx: &'a mut Context<Self::Actor>,
    ) -> Fut<'a> {
        act.handle(self.message, ctx);
        Box::pin(future::ready(()))
    }
}

pub(crate) struct AsyncNonReturningEnvelope<A: Actor, M: Message> {
    message: M,
    phantom: PhantomData<A>,
}

impl<A: Actor, M: Message> AsyncNonReturningEnvelope<A, M> {
    pub(crate) fn new(message: M) -> Self {
        AsyncNonReturningEnvelope {
            message,
            phantom: PhantomData,
        }
    }
}

impl<A, M> Envelope for AsyncNonReturningEnvelope<A, M>
where
    A: AsyncHandler<M> + Send,
    for<'a> A::Responder<'a>: Future<Output = M::Result>,
    M: Message,
{
    type Actor = A;

    fn handle<'a>(
        self: Box<Self>,
        act: &'a mut Self::Actor,
        ctx: &'a mut Context<Self::Actor>,
    ) -> Fut<'a> {
        Box::pin(act.handle(self.message, ctx).map(|_| ()))
    }
}
