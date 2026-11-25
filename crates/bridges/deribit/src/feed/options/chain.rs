//! Deribit crypto-options chain provider (public REST, no API key) — ports
//! `vike-trader-app data/options/deribit.py`. This is the READ side (chain snapshots for the
//! options grid); order entry stays on the `VenueRestDeribit` / `ExecutionClient` path.
//!
//! One `get_book_summary_by_currency` call returns the whole chain for a currency
//! (bid/ask/mark/mark_iv/OI/volume/underlying_price). Pure parse helpers take the raw
//! `result` rows and are unit-tested with captured payloads (the venue-mapper anatomy); the
//! HTTP method is a thin shell with a short cache so `list_expiries` + `fetch_chain` share a
//! single request. The chain snapshot types (`OptionChain`, `OptionChainExpiry`, `OptionChainStrike`, `OptionQuote`,
//! `OptionKind`) are `vike-marketdata`'s, the venue-free feed vocabulary (decision 0126); the
//! pricing (`enrich_quote`) and the calendar constructor (`make_expiry`) are
//! `vike-options`' (the layering twin of `options/deribit.py` importing `options/{model,greeks}`).
//! The opt-in `kind=chain` RECORDING of a fetched chain is not here: the bridge hands back the chain
//! and the daemon that owns the store records it (`crates/vike-datahub/src/options/gateway.rs`'s
//! `record_chain`).
//!
//! Contract notes:
//! - ⚠ **`parse_instrument_name` reads identity from the NAME, and here that is the only source.**
//!   Everywhere else this crate takes strike, right and expiry from the instrument payload
//!   (`crates/bridges/deribit/src/catalog.rs`'s `contract_of`), but a
//!   `get_book_summary_by_currency` row carries `instrument_name`, prices, IV, OI and volume and
//!   NO `strike`, `option_type` or `expiration_timestamp` — so the name is all there is to group
//!   by. Replacing it would mean a second request (`public/get_instruments`) joined by name per
//!   chain fetch; until that is wanted, this parser stays.
//! - It accepts BOTH shapes: legacy coin-settled "BTC-27JUN26-100000-C" and USDC-margined
//!   "SOL_USDC-26JUN26-90-P" (the `_USD[CT]` settlement suffix is dropped so the base normalizes to
//!   "SOL"). It is deliberately LOOSER than `execution/rest.rs`'s exec-side `option_base_asset` (decimal
//!   strikes, settlement suffix), which is now only that parser's fallback for a row publishing no
//!   `base_currency`.
//! - Coin-settled premiums arrive in COIN units → scaled to USD by the row's underlying price
//!   (fallback: chain spot); the USDC book already quotes USD → passed through unscaled. This path
//!   decides the unit per BOOK (`OptionUnderlying::usd_quoted`, the book the venue's own listing
//!   names: `crate::option_listing`); the live `markprice`/`ticker` folds in
//!   `vike-options` decide it per OPTION, from the settlement currency the instrument payload
//!   carries (`crates/vike-options/src/chain/index.rs`'s `OptionIndex`). The two agree
//!   because the USDC book holds exactly the USDC-settled options; a streamed and a re-polled
//!   premium disagreeing means they stopped agreeing.
//! - `r` (risk-free) is threaded explicitly (the Python twin uses its import-time env default;
//!   pass 0.0 to match — see vike-options `greeks`).
//! - `instrument_name` is carried VERBATIM on every quote (the `_USDC` suffix is not
//!   reconstructable from parsed fields) so the arm/trade path can use the exact venue id.

use std::collections::{BTreeSet, HashMap};

use indexmap::IndexMap;
use serde_json::Value;

use vike_marketdata::{
    OptionChain, OptionChainExpiry, OptionChainStrike, OptionKind, OptionQuote,
    OptionUnderlyingKind,
};
use vike_options::{enrich_quote, limit_strikes, make_expiry, years_to_expiry};

use crate::option_listing::{
    LISTING_MIN_GAP_MS, LISTING_TTL_MS, OptionUnderlying, option_underlyings,
};
use crate::refusals::DeribitOptionsRefusal;

