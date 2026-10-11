//! Deribit crypto-options chain provider (public REST, no API key) — ports
//! `vike-trader-app data/options/deribit.py`. This is the READ side (chain snapshots for the
//! options grid); order entry stays on the `DeribitRest` / `ExecutionClient` path.
//!
//! One `get_book_summary_by_currency` call returns the whole chain for a currency
//! (bid/ask/mark/mark_iv/OI/volume/underlying_price). Pure parse helpers take the raw
//! `result` rows and are unit-tested with captured payloads (the venue-mapper anatomy); the
//! HTTP method is a thin shell with a short cache so `list_expiries` + `fetch_chain` share a
//! single request. Venue-free pricing/model types come from `vike-options` (the layering twin
//! of `options/deribit.py` importing `options/{model,greeks}`).
//!
//! Contract notes:
//! - `parse_instrument_name` accepts BOTH shapes: legacy coin-settled "BTC-27JUN26-100000-C"
//!   and USDC-margined "SOL_USDC-26JUN26-90-P" (the `_USD[CT]` settlement suffix is dropped so
//!   the base normalizes to "SOL"). It is deliberately LOOSER than `client.rs`'s exec-side
//!   `option_base_asset` (decimal strikes, settlement suffix) — do not unify until the Phase-3
//!   `vike-deribit` crate holds both sites.
//! - Coin-settled premiums arrive in COIN units → scaled to USD by the row's underlying price
//!   (fallback: chain spot); the USDC book already quotes USD → passed through unscaled.
//! - `r` (risk-free) is threaded explicitly (the Python twin uses its import-time env default;
//!   pass 0.0 to match — see vike-options `greeks`).
//! - `instrument_name` is carried VERBATIM on every quote (the `_USDC` suffix is not
//!   reconstructable from parsed fields) so the arm/trade path can use the exact venue id.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use indexmap::IndexMap;
use serde_json::Value;

use vike_data::{ChainRecorder, ChainRow};
use vike_options::{
    Expiry, OptionChain, OptionKind, OptionQuote, StrikeRow, UnderlyingKind, enrich_quote,
    expiry_ms, limit_strikes, make_expiry, years_to_expiry,
};

const BASE: &str = "https://www.deribit.com/api/v2/public/get_book_summary_by_currency";
const CACHE_TTL_MS: i64 = 5_000;

/// Currency to query Deribit's book-summary with: BTC/ETH have their own coin-settled books;
/// the altcoins (SOL, and others as they're added) are listed only under the shared USDC book.
/// Twin of `_BOOK_CURRENCY` (`dict.get(u, u)` — unknown coins query their own name).
fn book_currency(underlying: &str) -> &str {
    match underlying {
        "SOL" => "USDC",
        other => other,
    }
}

/// Whether an underlying's option premiums arrive ALREADY USD-quoted (the shared USDC book — SOL
/// etc., `mark`/`bid`/`ask` passed through unscaled) vs coin-settled (BTC/ETH, premiums in coin
/// units that scale to USD by the spot). The single source of truth both the REST
/// [`build_chain_from_summary`] (via its `usd_quoted` flag) and the LIVE `markprice.options` fold key
/// off, so a streamed update and a re-poll scale a premium identically. Twin of
/// `book_currency(u) == "USDC"`.
pub fn is_usd_quoted(underlying: &str) -> bool {
    book_currency(underlying) == "USDC"
}

