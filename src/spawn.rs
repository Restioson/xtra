use std::future::Future;

#[cfg(feature = "with-async_std-1")]
pub use async_std_impl::*;
#[cfg(feature = "with-smol-1")]
pub use smol_impl::*;
#[cfg(feature = "with-tokio-1")]
pub use tokio_impl::*;
#[cfg(feature = "with-wasm_bindgen-0_2")]
pub use wasm_bindgen_impl::*;

use crate::{Actor, ActorManager, Address};

/// An `Spawner` represents anything that can spawn a future to be run in the background. This is
/// used to spawn actors.
pub trait Spawner {
    /// Spawn the given future.
    fn spawn<F: Future<Output = ()> + Send + 'static>(&mut self, fut: F);
}

#[cfg(feature = "with-async_std-1")]
mod async_std_impl {
    use super::*;

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

#[cfg(feature = "with-smol-1")]
mod smol_impl {
    use super::*;

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

#[cfg(feature = "with-tokio-1")]
mod tokio_impl {
    use super::*;

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

#[cfg(feature = "with-wasm_bindgen-0_2")]
mod wasm_bindgen_impl {
    use super::*;

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
