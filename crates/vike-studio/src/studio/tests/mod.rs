//! `StudioState`'s unit tests, split by theme into this folder's children. Every test kept its
//! body and its name, so the one thing that changed in a test's id is its module path
//! (`studio::tests::<child>::<name>`).
//!
//! * `support` - the fixtures more than one theme needs (`state_new`, `seeded_store`,
//!   `spawn_backtesthub_server`, `answered_build`). Every other child imports what it uses.
//! * `tabs_and_qa` - the right-tab strip, the QA capture hooks (tab pick, autorun) and the sweep
//!   ladder they seed.
//! * `shell_state` - a fresh state, the Rhai verdict `poll` holds, the "why is this disabled"
//!   sentences and the backend switch.
//! * `worker_polling` - `poll` folding each worker's answer (or its death) into the shell, and
//!   `cancel`.
//! * `saved_strategies` - save / load / delete / Compare All over the Saved list.
//! * `file` - the Strategy files mode: spec, dirty chip, restore, run, save, compare.
//! * `rust_build` - Rust mode's Build -> sha -> Run ordering and its staleness guard.
//! * `rust_buffer` - what Strategy file mode does to the Rust buffer: the Rhai-writer refusals, the
//!   save re-baseline, the restart restore.
//! * `copilot` - the AI Copilot's candidates, each a server Run of the same held-out bars, and its
//!   refusal on the Named backend.
//! * `device_key` - every remote backtesthub dial signed with this PC's device key, and its refusal
//!   in words when the server's access list does not hold the device.
//! * `wire_fields` - the funding switch and a Rhai script's values on every run, and what the panes say
//!   on a server that predates them.
//! * `research` - the Research pane over the shell: a study runs on the server over the Source
//!   backend only, a finished one re-asks the runs, and the study result screen, saved or not.
//!
//! ⚠ **The `#[cfg(test)]` on each child looks redundant, and is not.** This whole module is already
//! test-only, but `crates/vike-utility/src/libm_walk.rs`'s `cfg_test_module_files` - the resolver the
//! text gates in vike-ops's tests skip test files through - follows ONLY a `#[cfg(test)] mod NAME;`
//! declaration, and not transitively. A bare `mod` child would be scanned as PRODUCTION by every one
//! of them.

#[cfg(test)]
mod support;

#[cfg(test)]
mod tabs_and_qa;

#[cfg(test)]
mod shell_state;

#[cfg(test)]
mod worker_polling;

#[cfg(test)]
mod saved_strategies;

#[cfg(test)]
mod file;

#[cfg(test)]
mod rust_build;

#[cfg(test)]
mod rust_buffer;

#[cfg(test)]
mod copilot;

#[cfg(test)]
mod device_key;

#[cfg(test)]
mod research;

#[cfg(test)]
mod sdk_source;

#[cfg(test)]
mod wire_fields;
