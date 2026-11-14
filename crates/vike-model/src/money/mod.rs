//! Positions and money: cost basis, equity, margin and liquidation, sizing and fees.
//!
//! `fill` lives here beside `position` rather than beside the order files: `position` owns the
//! `Fill`/`Trade` records and `fill::compute_fill` is the cost-basis primitive that folds them.
//! The sizing law is `contract_value`'s (`ContractValue::unit_value` / `units_worth`), beside every
//! other money formula of one unit.

pub mod contract_value;
pub mod currency;
pub mod equity;
pub mod fees;
pub mod fill;
pub mod ledger;
pub mod liquidation;
pub mod lot;
pub mod margin;
pub mod margin_mode;
pub mod position;
pub mod trade_closed;
