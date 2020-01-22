# Breaking Changes by Version

## 0.2.0

- Removal of the `with-runtime` feature
    - *How to upgrade:* You probably weren't using this anyway, but rather use `with-tokio-*` or `with-async_std-*`
    instead.
- `Actor::stopped` does not take `&mut Context` any longer
    - *How to upgrade:* move stopping code that requires `Context` to `Actor::stopping`
- `Address` methods were moved to `AddressExt` to accommodate new `Address` types