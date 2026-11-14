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
//!
//! # Built once, at the start, from the venue's address rows
//!
//! A source dials the base its venue's `venue_url` rows name (decision 0265), so the registries are
//! tables of BUILDERS, `(venue, build)`, and [`kline_sources`] / [`funding_sources`] build them ONCE
//! from the boot's one read of the rows (`vike_secrets::venue_setting::load_venue_settings`). The
//! library reads no store. A row whose build is refused stays in the registry as an `Unbuilt`
//! source: the venue is still advertised in its place (the order is observable) and every fetch is
//! refused with the reason, which names the missing row.

use std::collections::BTreeMap;
use std::ops::Deref;

use vike_data::source::{FundingRateSource, KlineSource, SourceError};
use vike_marketdata::Bar;
use vike_marketdata::FundingRatePoint;
use vike_secrets::venue_setting::VenueSettings;
use vike_venues::venue_urls::{UrlRefusal, VenueUrls};

use super::{FetchContext, cancelled};
use crate::data::venues::venue_urls;

/// How one kline row is built from its venue's addresses: the bridge's `from_urls`, boxed.
pub type KlineBuild = fn(&VenueUrls) -> Result<Box<dyn KlineSource>, UrlRefusal>;

/// How one funding row is built from its venue's addresses.
pub type FundingBuild = fn(&VenueUrls) -> Result<Box<dyn FundingRateSource>, UrlRefusal>;

/// **Every kline source this daemon can dispatch**, as `(venue, build)` rows. The ONE roster: the
/// daemon's `facts` and its dispatch are both built FROM it ([`kline_sources`]), and the datahub's
/// table folds the client's copy of its order. Every row builds through its bridge's `from_urls`.
///
/// ⚠ **ORDER IS OBSERVABLE.** It is what the unknown-venue refusal prints (here
/// `crate::server::fetch::pre_check`'s, and in the datahub the table's `supported`) and what the
/// advertisement preserves, and `crates/vike-datahub/tests/backfill_serve/backfill_roundtrip.rs`'s
/// `the_real_table_names_every_kline_venue` pins the rendered order. This is the collectors' own
/// order — the three that shipped first, then the three 0059 Phase 2 dispatched.
///
/// ⚠ **Each row's builder names `vike_<venue>::<module>::<Type>`, and the gate DERIVES the registered
/// set from that spelling.** (It was `&crate::venues::<venue>::<Type>` in `vike-backfill` until
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
/// ⚠ **`fxcm` is the seventh row and the one that is not a venue API**: `KlineSourceFxcm` reads FXCM's public
/// candle ARCHIVE (anonymous GETs of weekly `.csv.gz` files, no ForexConnect, no session, no key), so it is a keyless
/// row like the others. It is a ONE-BATCH row, the whole window through one `fetch`, and it answers a week the archive
/// does not hold with an error, never an empty `Vec` (`crates/bridges/fxcm/src/history.rs`'s module doc).
///
/// ⚠ **A credentialed source is NOT a row here, however tidy it looks.** OANDA's source is built
/// around a token PROVIDER, and it must not be a row: a row of this registry is answered on the
/// keyless `Fetch` (no credential spent by design), where a client would make this daemon spend the
/// operator's token — 0062's decision 3 and 0097's verdict 3 broken at once. It is
/// `vike_oanda::credentialed_klines`' spec, served by `crate::rest::credentialed_klines` behind
/// `FetchCredentialed` (Write scope) only.
pub static KLINE_SOURCES: &[(&str, KlineBuild)] = &[
    ("binance", |u| Ok(Box::new(vike_binance::history::KlineSourceBinance::from_urls(u)?))),
    ("bybit", |u| Ok(Box::new(vike_bybit::history::KlineSourceBybit::from_urls(u)?))),
    ("okx", |u| Ok(Box::new(vike_okx::history::KlineSourceOkx::from_urls(u)?))),
    ("aster", |u| Ok(Box::new(vike_aster::history::KlineSourceAster::from_urls(u)?))),
    ("deribit", |u| Ok(Box::new(vike_deribit::history::KlineSourceDeribit::from_urls(u)?))),
    ("hyperliquid", |u| {
        Ok(Box::new(vike_hyperliquid::history::klines::KlineSourceHyperliquid::from_urls(u)?))
    }),
    ("fxcm", |u| Ok(Box::new(vike_fxcm::history::KlineSourceFxcm::from_urls(u)?))),
];

/// **Every funding-rate source the data plane serves** — the ONE funding roster, as `(venue, build)`
/// rows like [`KLINE_SOURCES`]. It replaced `vike-backfill`'s `funding_rate::source_by_name`
/// (docs/decisions/0094). The two that shipped first, then bybit, okx, deribit and aster appended
/// (keyless public reads, each in its own bridge's `data` module), so the order every refusal already
/// printed keeps its head. Every row builds through its bridge's `from_urls`.
pub static FUNDING_SOURCES: &[(&str, FundingBuild)] = &[
    ("binance", |u| Ok(Box::new(vike_binance::history::FundingRateSourceBinance::from_urls(u)?))),
    ("hyperliquid", |u| {
        Ok(Box::new(vike_hyperliquid::history::funding::FundingRateSourceHyperliquid::from_urls(
            u,
        )?))
    }),
    ("bybit", |u| Ok(Box::new(vike_bybit::history::FundingRateSourceBybit::from_urls(u)?))),
    ("okx", |u| Ok(Box::new(vike_okx::history::FundingRateSourceOkx::from_urls(u)?))),
    ("deribit", |u| Ok(Box::new(vike_deribit::history::FundingRateSourceDeribit::from_urls(u)?))),
    ("aster", |u| Ok(Box::new(vike_aster::history::FundingRateSourceAster::from_urls(u)?))),
];

