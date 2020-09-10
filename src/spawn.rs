use std::future::Future;

pub trait ActorSpawner {
    fn spawn<F: Future<Output = ()> + Send + 'static>(&self, fut: F);
}

#[cfg(feature = "with-smol-1")]
pub use smol_impl::*;
#[cfg(feature = "with-async_std-1")]
pub use async_std_impl::*;
#[cfg(feature = "with-tokio-0_2")]
pub use tokio_impl::*;
#[cfg(feature = "with-wasm_bindgen-0_2")]
pub use smol_impl::*;

#[cfg(feature = "with-smol-1")]
mod smol_impl {
    use super::*;

    pub enum Smol<'a> {
        Global,
        Handle(&'a smol::Executor)
    }

    impl<'a> ActorSpawner for Smol<'a> {
        fn spawn<F: Future<Output = ()> + Send + 'static>(&self, fut: F) {
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

    pub struct AsyncStd;

    impl ActorSpawner for AsyncStd {
        fn spawn<F: Future<Output = ()> + Send + 'static>(&self, fut: F) {
            async_std::task::spawn(fut);
        }
    }
}

#[cfg(feature = "with-tokio-0_2")]
mod tokio_impl {
    use super::*;

    pub enum Tokio<'a> {
        Global,
        Handle(&'a tokio::runtime::Handle)
    }

    impl<'a> ActorSpawner for Tokio<'a> {
        fn spawn<F: Future<Output = ()> + Send + 'static>(&self, fut: F) {
            match self {
                Tokio::Global => tokio::spawn(fut),
                Tokio::Handle(handle) => handle.spawn(fut)
            };
        }
    }
}

#[cfg(feature = "with-wasm_bindgen-0_2")]
mod wasm_bindgen_impl {
    use super::*;

    pub struct WasmBindgen;

    impl ActorSpawner for WasmBindgen {
        fn spawn<F: Future<Output = ()> + Send + 'static>(&self, fut: F) {
            wasm_bindgen_futures::spawn_local(fut)
        }
    }
}
