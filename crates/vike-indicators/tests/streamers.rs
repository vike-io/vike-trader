//! The `streamers_*_exact` suites as ONE test binary (`cargo test -p vike-indicators --test streamers`).
//!
//! Each suite is still its own file, `tests/streamers/<suite>.rs`, and here a MODULE named after it, so
//! a test's id is `<suite>::<test>` and `cargo test -p vike-indicators --test streamers --
//! streamers_ema_exact::` runs one suite. Every test and every assertion is the one the suite's own
//! binary ran.
//!
//! Why one binary: every suite reads `tests/common/streamers.rs` (the two generators, the awkward
//! series and the stream-equals-batch assertions), and as eight binaries each compiled its own copy;
//! declared ONCE below, it compiles once and each suite reaches it as `crate::streamers`. The grouping
//! rule is `crates/vike-backtest/CLAUDE.md`'s "Test-binary consolidation": every member is plain (no
//! feature gate, no proptest sidecar, no process-global mutation).

#[path = "common/streamers.rs"]
mod streamers;

#[path = "streamers/streamers_bands_exact.rs"]
mod streamers_bands_exact;
#[path = "streamers/streamers_compound_exact.rs"]
mod streamers_compound_exact;
#[path = "streamers/streamers_directional_exact.rs"]
mod streamers_directional_exact;
#[path = "streamers/streamers_ema_exact.rs"]
mod streamers_ema_exact;
#[path = "streamers/streamers_multi_exact.rs"]
mod streamers_multi_exact;
#[path = "streamers/streamers_stats_exact.rs"]
mod streamers_stats_exact;
#[path = "streamers/streamers_wilder_exact.rs"]
mod streamers_wilder_exact;
#[path = "streamers/streamers_window_exact.rs"]
mod streamers_window_exact;
