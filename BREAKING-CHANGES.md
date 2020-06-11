# Breaking Changes by Version

## 0.4.0

- The `stable` feature was removed. In order to enable the nightly API, enable the new `nightly` feature.

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