/// `"BTC-27JUN26-100000-C"` → `("BTC", "2026-06-27", 100000.0, Call)`; `None` if not an
/// option. Twin of `parse_instrument_name` (regex
/// `^([A-Z]+)(?:_USD[CT])?-(\d{1,2})([A-Z]{3})(\d{2})-(\d+(?:\.\d+)?)-([CP])$`, hand-rolled —
/// no regex dep), including the reject-non-month-token gate.
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
    // expiry: \d{1,2} [A-Z]{3} \d{2}
    if !expiry.is_ascii() || expiry.len() < 6 || expiry.len() > 7 {
        return None;
    }
    let (day_s, rest) = expiry.split_at(expiry.len() - 5);
    let (mon_s, yy_s) = rest.split_at(3);
    if !day_s.bytes().all(|b| b.is_ascii_digit()) || !yy_s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let month: u32 = match mon_s {
        "JAN" => 1,
        "FEB" => 2,
        "MAR" => 3,
        "APR" => 4,
        "MAY" => 5,
        "JUN" => 6,
        "JUL" => 7,
        "AUG" => 8,
        "SEP" => 9,
        "OCT" => 10,
        "NOV" => 11,
        "DEC" => 12,
        _ => return None, // any [A-Z]{3} shape reaches here; reject non-month tokens
    };
    let day: u32 = day_s.parse().ok()?;
    // a name the calendar does not have ("31FEB26", "00JAN26") would panic `make_expiry` downstream
    let year: i32 = 2000 + yy_s.parse::<i32>().ok()?;
    if !(1..=days_in_month(year, month)).contains(&day) {
        return None;
    }
    // strike: \d+(\.\d+)? (shape-checked — f64::parse alone would admit "1e5"/"inf")
    let (int_p, frac_p) = match strike_s.find('.') {
        Some(i) => (&strike_s[..i], Some(&strike_s[i + 1..])),
        None => (strike_s, None),
    };
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if !digits(int_p) || !frac_p.is_none_or(digits) {
        return None;
    }
    let strike: f64 = strike_s.parse().ok()?;
    let kind = OptionKind::from_cp(cp)?;
    Some((base.to_string(), format!("20{yy_s}-{month:02}-{day:02}"), strike, kind))
}

/// Days in a Gregorian month (`month` is 1..=12).
fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Distinct option expiries present in a book-summary payload, ascending — twin of
/// `list_expiries_from_summary`. When `currency` is given (e.g. "SOL"), only that base coin's
/// instruments count — the shared USDC book mixes several coins, so without this filter SOL's
/// expiries would pick up BTC/ETH/XRP dates.
pub fn list_expiries_from_summary(
    rows: &[Value],
    now_ms: i64,
    currency: Option<&str>,
) -> Vec<Expiry> {
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
    let mut by_strike: IndexMap<u64, StrikeRow> = IndexMap::new();
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
            iv: f("mark_iv").map(|iv| iv / 100.0), // mark_iv % -> decimal
            open_interest: f("open_interest"),
            volume: f("volume"),
            instrument_name: Some(name.to_string()),
            ..OptionQuote::new(strike, kind)
        };
        let q = enrich_quote(q, spot, t, r);
        let slot = by_strike.entry(strike.to_bits()).or_insert(StrikeRow {
            strike,
            call: None,
            put: None,
        });
        match kind {
            OptionKind::Call => slot.call = Some(q),
            OptionKind::Put => slot.put = Some(q),
        }
    }
    let mut chain_rows: Vec<StrikeRow> = by_strike.into_values().collect();
    chain_rows.sort_by(|a, b| a.strike.total_cmp(&b.strike));
    OptionChain {
        underlying: currency.to_string(),
        underlying_kind: UnderlyingKind::Crypto,
        underlying_price: spot,
        expiry: make_expiry(expiry_iso, now_ms),
        asof_ms: now_ms,
        source: "deribit".to_string(),
        rows: chain_rows,
    }
}

