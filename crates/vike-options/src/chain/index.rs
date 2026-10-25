//! The Deribit **option index**: an instrument NAME mapped to the contract the venue's own
//! instrument payload defines — what the streamed-row folds ([`crate::chain::fold`]) and the
//! Greeks join read instead of parsing the name.
//!
//! # Why it exists
//!
//! The streamed rows (`vike_marketdata::OptionTicker`, as `vike_deribit::options_feed`'s
//! `parse_markprice_options` and `parse_ticker` fill it: its `symbol` is the venue's bare name)
//! and a held position carry only the instrument NAME, and Deribit's ticker channel has no strike
//! field. Identity lives on the payload instead
//! (`docs/decisions/0126-every-instrument-class-is-a-typed-contract-in-vike-model-and-feed-values-live-in-vike-marketdata.md`):
//! `crates/bridges/deribit/src/catalog.rs`'s `contract_of` reads right, strike, expiry, margining
//! and settlement from each `public/get_instruments` row. This index is that contract, keyed by
//! name, built by [`OptionIndex::from_instruments`] from whatever instrument list the caller holds
//! (a catalog listing, or the venue's own `get_instruments` rows): this crate fetches nothing.
//!
//! # What a lookup answers
//!
//! * [`OptionIndex::address`] — an OPTION's place on the grid: its underlying (the payload's
//!   `base_currency`, the key an [`UnderlyingChains`] map uses), its expiry as the grid's ISO
//!   date (the UTC calendar date of `expiry_ms`; Deribit expires at 08:00 UTC, so it is the date in
//!   the name), strike, right, and whether its premium is ALREADY USD ([`premium_is_usd`]).
//! * [`OptionIndex::delta_one_underlying`] — a perpetual's or dated future's underlying, for the
//!   Greeks tool's linear hedge legs.
//!
//! ⚠ **A name the index does not hold is SKIPPED, never parsed.** A fold leaves the grid
//! untouched, and the Greeks tool lists the position as unpriced. There is no name-grammar
//! fallback, on purpose (owner, 2026-10-11: "no fallbacks, delete the legacy path"). The gap is
//! for the HOLDER to log, once per refresh decision ([`index_refresh_due`] says when,
//! [`OptionIndex::missing_from`] says what), never per streamed row: the folds run on 100 ms
//! frames. This module logs nothing and fetches nothing.
//!
//! The index is MAINNET's (the same host as the chains it joins onto); Deribit order entry is
//! TESTNET (`crates/bridges/deribit/CLAUDE.md`), so a held position on an instrument only testnet
//! lists is not in the index and reads as unpriced.

use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use chrono::Datelike;
use vike_marketdata::{OptionKind, UnderlyingChains};
use vike_model::{Contract, Instrument, OptionSpec};

/// The venue whose instruments this index holds; a row of any other venue is ignored.
const VENUE: &str = "deribit";

/// While the index is EMPTY (never fetched, or every fetch failed), how long between attempts.
const INDEX_RETRY: Duration = Duration::from_secs(60);

/// While the index is populated but the grid holds a name it lacks (a new listing), the shortest
/// time between refetches. Ten minutes: a refetch is ~4.6 MB of option rows (MEASURED 2026-10-04,
/// `crates/bridges/deribit/src/catalog.rs`'s module doc), and a listing the grid shows a few
/// minutes before its streamed rows apply is a cost the 12 s REST poll already covers.
const INDEX_REFRESH: Duration = Duration::from_secs(600);

/// Name → the contract its payload defines. `Default` is the empty index (every lookup misses).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct OptionIndex {
    /// Every option, by verbatim venue name.
    options: HashMap<String, OptionSpec>,
    /// Every perpetual and dated future, by name, to its underlying (the row's `base_currency`).
    delta_one: HashMap<String, String>,
}

/// One option's place on the Options grid, from [`OptionIndex::address`].
#[derive(Clone, Debug, PartialEq)]
pub struct OptionAddress<'a> {
    /// The [`UnderlyingChains`] map key (`"BTC"`, `"ETH"`, `"SOL"`).
    pub underlying: &'a str,
    /// The grid's expiry key: the UTC date of the contract's expiry instant, `YYYY-MM-DD`.
    pub expiry_iso: String,
    pub strike: f64,
    pub right: OptionKind,
    /// The premium is already USD (pass through) rather than coin units (scale by spot).
    pub usd_quoted: bool,
}