/// The keyless book-summary method's path, below the public REST base (the `live` `data` rest
/// row: mainnet, whatever the account's tier).
const PATH_BOOK_SUMMARY: &str = "/api/v2/public/get_book_summary_by_currency";
const CACHE_TTL_MS: i64 = 5_000;

/// `"BTC-27JUN26-100000-C"` → `("BTC", "2026-06-27", 100000.0, Call)`; `None` if not an
/// option. Twin of `parse_instrument_name` (regex
/// `^([A-Z]+)(?:_USD[CT])?-(\d{1,2})([A-Z]{3})(\d{2})-(\d+(?:[.d]\d+)?)-([CP])$`, hand-rolled —
/// no regex dep; the expiry is `vike_venues::deribit_expiry`'s, the one reading of `DDMMMYY`), including the
/// reject-non-month-token gate. Name-derived ON PURPOSE: its input,
/// a book-summary row, publishes no strike, right or expiry field (module doc).
///
/// A strike with a fraction is read in the venue's own form, a lower-case `d` as the decimal mark
/// (`XRP_USDC-26SEP25-1d6-P` is a 1.6 strike: every linear altcoin book writes it so, measured on the
/// venue's trade log 2026-11-16), as well as with a `.`. The ONE reading of an option name: the live chain
/// (this file) and the trade log (`crate::option_trades`) both ask it.
pub fn parse_instrument_name(name: &str) -> Option<(String, String, f64, OptionKind)> {
    let parts: Vec<&str> = name.split('-').collect();
    if parts.len() != 4 {
        return None;
    }
    let (head, expiry, strike_s, cp) = (parts[0], parts[1], parts[2], parts[3]);
    // base: [A-Z]+ with an optional _USDC/_USDT settlement suffix (captured and discarded)
    let base = head.strip_suffix("_USDC").or_else(|| head.strip_suffix("_USDT")).unwrap_or(head);
    if base.is_empty() || !base.bytes().all(|b| b.is_ascii_uppercase()) {
        return None;
    }
    // expiry: \d{1,2} [A-Z]{3} \d{2}, a real month and a day the calendar has (a name it does not have,
    // "31FEB26", "00JAN26", would panic `make_expiry` downstream)
    let (year, month, day) = vike_venues::deribit_expiry(expiry)?;
    // strike: \d+([.d]\d+)? (shape-checked — f64::parse alone would admit "1e5"/"inf"); the venue's
    // decimal mark is a lower-case `d`
    let (int_p, fraction_p) = match strike_s.find(['.', 'd']) {
        Some(i) => (&strike_s[..i], Some(&strike_s[i + 1..])),
        None => (strike_s, None),
    };
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if !digits(int_p) || !fraction_p.is_none_or(digits) {
        return None;
    }
    let strike: f64 = match fraction_p {
        Some(fraction) => format!("{int_p}.{fraction}").parse().ok()?,
        None => int_p.parse().ok()?,
    };
    let kind = OptionKind::from_cp(cp)?;
    Some((base.to_string(), format!("{year}-{month:02}-{day:02}"), strike, kind))
}

/// Distinct option expiries present in a book-summary payload, ascending — twin of
/// `list_expiries_from_summary`. When `currency` is given (e.g. "SOL"), only that base coin's
/// instruments count — the shared USDC book mixes several coins, so without this filter SOL's
/// expiries would pick up BTC/ETH/XRP dates.
pub fn list_expiries_from_summary(
    rows: &[Value],
    now_ms: i64,
    currency: Option<&str>,
) -> Vec<OptionChainExpiry> {
    let mut dates: BTreeSet<String> = BTreeSet::new(); // ISO strings: lexicographic == ascending
    for r in rows {
        let name = r.get("instrument_name").and_then(|v| v.as_str()).unwrap_or("");
        if let Some((base, date_iso, _, _)) = parse_instrument_name(name)
            && (currency.is_none() || currency == Some(base.as_str()))
        {
            dates.insert(date_iso);
        }
    }
    dates.into_iter().map(|d| make_expiry(&d, now_ms)).collect()
}