/// Flatten one enriched [`OptionChain`] (ONE expiry — the natural `fetch_chain` unit) into
/// `kind=chain` snapshot rows — the pure half of the opt-in chain-recording hook. Every row
/// carries the chain's `asof_ms` as `ts` (all rows of one snapshot share it), the 08:00-UTC
/// [`expiry_ms`] of the chain's expiry date, and the VERBATIM venue `instrument_name` (the
/// `_USDC` suffix survives — same contract as the arm/trade path). A quote without an
/// `instrument_name` (never produced by [`build_chain_from_summary`], but the model allows it)
/// gets a synthesized `"{underlying}:{expiry}:{strike}:{C|P}"` id so the row still has a usable
/// per-instrument grouping identity for `chain_as_of`. Quote/greek fields copy through as-is:
/// absent stays absent, never 0.0.
///
/// TIMESTAMP SEMANTICS: `ts` is the chain's `asof_ms` — the FETCH-observation instant, not the
/// venue's own publish time. [`DeribitOptionsProvider::summary`] serves rows from a
/// [`CACHE_TTL_MS`] (5s) cache, so a recorded row's `ts` can post-date the actual venue observation
/// by up to that TTL, and two expiries built from one cached payload in the same refresh pass carry
/// slightly different `asof` stamps (each `fetch_chain` re-reads the clock). Immaterial at the
/// recorder's 1-minute default cadence, but it IS a bounded skew in a point-in-time series — a
/// consumer needing sub-5s fidelity must not treat `ts` as the venue's timestamp.
pub fn chain_snapshot_rows(chain: &OptionChain) -> Vec<ChainRow> {
    let exp_ms = expiry_ms(&chain.expiry.date);
    let mut out = Vec::new();
    for sr in &chain.rows {
        for (q, is_call) in [(&sr.call, true), (&sr.put, false)] {
            let Some(q) = q else { continue };
            out.push(ChainRow {
                ts: chain.asof_ms,
                underlying: chain.underlying.clone(),
                instrument: q.instrument_name.clone().unwrap_or_else(|| {
                    let cp = if is_call { "C" } else { "P" };
                    format!("{}:{}:{}:{}", chain.underlying, chain.expiry.date, sr.strike, cp)
                }),
                expiry_ms: exp_ms,
                strike: sr.strike,
                is_call,
                bid: q.bid,
                ask: q.ask,
                mark: q.mark,
                iv: q.iv,
                open_interest: q.open_interest,
                volume: q.volume,
                delta: q.delta,
                gamma: q.gamma,
                theta: q.theta,
                vega: q.vega,
            });
        }
    }
    out
}

/// Record one enriched chain into the `kind=chain` PIT store (opt-in) — the mirror of `exec.rs`'s
/// `record_properties` for the options surface. Best-effort: the recorder no-ops when disabled
/// (`flags.record_chains` off) and swallows store errors, so the fetch path never fails on
/// recording. The observation instant for the recorder's idempotency bucket is the chain's own
/// `asof_ms` (in ns), keeping bucket and row `ts` in agreement (see
/// [`chain_snapshot_rows`] on what that instant actually is).
///
/// The disabled check comes FIRST so a threaded-but-off recorder is a true no-op: under the natural
/// always-thread wiring, the app's 30s × 8-expiry × 3-underlying refresh loop would otherwise flatten
/// and immediately drop the whole row set (a `String` clone per row for underlying + instrument)
/// every pass. Not the hot fold, so it is cheap either way — but "disabled costs nothing" is the
/// contract `PropertiesRecorder` sets, and this mirrors it.
pub fn record_chain(rec: &ChainRecorder, chain: &OptionChain) {
    if !rec.enabled() {
        return;
    }
    let rows = chain_snapshot_rows(chain);
    rec.record(&chain.source, &chain.underlying, &rows, chain.asof_ms.saturating_mul(1_000_000));
}

use vike_model::now_ms;

/// Options provider for Deribit BTC/ETH/SOL options — twin of `DeribitOptionsProvider`
/// (name "deribit", asset class crypto). Keyless public REST; a 5s cache means
/// `list_expiries` + N×`fetch_chain` in one refresh pass share a single HTTP call.
pub struct DeribitOptionsProvider {
    agent: ureq::Agent,
    cache: HashMap<String, (i64, Vec<Value>)>, // book -> (fetched_ms, result rows)
    /// Opt-in `kind=chain` snapshot recorder ([`record_chain`] on every [`Self::fetch_chain`]).
    /// `None` (the default) = byte-identical to pre-recording behavior.
    ///
    /// ⚠ **NO PRODUCTION PATH TODAY, and this said the opposite** ("PRODUCTION PATH (live, not
    /// aspirational)"). It described `vike-app`'s `App::new` opening a chain recorder from the
    /// environment and handing the result to `vike_app_core::tools::spawn_tool_fetchers`. That root
    /// went with the desktop cut: `crates/vike-desktop/src/main.rs` now binds `chain_rec` to `None`
    /// outright, and `vike_data::ChainRecorder::open` (which takes the flag as a parameter since
    /// decision 0111) has no call site anywhere in the tree. So the flag records nothing in any
    /// binary — its own `vike_config::CONSUMPTION` row (`flags.record_chains`, a
    /// `Reader::Uncalled`) is the gated authority for that, and this comment contradicting it is
    /// how the claim survived.
    ///
    /// The seam itself is intact and unmounted: [`Self::with_chain_recorder`] still takes one, and
    /// the shape a future root would use is the same as `PropertiesRecorder` reaching
    /// `DeribitRest::with_properties_recorder` from `vike-mount`.
    chain_rec: Option<Arc<ChainRecorder>>,
}

