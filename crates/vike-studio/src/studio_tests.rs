//! `StudioState`'s unit tests, split by theme into the sibling `studio_tests/` directory. A PURE
//! MOVE out of what was one file: every test kept its body and its name, so the one thing that
//! changed in a test's id is its module path (`studio_tests::<child>::<name>`).
//!
//! * `support` - the fixtures more than one theme needs (`state_new`, `seeded_store`,
//!   `spawn_compute_server`, `answered_build`). Every other child imports what it uses.
//! * `tabs_and_qa` - the right-tab strip, the QA capture hooks (tab pick, autorun) and the sweep
//!   ladder they seed.
//! * `shell_state` - a fresh state, the Rhai verdict `poll` holds, the "why is this disabled"
//!   sentences and the backend switch.
//! * `worker_polling` - `poll` folding each worker's answer (or its death) into the shell, and
//!   `cancel`.
//! * `saved_strategies` - save / load / delete / Compare All over the Saved list.
//! * `native` - the registry-strategy mode: spec, dirty chip, restore, run, save, compare.
//! * `plugin_build` - Plugin mode's Build -> sha -> Run ordering and its staleness guard.
//! * `plugin_buffer` - what Plugin mode does to the Rust buffer: the Rhai-writer refusals, the
//!   save re-baseline, the restart restore.
//!
//! ⚠ **Both attributes on each declaration below look redundant, and neither is.**
//!
//! * `#[path = "studio_tests/<child>.rs"]`: this file is itself loaded by a `#[path]` in
//!   `studio.rs`, and rustc (and rustfmt) treat every `#[path]`-loaded file as a `mod.rs` - so a
//!   bare `mod support;` here looks for a `support.rs` BESIDE this file, not inside the directory
//!   named after it (MEASURED with rustfmt, 2026-10-05: "failed to resolve mod `support`"). The
//!   explicit path is also what the shared test-module resolver reads, so it and the compiler
//!   agree on where each child lives.
//! * `#[cfg(test)]`: this whole file is already test-only, but
//!   `crates/vike-model/src/libm_walk.rs`'s `cfg_test_module_files` - the resolver the text gates
//!   in vike-ops's tests skip test files through - follows ONLY a `#[cfg(test)] mod NAME;`
//!   declaration. A bare `mod` child would be scanned as PRODUCTION by every one of them.

#[path = "studio_tests/support.rs"]
#[cfg(test)]
mod support;

#[path = "studio_tests/tabs_and_qa.rs"]
#[cfg(test)]
mod tabs_and_qa;

#[path = "studio_tests/shell_state.rs"]
#[cfg(test)]
mod shell_state;

#[path = "studio_tests/worker_polling.rs"]
#[cfg(test)]
mod worker_polling;

#[path = "studio_tests/saved_strategies.rs"]
#[cfg(test)]
mod saved_strategies;

#[path = "studio_tests/native.rs"]
#[cfg(test)]
mod native;

#[path = "studio_tests/plugin_build.rs"]
#[cfg(test)]
mod plugin_build;

#[path = "studio_tests/plugin_buffer.rs"]
#[cfg(test)]
mod plugin_buffer;