/// Deribit quotes option premiums in coin units (fractions of BTC/ETH/SOL); scale to USD by
/// the underlying price so they line up with the USD Theor/Strike/Distance columns. Twin of
/// `_usd` — a falsy scale (0.0, "no underlying to scale by") yields absent, never zero.
fn usd(v: Option<f64>, scale: f64) -> Option<f64> {
    if scale == 0.0 {
        return None;
    }
    v.map(|x| x * scale)
}

/// Group a book-summary payload into an [`OptionChain`] for one expiry, greeks enriched —
/// twin of `build_chain_from_summary`.
///
/// `usd_quoted` selects the premium unit convention: the coin-settled BTC/ETH books quote
/// premiums in COIN units (scaled to USD by the underlying price), while the USDC-margined
/// altcoin book (SOL etc.) already quotes premiums in USD — those pass through unscaled.
pub fn build_chain_from_summary(
    currency: &str,
    rows: &[Value],
    expiry_iso: &str,
    now_ms: i64,
    usd_quoted: bool,
    r: f64,
) -> OptionChain {
    let t = years_to_expiry(expiry_iso, now_ms);
    let mut spot: Option<f64> = None;
    // Python: insertion-ordered dict keyed by strike, `sorted(items)` at the end — mirrored as
    // IndexMap + final sort (strikes are finite-positive; total_cmp == numeric order).
    let mut by_strike: IndexMap<u64, OptionChainStrike> = IndexMap::new();
    for row in rows {
        let name = row.get("instrument_name").and_then(|v| v.as_str()).unwrap_or("");
        let Some((base, e_iso, strike, kind)) = parse_instrument_name(name) else { continue };
        // The USDC book mixes coins (BTC_USDC/SOL_USDC/XRP_USDC); keep only the requested coin.
        if base != currency || e_iso != expiry_iso {
            continue;
        }
        let f = |k: &str| row.get(k).and_then(|v| v.as_f64());
        if spot.is_none() {
            spot = f("underlying_price"); // first-seen row sets the chain spot
        }
        // Coin-settled premiums arrive in coin units -> scale to USD by this row's underlying
        // price (fall back to the chain spot). USDC-margined premiums are already USD -> 1.0.
        // Greeks/Theor come from IV+spot, so this only affects the dollar columns.
        let scale =
            if usd_quoted { 1.0 } else { f("underlying_price").unwrap_or(spot.unwrap_or(0.0)) };
        let q = OptionQuote {
            bid: usd(f("bid_price"), scale),
            ask: usd(f("ask_price"), scale),
            mark: usd(f("mark_price"), scale),
            iv: f("mark_iv").map(super::iv_from_percent),
            open_interest: f("open_interest"),
            volume: f("volume"),
            instrument_name: Some(name.to_string()),
            ..OptionQuote::new(strike, kind)
        };
        let q = enrich_quote(q, spot, t, r);
        let slot = by_strike.entry(strike.to_bits()).or_insert(OptionChainStrike {
            strike,
            call: None,
            put: None,
        });
        match kind {
            OptionKind::Call => slot.call = Some(q),
            OptionKind::Put => slot.put = Some(q),
        }
    }
    let mut chain_rows: Vec<OptionChainStrike> = by_strike.into_values().collect();
    chain_rows.sort_by(|a, b| a.strike.total_cmp(&b.strike));
    OptionChain {
        underlying: currency.to_string(),
        underlying_kind: OptionUnderlyingKind::Crypto,
        underlying_price: spot,
        expiry: make_expiry(expiry_iso, now_ms),
        asof_ms: now_ms,
        source: "deribit".to_string(),
        rows: chain_rows,
    }
}

use vike_model::now_ms;

/// The read the option listing is parsed from: one `public/get_instruments` answer, whole.
type InstrumentsRead = Box<dyn Fn() -> Result<Value, String> + Send>;

