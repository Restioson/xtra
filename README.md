# xtra
A tiny, fast, and safe actor framework. It is modelled around Actix (copyright and license [here](https://github.com/Restioson/xtra/blob/master/LICENSE-ACTIX)).

For better ergonomics with xtra, try the [spaad](https://crates.io/crates/spaad) crate.

## Features
- Safe: there is no unsafe code in xtra.
- Tiny: xtra is only ~1.1kloc.
- Lightweight: it only depends on `futures` and `async_trait` by default.
- Asynchronous and synchronous message handlers.
- Simple asynchronous message handling interface which allows `async`/`await` syntax even when borrowing `self`.
- Does not depend on its own runtime and can be run with any futures executor ([Tokio](https://tokio.rs/) and 
[async-std](https://async.rs/) have the `Actor::spawn` convenience method implemented out of the box).
- Quite fast. Running on Tokio, <170ns time from sending a message to it being processed for sending without waiting for a 
result on my development machine with an AMD Ryzen 3 3200G.
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

Too verbose? Check out the [spaad](https://crates.io/crates/spaad) sister-crate!

## Okay, sounds great! How do I use it?
Check out the [docs](https://docs.rs/xtra) and the [examples](https://github.com/Restioson/xtra/blob/master/examples)
to get started! Enabling the `with-tokio-0_2` or `with-async_std-1` features are recommended in order to enable some 
convenience methods (such as `Actor::spawn`). Which you enable will depend on which executor you want to use (check out
their docs to learn more about each). If you have any questions, feel free to [open an issue](https://github.com/Restioson/xtra/issues/new)
or message me on the [Rust discord](https://bit.ly/rust-community).

## Nightly API

There is also a different nightly API, which is **incompatible with the stable api**.. For an example, check out
`examples/nightly.rs`. To switch to it, enable the `nightly` feature in the Cargo.toml. This API uses
GATs and Type Alias Impl Trait to remove one boxing of a future, but according to my benchmarks, this impact has little 
effect. Your mileage may vary. GATs are unstable and can cause undefined behaviour in safe rust, and the combination of
GAT + TAIT can break rustdoc. Therefore, the tradeoff is a (possibly negligible) performance boost for less
support and instability. Generally, the only situation I would recommend this to be used in is for code written for xtra
0.2.

## Latest Breaking Changes
From version 0.3.x to 0.4.0:
- The `stable` feature was removed. In order to enable the nightly API, enable the new `nightly` feature.

See the full list of breaking changes by version [here](https://github.com/Restioson/xtra/blob/master/BREAKING-CHANGES.md)
