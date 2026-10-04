//! Positions and money: cost basis, cash, equity, margin and liquidation, sizing and fees.
//!
//! `fill` lives here beside `position` rather than beside the order files: `position` owns the
//! `Fill`/`Trade` records and `fill::compute_fill` is the cost-basis primitive that folds them.

pub mod cash;
pub mod equity;
pub mod fees;
pub mod fill;
pub mod liquidation;
pub mod margin;
pub mod position;
pub mod sizing;
