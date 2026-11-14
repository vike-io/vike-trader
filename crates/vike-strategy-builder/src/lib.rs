//! The service that compiles an authenticated caller's Rust strategy into a
//! `vike-rust-strategy`-shaped cdylib.
//!
//! ⚠ **Split out of `vike-rust-strategy` by a controller ruling, not by the original
//! runtime-loaded-Rust-strategies plan.** `vike-rust-strategy` used to hold BOTH the loader (which
//! a compiled strategy file links against) and this builder service (which pulls `vike-datahub-client`
//! and `serde_json` — and `tracing` too, until that one was dropped for emitting to no subscriber;
//! `src/builder.rs`'s header carries that argument) — so every compiled user strategy file inherited the whole service
//! dependency tree, directly undercutting the design's own reason for choosing DYNAMIC linkage
//! (`docs/superpowers/specs/2026-09-21-runtime-loaded-rust-strategies-design.md`: a strategy file should be
//! tens of kilobytes, not megabytes). `vike-rust-strategy` now keeps only what a strategy file genuinely
//! needs (`abi`, `guest`, `host`, `loader`, `fingerprint`, and the `template/` this crate renders);
//! this crate takes the builder half: `build_strategy_file` and the cdylib template invocation
//! (`render.rs` — named for what it does, not `build.rs`; see that module's own doc for why the
//! filename itself was load-bearing), the authenticated wire protocol, the retention pruner, and
//! the `vike-strategy-builder` binary.
//!
//! ⚠ **This crate is NOT part of the quarantined FFI surface and carries no `unsafe`** — it
//! shells out to `cargo` and speaks an authenticated TCP wire protocol, neither of which touches
//! the C-ABI boundary `vike-rust-strategy` owns. It takes the workspace's ordinary
//! `unsafe_code = "forbid"` lint (`[lints] workspace = true`), unlike its sibling. The build jail
//! keeps that: `jail` says what a build is granted and `vike-sandbox` applies it, its kernel calls
//! through the `landlock` and `seccompiler` crates' safe interfaces, applied to a thread, never
//! through a `pre_exec` hook.
//!
//! Design: `docs/superpowers/specs/2026-09-21-runtime-loaded-rust-strategies-design.md`.

pub mod builder;
/// The CLIENT half of [`builder`]'s protocol — the call a Studio makes to ask for a build. It
/// lives beside the server rather than in the caller so one crate owns both ends of one exchange;
/// its own module doc carries the argument.
pub mod client;
// The build jail's grants (decision 0273, step 1). No outer `///` here: rustdoc would merge it with
// `jail.rs`'s `//!` and resolve that module doc's links from THIS scope, where `Grants` is not.
pub mod jail;
pub mod render;
