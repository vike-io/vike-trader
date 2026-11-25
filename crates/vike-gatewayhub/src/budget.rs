//! `budget` — the request budgets of every venue this daemon talks to, in SHADOW mode: it counts,
//! classifies, warns and exposes its counters, and **refuses nothing** (decision D4 of wave 1;
//! `docs/decisions/0164-one-daemon-owns-every-venue-connection-vike-gatewayhub.md`).
//!
//! # Why shadow, and what it is for
//!
//! A venue's rate limit is a fact about the venue, spent by everything on this box's IP. Every venue
//! connection is this daemon's now, but only the trade dispatch (`crate::trade::dispatch`'s `spend`, per
//! order-class command) and the connection chain's reads ([`reads`]: the bridges' history replays and the
//! reconcile fetches) spend here: the data and REST lanes spend nothing, so the counters see part of the
//! traffic. An order reserve computed from them would be a guess, and a refusal built on a guess is a new
//! way to lose data. So the whole mechanism — keys,
//! classes, the reserve arithmetic, the counters, the would-throttle detector — runs with enforcement
//! off ([`mode::MODE`]), and the counters exist and have the CI box evidence behind them by the time
//! something may act on them.
//!
//! The datahub's own per-lane arithmetic (the catalog and seed token buckets, `MARKET_DATA_LINGER`,
//! `MARKET_DATA_MAX_KEYS_PER_VENUE`) STAYS in the datahub and keeps doing the reservation decisions 0062 and
//! 0052 give it; this module neither replaces nor second-guesses it.
//!
//! # The pieces
//!
//! * [`key`] — [`key::BudgetKey`] (`venue/host/scope`) and [`key::BudgetScope`].
//! * [`class`] — [`class::BudgetClass`]: `Order`, `Live`, `Interactive`, `Bulk`.
//! * [`ceiling`] — the cap a counter judges against, read from `vike_venues::rate_limits_for`
//!   (axes `orders`, `history`, `websocket_sends`, `rest_ip_weight`; the table has no connection axis).
//! * [`reserve`] — the PURE calculator that splits a cap into an order reserve and a shared pool.
//! * [`window`] — the sliding-window sum, on its own ring of instants. Not
//!   `vike_bridge_core::ratelimit::RateGate`: that is a blocking FIFO limiter, this is a passive
//!   counter, and a default build links no bridge-core.
//! * [`mod@line`] — one counter and the would-throttle detector (a warning once per line per window).
//! * [`ledger`] — [`ledger::Budget`], the daemon's one ledger, and its [`ledger::BudgetSnapshot`], the
//!   value `Request::GatewayStatus` reports as counter rows (`gatewayhub_cli/wire.rs`'s `budget_counters`).
//! * [`mode`] — the one constant ([`mode::MODE`]) that says whether any of it may refuse.
//! * [`reads`] — the connection chain's reads: the sink the bridges' replays charge through, the reconcile fetch's
//!   charge, and the read meter each spends (never the `Orders` axis, never the order reserve).
//!
//! # The clock is an argument
//!
//! Nothing here reads a clock: every call that depends on time takes `now: std::time::Instant`.
//! The arithmetic is therefore tested with synthetic instants, and a caller on the hot path pays for
//! the one `Instant::now()` it already has.

pub mod ceiling;
pub mod class;
pub mod key;
pub mod ledger;
pub mod line;
pub mod mode;
pub mod reads;
pub mod reserve;
pub mod window;

#[path = "budget_tests.rs"]
#[cfg(test)]
mod budget_tests;
