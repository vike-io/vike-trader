//! Test fixtures more than one crate's tests share, compiled ONCE into this lib for its own unit tests
//! and under the `test-support` feature (only dev edges turn it on: this crate's own self-referential
//! one, the tradehub's and the gatewayhub's), instead of once per test binary that carried a copy.
//! A fixture that names venues does not belong here (this crate names no venue, decision 0096): the
//! arming credential sets are `vike_gatewayhub::testkit::arming_fixtures`, handed to
//! [`arming_parity`] as functions.

pub mod account_book;
pub mod arming_parity;
pub mod arming_row;
pub mod doubles;
pub mod preflight;
pub mod rig;
pub mod risk;
