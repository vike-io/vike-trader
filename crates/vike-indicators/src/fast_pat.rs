//! What the candlestick patterns of `fast_m.rs`, `fast_n.rs` and `fast_q.rs` share: the number of
//! bodies in the rolling average body.
//!
//! The average itself (`patterns.rs`'s old `avg_body`: the SMA of the last `CTX` bodies
//! `|close - open|`, folded oldest first from `-0.0` as `iter().sum::<f64>()` does) is written three
//! times on purpose, each tuned to the code around it (a shift register of bodies in `fast_m.rs`, a
//! lazily folded ring in `fast_n.rs`, a shift register of partial folds in `fast_q.rs`): all three
//! give the old bits exactly, because a pattern compares bodies with this average and a signal of
//! `+100`, `-100` or `0` must not flip because a sum rounded differently.

/// Bars of context for the rolling average body (`patterns.rs`'s `CTX`).
pub(crate) const CTX: usize = 10;
