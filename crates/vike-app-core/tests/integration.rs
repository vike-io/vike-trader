//! **vike-app-core's plain integration suites as ONE test binary** (`cargo test -p vike-app-core --test integration`).
//!
//! Each suite keeps its file, `tests/<suite>.rs`, and is a MODULE here, so a test's id is `<suite>::<test>`
//! and `cargo test -p vike-app-core --test integration -- <suite>::` runs one suite. Every test and assertion
//! is the one the suite's own binary ran; one binary links instead of eleven. Grouped by
//! `crates/vike-backtest/CLAUDE.md`'s "Test-binary consolidation": no member mutates process-global state
//! (each egui harness owns its `Context`, each scratch directory and loopback port is its own), because a
//! threaded `cargo test` runs them all in this one process. The suites that stay their own binary, and why,
//! are the `[[test]]` rows of `crates/vike-app-core/Cargo.toml`.
//!
//! ⚠ The manifest turns test discovery OFF (`autotests = false`) so every file keeps its path. A new
//! `tests/*.rs` is built only as a `mod` here or a `[[test]]` row, and
//! `crates/vike-ops/tests/hygiene/test_registry_gate.rs`'s `every_root_test_file_is_built_where_discovery_is_off`
//! fails until it is one or the other.

mod backfill_wire_scripted;
mod catalog_refresh;
mod catalog_wire;
mod chart_follows_backend_series;
mod credential_editor_completeness_gate;
mod data_manager_credentials_panel;
mod libm_platform_probe;
mod no_shared_key_messages;
mod polymarket_proxy_journal;
mod qa_shot_account;
mod qa_slugs;
