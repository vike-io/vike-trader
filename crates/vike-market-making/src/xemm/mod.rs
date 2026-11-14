//! The CROSS-EXCHANGE market maker's (xEMM's) MATHS: the pricing arithmetic of one tick
//! ([`pricing`], ported from Hummingbot's cross-exchange market making strategy, Apache-2.0,
//! `hummingbot/strategy/cross_exchange_market_making`) and the risk half, vike's own: the halt
//! vocabulary and the freshness and naked-band verdicts ([`guards`]), the hedge ledger ([`hedge`])
//! and the diagnostic basis estimate ([`basis`]).
//!
//! The maker that drives them (its struct, its `Strategy` impl, its quote emission and its saved
//! state) is the strategy FILE `strategies/market_making/cross_exchange_maker.rs`, whose module doc
//! carries the design; its white-box tests sit inside it. Everything here is pure, with in-file
//! `#[cfg(test)]` tests.

pub mod basis;
pub mod guards;
pub mod hedge;
pub mod pricing;