impl OptionIndex {
    /// Index the deribit options and futures among `instruments` (any other venue or class is
    /// ignored). A later row with a name already seen replaces the earlier one.
    pub fn from_instruments<'a>(instruments: impl IntoIterator<Item = &'a Instrument>) -> Self {
        let mut index = Self::default();
        for inst in instruments {
            if inst.venue != VENUE {
                continue;
            }
            match &inst.contract {
                Contract::Option(spec) => {
                    index.options.insert(inst.raw_symbol.clone(), spec.clone());
                }
                Contract::Perp(_) | Contract::Future(_) if !inst.base.is_empty() => {
                    index.delta_one.insert(inst.raw_symbol.clone(), inst.base.clone());
                }
                _ => {}
            }
        }
        index
    }

    /// The option contract named `name`, if indexed.
    pub fn option(&self, name: &str) -> Option<&OptionSpec> {
        self.options.get(name)
    }

    /// The underlying of the perpetual or dated future named `name`, if indexed.
    pub fn delta_one_underlying(&self, name: &str) -> Option<&str> {
        self.delta_one.get(name).map(String::as_str)
    }

    /// How many instruments the index holds (options plus futures).
    pub fn len(&self) -> usize {
        self.options.len() + self.delta_one.len()
    }

    /// Whether the index holds nothing — every lookup misses.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Where the option named `name` sits on the grid. `None` when the name is not an indexed
    /// option, or its contract names no underlying or carries an expiry outside the calendar.
    pub fn address(&self, name: &str) -> Option<OptionAddress<'_>> {
        let spec = self.options.get(name)?;
        Some(OptionAddress {
            underlying: spec.underlying.as_deref().filter(|u| !u.is_empty())?,
            expiry_iso: expiry_date_iso(spec.expiry_ms)?,
            strike: spec.strike,
            right: spec.right,
            usd_quoted: premium_is_usd(spec),
        })
    }

    /// The grid's quote names this index does not hold: their count and the first one (for the
    /// log line). `None` when every name is indexed.
    pub fn missing_from(
        &self,
        by_underlying: &BTreeMap<String, UnderlyingChains>,
    ) -> Option<(usize, String)> {
        let mut missing = by_underlying
            .values()
            .flat_map(|bundle| bundle.chains.values())
            .flat_map(|chain| chain.rows.iter())
            .flat_map(|row| [row.call.as_ref(), row.put.as_ref()])
            .flatten()
            .filter_map(|q| q.instrument_name.as_deref())
            .filter(|name| !self.options.contains_key(*name));
        let first = missing.next()?.to_string();
        Some((1 + missing.count(), first))
    }
}

/// Whether an option's premium arrives ALREADY in USD. On Deribit a premium is quoted in the
/// option's SETTLEMENT currency: a USDC-settled (linear) option quotes USDC, which the grid shows
/// as USD unscaled; a coin-settled (inverse) one quotes coin, which the fold scales by the chain
/// spot. So the payload's `settlement_currency` decides it, never the name or the underlying.
pub fn premium_is_usd(spec: &OptionSpec) -> bool {
    matches!(spec.settle.as_str(), "USD" | "USDC" | "USDT")
}

/// The UTC calendar date of an epoch-ms instant, `YYYY-MM-DD` — the Options grid's expiry key.
fn expiry_date_iso(expiry_ms: i64) -> Option<String> {
    let date = chrono::DateTime::from_timestamp_millis(expiry_ms)?.date_naive();
    Some(format!("{:04}-{:02}-{:02}", date.year(), date.month(), date.day()))
}

/// Whether the index's holder should refetch it now. `since_last_attempt` is the time since the
/// last fetch attempt (`None`: never tried). An empty index retries every `INDEX_RETRY` (60 s); a
/// populated one refetches only when the grid holds a name it lacks, at most every
/// `INDEX_REFRESH` (600 s).
///
/// PURE: it decides and logs nothing (this crate has no `tracing`). A holder that wants the gap
/// in its log calls [`OptionIndex::missing_from`] when this returns `true` for a NON-empty index
/// and logs the `(count, first)` it answers, once per decision, never per streamed row.
pub fn index_refresh_due(
    index: &OptionIndex,
    by_underlying: &BTreeMap<String, UnderlyingChains>,
    since_last_attempt: Option<Duration>,
) -> bool {
    let Some(elapsed) = since_last_attempt else { return true };
    if index.is_empty() {
        return elapsed >= INDEX_RETRY;
    }
    if elapsed < INDEX_REFRESH {
        return false;
    }
    index.missing_from(by_underlying).is_some()
}

/// A deribit option [`Instrument`] for tests, built the way the catalog builds one: the margining
/// follows the settlement (coin-settled = inverse, else linear), the expiry is 08:00 UTC of
/// `expiry_iso`.
#[cfg(test)]
pub(crate) fn test_option(
    name: &str,
    underlying: &str,
    expiry_iso: &str,
    strike: f64,
    right: OptionKind,
    settle: &str,
) -> Instrument {
    let margining = if settle == underlying {
        vike_model::Margining::Inverse
    } else {
        vike_model::Margining::Linear
    };
    Instrument::new(
        VENUE,
        name,
        underlying,
        settle,
        Contract::Option(OptionSpec {
            right,
            strike,
            expiry_ms: crate::expiry_ms(expiry_iso),
            style: vike_model::ExerciseStyle::European,
            margining,
            settle: settle.to_string(),
            value_ccy: Some(underlying.to_string()),
            underlying: Some(underlying.to_string()),
        }),
    )
}

/// A deribit perpetual or dated future [`Instrument`] for tests (`base` is its underlying).
#[cfg(test)]
pub(crate) fn test_delta_one(name: &str, base: &str) -> Instrument {
    Instrument::new(
        VENUE,
        name,
        base,
        "USD",
        Contract::Perp(vike_model::PerpSpec {
            margining: vike_model::Margining::Inverse,
            settle: base.to_string(),
            value_ccy: Some("USD".to_string()),
            funding_interval_ms: None,
            settling_ms: None,
        }),
    )
}

#[cfg(test)]
mod tests;
