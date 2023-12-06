use xtra::prelude::*;

#[derive(xtra::Actor)]
struct Greeter;

struct Hello(String);

impl Handler<Hello> for Greeter {
    type Return = ();

    async fn handle(&mut self, Hello(name): Hello, _: &mut Context<Self>) -> Self::Return {
        println!("Hello {}", name)
    }
}

/// This examples demonstrates `xtra`'s feature of backpressure when waiting for the result of a handler asynchronously.
///
/// Backpressure allows an actor to throttle the sending of new messages by putting a limit on how many messages can be queued in the mailbox at once.
/// To demonstrate backpressure we set up the following environment:
///
/// 1. Single-threaded tokio executor: This ensures that only one task can run at any time.
/// 2. Call `detach` on the [`SendFuture`](xtra::SendFuture) returned by [`Address::send`]: The actor needs to be busy working off messages and thus we cannot synchronously wait for the result.
/// 3. Print "Greeting world!" to stdout before we dispatch a message.
/// 4. Print "Hello world!" upon executing the handler.
///
/// By varying the `mailbox_capacity` and observing the output on stdout, we can see the effects of backpressure:
///
/// ## `mailbox_capacity = Some(0)`
///
/// A `mailbox_capacity` of `Some(0)` will yield `Pending` on the `SendFuture` until the actor is actively requesting the next message from its event-loop.
/// We will thus see a fairly **interleaved** pattern of:
///
/// - "Greeting world!"
/// - "Hello world!"
///
/// This makes sense because the next "Greeting world!" can only be printed once the `SendFuture` is complete which only happens as soon as the actor is ready to take another message which in turn means it is done with processing the current message.
///
/// ## `mailbox_capacity = Some(N)`
///
/// Setting `mailbox_capacity` to `Some(N)` allows us to dispatch up to `N` message to the actor before the returned `SendFuture` returns `Pending`.
/// We can observe this by seeing N+1 blocks of "Greeting world!".
///
/// The task for dispatching messages can dispatch N messages without yielding `Pending`. Due to the single-threaded executor, nothing else is executing during that time.
/// Once the dispatching task yields `Pending`, the runtime switches to the actor's task and processes all queued messages. An empty mailbox will yield `Pending` and the
/// entire dance starts again.
///
/// ## `mailbox_capacity = None`
///
/// A `mailbox_capacity` of `None` creates an interesting output: We only see "Greeting world!" messages.
/// This is easily explained though. `None` creates an unbounded mailbox, meaning there is no artificial upper limit to how many messages we can queue.
///
/// The example ends after the loop and thus, the runtime is immediately destroyed after which means none of the queue handlers ever get to actually execute.
#[tokio::main(flavor = "current_thread")]
async fn main() {
    let address = xtra::spawn_tokio(Greeter, Mailbox::unbounded());

    for _ in 0..100 {
        let name = "world!".to_owned();

        println!("Greeting {}", name);
        let _ = address.send(Hello(name)).detach().await;
    }
}
