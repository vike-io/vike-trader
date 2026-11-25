//! Pure non-math helpers that two or more crates used to copy (decision 0141, amended by 0294): one
//! home near the bottom of the workspace graph (`layer = 3`), named at the module, never re-exported
//! at the crate root.
//!
//! A helper belongs here only when it is pure (or a filesystem write, or a std lock or thread
//! helper, whose POLICY is the point of sharing it: `atomic_file`, `unreadable_file`, `mutex`, `thread`), needs no dependency
//! this crate does not already have, and carries no domain meaning. Moving a helper here deletes its copies in the same change. **A formula never belongs
//! here**: every pure formula lives in `vike-math` (layer 2), whatever its number of users
//! (decision 0294), and the next one must not land in this crate because a module here sounds
//! numeric.
//!
//! It names no `vike-*` crate. That is what lets a crate that may not link the domain vocabulary
//! (`vike-model`) or the market-data vocabulary (`vike-marketdata`) call these helpers instead of
//! carrying a copy. It has no unconditional dependency at all: std only.
//!
//! It is NOT published (decision 0294 returned it to `publish = false`; it was never published).
//!
//! ⚠ **One module is the exception to "no dependency this crate does not already have":** `cli`
//! (decision 0155) is the shared layer over `clap` that every program's command line goes through,
//! and it exists only behind the `cli` Cargo feature, OFF by default, with `clap` an OPTIONAL
//! dependency. A crate that names `vike-utility` without the feature compiles no `clap`; a program
//! turns it on with `vike-utility = { workspace = true, features = ["cli"] }`. The feature is the whole
//! of the exception: nothing else here may name `clap`.
//!
//! The second is `serde_fields`, behind the `serde` feature with `serde` an OPTIONAL dependency: the
//! field names a derived `Deserialize` reads, asked of the derive (CLI/MCP plan P1 item 2, which moved
//! vike-mcp's and vike-model's copies here). Nothing else here may name `serde`, and a third
//! feature-gated dependency needs its own record.
//!
//! `libm_walk` (decision 0074) is test machinery, not a helper: the source-text parser every crate's
//! `libm_platform_probe` and the vike-ops source gates share. It compiles only under
//! `cfg(any(test, feature = "test-support"))`, the feature is off by default and adds no dependency,
//! and a consumer turns it on through a DEV edge only. It lives here, on the helper floor, so the
//! formula floor and this crate reach it without a dev edge onto the vocabulary that names them both.
//!
//! Every caller names the module path: `vike_utility::cli::Program`, `vike_utility::fnv::fnv1a64`,
//! `vike_utility::calendar::civil_from_days`. The two `#[macro_export]` macros are the one exception
//! Rust imposes (an exported `macro_rules!` has no module path): `vike_utility::word_enum!`, the one
//! spelling of a closed word vocabulary, and `vike_utility::refusal_enum!` (`errors`), which declares a
//! refusal enum and its ids through it.

pub mod atomic_file;
pub mod bytes;
pub mod calendar;
#[cfg(feature = "cli")]
pub mod cli;
pub mod errors;
pub mod fnv;
pub mod hex;
pub mod html;
/// The SHARED text parser behind every crate's `libm_platform_probe` (decision 0074). A default build
/// compiles none of it: test machinery reached as a dev-dependency feature. Its own planted-fixture
/// self-test deliberately lives in `tests/`; the module doc says why.
#[cfg(any(test, feature = "test-support"))]
pub mod libm_walk;
pub mod mutex;
pub mod number_text;
pub mod panic;
pub mod percent;
pub mod poller;
pub mod recent_set;
pub mod retry;
pub mod runs;
#[cfg(feature = "serde")]
pub mod serde_fields;
pub mod thread;
pub mod token_bucket;
pub mod unreadable_file;
pub mod url;
// `word_enum!` (`ALL`, `WORDS`, `word`, `from_word`, `parse`): a `#[macro_export]` macro, so its one
// name is `vike_utility::word_enum!` and the module itself stays private.
mod word_enum;
