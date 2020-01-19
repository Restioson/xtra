use crate::{Actor, Context, Handler, Message, SyncResponder};
use futures::channel::oneshot::{self, Receiver, Sender};
use futures::{future, Future, FutureExt};
use std::marker::PhantomData;
use std::pin::Pin;

type Fut<'a> = Pin<Box<dyn Future<Output = ()> + Send + 'a>>;

// TODO Nick12 ISSUE: this CAN'T have 'a because i need to construct dyn Envelope<'a>
pub(crate) trait Envelope: Send {
    type Actor: Actor + ?Sized;

    // We could return some enum like:
    //
    // enum Return<'a> {
    //     Fut(Fut<'a>),
    //     Noop,
    // }
    //
    // But this is actually about 10% *slower* for `do_send`. I don't know why. Maybe something to
    // do with branch [mis]prediction or compiler optimisation
    fn handle(
        &mut self,
        act: &mut Self::Actor,
        ctx: &mut Context<Self::Actor>,
    ) -> Fut;
}

pub(crate) struct SyncReturningEnvelope<A: Actor + ?Sized, M: Message> {
    message: Option<M>, // Options so that we can opt.take()
    result_sender: Option<Sender<M::Result>>,
    phantom: PhantomData<A>,
}

impl<A: Actor + ?Sized, M: Message> SyncReturningEnvelope<A, M> {
    pub(crate) fn new(message: M) -> (Self, Receiver<M::Result>) {
        let (tx, rx) = oneshot::channel();
        let envelope = SyncReturningEnvelope {
            message: Some(message),
            result_sender: Some(tx),
            phantom: PhantomData,
        };

        (envelope, rx)
    }
}

impl<'a, A: Actor + Handler<'a, M>, M: Message> Envelope for SyncReturningEnvelope<A, M>
where
    A: Send,
    A::Responder: SyncResponder<M> + Send,
{
    type Actor = A;

    fn handle(
        &mut self,
        act: &mut Self::Actor,
        ctx: &mut Context<Self::Actor>,
    ) -> Fut {
        let message_result = act.handle(self.message.take().expect("Message must be Some"), ctx);

        // We don't actually care if the receiver is listening
        let _ = self
            .result_sender
            .take()
            .expect("Sender must be Some")
            .send(message_result.cast());

        Box::pin(future::ready(()))
    }
}

pub(crate) struct AsyncReturningEnvelope<A: Actor + ?Sized, M: Message> {
    message: Option<M>, // Options so that we can opt.take()
    result_sender: Option<Sender<M::Result>>,
    phantom: PhantomData<A>,
}

impl<A: Actor + ?Sized, M: Message> AsyncReturningEnvelope<A, M> {
    pub(crate) fn new(message: M) -> (Self, Receiver<M::Result>) {
        let (tx, rx) = oneshot::channel();
        let envelope = AsyncReturningEnvelope {
            message: Some(message),
            result_sender: Some(tx),
            phantom: PhantomData,
        };

        (envelope, rx)
    }
}

impl<'a, A: Actor + Handler<'a, M>, M: Message> Envelope for AsyncReturningEnvelope<A, M>
where
    A::Responder: Future<Output = M::Result> + Send,
{
    type Actor = A;

    fn handle(
        &'a mut self,
        act: &'a mut Self::Actor,
        ctx: &'a mut Context<Self::Actor>,
    ) -> Fut<'a>
    {
        Box::pin(
            act.handle(self.message.take().expect("Message must be Some"), ctx)
                .map(move |r| {
                    // We don't actually care if the receiver is listening
                    let _ = self
                        .result_sender
                        .take()
                        .expect("Sender must be Some")
                        .send(r);
                }),
        )
    }
}

pub(crate) struct NonReturningEnvelope<A: Actor + ?Sized, M: Message> {
    message: Option<M>, // Option so that we can opt.take()
    phantom: PhantomData<A>,
}

impl<A: Actor + ?Sized, M: Message> NonReturningEnvelope<A, M> {
    pub(crate) fn new(message: M) -> Self {
        NonReturningEnvelope {
            message: Some(message),
            phantom: PhantomData,
        }
    }
}

impl<'a, A: Actor + Handler<'a, M> + ?Sized, M: Message> Envelope for NonReturningEnvelope<A, M> {
    type Actor = A;

    fn handle(
        &mut self,
        act: &mut Self::Actor,
        ctx: &mut Context<Self::Actor>,
    ) -> Fut {
        act.handle(self.message.take().expect("Message must be Some"), ctx);
        Box::pin(future::ready(()))
    }
}
