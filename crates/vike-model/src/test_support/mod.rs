//! Test helpers that several crates' tests carried as identical private copies, each moved here
//! once. A helper joins only when every copy was the same code after renaming; one whose copies
//! differ in any constant is not a duplicate and stays with its crate.
//!
//! ⚠ **The source-walking gates read this module as PRODUCTION code.** Its
//! `any(test, feature = "test-support")` gate is deliberately not one of
//! `vike_model::libm_walk::cfg_test_ranges`'s test-item markers (that module's doc says why: the
//! code ships whenever the feature is on). So nothing here may call a platform transcendental, read
//! the clock, the environment or a temp directory, or bake in a path at compile time — and a helper
//! that needs the CONSUMER's directory takes it as a parameter, because a compile-time manifest
//! path here would name THIS crate's directory for every consumer.
//!
//! Gated like `MockBroker` and `libm_walk`: a default build compiles none of it, and a consumer
//! enables `vike-model/test-support` on a DEV edge only.

pub mod bars;
pub mod etxtbsy;
pub mod maker;
pub mod text;
