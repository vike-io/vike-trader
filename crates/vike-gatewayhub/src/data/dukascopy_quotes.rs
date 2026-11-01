//! **dukascopy's PRICE STREAM** (any-symbol, decision 0174): the credentialed stream the strategy lane opens for
//! `cs_venue=dukascopy`, so the tradehub's control door can price a pair its engine was not started on.
//!
//! A different kind of credentialed stream from the other four: nothing is mounted on dukascopy (a profile
//! cannot name it), and this stream **logs in to nothing**. The JForex sidecar is ONE child process per daemon,
//! started by the TRADE lane for the one dukascopy account that is armed at `demo` (`vike_dukascopy::adapter`);
//! its quotes reach this stream over the process's price bus (`vike_dukascopy::quotes`), which the sidecar's
//! exec client attaches itself to. So:
//!
//! * **LAZY, end to end.** Building it dials nothing and subscribes nothing; the first `subscribe_quotes` (the
//!   door asking a price for a pair) writes the `subscribe` command to the sidecar that is already up. No
//!   armed account and no sidecar means no price, said in words (`Listening::want`'s refusals), never a second
//!   login: that would put two JVMs on one platform cache (`crates/bridges/dukascopy/src/adapter.rs`'s module doc).
//! * **QUOTES ONLY.** Bars, trades and the book are refused by name; the price lane needs nothing else.
//! * **REAL-TIME, two-sided quotes only**: the bus has already dropped any tick that is one-sided, crossed, untimed
//!   or not current (`vike_dukascopy::quotes::accept`), so every quote this stream emits is one, and the stream's
//!   label is sent as real-time on a symbol's first quote ([`vike_model::DataDelay::RealTime`], read off that tick's own time).
//! * **NO link state is disclosed**: no `GapStart`, no `Live`. A dead sidecar and a closed FX market are both
//!   silence, and the `dukascopy` row of `vike_venues::link_deadman_default` is `Inert` for that reason; the
//!   tradehub's price tap ages the last quote out (fail-closed) and the two hub links speak for the venue.
//! * **Bounded** by the bus (`vike_dukascopy::quotes::MAX_PRICED_SYMBOLS`), and one stream at a time owns the
//!   bus (a newer one replaces the older, which the bus warns about, naming both, and whose asks then refuse: the
//!   symbols it already streams too, `PriceStream::subscribe_quotes`).
//! * **The sink is fed from the bus's own thread**, never the sidecar's reader (`vike_dukascopy::quotes`'s module
//!   doc): a slow or panicking sink costs this stream its quotes and nothing else.
//!
//! ⚠ **What this needs from the box**: a `jforex-bridge.jar` of the release that announces `ticks` (an older jar
//! is refused in words), and a dukascopy account armed at `demo` in THIS gatewayhub process.
//!
//! The stream itself is `vike_bridge_core::price_stream::PriceStream` over this venue's source, which says
//! everything venue-specific (its words, whether a repeat ask goes back to the source): the `PriceBus` and
//! `BusHold` impls of `crates/bridges/dukascopy/src/quotes.rs`. `super::credentialed` builds it; this module
//! holds its contract and its tests.

#[cfg(test)]
#[path = "dukascopy_quotes_tests.rs"]
mod dukascopy_quotes_tests;
