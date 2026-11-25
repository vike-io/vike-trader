//! The shipped TAKER strategy files (plan 2b's unit C1: every file but the makers and the
//! gallery-born ones), each compiled here as a `#[path]` module so the strategy-lab crate's tests, the
//! test stand-in ([`crate::stand_in`]) and the dev tools that drive one by hand all read the FILE.
//! Each file's own tests sit inside it (`#[cfg(test)]`, naming `crate::test_support`).

// ⚠ An `allow`, not an `expect`: each file's surface is wider than what reads it here, and which of
// its items go unused differs per file, so an `expect` would be unfulfilled on some of them.
#[allow(dead_code, clippy::allow_attributes)]
#[path = "../../../strategies/trend/buy_hold.rs"]
pub mod buy_hold;
#[allow(dead_code, clippy::allow_attributes)]
#[path = "../../../strategies/prediction_markets/cheap_catch_updown_fair_value.rs"]
pub mod cheap_catch_updown_fair_value;
#[allow(dead_code, clippy::allow_attributes)]
#[path = "../../../strategies/options/covered_call.rs"]
pub mod covered_call;
#[allow(dead_code, clippy::allow_attributes)]
#[path = "../../../strategies/grid_dca/dca_accumulate.rs"]
pub mod dca_accumulate;
#[allow(dead_code, clippy::allow_attributes)]
#[path = "../../../strategies/funding/funding_capture.rs"]
pub mod funding_capture;
#[allow(dead_code, clippy::allow_attributes)]
#[path = "../../../strategies/funding/funding_carry.rs"]
pub mod funding_carry;
#[allow(dead_code, clippy::allow_attributes)]
#[path = "../../../strategies/grid_dca/grid.rs"]
pub mod grid;
#[allow(dead_code, clippy::allow_attributes)]
#[path = "../../../strategies/trend/momentum.rs"]
pub mod momentum;
#[allow(dead_code, clippy::allow_attributes)]
#[path = "../../../strategies/pairs/pairs_zscore.rs"]
pub mod pairs_zscore;
#[allow(dead_code, clippy::allow_attributes)]
#[path = "../../../strategies/portfolio/rotation_top_k.rs"]
pub mod rotation_top_k;
#[allow(dead_code, clippy::allow_attributes)]
#[path = "../../../strategies/prediction_markets/sport_copy_follower.rs"]
pub mod sport_copy_follower;
#[allow(dead_code, clippy::allow_attributes)]
#[path = "../../../strategies/trend/trailing_scalper.rs"]
pub mod trailing_scalper;