/// **The kline registry, built** from `venue_settings` (the boot's one read of the rows), in
/// [`KLINE_SOURCES`]' order. A row whose build is refused is an `Unbuilt` source in its place.
pub fn kline_sources(
    venue_settings: &BTreeMap<String, VenueSettings>,
) -> Vec<Box<dyn KlineSource>> {
    KLINE_SOURCES
        .iter()
        .map(|&(venue, build)| {
            build(&venue_urls(venue_settings, venue)).unwrap_or_else(|why| {
                Box::new(Unbuilt::logged("klines", venue, &why)) as Box<dyn KlineSource>
            })
        })
        .collect()
}

/// **The funding registry, built**, as [`kline_sources`] builds the kline one.
pub fn funding_sources(
    venue_settings: &BTreeMap<String, VenueSettings>,
) -> Vec<Box<dyn FundingRateSource>> {
    FUNDING_SOURCES
        .iter()
        .map(|&(venue, build)| {
            build(&venue_urls(venue_settings, venue)).unwrap_or_else(|why| {
                Box::new(Unbuilt::logged("funding", venue, &why)) as Box<dyn FundingRateSource>
            })
        })
        .collect()
}

/// **A registry row whose addresses refused it** (`vike_venues::venue_urls::UrlRefusal`): it answers
/// for its venue, so the advertisement keeps its order, and refuses every fetch with the reason.
///
/// The fetch fails as `SourceError::Fetch`, never `Refused`: the datahub keeps a `Refused` as "this
/// window cannot be expressed", and a missing address row is cured by a row and a restart.
#[derive(Debug)]
struct Unbuilt {
    venue: &'static str,
    why: String,
}

impl Unbuilt {
    /// The stand-in for `venue`'s row of the `lane` registry, logged once at the build.
    fn logged(lane: &str, venue: &'static str, why: &UrlRefusal) -> Self {
        let why = format!("{venue} {lane} is NOT RUNNING: {why}");
        tracing::error!("vike-gatewayhub: {why}");
        Self { venue, why }
    }

    fn refusal(&self) -> SourceError {
        SourceError::Fetch(self.why.clone())
    }
}

impl KlineSource for Unbuilt {
    fn venue(&self) -> &str {
        self.venue
    }

    fn fetch(&self, _: &str, _: &str, _: i64, _: i64) -> Result<Vec<Bar>, SourceError> {
        Err(self.refusal())
    }
}

impl FundingRateSource for Unbuilt {
    fn venue(&self) -> &str {
        self.venue
    }

    fn fetch(&self, _: &str, _: i64, _: i64) -> Result<Vec<FundingRatePoint>, SourceError> {
        Err(self.refusal())
    }
}

/// The venues of a kline registry, in its order — the lane's advertisement.
pub fn kline_venues<'a, S: Deref<Target = dyn KlineSource + 'a>>(sources: &[S]) -> Vec<String> {
    sources.iter().map(|s| s.venue().to_string()).collect()
}

/// The venues of a funding registry, in its order.
pub fn funding_venues<'a, S: Deref<Target = dyn FundingRateSource + 'a>>(
    sources: &[S],
) -> Vec<String> {
    sources.iter().map(|s| s.venue().to_string()).collect()
}

/// The refusal for a venue a registry does not carry: nothing was asked of any venue, and retrying
/// cannot help. The server refuses an unserved venue before the lane runs
/// (`crate::server::fetch::pre_check`); this is the lane's own answer for a caller that did not.
fn unserved(lane: &str, venue: &str, served: &[String]) -> SourceError {
    SourceError::Refused(format!(
        "gatewayhub: `{venue}` is not served by the {lane} lane (served: [{}]); nothing was asked \
         of any venue",
        served.join(", ")
    ))
}

/// `Fetch { Klines }`: the named venue's source, called once. `SourceError::{Fetch, Refused}` keep
/// their variant and their inner text byte for byte: the source's own error crosses as it is.
pub fn fetch_klines<'a, S: Deref<Target = dyn KlineSource + 'a>>(
    sources: &[S],
    venue: &str,
    symbol: &str,
    interval: &str,
    start_ms: i64,
    end_ms: i64,
    ctx: &FetchContext,
) -> Result<Vec<Bar>, SourceError> {
    let Some(source) = sources.iter().find(|s| s.venue() == venue) else {
        return Err(unserved("klines", venue, &kline_venues(sources)));
    };
    if ctx.is_cancelled() {
        return Err(cancelled("before the venue was asked"));
    }
    source.fetch(symbol, interval, start_ms, end_ms)
}

/// `Fetch { Funding }`: the named venue's funding source, called once; the rows cross as the
/// source's own `FundingRatePoint`s (bit exact).
pub fn fetch_funding<'a, S: Deref<Target = dyn FundingRateSource + 'a>>(
    sources: &[S],
    venue: &str,
    symbol: &str,
    start_ms: i64,
    end_ms: i64,
    ctx: &FetchContext,
) -> Result<Vec<FundingRatePoint>, SourceError> {
    let Some(source) = sources.iter().find(|s| s.venue() == venue) else {
        return Err(unserved("funding", venue, &funding_venues(sources)));
    };
    if ctx.is_cancelled() {
        return Err(cancelled("before the venue was asked"));
    }
    source.fetch(symbol, start_ms, end_ms)
}

#[cfg(test)]
#[path = "kline_sources_tests.rs"]
mod kline_sources_tests;
