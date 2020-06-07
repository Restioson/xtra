# xtra
A tiny, fast, and safe actor framework. It is modelled around Actix (copyright and license [here](https://github.com/Restioson/xtra/blob/master/LICENSE-ACTIX)).

For better ergonomics with xtra, try the [spaad](https://crates.io/crates/spaad) crate.

## Features
- Safe: there is no unsafe code in xtra.
- Small and lightweight: it only depends on `futures` and `async_trait` by default.
- Asynchronous and synchronous message handlers.
- Simple asynchronous message handling interface which allows `async`/`await` syntax even when borrowing `self`.
- Does not depend on its own runtime and can be run with any futures executor ([Tokio](https://tokio.rs/) and 
[async-std](https://async.rs/) have the `Actor::spawn` convenience method implemented out of the box).
- Quite fast (under Tokio, <170ns time from sending a message to it being processed for sending without waiting for a 
result on my development machine with an AMD Ryzen 3 3200G)
- However, it is also relatively new and less mature than other options.

## Example
```rust
use xtra::prelude::*;
use async_trait::async_trait;

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

// In the real world, the synchronous SyncHandler trait would be better-suited
#[async_trait]
impl Handler<Print> for Printer {
    async fn handle(&mut self, print: Print, _ctx: &mut Context<Self>) {
        self.times += 1; // Look ma, no ActorFuture!
        println!("Printing {}. Printed {} times so far.", print.0, self.times);
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
written with xtra nightly (on the server).

## Okay, sounds great! How do I use it?
Check out the [docs](https://docs.rs/xtra) and the [examples](https://github.com/Restioson/xtra/blob/master/examples)
to get started! Enabling the `with-tokio-0_2` or `with-async_std-1` features are recommended in order to enable some 
convenience methods (such as `Actor::spawn`). Which you enable will depend on which executor you want to use (check out
their docs to learn more about each). If you have any questions, feel free to [open an issue](https://github.com/Restioson/xtra/issues/new)
or message me on the [Rust discord](https://bit.ly/rust-community).

## Latest Breaking Changes
From version 0.2.x to 0.3.0:
- The default API of the `Handler` trait has now changed to an `async_trait` so that xtra can compile on stable.
    - *How to upgrade, alternative 1:* change the implementations by annotating the implementation with `#[async_trait]`,
      removing `Responder` and making `handle` an `async fn` which directly returns the message's result.
    - *How to upgrade, alternative 2:* if you want to avoid the extra box, you can disable the default `stable` feature
      in your `Cargo.toml` to keep the old API.

See the full list of breaking changes by version [here](https://github.com/Restioson/xtra/blob/master/BREAKING-CHANGES.md)

Note: this crate has been yanked a bunch in `0.2`. This is because of a git mess-up on my part, `cargo doc` not playing
nice with type alias impl trait and GATs, a mistake in the code making `MessageChannel` unusable, and having to mitigate
[this bug in `futures`](https://github.com/rust-lang/futures-rs/issues/2052). Apologies!

## To do
- Examples in documentation
