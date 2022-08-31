/// Spawns the given actor into the tokio runtime, returning an [`Address`](crate::Address) to it.
///
/// The 2nd parameter is the mailbox size with `None` creating an unbounded mailbox.
#[cfg(feature = "tokio")]
#[cfg_attr(docsrs, doc(cfg(feature = "tokio")))]
pub fn spawn_tokio<A>(actor: A, mailbox_size: Option<usize>) -> crate::Address<A>
where
    A: crate::Actor<Stop = ()>,
{
    let (address, mailbox) = crate::Mailbox::new(mailbox_size);

    tokio::spawn(crate::run(mailbox, actor));

    address
}

/// Spawns the given actor into the async_std runtime, returning an [`Address`](crate::Address) to it.
///
/// The 2nd parameter is the mailbox size with `None` creating an unbounded mailbox.
#[cfg(feature = "async_std")]
#[cfg_attr(docsrs, doc(cfg(feature = "async_std")))]
pub fn spawn_async_std<A>(actor: A, mailbox_size: Option<usize>) -> crate::Address<A>
where
    A: crate::Actor<Stop = ()>,
{
    let (address, mailbox) = crate::Mailbox::new(mailbox_size);

    async_std::task::spawn(crate::run(mailbox, actor));

    address
}

/// Spawns the given actor into the smol runtime, returning an [`Address`](crate::Address) to it.
///
/// The 2nd parameter is the mailbox size with `None` creating an unbounded mailbox.
#[cfg(feature = "smol")]
#[cfg_attr(docsrs, doc(cfg(feature = "smol")))]
pub fn spawn_smol<A>(actor: A, mailbox_size: Option<usize>) -> crate::Address<A>
where
    A: crate::Actor<Stop = ()>,
{
    let (address, mailbox) = crate::Mailbox::new(mailbox_size);

    smol::spawn(crate::run(mailbox, actor)).detach();

    address
}

/// Spawns the given actor onto the thread-local runtime via `wasm_bindgen_futures`, returning an [`Address`](crate::Address) to it.
///
/// The 2nd parameter is the mailbox size with `None` creating an unbounded mailbox.
#[cfg(feature = "wasm_bindgen")]
#[cfg_attr(docsrs, doc(cfg(feature = "wasm_bindgen")))]
pub fn spawn_wasm_bindgen<A>(actor: A, mailbox_size: Option<usize>) -> crate::Address<A>
where
    A: crate::Actor<Stop = ()>,
{
    let (address, mailbox) = crate::Mailbox::new(mailbox_size);

    wasm_bindgen_futures::spawn_local(crate::run(mailbox, actor));

    address
}
