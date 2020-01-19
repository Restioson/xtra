use crate::{Actor, Context, Handler, Message, SyncResponder};
use futures::channel::oneshot::{self, Receiver, Sender};
use futures::{Future, FutureExt};
use std::marker::PhantomData;

type Fut<'a> = Box<dyn Future<Output = ()> + Unpin + Send + 'a>;

pub(crate) enum EnvelopeHandleResult<'a> {
    Fut(Fut<'a>),
    Sync,
}

pub(crate) trait Envelope: Send {
    type Actor: Actor + ?Sized;

    fn handle<'a>(
        &'a mut self,
        act: &'a mut Self::Actor,
        ctx: &'a mut Context<Self::Actor>,
    ) -> EnvelopeHandleResult<'a>;
}

pub(crate) struct SyncReturningEnvelope<A: Actor + ?Sized + Handler<M>, M: Message> {
    message: Option<M>, // Options so that we can opt.take()
    result_sender: Option<Sender<M::Result>>,
    phantom: PhantomData<A>,
}

impl<A: Actor + ?Sized + Handler<M>, M: Message> SyncReturningEnvelope<A, M> {
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

impl<A: Actor + Handler<M>, M: Message> Envelope for SyncReturningEnvelope<A, M>
where
    A::Responder: Send,
    A::Responder: SyncResponder<M>,
{
    type Actor = A;

    fn handle(
        &mut self,
        act: &mut Self::Actor,
        ctx: &mut Context<Self::Actor>,
    ) -> EnvelopeHandleResult {
        let message_result = act.handle(self.message.take().expect("Message must be Some"), ctx);

        // We don't actually care if the receiver is listening
        let _ = self
            .result_sender
            .take()
            .expect("Sender must be Some")
            .send(message_result.cast());

        EnvelopeHandleResult::Sync
    }
}

pub(crate) struct AsyncReturningEnvelope<A: Actor + ?Sized + Handler<M>, M: Message> {
    message: Option<M>, // Options so that we can opt.take()
    result_sender: Option<Sender<M::Result>>,
    phantom: PhantomData<A>,
}

impl<A: Actor + Handler<M>, M: Message> Envelope for AsyncReturningEnvelope<A, M>
where
    A::Responder: Send,
    A::Responder: Future<Output = M::Result>,
{
    type Actor = A;

    fn handle<'a>(
        &'a mut self,
        act: &'a mut Self::Actor,
        ctx: &'a mut Context<Self::Actor>,
    ) -> EnvelopeHandleResult {
        EnvelopeHandleResult::Fut(Box::new(
            act.handle(self.message.take().expect("Message must be Some"), ctx)
                .map(move |r| {
                    // We don't actually care if the receiver is listening
                    let _ = self
                        .result_sender
                        .take()
                        .expect("Sender must be Some")
                        .send(r);
                }),
        ))
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

impl<A: Actor + Handler<M> + ?Sized, M: Message> Envelope for NonReturningEnvelope<A, M> {
    type Actor = A;

    fn handle(
        &mut self,
        act: &mut Self::Actor,
        ctx: &mut Context<Self::Actor>,
    ) -> EnvelopeHandleResult {
        act.handle(self.message.take().expect("Message must be Some"), ctx);
        EnvelopeHandleResult::Sync
    }
}
