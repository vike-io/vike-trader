//! THE correctness gate: for every indicator, `on_bar` folded over a synthetic
//! series must equal `vectorize` on that series, BIT-FOR-BIT. This proves the
//! streaming path and the batch path are one source of truth. Bits are compared
//! via `f64::to_bits`; both-NaN counts as equal (warm-up NaNs).
//!
//! This binary also holds the three other suites that share `tests/common/mod.rs` — `lookback`,
//! `params` and `pairs_parity`, each a module named after the file it was — so `common` compiles
//! ONCE instead of once per binary (`cargo test -p vike-indicators --test parity -- lookback::`). The
//! lib route was not open: `common`'s `synth_bars` draws with the platform `sin`/`cos`, which `src/`'s
//! libm gates refuse, and switching it to `libm` would change every suite's input series. Every member
//! is plain (no feature gate, no proptest sidecar, no process-global mutation); `lookback`'s two
//! `#[ignore]`d tests are print-only probes (no network, no clock, no shared state), not the live or
//! heavy tests `crates/vike-backtest/CLAUDE.md`'s "Test-binary consolidation" keeps out of a group.

mod common;
use common::{assert_lines_bit_eq, bits_eq, fold_stream, synth_bars};

#[path = "parity/lookback.rs"]
mod lookback;
#[path = "parity/pairs_parity.rs"]
mod pairs_parity;
#[path = "parity/params.rs"]
mod params;

#[path = "parity/constant_window.rs"]
mod constant_window;
#[path = "parity/finite_window_shapes.rs"]
mod finite_window_shapes;
#[path = "parity/past_the_trim.rs"]
mod past_the_trim;
#[path = "parity/stream_vs_batch.rs"]
mod stream_vs_batch;
#[path = "parity/variance_fold_gate.rs"]
mod variance_fold_gate;
