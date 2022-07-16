# Breaking Changes by Version

## 0.6.0

- Sealed `RefCounter` and `MessageChannel` traits
- `Message` no longer exists - `Return` is now specified on the `Handler` trait itself.
- `Context::notify_interval` and `Context::notify_after` are now subject to back-pressure, in case the address mailbox
  is full. These aren't API breaking but a semantic changes.
- `stopping` has been removed in favour of `stop_self` and `stop_all`. If logic to determine if the actor should stop
  must be executed, it should be done rather at the point of calling `stop_{self,all}`.
- Previously, `stop_all` would immediately disconnect the address. However, `stop_self` done on every actor would actually
  not do this in one case - if there were a free-floating (not executing an actor event loop) Context. This change brings
  `stop_all` in line with `stop_self`.
- `MessageChannel` is now a `struct` that can be constructed from an `Address` via `MessageChannel::new` or using
  `From`/`Into`.
- `AddressSink` was removed in favor of using `impl Trait` for the `Address::into_sink` method.
- `Context::attach_stream` was removed in favor of composing `Stream::forward` and `Address::into_sink`.
- `KeepRunning` was removed as `Context::attach_stream` was its last usage.
- `InstrumentedExt` was removed. All messages are now instrumented automatically when `instrumentation` is enabled.
- `stop_all` now does not drain all messages when called, and acts just like `stop_self` on all active actors.
- `Context::attach` is removed in favor of implementing `Clone` for `Context`. If you want to run multiple actors on a
  `Context`, simply clone it before calling `run`.
- Remove `Context::notify_after` without a direct replacement. To delay the sending of a message, users are encouraged
  to use the `sleep` function of their executor of choice and combine it with `Address::send` into a new future. To
  cancel the sleeping early in case the actor stops, use `xtra::scoped`.
- Remove `Context::notify_interval` without a direct replacement. Users are encouraged to write their own loop within
  which they call `Address:send`.

## 0.5.0

- **The `SyncHandler` trait has been removed.** This simplifies the API and should not change the performance on stable.
    - *How to upgrade:* change all implementations of the `SyncHandler` trait to the normal `Handler` trait.
- **All `Actor` lifecycle messages are now async.** This allows to do more kinds of things in lifecycle methods,
  while adding no restrictions.
    - *How to upgrade:* add `async` to the function definition of all actor lifecycle methods, add `#[async_trait::async_trait]`
      to the `impl Actor` block.
- `Actor` now requires `Send` to implement. Previously, the trait itself did not, but using it did require `Send`.
    - *How to upgrade:* you probably never had a non-`Send` actor in the first place.
- The `{Weak}{Address|MessageChannel}::attach_stream` methods now require that the actor implements `Handler<M>` where 
  `M: Into<KeepRunning> + Send`. This is automatically implemented for `()`, returning `KeepRunning::Yes`. This allows
  the user more control over the future spawned by `attach_stream`, but is breaking if the message returned did not
  implement `Into<KeepRunning>`.
    - *How to upgrade:* implement `Into<KeepRunning>` for all message types used in `attach_stream`. To mimic previous
      behaviour, return `KeepRunning::Yes` in the implementation.
- `Address` and `WeakAddress` lost their `Sink` implementations. They can now be turned into `Sink`s by calling
  `.into_sink()`.
    - *How to upgrade:* convert any addresses to be used as sinks into sinks with `.into_sink()`, cloning where necessary.
- `{Address|MessageChannel}Ext` were removed in favour of inherent implementations,
    - *How to upgrade:* in most cases, this will not have broken anything. If it is directly imported, remove the import.
      If you need to be generic over weak and strong addresses, be generic over `Address<A, Rc>` where `Rc: RefCounter` for
      addresses and use `MessageChannel<M>` trait objects.
- `{Weak}MessageChannel` became traits rather than concrete types. In order to use them, simply cast an address to
  the correct trait object (e.g `&addr as &dyn MessageChannel<M>` or `Box::new(addr)`). The `into_channel` and `channel`
  methods were also removed.
    - *How to upgrade:* replace `{into_}channel` calls with casts to trait objects, and replace references to the old
      types with trait objects, boxed if necessary. If you were using the `Sink` implementations of these types, first
      create an `AddressSink` through `.into_sink()`, and then cast it to a `MessageSink` trait object.
- `Address::into_downgraded` was removed. This did nothing different to `downgrade`, except dropping the address after 
  the call.
    - *How to upgrade:* Simply call `Address::downgrade`, dropping the strong address afterwards if needed.
- Some types were moved out of the root crate and into modules.
    - *How to upgrade:* search for the type's name in the documentation and refer to it by its new path.
- `Actor::spawn` and `Actor::create` now take an`Option<usize>` for the mailbox size.
    - *How to upgrade:* choose a suitable size of mailbox for each spawn call, or pass `None` to give them unbounded
      mailboxes.
- Many context methods now return `Result<..., ActorShutdown>` to represent failures due to the actor having been shut
  down.

## 0.4.0

- The `stable` feature was removed. ~~In order to enable the nightly API, enable the new `nightly` feature.~~ *Edit: as
  of 0.5.0, the nightly API has been removed.*

## 0.3.0

- The default API of the `Handler` trait has now changed to an `async_trait` so that xtra can compile on stable.
    - *How to upgrade, alternative 1:* change the implementations by annotating the implementation with `#[async_trait]`,
      removing `Responder` and making `handle` an `async fn` which directly returns the message's result.
    - *How to upgrade, alternative 2:* if you want to avoid the extra box, you can disable the default `stable` feature
      in your `Cargo.toml` to keep the old API.

## 0.2.0

- Removal of the `with-runtime` feature
    - *How to upgrade:* you probably weren't using this anyway, but rather use `with-tokio-*` or `with-async_std-*`
    instead.
- `Address` methods were moved to `AddressExt` to accommodate new `Address` types
    - *How to upgrade:* add `use xtra::AddressExt` to wherever address methods are used (or, better yet, 
    `use xtra::prelude::*`)
- All `*_async` methods were removed. Asynchronous and synchronous messages now use the same method for everything.
    - *How to upgrade:* simply switch from the `[x]_async` method to the `[x]` method.
- `AsyncHandler` was renamed to `Handler`, and the old `Handler` to `SyncHandler`. Also, a `Handler` and `SyncHandler` implementation can no longer coexist.
    - *How to upgrade:* rename all `Handler` implementations to `SyncHandler`, and all `AsyncHandler` implementations to `Handler`.
