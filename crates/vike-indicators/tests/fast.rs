//! The `fast_*` and `series_*` suites as ONE test binary (`cargo test -p vike-indicators --test fast`).
//!
//! Each suite is still its own file, `tests/fast/<suite>.rs`, and here a MODULE named after it, so a
//! test's id is `<suite>::<test>` and `cargo test -p vike-indicators --test fast -- fast_n::` runs one
//! suite. Every test and every assertion is the one the suite's own binary ran.
//!
//! Why one binary: every suite reads `tests/common/fast.rs` (the walk, the scales, the tolerance check,
//! the textbook references and the old candle helpers), and as twenty binaries each compiled its own
//! copy; declared ONCE below, it compiles once and each suite reaches it as `crate::fast`, as
//! `common/old_two_bar.rs` (`fast_q`'s old kernels) always did. The grouping rule is
//! `crates/vike-backtest/CLAUDE.md`'s "Test-binary consolidation": every member is plain (no feature
//! gate, no proptest sidecar, no process-global mutation), and the `#[ignore]`d tests are print-only
//! measurements (no network, no clock, no shared state).

#[path = "common/fast.rs"]
mod fast;

#[path = "fast/fast_a.rs"]
mod fast_a;
#[path = "fast/fast_b.rs"]
mod fast_b;
#[path = "fast/fast_c.rs"]
mod fast_c;
#[path = "fast/fast_d1.rs"]
mod fast_d1;
#[path = "fast/fast_d2.rs"]
mod fast_d2;
#[path = "fast/fast_e.rs"]
mod fast_e;
#[path = "fast/fast_f.rs"]
mod fast_f;
#[path = "fast/fast_g.rs"]
mod fast_g;
#[path = "fast/fast_h.rs"]
mod fast_h;
#[path = "fast/fast_i.rs"]
mod fast_i;
#[path = "fast/fast_j.rs"]
mod fast_j;
#[path = "fast/fast_k.rs"]
mod fast_k;
#[path = "fast/fast_l.rs"]
mod fast_l;
#[path = "fast/fast_m.rs"]
mod fast_m;
#[path = "fast/fast_n.rs"]
mod fast_n;
#[path = "fast/fast_o.rs"]
mod fast_o;
#[path = "fast/fast_q.rs"]
mod fast_q;
#[path = "fast/fast_r.rs"]
mod fast_r;
#[path = "fast/series_ema_macd_bb_atr.rs"]
mod series_ema_macd_bb_atr;
#[path = "fast/series_sma_rsi.rs"]
mod series_sma_rsi;
