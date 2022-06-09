# xtra
A tiny, fast, and safe actor framework. It is modelled around Actix (copyright and license [here](https://github.com/Restioson/xtra/blob/master/LICENSE-ACTIX)).

For better ergonomics with xtra, try the [spaad](https://crates.io/crates/spaad) crate.

## Features
- Safe: there is no unsafe code in xtra.
- Tiny: xtra is less than 1k loc.
- Lightweight: xtra has few dependencies, most of which are lightweight (except `futures`).
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
use xtra::spawn::Tokio;

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

#[async_trait]
impl Handler<Print> for Printer {
    type Return = ();

    async fn handle(&mut self, print: Print, _ctx: &mut Context<Self>) {
        self.times += 1; // no ActorFuture or anything just to access `self`
        println!("Printing {}. Printed {} times so far.", print.0, self.times);
    }
}

#[tokio::main]
async fn main() {
    let addr = Printer::new().create(None).spawn(&mut Tokio::Global);
    loop {
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
to get started! Enabling the `with-tokio-1`, `with-async_std-1`, `with-smol-1`, or `with-wasm_bindgen-0_2` features
is recommended in order to enable some  convenience methods (such as `Actor::spawn`). Which you enable will depend on
which executor you want to use (check out their docs to learn more about each). If you have any questions, feel free to
[open an issue](https://github.com/Restioson/xtra/issues/new) or message me on the [Rust discord](https://bit.ly/rust-community).

Keep in mind that `xtra` has a MSRV of 1.56.0.

## Cargo features

- `timing`: enables the `notify_interval` method, and brings in the
  [futures-timer](https://github.com/async-rs/futures-timer) crate.
- `with-async_std-1`: enables integration with [async-std](https://async.rs/).
- `with-smol-1`: enables integration with [smol](https://github.com/smol-rs/smol). Note that this requires smol 1.1 as
  1.1 had a minor breaking change from 1.0 which leads to xtra no longer compiling on 1.0 and 1.1 simultaneously.
- `with-tokio-1`: enables integration with [tokio](https://tokio.rs).
- `with-wasm_bindgen-0_2`: enables integration with [wasm-bindgen](https://github.com/rustwasm/wasm-bindgen), and
  particularly its futures crate.
- `with-tracing-0_1`: enables the `Instrumented` wrapper and `InstrumentedExt` traits, to integrate with
  [tracing](https://tracing.rs). This allows a tracing span to follow through execution of a message, either as a child
  of the sending span, or as a separate span marked as `follows_from` the sending span.

## Latest Breaking Changes
To see the breaking changes for each version, see [here](https://github.com/Restioson/xtra/blob/master/BREAKING-CHANGES.md).
The latest version is 0.6.0.
