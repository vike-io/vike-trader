//! Shared test fixtures: the seeded random-walk bars and the two Rhai strategies. Compiled ONCE by the
//! lib — as `crate::test_support` for the unit tests and, behind `test-support` (only the self
//! dev-dependency turns it on), as `vike_studio_core::test_support` for the integration binaries,
//! which used to `#[path]`-include each file.

pub mod cross_script;
pub mod sweep_script;
pub mod walk;