/// The listing a provider holds: when it was read, and what it lists.
struct Listing {
    read_ms: i64,
    underlyings: Vec<OptionUnderlying>,
}

/// Options provider for the options Deribit LISTS (name "deribit", asset class crypto): which
/// underlyings, their books and their indexes are the venue's own instrument rows
/// (`crate::option_listing`), held for `LISTING_TTL_MS`. Keyless public REST; a 5s cache means
/// `list_expiries` + N×`fetch_chain` in one refresh pass share a single HTTP call.
pub struct DeribitOptionsProvider {
    agent: ureq::Agent,
    /// The book-summary endpoint: the mainnet `live` `data` row plus [`PATH_BOOK_SUMMARY`] always,
    /// except under `with_book_summary_url` (`test-support` only: a test standing a loopback server
    /// where Deribit goes).
    summary_url: String,
    cache: HashMap<String, (i64, Vec<Value>)>, // book -> (fetched_ms, result rows)
    /// `public/get_instruments?currency=any&kind=option` on the same `live` `data` row, the read the
    /// listing is parsed from.
    instruments: InstrumentsRead,
    /// The last listing read that answered, kept through a failed read.
    listing: Option<Listing>,
    /// When the listing was last ASKED for, answered or not: no read sooner than `LISTING_MIN_GAP_MS`
    /// after it.
    listing_asked_ms: Option<i64>,
    /// Why the last listing read failed, cleared by one that answers.
    listing_error: Option<DeribitOptionsRefusal>,
}

impl DeribitOptionsProvider {
    /// The provider on deribit's address rows `urls`: the public REST base, mainnet's whatever any
    /// account's tier (deribit's option prices are the real book's).
    ///
    /// # Errors
    /// The row's refusal.
    pub fn from_urls(
        urls: &vike_venues::venue_urls::VenueUrls,
    ) -> Result<Self, vike_venues::venue_urls::UrlRefusal> {
        use vike_venues::venue_urls::{UrlChannel, UrlProtocol, UrlType};
        let base = urls.url(UrlType::Live, UrlChannel::Data, UrlProtocol::Rest, "")?;
        let instruments = crate::catalog::instruments_url(base, "any", "option");
        Ok(Self::at(&format!("{base}{PATH_BOOK_SUMMARY}"), json_read(instruments)))
    }

    /// The provider reading its book summary at `summary_url` and its listing through `instruments`.
    fn at(summary_url: &str, instruments: InstrumentsRead) -> Self {
        // A keyless public GET: it follows redirects ON PURPOSE and to the shared stated limit (ureq's
        // own default of ten was never a decision), unlike the venue's signed calls, which follow none
        // (`vike_transport::http`'s module doc). The bounded public-GET agent rather than the shared public one,
        // only because a 429 or 5xx is an `Err` here (`StatusPolicy::Error`).
        use vike_transport::http::{CallBound, StatusPolicy};
        let agent = vike_transport::http::public_get_agent_bounded(
            None,
            CallBound::Total(std::time::Duration::from_secs(10)),
            StatusPolicy::Error,
        );
        Self {
            agent,
            summary_url: summary_url.to_string(),
            cache: HashMap::new(),
            instruments,
            listing: None,
            listing_asked_ms: None,
            listing_error: None,
        }
    }

    /// Point the book-summary read at `url` instead of the mainnet row's (the query string is
    /// appended as for it). ⚠ TEST-ONLY, and not by convention: it exists only in this crate's own
    /// tests and under its `test-support` feature, which only a dev edge turns on
    /// (`crates/vike-ops/tests/architecture/test_surface_gate.rs` refuses a shipped one), so no
    /// shipped binary contains it and `summary_url` is the `live` `data` row's in every production
    /// provider. It is how `crates/bridges/deribit/src/options_tests.rs` and
    /// `crates/vike-gatewayhub/tests/lanes/rest_lane_surfaces.rs` drive the real options lane against
    /// canned payloads (`crate::options_fixture`).
    ///
    /// ⚠ It also UNSETS the listing read, so a test that moved the book can never read the venue's
    /// real listing: point that at the stand-in AFTER this, with [`Self::with_instruments_url`], or
    /// every listing answers an error.
    #[cfg(any(test, feature = "test-support"))]
    pub fn with_book_summary_url(mut self, url: impl Into<String>) -> Self {
        self.summary_url = url.into();
        self.instruments = Box::new(|| {
            Err("deribit option instruments: this test provider has no listing read \
                 (`with_instruments_url`)"
                .to_string())
        });
        self
    }

