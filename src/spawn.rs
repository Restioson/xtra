use std::future::Future;

#[cfg(feature = "async_std")]
pub use async_std_impl::*;
#[cfg(feature = "smol")]
pub use smol_impl::*;
#[cfg(feature = "tokio")]
pub use tokio_impl::*;
#[cfg(feature = "wasm_bindgen")]
pub use wasm_bindgen_impl::*;

/// An `Spawner` represents anything that can spawn a future to be run in the background. This is
/// used to spawn actors.
pub trait Spawner {
    /// Spawn the given future.
    fn spawn<F: Future<Output = ()> + Send + 'static>(&mut self, fut: F);
}

#[cfg(feature = "async_std")]
mod async_std_impl {
    use super::*;
    use crate::{Actor, ActorManager, Address};

    /// The async std runtime.
    #[derive(Copy, Clone, Debug, Default)]
    pub struct AsyncStd;

    impl Spawner for AsyncStd {
        fn spawn<F: Future<Output = ()> + Send + 'static>(&mut self, fut: F) {
            async_std::task::spawn(fut);
        }
    }

    /// An extension trait used to allow ergonomic spawning of an actor onto the global runtime.
    pub trait AsyncStdGlobalSpawnExt<A: Actor> {
        /// Spawn the actor onto the global runtime
        fn spawn_global(self) -> Address<A>;
    }

    impl<A: Actor<Stop = ()>> AsyncStdGlobalSpawnExt<A> for ActorManager<A> {
        fn spawn_global(self) -> Address<A> {
            self.spawn(&mut AsyncStd)
        }
    }
}

#[cfg(feature = "smol")]
mod smol_impl {
    use super::*;
    use crate::{Actor, ActorManager, Address};

    /// The smol runtime.
    #[derive(Copy, Clone, Debug)]
    pub enum Smol<'a> {
        /// The global executor.
        Global,
        /// A specific smol executor.
        Handle(&'a smol::Executor<'a>),
    }

    impl<'a> Default for Smol<'a> {
        fn default() -> Self {
            Smol::Global
        }
    }

    impl<'a> Spawner for Smol<'a> {
        fn spawn<F: Future<Output = ()> + Send + 'static>(&mut self, fut: F) {
            let task = match self {
                Smol::Global => smol::spawn(fut),
                Smol::Handle(e) => e.spawn(fut),
            };
            task.detach();
        }
    }

    /// An extension trait used to allow ergonomic spawning of an actor onto the global runtime.
    pub trait SmolGlobalSpawnExt<A: Actor> {
        /// Spawn the actor onto the global runtime
        fn spawn_global(self) -> Address<A>;
    }

    impl<A: Actor<Stop = ()>> SmolGlobalSpawnExt<A> for ActorManager<A> {
        fn spawn_global(self) -> Address<A> {
            self.spawn(&mut Smol::Global)
        }
    }
}

#[cfg(feature = "tokio")]
mod tokio_impl {
    use super::*;
    use crate::{Actor, ActorManager, Address};

    /// The Tokio runtime.
    #[derive(Copy, Clone, Debug)]
    pub enum Tokio<'a> {
        /// The global executor.
        Global,
        /// A handle to a specific executor.
        Handle(&'a tokio::runtime::Runtime),
    }

    impl<'a> Default for Tokio<'a> {
        fn default() -> Self {
            Tokio::Global
        }
    }

    impl<'a> Spawner for Tokio<'a> {
        fn spawn<F: Future<Output = ()> + Send + 'static>(&mut self, fut: F) {
            match self {
                Tokio::Global => tokio::spawn(fut),
                Tokio::Handle(handle) => handle.spawn(fut),
            };
        }
    }

    /// An extension trait used to allow ergonomic spawning of an actor onto the global runtime.
    pub trait TokioGlobalSpawnExt<A: Actor> {
        /// Spawn the actor onto the global runtime
        fn spawn_global(self) -> Address<A>;
    }

    impl<A: Actor<Stop = ()>> TokioGlobalSpawnExt<A> for ActorManager<A> {
        fn spawn_global(self) -> Address<A> {
            self.spawn(&mut Tokio::Global)
        }
    }
}

#[cfg(feature = "wasm_bindgen")]
mod wasm_bindgen_impl {
    use super::*;
    use crate::{Actor, ActorManager, Address};

    /// Spawn rust futures in WASM on the current thread in the background.
    #[derive(Copy, Clone, Debug, Default)]
    pub struct WasmBindgen;

    impl Spawner for WasmBindgen {
        fn spawn<F: Future<Output = ()> + Send + 'static>(&mut self, fut: F) {
            wasm_bindgen_futures::spawn_local(fut)
        }
    }

    /// An extension trait used to allow ergonomic spawning of an actor onto the global runtime.
    pub trait WasmBindgenGlobalSpawnExt<A: Actor> {
        /// Spawn the actor onto the global runtime
        fn spawn_global(self) -> Address<A>;
    }

    impl<A: Actor<Stop = ()>> WasmBindgenGlobalSpawnExt<A> for ActorManager<A> {
        fn spawn_global(self) -> Address<A> {
            self.spawn(&mut WasmBindgen)
        }
    }
}
