//! `budget` — the request budgets of every venue this daemon talks to, in SHADOW mode: it counts,
//! classifies, warns and exposes its counters, and **refuses nothing** (decision D4 of wave 1;
//! `docs/decisions/0164-one-daemon-owns-every-venue-connection-vike-gatewayhub.md`).
//!
//! # Why shadow, and what it is for
//!
//! A venue's rate limit is a fact about the venue, spent by everything on this box's IP. Today the
//! tradehub's orders and its own feeds are INVISIBLE to this daemon (it learns of them in wave 2),
//! so this daemon sees only the datahub's half of each IP budget: an order reserve computed from
//! that half would be a guess, and a refusal built on a guess is a new way to lose data. Wave 1
//! therefore builds the whole mechanism — keys, classes, the reserve arithmetic, the counters, the
//! would-throttle detector — and runs it with enforcement off ([`mode::MODE`]), so the counters
//! exist and have the CI box evidence behind them by the time something may act on them.
//!
//! The datahub's own per-lane arithmetic (the catalog and seed token buckets, `MD_LINGER`,
//! `MD_MAX_KEYS_PER_VENUE`) STAYS in the datahub and keeps doing the reservation decisions 0062 and
//! 0052 give it; this module neither replaces nor second-guesses it.
//!
//! # The pieces
//!
//! * [`key`] — [`key::BudgetKey`] (`venue/host/scope`) and [`key::Scope`].
//! * [`class`] — [`class::Class`]: `Order`, `Live`, `Interactive`, `Bulk`.
//! * [`ceiling`] — the cap a counter judges against, read from `vike_venues::rate_limits_for`
//!   (axes `orders`, `history`, `ws_sends`, `rest_ip_weight`; the table has no connection axis).
//! * [`reserve`] — the PURE calculator that splits a cap into an order reserve and a shared pool.
//! * [`window`] — the sliding-window sum, on its own ring of instants. Not
//!   `vike_bridge_core::ratelimit::RateGate`: that is a blocking FIFO limiter, this is a passive
//!   counter, and this crate links no bridge-core.
//! * [`mod@line`] — one counter and the would-throttle detector (a warning once per line per window).
//! * [`ledger`] — [`ledger::Budget`], the daemon's one ledger, and its [`ledger::Snapshot`], the
//!   value a future status verb serialises.
//! * [`mode`] — the one constant ([`mode::MODE`]) that says whether any of it may refuse.
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
pub mod reserve;
pub mod window;

#[path = "budget_tests.rs"]
#[cfg(test)]
mod budget_tests;
