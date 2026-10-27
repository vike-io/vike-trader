//! **Every history source this daemon fetches from**: the kline registry and the funding registry,
//! MOVED from `crates/vike-datahub/src/backfill.rs` (decision 0164: the datahub keeps the commit key,
//! the still-forming guard and the append, and this daemon makes the venue call), and the dispatch
//! that answers a `Fetch { Klines | Funding }` from them.
//!
//! The datahub's backfill table FOLDS the client's rosters
//! (`vike_gatewayhub_client::history::KLINE_VENUES`, `FUNDING_VENUES`), and this daemon advertises its
//! own, so the three lists are one list in three places; the tests below pin the statics equal, IN
//! ORDER, to both the client's rosters and `crate::registry`'s.
//!
//! # Cancellation: what stops and what cannot
//!
//! A `KlineSource::fetch` is ONE call that does the venue's own paging inside and exposes no page
//! boundary, so the only point this lane can stop at is BEFORE the call: a fetch cancelled while it
//! waited never reaches the venue. A fetch cancelled DURING the call finishes (a binance window is a
//! handful of pages) and the server discards the rows. That is the in-process behaviour exactly: the
//! datahub's one-batch lanes ignore the stop probe too (`BackfillFn`'s doc there).

use vike_data::source::{FundingRateSource, KlineSource};
use vike_gatewayhub_client::proto::{FetchFailure, WireFundingPoint};
use vike_marketdata::Bar;

use super::{FetchCtx, cancelled};

/// **Every kline source this daemon can dispatch.** The ONE roster: the daemon's `facts` and its
/// dispatch are both built FROM it, and the datahub's table folds the client's copy of its order.
///
/// A `static` (not a `const`) so the lookups below hand out a genuinely `'static` borrow.
///
/// ⚠ **ORDER IS OBSERVABLE.** It is what the unknown-venue refusal prints (here
/// `crate::server::fetch::pre_check`'s, and in the datahub the table's `supported`) and what the
/// advertisement preserves, and `crates/vike-datahub/tests/backfill_roundtrip.rs`'s
/// `the_real_table_names_every_kline_venue` pins the rendered order. This is the collectors' own
/// order — the three that shipped first, then the three 0059 Phase 2 dispatched.
///
/// ⚠ **Each row spells `&vike_<venue>::<module>::<Type>`, and the gate DERIVES the registered set
/// from that spelling.** (It was `&crate::venues::<venue>::<Type>` in `vike-backfill` until
/// docs/decisions/0094 moved every impl into the bridge crate that already owned the fetcher it
/// wraps, and this whole static up into the one process that actually dispatches a venue string to
/// a collector — then the datahub, now this daemon, so `vike-backfill` names no kline bridge.)
/// `crates/vike-ops/tests/venues/collector_dispatch_gate.rs` is a text scan — it lives in `vike-ops`,
/// which may not take a normal edge to this crate (layer 15 against this crate's 65), and a
/// dev-edge would drag every kline bridge crate into a test build every PR runs — and it holds this
/// static equal, in order, to the client's `KLINE_VENUES` that the datahub's backfill table folds.
/// A row written some other way is a row that scan cannot see,
/// and the failure is safe: the venue then reads as WRITTEN-BUT-UNREGISTERED and the gate goes red.
///
/// ⚠ **`fxcm` is the seventh row and the one that is not a venue API**: `FxcmKlines` reads FXCM's public
/// candle ARCHIVE (anonymous GETs of weekly `.csv.gz` files, no ForexConnect, no session, no key), so it is a keyless
/// row like the others. It is a ONE-BATCH row, the whole window through one `fetch`, and it answers a week the archive
/// does not hold with an error, never an empty `Vec` (`crates/bridges/fxcm/src/data.rs`'s module doc).
///
/// ⚠ **A credentialed source is NOT a row here, however tidy it looks.** OANDA's source is built
/// around a token PROVIDER, so it cannot be a `&'static` unit struct, and it must not be: a row of
/// this registry is answered on the keyless `Fetch` (no credential spent by design), where a client would
/// make this daemon spend the operator's token — 0062's decision 3 and 0097's verdict 3 broken at once. It is
/// `crate::rest::oanda`'s, behind `FetchCredentialed` (Write scope) only.
pub static KLINE_SOURCES: &[&'static dyn KlineSource] = &[
    &vike_binance::data::BinanceKlines,
    &vike_bybit::data::BybitKlines,
    &vike_okx::data::OkxKlines,
    &vike_aster::data::AsterKlines,
    &vike_deribit::data::DeribitKlines,
    &vike_hyperliquid::feeds::history::HyperliquidKlines,
    &vike_fxcm::data::FxcmKlines,
];

