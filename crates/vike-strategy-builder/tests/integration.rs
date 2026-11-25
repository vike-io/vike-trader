//! **vike-strategy-builder's integration tests as ONE test binary** (`cargo test -p vike-strategy-builder --test integration`).
//!
//! Each file keeps its path, `tests/<name>.rs`; here it is a MODULE, so a test's id is `<name>::<test>` and
//! `cargo test -p vike-strategy-builder --test integration -- <name>::` runs one file (the `#[ignore]`d ones with
//! `--ignored <name>::`). One binary is compiled and linked instead of one per file, and the two helpers under
//! `tests/common/` compile once: a member names them `crate::checkout_shape` and `crate::stderr_lines`. Grouped by
//! the rule of `crates/vike-backtest/CLAUDE.md`'s "Test-binary consolidation": no member mutates process-global
//! state (the daemons and `cargo` run as child processes, each given its environment on its `Command`). The two
//! Linux-only files (`build_jail`, `child_env`) gate themselves with an inner `#![cfg(target_os = "linux")]`, which
//! drops the module elsewhere. Test discovery is OFF (`autotests = false`), so a new `tests/*.rs` builds only as a
//! `mod` here or a `[[test]]` row, and `crates/vike-ops/tests/hygiene/test_registry_gate.rs`'s `every_root_test_file_is_built_where_discovery_is_off` fails until it is one.

#[path = "common/checkout_shape.rs"]
mod checkout_shape;
#[path = "common/stderr_lines.rs"]
mod stderr_lines;

mod build_errors;
mod build_jail;
mod builder_auth;
mod builder_live_smoke;
mod child_env;
mod refusal_reaches_stderr;
