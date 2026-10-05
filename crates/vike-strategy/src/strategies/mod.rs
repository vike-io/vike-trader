//! The concrete strategies — every `impl<B: Broker> Strategy<B>` this crate ships — apart from the
//! framework they are built on (`controller`, `position_executor`, `registry` at the crate root).
//!
//! Each strategy's public vocabulary is ALSO named at the crate root (`vike_strategy::Grid`,
//! `vike_strategy::PairsZScore`, …), and that root spelling is the one callers use; a module path
//! here is for the items the root does not carry.

// The Polymarket up/down fair-value taker and its two leaves, arrived from vike-backtest. They
// are `impl<B: Broker> Strategy<B>` like every other strategy here; they lived in the simulator
// only because that is where they were first written.
pub mod cheap_np;
pub mod cheap_np_ask;
// The shared fair-value math the taker reads — `trailing_sigma` (the `libm::log` site this crate
// takes `libm` for) plus the cheap-band predicates. Its `p_up` is a re-export of `vike_model::p_up`
// and is deliberately NOT re-exported at this crate's root: that would mint a second name for a
// vike-model symbol.
pub mod fair_value;
pub mod funding_capture;
pub mod funding_carry;
pub mod grid_dca;
pub mod pairs;
// The Polymarket sports/esports copy-trading taker, arrived from vike-backtest with the above.
pub mod sport_taker;
pub mod trailing_scalper;