/// **Every funding-rate source the data plane serves** — the ONE funding roster. It replaced
/// `vike-backfill`'s `funding_rate::source_by_name` (docs/decisions/0094).
pub static FUNDING_SOURCES: &[&'static dyn FundingRateSource] =
    &[&vike_binance::data::BinanceFunding, &vike_hyperliquid::feeds::funding::HyperliquidFunding];

/// The venues of a kline registry, in its order — the lane's advertisement.
pub fn kline_venues(sources: &[&dyn KlineSource]) -> Vec<String> {
    sources.iter().map(|s| s.venue().to_string()).collect()
}

/// The venues of a funding registry, in its order.
pub fn funding_venues(sources: &[&dyn FundingRateSource]) -> Vec<String> {
    sources.iter().map(|s| s.venue().to_string()).collect()
}

/// The refusal for a venue a registry does not carry: nothing was asked of any venue, and retrying
/// cannot help. The server refuses an unserved venue before the lane runs
/// (`crate::server::fetch::pre_check`); this is the lane's own answer for a caller that did not.
fn unserved(lane: &str, venue: &str, served: &[String]) -> FetchFailure {
    FetchFailure::Refused(format!(
        "gatewayhub: `{venue}` is not served by the {lane} lane (served: [{}]); nothing was asked \
         of any venue",
        served.join(", ")
    ))
}

/// `Fetch { Klines }`: the named venue's source, called once. `SourceError::{Fetch, Refused}` keep
/// their variant and their inner text byte for byte (`FetchFailure::from_source_error`).
pub fn fetch_klines(
    sources: &[&dyn KlineSource],
    venue: &str,
    symbol: &str,
    interval: &str,
    start_ms: i64,
    end_ms: i64,
    ctx: &FetchCtx,
) -> Result<Vec<Bar>, FetchFailure> {
    let Some(source) = sources.iter().copied().find(|s| s.venue() == venue) else {
        return Err(unserved("klines", venue, &kline_venues(sources)));
    };
    if ctx.is_cancelled() {
        return Err(cancelled("before the venue was asked"));
    }
    source.fetch(symbol, interval, start_ms, end_ms).map_err(FetchFailure::from_source_error)
}

/// `Fetch { Funding }`: the named venue's funding source, called once; the rows cross as
/// `WireFundingPoint`s (a field-for-field mirror, bit exact).
pub fn fetch_funding(
    sources: &[&dyn FundingRateSource],
    venue: &str,
    symbol: &str,
    start_ms: i64,
    end_ms: i64,
    ctx: &FetchCtx,
) -> Result<Vec<WireFundingPoint>, FetchFailure> {
    let Some(source) = sources.iter().copied().find(|s| s.venue() == venue) else {
        return Err(unserved("funding", venue, &funding_venues(sources)));
    };
    if ctx.is_cancelled() {
        return Err(cancelled("before the venue was asked"));
    }
    source
        .fetch(symbol, start_ms, end_ms)
        .map(|points| points.into_iter().map(WireFundingPoint::from).collect())
        .map_err(FetchFailure::from_source_error)
}

#[cfg(test)]
#[path = "kline_sources_tests.rs"]
mod kline_sources_tests;
