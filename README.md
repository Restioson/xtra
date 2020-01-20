# xtra
A tiny (<1k LOC) actor framework. It is modelled around Actix. It's probably best to not use this production.

## Features
- Asynchronous and synchronous responders
- Simple asynchronous message handling interface which allows `async`/`await` syntax (no more `ActorFuture` - 
asynchronous responders just return `impl Future`)
- Does not depend on its own runtime and can be run with any futures executor ([Tokio](https://tokio.rs/) and 
[async-std](https://async.rs/) have the `Actor::spawn` convenience method implemented out of the box).
- Quite fast (<200ns time from sending a message to it being processed for sending without waiting for a result)

## To do
- Thread-local actors that are `!Send`
- Documentation
- Actor notifications (sending messages to self) and creating an address from the actor's context
- Scheduling of repeated-at-interval and time-delayed messages for actors

## Limitations
The main limitation of this crate is that it extensively uses unstable features. For example, to get rid of
`ActorFuture`, [Generic Associated Types (GATs)](https://github.com/rust-lang/rfcs/blob/master/text/1598-generic_associated_types.md)
must be used. This is an incomplete and unstable feature, which [appears to be a way off from stabilisation](https://github.com/rust-lang/rust/issues/44265).
It also uses [`impl Trait` Type Aliases](https://github.com/rust-lang/rfcs/pull/2515) to avoid `Box`ing the futures
returned from the `AsyncHandler` trait (the library, however, is not alloc-free). This means that it requires nightly to
use, and may be unstable.
