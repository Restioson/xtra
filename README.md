# xtra
A tiny, fast, and safe actor framework. It is modelled around Actix (copyright and license [here](https://github.com/Restioson/xtra/blob/master/LICENSE-ACTIX)).

For better ergonomics with xtra, try the [spaad](https://crates.io/crates/spaad) crate.

## Features
- Safe: there is no unsafe code in xtra.
- Tiny: xtra is only ~1.1kloc.
- Lightweight: it only depends on `futures` and `async_trait` by default.
- Asynchronous and synchronous message handlers.
- Simple asynchronous message handling interface which allows `async`/`await` syntax even when borrowing `self`.
- Does not depend on its own runtime and can be run with any futures executor ([Tokio](https://tokio.rs/),
  [async-std](https://async.rs/), [smol](https://github.com/stjepang/smol), and 
  [wasm-bindgen-futures](https://rustwasm.github.io/wasm-bindgen/api/wasm_bindgen_futures/) have the `Actor::spawn`
  convenience method implemented out of the box).
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
written with xtra and spaad on the server.

Too verbose? Check out the [spaad](https://crates.io/crates/spaad) sister-crate!

## Okay, sounds great! How do I use it?
Check out the [docs](https://docs.rs/xtra) and the [examples](https://github.com/Restioson/xtra/blob/master/examples)
to get started! Enabling the `with-tokio-0_2`, `with-async_std-1`, `with-smol-0_3`, or `with-wasm-bindgen-0_2` features
is recommended in order to enable some  convenience methods (such as `Actor::spawn`). Which you enable will depend on
which executor you want to use (check out their docs to learn more about each). If you have any questions, feel free to
[open an issue](https://github.com/Restioson/xtra/issues/new) or message me on the [Rust discord](https://bit.ly/rust-community).

## Latest Breaking Changes
- **The `SyncHandler` trait has been removed.** This simplifies the API and should not change the performance on stable.
    - *How to upgrade:* change all implementations of the `SyncHandler` trait to the normal `Handler` trait.
- **All `Actor` lifecycle messages are now async.** This allows to do more kinds of things in lifecycle methods,
  while adding no restrictions.
    - *How to upgrade:* add `async` to the function definition of all actor lifecycle methods.
- `Actor` now requires `Send` to implement. Previously, the trait itself did not, but using it did require `Send`.
    - *How to upgrade:* you probably never had a non-`Send` actor in the first place.
- The `{Weak}Address::attach_stream` method now requires that the actor implements `Handler<M>` where 
  `M: Into<KeepRunning> + Send`. This is automatically implemented for `()`, returning `KeepRunning::Yes`. This allows
  the user more control over the future spawned by `attach_stream`, but is breaking if the message returned did not
  implement `Into<KeepRunning>`/
    - *How to upgrade:* implement `Into<KeepRunning>` for all message types used in `attach_stream`. To mimic previous
      behaviour, return `KeepRunning::Yes` in the implementation.

See the full list of breaking changes by version [here](https://github.com/Restioson/xtra/blob/master/BREAKING-CHANGES.md)
