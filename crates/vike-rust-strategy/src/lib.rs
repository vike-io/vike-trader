//! The quarantined FFI surface for runtime-loaded user Rust strategies.
//!
//! ⚠ This crate is one of the few in the tree that may contain `unsafe`
//! (`crates/vike-ops/tests/hygiene/unsafe_and_toolchain_gate.rs` is the authority, and it alone
//! says how many). It exists so that nothing above it does: `vike-studio-core`, `vike-backend`
//! and every consumer receive an ordinary `Box<dyn Strategy<OrderBrokerSimulator>>`.
//!
//! ⚠ **The BUILDER service lives in the sibling crate `vike-strategy-builder`, never here**, so
//! a compiled strategy file links only what it needs: the C-ABI vocabulary, both sides' wrappers, the
//! dlopen loader and the toolchain fingerprint.
//!
//! ⚠ **The `template/*.in` files live here too, and `vike-strategy-builder` reaches them through
//! [`TEMPLATE_CARGO_TOML`]/[`TEMPLATE_LIB_RS`] below — an ordinary Cargo dependency edge, not a
//! bare filesystem path.** A first cut of this split reached the templates from the builder crate
//! with a raw `include_str!("../../vike-rust-strategy/template/...")` — a coupling `cargo tree`
//! and `layer_gate.rs` cannot see, invisible right up until a rename breaks it. `builder ->
//! strategy_file` is DOWN the stack (`vike-strategy-builder` is layer 65, this crate 15), the permitted
//! direction — only `strategy_file -> builder` would be wrong, and exposing a `pub const` here is not
//! that: a compiled strategy file still links only what THIS crate needs, because a constant string is
//! not a dependency edge of its OWN — `vike-strategy-builder`'s dependency tree stays exactly as
//! absent from a strategy file's link as it was before. The files stay `.in` (never `.rs`): a
//! `{{ENTRY}}`/`{{NAME}}` placeholder is not valid Rust, and `.in` keeps them outside
//! `crates/vike-ops/tests/hygiene/unsafe_and_toolchain_gate.rs`'s file walk (`src/`, `build.rs`, and —
//! for the `unsafe`-exempt crates — `tests/`/`benches`/`examples`; `template/` is none of
//! those, and `.in` is not `.rs` either).
//!
//! Design: `docs/superpowers/specs/2026-09-21-runtime-loaded-rust-strategies-design.md`.

#![warn(clippy::undocumented_unsafe_blocks)]
#![warn(clippy::allow_attributes)]

pub mod abi;
pub mod artifact;
pub mod fingerprint;
pub mod guest;
pub mod host;
pub mod loader;
/// The broker double this crate's integration binaries share (`test-support` only: a default build,
/// and so every strategy file, never compiles it).
#[cfg(feature = "test-support")]
pub mod test_support;

/// The cdylib template's `Cargo.toml`, with `{{NAME}}`/`{{VIKE_MODEL_PATH}}`/
/// `{{VIKE_RUST_STRATEGY_PATH}}` placeholders — rendered by `vike-strategy-builder`'s
/// `build_strategy_file`. See this module's doc for why this is a `pub const` reached over an ordinary
/// dependency edge rather than a bare `include_str!` path crossing the crate boundary.
pub const TEMPLATE_CARGO_TOML: &str = include_str!("../template/Cargo.toml.in");

/// The cdylib template's `lib.rs`, with the `{{ENTRY}}`, `{{REFUSE}}` and `{{ECHO}}` placeholders the
/// builder renders from what the strategy file exports; the file itself is its module `strategy`
/// (`src/strategy.rs`), written UNMODIFIED.
pub const TEMPLATE_LIB_RS: &str = include_str!("../template/lib.rs.in");