    /// Point the listing read at `url` (a whole `public/get_instruments` answer). TEST-ONLY, under
    /// `test-support`, as [`Self::with_book_summary_url`] is.
    #[cfg(any(test, feature = "test-support"))]
    pub fn with_instruments_url(mut self, url: impl Into<String>) -> Self {
        self.instruments = json_read(url.into());
        self
    }

    /// The underlyings the venue lists options on, with their books and indexes, in the listing's order:
    /// the held listing, read again once it is `LISTING_TTL_MS` old. A read that fails keeps the
    /// listing held (an `Err` only while none ever answered); see [`Self::refresh_underlyings`] to read
    /// it now.
    pub fn list_underlyings(&mut self) -> Result<Vec<OptionUnderlying>, DeribitOptionsRefusal> {
        self.listing_at(now_ms(), false).map(<[OptionUnderlying]>::to_vec)
    }

    /// [`Self::list_underlyings`] read again NOW, unless a read was asked for within
    /// `LISTING_MIN_GAP_MS`, which answers the listing held. A failed read keeps the listing held and
    /// is an `Err`, so the asker learns the refresh did not happen.
    pub fn refresh_underlyings(&mut self) -> Result<Vec<OptionUnderlying>, DeribitOptionsRefusal> {
        let now = now_ms();
        let asked = self.listing_asked_ms;
        let listed = self.listing_at(now, true).map(<[OptionUnderlying]>::to_vec)?;
        match (&self.listing_error, asked != self.listing_asked_ms) {
            (Some(why), true) => Err(why.clone()),
            _ => Ok(listed),
        }
    }

    /// The listing held, without a read: `None` before one answered.
    pub fn listed(&self) -> Option<&[OptionUnderlying]> {
        self.listing.as_ref().map(|l| l.underlyings.as_slice())
    }

    /// The listing at `now`, read first when none is held, when it is stale, or when `force` asks, and
    /// no read was asked for within `LISTING_MIN_GAP_MS`.
    fn listing_at(
        &mut self,
        now: i64,
        force: bool,
    ) -> Result<&[OptionUnderlying], DeribitOptionsRefusal> {
        let stale =
            self.listing.as_ref().is_none_or(|l| force || now - l.read_ms >= LISTING_TTL_MS);
        let may_ask = self.listing_asked_ms.is_none_or(|at| now - at >= LISTING_MIN_GAP_MS);
        if stale && may_ask {
            self.listing_asked_ms = Some(now);
            match self.read_listing() {
                Ok(underlyings) => {
                    self.listing = Some(Listing { read_ms: now, underlyings });
                    self.listing_error = None;
                }
                Err(why) => {
                    if self.listing.is_some() {
                        tracing::warn!(
                            target: "vike_deribit::feed::options::chain",
                            "{why}; the option listing read earlier is kept"
                        );
                    }
                    self.listing_error = Some(why);
                }
            }
        }
        match (&self.listing, &self.listing_error) {
            (Some(l), _) => Ok(&l.underlyings),
            (None, Some(why)) => Err(why.clone()),
            (None, None) => Err(DeribitOptionsRefusal::NoListingYet),
        }
    }

    /// One listing read: the instruments answer through [`listing_of`].
    fn read_listing(&self) -> Result<Vec<OptionUnderlying>, DeribitOptionsRefusal> {
        listing_of(&(self.instruments)().map_err(|why| DeribitOptionsRefusal::ListingRead { why })?)
    }

