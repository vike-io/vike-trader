//! **fxcm's PRICE STREAM** (any-symbol, decision 0174): the credentialed stream the datahub's broker opens for
//! `cs_venue=fxcm`, so the tradehub's control door can price a pair its engine was not started on. Every holder of
//! the broker shares that one client (decision 0193): a second client would be a second consumer of the desk, and
//! would silence the first.
//!
//! The same kind of stream as dukascopy's ([`super::dukascopy_quotes`]) and for the same reason: nothing is mounted
//! on fxcm (a profile cannot name it: the venue has no market-data seam) and this stream **logs in to nothing**.
//! FXCM sessions are scarce, so the quotes come off the Offers table of the account's RUNNING reconcile session,
//! which the trade lane's reconcile client registered (`vike_fxcm::reads`) and `vike_fxcm::quotes::PriceDesk` polls:
//!
//! * **LAZY, end to end.** Building it dials nothing and asks the venue nothing; the first `subscribe_quotes` (the
//!   door asking a price for a pair) starts the desk's poller, which reads the table every couple of seconds for
//!   the pairs wanted and for no others. No reconcile session (reconciliation off, a failed login, a box whose
//!   only FXCM account is labelled) means no price, said in words (`Listening::want`'s refusals): a second login
//!   is never opened.
//! * **QUOTES ONLY.** Bars, trades and the book are refused by name; the price lane needs nothing else.
//! * **REAL-TIME, two-sided quotes only**: the desk has already refused a row whose market is closed, whose quote
//!   is one-sided, crossed, untimed, from the future or older than 120 s by its own time
//!   (`vike_fxcm::quotes::judge`), so every quote this stream emits is a live one, and the stream's label is sent as
//!   real-time on a symbol's first quote ([`vike_model::DataDelay::RealTime`]).
//! * **NO link state is disclosed**: no `GapStart`, no `Live`. A closed FX market and a wedged session are both
//!   silence, and the `fxcm` row of `vike_venues::link_deadman_default` is `Inert` for that reason; the tradehub's
//!   price tap ages the last quote out (fail-closed) and the two hub links speak for the venue. The words are the
//!   desk's, in this daemon's log.
//! * **Bounded** by the desk (`vike_fxcm::quotes::MAX_PRICED_SYMBOLS`), one request every couple of seconds at
//!   most, and one stream at a time owns the desk (a newer one replaces the older, which the desk warns about and
//!   whose asks refuse while the newer one stands, the symbols it already streamed too).
//!
//! ⚠ **What this needs from the box**: a shim that exports `fc_offers` (an older one is refused in words by the
//! read, once a minute), and an FXCM `demo` account running with reconciliation ON.
//!
//! The stream itself is `vike_bridge_core::price_stream::PriceStream` over this venue's source, which says
//! everything venue-specific (its words, whether a repeat ask goes back to the source): the `PriceBus` and
//! `BusHold` impls of `crates/bridges/fxcm/src/quotes.rs`. `super::credentialed` builds it; this module
//! holds its contract and its tests.

#[cfg(test)]
#[path = "fxcm_quotes_tests.rs"]
mod fxcm_quotes_tests;
