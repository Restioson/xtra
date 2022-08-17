# macros-test

This is a testing crate for the macros provided by xtra as they are re-exported from the main `xtra` library.

Moving the tests into a separate crate allows us to define test utils in the crate's `lib.rs` file and ensures the user-facing public macro API is as we expect.
Users are meant to access the macros through the main `xtra` crate.
The `xtra-macros` crate is an implementation detail that is enforced by `cargo` because proc-macros need to be in their own crate.
