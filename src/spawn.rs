use std::future::Future;

#[cfg(feature = "with-async_std-1")]
pub use async_std_impl::*;
#[cfg(feature = "with-smol-1")]
pub use smol_impl::*;
#[cfg(feature = "with-tokio-0_2")]
pub use tokio_impl::*;
#[cfg(feature = "with-wasm_bindgen-0_2")]
pub use wasm_bindgen_impl::*;

/// An `Spawner` represents anything that can spawn a future to be run in the background. This is
/// used to spawn actors.
pub trait Spawner {
    /// Spawn the given future.
    fn spawn<F: Future<Output = ()> + Send + 'static>(&mut self, fut: F);
}

#[cfg(feature = "with-smol-1")]
mod smol_impl {
    use super::*;

    /// The smol runtime.
    pub enum Smol<'a> {
        /// The global executor.
        Global,
        /// A specific smol executor.
        Handle(&'a smol::Executor),
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
}

#[cfg(feature = "with-async_std-1")]
mod async_std_impl {
    use super::*;

    /// The async std runtime.
    pub struct AsyncStd;

    impl Spawner for AsyncStd {
        fn spawn<F: Future<Output = ()> + Send + 'static>(&mut self, fut: F) {
            async_std::task::spawn(fut);
        }
    }
}

#[cfg(feature = "with-tokio-0_2")]
mod tokio_impl {
    use super::*;

    /// The Tokio runtime.
    pub enum Tokio<'a> {
        /// The global executor.
        Global,
        /// A handle to a specific executor.
        Handle(&'a tokio::runtime::Handle),
    }

    impl<'a> Spawner for Tokio<'a> {
        fn spawn<F: Future<Output = ()> + Send + 'static>(&mut self, fut: F) {
            match self {
                Tokio::Global => tokio::spawn(fut),
                Tokio::Handle(handle) => handle.spawn(fut),
            };
        }
    }
}

#[cfg(feature = "with-wasm_bindgen-0_2")]
mod wasm_bindgen_impl {
    use super::*;

    /// Spawn rust futures in WASM on the current thread in the background.
    pub struct WasmBindgen;

    impl Spawner for WasmBindgen {
        fn spawn<F: Future<Output = ()> + Send + 'static>(&mut self, fut: F) {
            wasm_bindgen_futures::spawn_local(fut)
        }
    }
}