    /// `underlying`'s listing entry, or the refusal naming what the venue lists.
    fn listed_underlying(
        &mut self,
        underlying: &str,
        now: i64,
    ) -> Result<OptionUnderlying, DeribitOptionsRefusal> {
        let listed = self.listing_at(now, false)?;
        listed.iter().find(|u| u.name == underlying).cloned().ok_or_else(|| {
            let names: Vec<&str> = listed.iter().map(|u| u.name.as_str()).collect();
            DeribitOptionsRefusal::NotListed {
                underlying: underlying.to_string(),
                listed: names.join(", "),
            }
        })
    }

    /// The (cached) raw `result` rows of one book.
    fn summary(&mut self, book: &str, now_ms: i64) -> Result<&[Value], DeribitOptionsRefusal> {
        let book = book.to_string();
        let fresh = self.cache.get(&book).is_some_and(|(ts, _)| now_ms - ts < CACHE_TTL_MS);
        if !fresh {
            let url = format!("{}?currency={book}&kind=option", self.summary_url);
            let mut response =
                self.agent.get(&url).call().map_err(|e| {
                    DeribitOptionsRefusal::BookSummaryUnasked { why: e.to_string() }
                })?;
            let body = response
                .body_mut()
                .read_to_string()
                .map_err(|e| DeribitOptionsRefusal::BookSummaryBody { why: e.to_string() })?;
            let v: Value = serde_json::from_str(&body)
                .map_err(|e| DeribitOptionsRefusal::BookSummaryJson { why: e.to_string() })?;
            let rows = v.get("result").and_then(|r| r.as_array()).cloned().unwrap_or_default();
            self.cache.insert(book.clone(), (now_ms, rows));
        }
        Ok(self.cache.get(&book).map(|(_, rows)| rows.as_slice()).unwrap_or(&[]))
    }

    /// Distinct expiries for an underlying the venue lists, ascending.
    pub fn list_expiries(
        &mut self,
        underlying: &str,
    ) -> Result<Vec<OptionChainExpiry>, DeribitOptionsRefusal> {
        let now = now_ms();
        let listed = self.listed_underlying(underlying, now)?;
        let rows = self.summary(&listed.book, now)?;
        Ok(list_expiries_from_summary(rows, now, Some(underlying)))
    }

    /// One expiry's chain, greeks enriched, optionally windowed to ±`strikes` around spot.
    pub fn fetch_chain(
        &mut self,
        underlying: &str,
        expiry_iso: &str,
        strikes: Option<usize>,
        r: f64,
    ) -> Result<OptionChain, DeribitOptionsRefusal> {
        let now = now_ms();
        // A linear book (SOL's, every altcoin's: USDC) quotes premiums in USD already.
        let listed = self.listed_underlying(underlying, now)?;
        let rows = self.summary(&listed.book, now)?;
        let chain =
            build_chain_from_summary(underlying, rows, expiry_iso, now, listed.usd_quoted(), r);
        Ok(limit_strikes(chain, strikes))
    }
}

/// The production listing read: a keyless `GET` of `url` through the shared public agent, as the
/// catalog reads the same endpoint.
/// **The listing in one `public/get_instruments` answer** (`currency=any`, `kind=option`), through
/// `crate::option_listing`'s rule: the ONE reading the live provider and the trade log's chain names
/// (`crate::option_trades`) share. An answer that lists nothing is an error, never an empty listing.
pub(crate) fn listing_of(answer: &Value) -> Result<Vec<OptionUnderlying>, DeribitOptionsRefusal> {
    let rows = answer
        .get("result")
        .and_then(Value::as_array)
        .ok_or(DeribitOptionsRefusal::NoResultList)?;
    let listed = option_underlyings(rows);
    if listed.is_empty() {
        return Err(DeribitOptionsRefusal::NoUnderlying { rows: rows.len() });
    }
    Ok(listed)
}

fn json_read(url: String) -> InstrumentsRead {
    Box::new(move || {
        vike_transport::http::get_json(&url).map_err(|e| format!("deribit option instruments: {e}"))
    })
}

#[path = "chain_tests.rs"]
#[cfg(test)]
mod chain_tests;
