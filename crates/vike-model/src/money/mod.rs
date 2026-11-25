//! Positions and money: cost basis, equity, margin and liquidation, sizing, fees and the
//! slippage table (`slippage`).
//!
//! `fill` lives here beside `position` rather than beside the order files: `position` owns the
//! `Fill`/`Trade` records and `fill::compute_fill` is the cost-basis primitive that folds them.
//! The sizing law is `contract_value`'s (`ContractValue::unit_value` / `units_worth`), beside every
//! other money formula of one unit.

pub mod account_position;
pub mod contract_value;
pub mod costs;
pub mod currency;
pub mod equity;
pub mod fees;
pub mod fill;
pub mod ledger;
pub mod liquidation;
pub mod lot;
pub mod margin;
pub mod margin_mode;
pub mod money_model;
pub mod position;
pub mod slippage;
pub mod trade_closed;
