# xtra
A tiny, fast, and safe actor framework. It is modelled around Actix (copyright and license [here](https://github.com/Restioson/xtra/blob/master/LICENSE-ACTIX)).

## Features
- Safe: there is no unsafe code in xtra (there is some necessary in `futures`, but that's par for the course).
- Small and lightweight: it only depends on `futures` by default.
- Asynchronous and synchronous message handlers.
- Simple asynchronous message handling interface which allows `async`/`await` syntax even when borrowing `self`.
- Does not depend on its own runtime and can be run with any futures executor ([Tokio](https://tokio.rs/) and 
[async-std](https://async.rs/) have the `Actor::spawn` convenience method implemented out of the box).
- Quite fast (under Tokio, <170ns time from sending a message to it being processed for sending without waiting for a 
result on my development machine with an AMD Ryzen 3 3200G)

## Caveats
- The main caveat of this crate is that it uses many unstable features. For example, to get rid of `ActorFuture`,
[Generic Associated Types (GATs)](https://github.com/rust-lang/rfcs/blob/master/text/1598-generic_associated_types.md)
must be used. This is an incomplete and unstable feature, which [appears to be a way off from stabilisation](https://github.com/rust-lang/rust/issues/44265).
It also uses [`impl Trait` Type Aliases](https://github.com/rust-lang/rfcs/pull/2515) to avoid `Box`ing the futures
returned from the `Handler` trait (the library, however, is not totally alloc-free). This means that it requires
nightly to use, and may be unstable in future as those features evolve. What you get in return for this is a cleaner,
simpler, and more expressive API. 
- It is also still very much under development, so it may not be ready for production code.

## Example
```rust
#![feature(type_alias_impl_trait, generic_associated_types)]

use futures::Future;
use xtra::prelude::*;

struct Printer {
    times: usize,
}

impl Printer {
    fn new() -> Self {
        Printer { times: 0 }
    }
}

impl Actor for Printer {}

struct Print(String);

impl Message for Print {
    type Result = ();
}

// In the real world, the synchronous SyncHandler trait would be better-suited (and is a few ns faster)
impl Handler<Print> for Printer {
    type Responder<'a> = impl Future<Output = ()> + 'a;

    fn handle(&mut self, print: Print, _ctx: &mut Context<Self>) -> Self::Responder<'_> {
        async move {
            self.times += 1; // Look ma, no ActorFuture!
            println!("Printing {}. Printed {} times so far.", print.0, self.times);
        }
    }
}

#[tokio::main]
async fn main() {
    let addr = Printer::new().spawn();
    loop {
        // Likewise, in the real world the `.do_send` method should be used here as it is about 2x as fast
        addr.send(Print("hello".to_string()))
            .await
            .expect("Printer should not be dropped");
    }
}
```

For a longer example, check out [Vertex](https://github.com/Restioson/vertex/tree/development), a chat application
written with xtra (on the server).

## Okay, sounds great! How do I use it?
Check out the [docs](https://docs.rs/xtra) and the [examples](https://github.com/Restioson/xtra/blob/master/examples)
to get started! Enabling the `with-tokio-0_2` or `with-async_std-1` features are recommended in order to enable some 
convenience methods (such as `Actor::spawn`). Which you enable will depend on which executor you want to use (check out
their docs to learn more about each). If you have any questions, feel free to [open an issue](https://github.com/Restioson/xtra/issues/new)
or message me on the [Rust discord](https://bit.ly/rust-community).

## Latest Breaking Changes
From version 0.1.x to 0.2.0:
- Removal of the `with-runtime` feature
    - *How to upgrade:* You probably weren't using this anyway, but rather use `with-tokio-*` or `with-async_std-*`
    instead.
- `Address` methods were moved to `AddressExt` to accommodate new `Address` types
    - *How to upgrade:* add `use xtra::AddressExt` to wherever address methods are used (or, better yet, 
    `use xtra::prelude::*`).
- All `*_async` methods were removed. Asynchronous and synchronous messages now use the same method for everything.
    - *How to upgrade:* simply switch from the `[x]_async` method to the `[x]` method.
- `AsyncHandler` was renamed to `Handler`, and the old `Handler` to `SyncHandler`. Also, a `Handler` and `SyncHandler` implementation can no longer coexist.
    - *How to upgrade:* rename all `Handler` implementations to `SyncHandler`, and all `AsyncHandler` implementations to `Handler`.

See the full list of breaking changes by version [here](https://github.com/Restioson/xtra/blob/master/BREAKING-CHANGES.md)

Note: this crate has been yanked a bunch in `0.2`. This is because of a git mess-up on my part, `cargo doc` not playing
nice with type alias impl trait and GATs, a mistake in the code making `MessageChannel` unusable, and having to mitigate
[this bug in `futures`](https://github.com/rust-lang/futures-rs/issues/2052). Apologies!

## To do
- Examples in documentation
