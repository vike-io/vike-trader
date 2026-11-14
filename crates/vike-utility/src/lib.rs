//! Pure helpers that two or more crates used to copy (decision 0141): one home at the bottom of the
//! workspace graph (`layer = 3`), named at the module, never re-exported at the crate root.
//!
//! A helper belongs here only when it is pure (or a filesystem write whose POLICY is the point of
//! sharing it), needs no dependency this crate does not already have, and carries no domain
//! meaning. Moving a helper here deletes its copies in the same change.
//!
//! It names no `vike-*` crate, and cannot: nothing in the workspace declares a rank below 3. That
//! is what lets a crate that may not link the domain vocabulary (`vike-model`) or the market-data
//! vocabulary (`vike-marketdata`) call these helpers instead of carrying a copy. Its one external
//! (unconditional) dependency is `libm`, for [`math::norm_cdf`]'s `erf`
//! (`docs/decisions/0032-transcendentals-come-from-the-libm-crate-not-the-platform.md`).
//!
//! ⚠ **One module is the exception to "no dependency this crate does not already have":** `cli`
//! (decision 0155) is the shared layer over `clap` that every program's command line goes through,
//! and it exists only behind the `cli` Cargo feature, OFF by default, with `clap` an OPTIONAL
//! dependency. A crate that names `vike-utility` without the feature compiles no `clap`; a program
//! turns it on with `vike-utility = { workspace = true, features = ["cli"] }`. The feature is the whole
//! of the exception: nothing else here may name `clap`, and a second feature-gated dependency needs
//! its own record.
//!
//! Every caller names the module path: `vike_utility::cli::Program`, `vike_utility::fnv::fnv1a64`,
//! `vike_utility::calendar::civil_from_days`.

pub mod atomic_file;
pub mod bytes;
pub mod calendar;
#[cfg(feature = "cli")]
pub mod cli;
pub mod fnv;
pub mod hex;
pub mod math;
pub mod percent;
pub mod recent_set;
pub mod retry;
