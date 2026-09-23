//! The quarantined FFI surface for runtime-loaded user Rust strategies.
//!
//! ⚠ This crate is one of three in the tree that may contain `unsafe`
//! (`crates/vike-ops/tests/unsafe_and_toolchain_gate.rs` is the authority). It exists
//! so that nothing above it does: `vike-studio-core`, `vike-backend` and every consumer
//! receive an ordinary `Box<dyn Strategy<SimBroker>>`.
//!
//! ⚠ **The BUILDER service (`build_plugin`, the authenticated wire protocol, the retention
//! pruner, the `vike-strategy-builder` binary) is NOT here — it moved to the sibling crate
//! `vike-strategy-builder` by a controller ruling.** This crate used to hold both halves, so
//! every compiled user plugin inherited the builder's whole dependency tree
//! (`vike-datahub-client`, `serde_json`, `tracing`), directly undercutting the design's own
//! reason for choosing DYNAMIC linkage (a plugin should be tens of kilobytes, not megabytes).
//! What stays here is exactly what a plugin genuinely needs: the C-ABI vocabulary, both sides'
//! wrappers, the dlopen loader, and the toolchain fingerprint.
//!
//! ⚠ **The `template/*.in` files live here too, and `vike-strategy-builder` reaches them through
//! [`TEMPLATE_CARGO_TOML`]/[`TEMPLATE_LIB_RS`] below — an ordinary Cargo dependency edge, not a
//! bare filesystem path.** A first cut of this split reached the templates from the builder crate
//! with a raw `include_str!("../../vike-strategy-plugin/template/...")` — a coupling `cargo tree`
//! and `layer_gate.rs` cannot see, invisible right up until a rename breaks it. `builder ->
//! plugin` is DOWN the stack (`vike-strategy-builder` is layer 65, this crate 41), the permitted
//! direction — only `plugin -> builder` would be wrong, and exposing a `pub const` here is not
//! that: a compiled plugin still links only what THIS crate needs, because a constant string is
//! not a dependency edge of its OWN — `vike-strategy-builder`'s dependency tree stays exactly as
//! absent from a plugin's link as it was before. The files stay `.in` (never `.rs`): a
//! `{{USER_SOURCE}}`/`{{NAME}}` placeholder is not valid Rust, and `.in` keeps them outside
//! `crates/vike-ops/tests/unsafe_and_toolchain_gate.rs`'s file walk (`src/`, `build.rs`, and —
//! for the three `unsafe`-exempt crates — `tests/`/`benches`/`examples`; `template/` is none of
//! those, and `.in` is not `.rs` either).
//!
//! Design: `docs/superpowers/specs/2026-09-21-runtime-loaded-rust-strategies-design.md`.

pub mod abi;
pub mod fingerprint;
pub mod guest;
pub mod host;
pub mod loader;

/// The cdylib template's `Cargo.toml`, with `{{NAME}}`/`{{VIKE_MODEL_PATH}}`/
/// `{{VIKE_STRATEGY_PLUGIN_PATH}}` placeholders — rendered by `vike-strategy-builder`'s
/// `build_plugin`. See this module's doc for why this is a `pub const` reached over an ordinary
/// dependency edge rather than a bare `include_str!` path crossing the crate boundary.
pub const TEMPLATE_CARGO_TOML: &str = include_str!("../template/Cargo.toml.in");

/// The cdylib template's `lib.rs`, with `{{USER_SOURCE}}` the only placeholder — the user's
/// `<name>.rs` spliced in UNMODIFIED, never reformatted or wrapped in a module.
pub const TEMPLATE_LIB_RS: &str = include_str!("../template/lib.rs.in");
