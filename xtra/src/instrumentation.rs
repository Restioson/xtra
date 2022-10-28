#[cfg(feature = "instrumentation")]
mod tracing;

#[cfg(feature = "instrumentation")]
pub use self::tracing::*;

#[cfg(not(feature = "instrumentation"))]
mod stub;

#[cfg(not(feature = "instrumentation"))]
pub use self::stub::*;
