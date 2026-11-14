//! The two shipped MARKET-MAKING strategy files (plan 2b's unit C2), compiled here as `#[path]`
//! modules so the check crate's tests and the tests of the cores and hubs that mount a maker all read
//! the FILE. Each file's own tests sit inside it (`#[cfg(test)]`), run by this library's unit tests.

// ⚠ An `allow`, not an `expect`: each file's whole public surface (every builder, both entry points)
// is compiled, and which of its items go unused here differs per file.
#[allow(dead_code, clippy::allow_attributes)]
#[path = "../../../strategies/market_making/cross_exchange_maker.rs"]
pub mod cross_exchange_maker;
#[allow(dead_code, clippy::allow_attributes)]
#[path = "../../../strategies/market_making/spread_maker.rs"]
pub mod spread_maker;
