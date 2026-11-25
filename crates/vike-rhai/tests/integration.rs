//! **vike-rhai's plain integration suites as ONE test binary** (`cargo test -p vike-rhai --test integration`).
//!
//! Each suite keeps its file, `tests/<suite>.rs`, and is a MODULE here, so a test's id is `<suite>::<test>`
//! and `cargo test -p vike-rhai --test integration -- <suite>::` runs one suite. Every test and assertion is
//! the one the suite's own binary ran; one binary links instead of nine. Grouped by
//! `crates/vike-backtest/CLAUDE.md`'s "Test-binary consolidation": no member mutates process-global state,
//! because a threaded `cargo test` runs them all in this one process. The capture two of them read,
//! `vike_log::capture::captured`, is scoped to the calling thread. `tests/install.rs`,
//! `tests/load_and_install.rs` and `tests/install_from_user_data.rs` stay their own binaries: each installs the
//! once-per-process indicator set (`crates/vike-rhai/CLAUDE.md`'s "The installed set is process-wide, once").
//!
//! ⚠ The manifest turns test discovery OFF (`autotests = false`) so every file keeps its path. A new
//! `tests/*.rs` is built only as a `mod` here or a `[[test]]` row, and
//! `crates/vike-ops/tests/hygiene/test_registry_gate.rs`'s `every_root_test_file_is_built_where_discovery_is_off`
//! fails until it is one or the other.

mod binding;
mod module_import_refused;
mod on_tick;
mod renamed_module_refused;
mod script_print_is_not_stdout;
mod strategy_object;
mod strategy_reads;
mod user_indicator;
mod user_indicator_params;
