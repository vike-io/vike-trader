//! THE correctness gate: for every indicator, `on_bar` folded over a synthetic
//! series must equal `vectorize` on that series, BIT-FOR-BIT. This proves the
//! streaming path and the batch path are one source of truth. Bits are compared
//! via `f64::to_bits`; both-NaN counts as equal (warm-up NaNs).

mod common;
use common::{assert_lines_bit_eq, bits_eq, fold_stream, synth_bars};

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