impl Default for DeribitOptionsProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl DeribitOptionsProvider {
    pub fn new() -> Self {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(std::time::Duration::from_secs(10)))
            .build()
            .into();
        Self { agent, cache: HashMap::new(), chain_rec: None }
    }

    /// Thread in the opt-in chain-snapshot recorder (best-effort; the recorder no-ops when
    /// disabled and swallows store errors). Call after `new`, like
    /// `DeribitRest::with_properties_recorder`.
    pub fn with_chain_recorder(mut self, rec: Arc<ChainRecorder>) -> Self {
        self.chain_rec = Some(rec);
        self
    }

    /// The threaded-in chain recorder, if any — lets a composition root confirm/report its own
    /// wiring (`provider.chain_recorder().is_some_and(|r| r.enabled())` = "chains are being
    /// captured"), and lets the wiring be asserted in a test instead of only by reading code.
    pub fn chain_recorder(&self) -> Option<&ChainRecorder> {
        self.chain_rec.as_deref()
    }

    pub fn list_underlyings(&self) -> [&'static str; 3] {
        ["BTC", "ETH", "SOL"]
    }

    /// The (cached) raw `result` rows for an underlying's book.
    fn summary(&mut self, underlying: &str, now_ms: i64) -> Result<&[Value], String> {
        let book = book_currency(underlying).to_string();
        let fresh = self.cache.get(&book).is_some_and(|(ts, _)| now_ms - ts < CACHE_TTL_MS);
        if !fresh {
            let url = format!("{BASE}?currency={book}&kind=option");
            let mut resp =
                self.agent.get(&url).call().map_err(|e| format!("deribit book summary: {e}"))?;
            let body = resp
                .body_mut()
                .read_to_string()
                .map_err(|e| format!("deribit book summary read: {e}"))?;
            let v: Value = serde_json::from_str(&body)
                .map_err(|e| format!("deribit book summary json: {e}"))?;
            let rows = v.get("result").and_then(|r| r.as_array()).cloned().unwrap_or_default();
            self.cache.insert(book.clone(), (now_ms, rows));
        }
        Ok(self.cache.get(&book).map(|(_, rows)| rows.as_slice()).unwrap_or(&[]))
    }

    /// Distinct expiries for an underlying, ascending.
    pub fn list_expiries(&mut self, underlying: &str) -> Result<Vec<Expiry>, String> {
        let now = now_ms();
        let rows = self.summary(underlying, now)?;
        Ok(list_expiries_from_summary(rows, now, Some(underlying)))
    }

    /// One expiry's chain, greeks enriched, optionally windowed to ±`strikes` around spot.
    pub fn fetch_chain(
        &mut self,
        underlying: &str,
        expiry_iso: &str,
        strikes: Option<usize>,
        r: f64,
    ) -> Result<OptionChain, String> {
        let now = now_ms();
        // SOL (and other altcoins) live in the shared USDC book — premiums already USD.
        let usd_quoted = book_currency(underlying) == "USDC";
        let rows = self.summary(underlying, now)?;
        let chain = build_chain_from_summary(underlying, rows, expiry_iso, now, usd_quoted, r);
        // Record the FULL chain (pre-`limit_strikes` — the strike window is a display concern,
        // never a recording one). Opt-in + best-effort: absent recorder / disabled env = no-op.
        if let Some(rec) = &self.chain_rec {
            record_chain(rec, &chain);
        }
        Ok(limit_strikes(chain, strikes))
    }
}

#[path = "chain_tests.rs"]
#[cfg(test)]
mod chain_tests;
