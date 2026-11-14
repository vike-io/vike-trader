//! The prediction-market helpers the shipped strategy files, the backtest's resolution source and
//! the research runners share, apart from the framework (`controller`, `position_executor` at the
//! crate root). No strategy lives here: each is a file under `strategies/`.
//!
//! The root names [`crate::TokenId`] and the copy-trading tracker's vocabulary
//! ([`crate::SignalTracker`], …); a module path here is for the items the root does not carry.

// The Polymarket up/down token grammar (`TokenId`), and the resting-ask re-scoring of its edge.
pub mod cheap_np;
pub mod cheap_np_ask;
// The shared fair-value math (`trailing_sigma`, the `libm::log` site; the cheap-band predicates).
// `p_up` lives in `vike_model`; this module only calls it.
pub mod fair_value;
// The Polymarket sports/esports copy-trading conviction detector and symbol grammar.
pub mod sport_taker;
