//! The service that compiles an authenticated caller's Rust strategy into a
//! `vike-strategy-plugin`-shaped cdylib.
//!
//! ⚠ **Split out of `vike-strategy-plugin` by a controller ruling, not by the original
//! runtime-loaded-Rust-strategies plan.** `vike-strategy-plugin` used to hold BOTH the loader (which
//! a compiled plugin links against) and this builder service (which pulls `vike-datahub-client`
//! and `serde_json` — and `tracing` too, until that one was dropped for emitting to no subscriber;
//! `src/builder.rs`'s header carries that argument) — so every compiled user plugin inherited the whole service
//! dependency tree, directly undercutting the design's own reason for choosing DYNAMIC linkage
//! (`docs/superpowers/specs/2026-09-21-runtime-loaded-rust-strategies-design.md`: a plugin should be
//! tens of kilobytes, not megabytes). `vike-strategy-plugin` now keeps only what a plugin genuinely
//! needs (`abi`, `guest`, `host`, `loader`, `fingerprint`, and the `template/` this crate renders);
//! this crate takes the builder half: `build_plugin` and the cdylib template invocation
//! (`render.rs` — named for what it does, not `build.rs`; see that module's own doc for why the
//! filename itself was load-bearing), the authenticated wire protocol, the retention pruner, and
//! the `vike-strategy-builder` binary.
//!
//! ⚠ **This crate is NOT part of the quarantined FFI surface and carries no `unsafe`** — it
//! shells out to `cargo` and speaks an authenticated TCP wire protocol, neither of which touches
//! the C-ABI boundary `vike-strategy-plugin` owns. It takes the workspace's ordinary
//! `unsafe_code = "forbid"` lint (`[lints] workspace = true`), unlike its sibling.
//!
//! Design: `docs/superpowers/specs/2026-09-21-runtime-loaded-rust-strategies-design.md`.

pub mod builder;
/// The CLIENT half of [`builder`]'s protocol — the call a Studio makes to ask for a build. It
/// lives beside the server rather than in the caller so one crate owns both ends of one exchange;
/// its own module doc carries the argument.
pub mod client;
pub mod render;
