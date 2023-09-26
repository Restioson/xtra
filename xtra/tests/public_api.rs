//! Dedicated tests for checking the public API of xtra.

use xtra::prelude::*;
use xtra::refcount::{Either, RefCounter};

pub trait AddressExt {}

// Ensures that we can abstract over addresses of any ref counter type.
impl<A, Rc: RefCounter> AddressExt for Address<A, Rc> {}

#[allow(dead_code)]
// The mere existence of this function already ensures that these public APIs exist, which is what we want to test!
#[allow(unknown_lints, clippy::let_underscore_future)]
fn functions_on_address_with_generic_rc_counter<A, Rc, Rc2>(
    address1: Address<A, Rc>,
    address2: Address<A, Rc2>,
) where
    A: Actor,
    Rc: RefCounter,
    Rc2: RefCounter,
    A: Handler<(), Return = ()>,
{
    address1.len();
    address1.capacity();
    let _ = address1.join();
    let _ = address1.send(());
    let _ = address1.broadcast(());
    address1.is_connected();
    address1.is_empty();
    let _ = address1.same_actor(&address2);
}

#[allow(dead_code)] // The mere existence of this function already ensures that these public APIs exist, which is what we want to test!
fn converting_address_to_either_rc<A, Rc>(address1: Address<A, Rc>)
where
    A: Actor,
    Rc: RefCounter + Into<Either>,
    A: Handler<(), Return = ()>,
{
    address1.as_either();
}
