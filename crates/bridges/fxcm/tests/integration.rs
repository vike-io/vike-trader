//! **vike-fxcm's top-level test files as ONE test binary** (`cargo test -p vike-fxcm --test integration`).
//!
//! Each file keeps its path, `tests/<name>.rs`, its `//!` contract and its children in `tests/<name>/`; here it is
//! a MODULE, so a test's id is `<name>::<test>` and `cargo test -p vike-fxcm --test integration -- <name>::` runs
//! one file. One binary is compiled and linked instead of one per file, by the rule of
//! `crates/vike-backtest/CLAUDE.md`'s "Test-binary consolidation": the one process-global write here is fxcm's kept
//! list, held alike (the same rows) by every member that holds it. Each member's own auto-discovered target is
//! switched off in this crate's `Cargo.toml` (`test = false`), which says why discovery stays on here.
//!
//! `shim_source` is the shim gates' shared reader of `crates/bridges/fxcm/src/shim/fcshim.cpp`
//! (`tests/shim_source/mod.rs`), declared once here so it compiles once; `fxcm_shim`'s children reach it as
//! `crate::shim_source`. Still their own binaries: `fcsdk_packaging`, `fcsdk_rpath_tag` and
//! `fxcm_login_triage_gate` (the public mirror deletes them, `scripts/publish_mirror.sh`'s DENY), `fxcm_no_kept_list`
//! (a process that holds no kept list) and `fxcm_smokes`, the smokes folder.

mod shim_source;

mod decoder_never_panics;
mod fcsdk_probe_is_watched;
mod fxcm_exit_events;
mod fxcm_login_failure;
mod fxcm_price_lane_money;
mod fxcm_reconcile_parse;
mod fxcm_shim;
mod fxcm_shim_order_permission;
