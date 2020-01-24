# Breaking Changes by Version

## 0.2.0

- Removal of the `with-runtime` feature
    - *How to upgrade:* You probably weren't using this anyway, but rather use `with-tokio-*` or `with-async_std-*`
    instead.
- `Address` methods were moved to `AddressExt` to accommodate new `Address` types
    - *How to upgrade:* add `use xtra::AddressExt` to wherever address methods are used (or, better yet, 
    `use xtra::prelude::*`)
- All `*_async` methods were removed. Asynchronous and synchronous messages now use the same method for everything.
    - *How to upgrade:* simply switch from the `[x]_async` method to the `[x]` method.
- `AsyncHandler` was renamed to `Handler`, and the old `Handler` to `SyncHandler`. Also, a `Handler` and `SyncHandler` implementation can no longer coexist.
    - *How to upgrade:* rename all `Handler` implementations to `SyncHandler`, and all `AsyncHandler` implementations to `Handler`.
