//! The two market-making strategy files, `strategies/market_making/spread_maker.rs` and
//! `strategies/market_making/cross_exchange_maker.rs`, compiled in this crate's library
//! (`vike_strategy_lab::makers`; each file's own tests run in the library's unit tests) and checked
//! against what they were copied from (plan 2b, unit C2):
//!
//! - `header`: each header declares exactly the keys its reader reads;
//! - `facts`: the spread maker's mount facts lower a profile's table to the maker the daemon mounted
//!   before plan 2b (`makers/maker_golden.txt`), and its echo is that maker line;
//! - `maker_backtest`: `crates/vike-simulator/tests/fills/maker_backtest.rs`, driven through the file.
//!
//! (`twins`, the file's maker and `vike_market_making`'s side by side, went with vike-market-making's copy.)

// Named at this root so each check reads a file as `super::<file>`.
use vike_strategy_lab::makers::spread_maker;

#[path = "makers/header.rs"]
mod header;

#[path = "makers/facts.rs"]
mod facts;

#[path = "makers/maker_backtest.rs"]
mod maker_backtest;
