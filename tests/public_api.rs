//! Dedicated tests for checking the public API of xtra.

use xtra::prelude::*;
use xtra::refcount::RefCounter;

pub trait AddressExt {}

// Ensures that we can abstract over addresses of any ref counter type.
impl<A, Rc: RefCounter> AddressExt for Address<A, Rc> {}
