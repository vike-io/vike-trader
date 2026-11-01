//! The settings-row round-trip suites as ONE test binary (`cargo test -p vike-config --test rows`).
//!
//! Each suite is still its own file, `tests/rows/<suite>.rs`, and here a MODULE named after it, so a
//! test's id is `<suite>::<test>` and `cargo test -p vike-config --test rows -- gatewayhub_addr_row::`
//! runs one suite. Every test and every assertion is the one the suite's own binary ran.
//!
//! Why one binary: every suite writes into `tests/common/seeded_store.rs`'s store, and as three
//! binaries each compiled its own copy; declared ONCE below, it compiles once and each suite reaches it
//! as `crate::seeded_store`. The grouping rule is `crates/vike-backtest/CLAUDE.md`'s "Test-binary
//! consolidation": every member is plain (no feature gate, no proptest sidecar, no process-global
//! mutation; each test plants its own temporary store).

#[path = "common/seeded_store.rs"]
mod seeded_store;

#[path = "rows/data_plane_rows.rs"]
mod data_plane_rows;
#[path = "rows/gatewayhub_addr_row.rs"]
mod gatewayhub_addr_row;
#[path = "rows/gatewayhub_connect_wait_row.rs"]
mod gatewayhub_connect_wait_row;
