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
    enrich_quote, expiry_ms, limit_strikes, make_expiry, years_to_expiry, AssetClass, Expiry,
    OptionChain, OptionKind, OptionQuote, StrikeRow,
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
    if expiry.len() < 6 || expiry.len() > 7 {
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
        if let Some((base, date_iso, _, _)) = parse_instrument_name(name) {
            if currency.is_none() || currency == Some(base.as_str()) {
                dates.insert(date_iso);
            }
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
        asset_class: AssetClass::Crypto,
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
/// (`VIKE_RECORD_CHAINS` unset) and swallows store errors, so the fetch path never fails on
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
    /// PRODUCTION PATH (live, not aspirational): `vike-app`'s `App::new` calls
    /// `vike_data::ChainRecorder::open_from_env(tick_store_root())` — `Some` only under
    /// `VIKE_RECORD_CHAINS=1` — and hands it to `vike_app_core::tools::spawn_tool_fetchers`, whose
    /// options-refresh thread builds this provider through
    /// `vike_app_core::tools::options_provider` → [`Self::with_chain_recorder`]. Same shape as
    /// `PropertiesRecorder` reaching `DeribitRest::with_properties_recorder` from `vike-mount`.
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

#[cfg(test)]
mod tests {
    //! Ports `tests/unit/data/test_options_deribit.py` (captured payloads inline).

    use super::*;

    /// 2026-06-02 08:00 UTC — the oracle tests' `_ms(2026, 6, 2)`.
    const NOW: i64 = 1_780_387_200_000;

    fn summary() -> Vec<Value> {
        // two expiries (27 Jun + 25 Sep 2026); within 27 Jun: two strikes, call+put on the first
        serde_json::json!([
            {"instrument_name": "BTC-27JUN26-100000-C", "bid_price": 0.05, "ask_price": 0.06,
             "mark_price": 0.055, "mark_iv": 62.5, "open_interest": 120.0, "volume": 8.0,
             "underlying_price": 104000.0},
            {"instrument_name": "BTC-27JUN26-100000-P", "bid_price": 0.04, "ask_price": 0.05,
             "mark_price": 0.045, "mark_iv": 61.0, "open_interest": 90.0, "volume": 3.0,
             "underlying_price": 104000.0},
            {"instrument_name": "BTC-27JUN26-110000-C", "bid_price": 0.02, "ask_price": 0.03,
             "mark_price": 0.025, "mark_iv": 64.0, "open_interest": 50.0, "volume": 1.0,
             "underlying_price": 104000.0},
            {"instrument_name": "BTC-25SEP26-120000-C", "bid_price": 0.01, "ask_price": 0.02,
             "mark_price": 0.015, "mark_iv": 70.0, "open_interest": 10.0, "volume": 1.0,
             "underlying_price": 104000.0},
            {"instrument_name": "BTC-PERPETUAL", "mark_price": 104000.0}
        ])
        .as_array()
        .unwrap()
        .clone()
    }

    fn usdc_summary() -> Vec<Value> {
        // The shared USDC book mixes coins; SOL's chain must pick out only SOL_USDC rows.
        serde_json::json!([
            {"instrument_name": "SOL_USDC-26JUN26-90-P", "bid_price": 17.5, "ask_price": 18.0,
             "mark_price": 17.75, "mark_iv": 60.0, "open_interest": 40.0, "volume": 5.0,
             "underlying_price": 74.5},
            {"instrument_name": "SOL_USDC-26JUN26-90-C", "bid_price": 1.0, "ask_price": 1.2,
             "mark_price": 1.1, "mark_iv": 61.0, "open_interest": 30.0, "volume": 2.0,
             "underlying_price": 74.5},
            {"instrument_name": "BTC_USDC-31JUL26-115000-P", "bid_price": 5000.0, "ask_price": 5100.0,
             "mark_price": 5050.0, "mark_iv": 55.0, "underlying_price": 104000.0},
            {"instrument_name": "XRP_USDC-26JUN26-3-C", "bid_price": 0.1, "ask_price": 0.12,
             "mark_price": 0.11, "mark_iv": 70.0, "underlying_price": 2.4}
        ])
        .as_array()
        .unwrap()
        .clone()
    }

    #[test]
    fn parses_instrument_names() {
        assert_eq!(
            parse_instrument_name("BTC-27JUN26-100000-C"),
            Some(("BTC".into(), "2026-06-27".into(), 100000.0, OptionKind::Call))
        );
        assert_eq!(parse_instrument_name("BTC-PERPETUAL"), None);
        assert_eq!(parse_instrument_name("BTC-27JUN26-100000-Z"), None);
        assert_eq!(parse_instrument_name("BTC-27XXX26-100000-C"), None); // non-month token
                                                                         // USDC-margined altcoin form: base carries a _USDC suffix, dropped -> "SOL"
        assert_eq!(
            parse_instrument_name("SOL_USDC-26JUN26-90-P"),
            Some(("SOL".into(), "2026-06-26".into(), 90.0, OptionKind::Put))
        );
        assert_eq!(
            parse_instrument_name("SOL_USDC-5JUN26-80-C"),
            Some(("SOL".into(), "2026-06-05".into(), 80.0, OptionKind::Call))
        );
    }

    #[test]
    fn lists_expiries_ascending_distinct() {
        let exps = list_expiries_from_summary(&summary(), NOW, None);
        let dates: Vec<&str> = exps.iter().map(|e| e.date.as_str()).collect();
        assert_eq!(dates, ["2026-06-27", "2026-09-25"]);
        assert_eq!(exps[0].dte, 25);
    }

    #[test]
    fn lists_expiries_filters_to_requested_coin_in_shared_book() {
        let exps = list_expiries_from_summary(&usdc_summary(), NOW, Some("SOL"));
        let dates: Vec<&str> = exps.iter().map(|e| e.date.as_str()).collect();
        assert_eq!(dates, ["2026-06-26"]); // only SOL, not the BTC 31 Jul row
    }

    #[test]
    fn builds_chain_groups_scales_and_enriches() {
        let chain = build_chain_from_summary("BTC", &summary(), "2026-06-27", NOW, false, 0.0);
        assert_eq!(chain.source, "deribit");
        assert_eq!(chain.asset_class, AssetClass::Crypto);
        assert_eq!(chain.underlying_price, Some(104000.0));
        // the 25 Sep / 120000 row is filtered out
        let strikes: Vec<f64> = chain.rows.iter().map(|r| r.strike).collect();
        assert_eq!(strikes, [100000.0, 110000.0]);
        let row = &chain.rows[0];
        let (call, put) = (row.call.as_ref().unwrap(), row.put.as_ref().unwrap());
        assert_eq!(call.iv, Some(0.625)); // mark_iv % -> decimal
        assert_eq!(put.iv, Some(0.61));
        // Deribit premiums are coin units, scaled to USD by underlying_price (exact arithmetic)
        assert_eq!(call.bid, Some(0.05 * 104000.0));
        assert_eq!(call.mark, Some(0.055 * 104000.0));
        assert!(call.delta.is_some(), "greeks enriched from IV");
        assert!(chain.rows[1].put.is_none(), "only a call at 110000");
    }

    #[test]
    fn builds_sol_chain_from_usdc_book_unscaled_premiums() {
        // usd_quoted=true: USDC premiums are already USD -> NOT scaled by the ~74.5 underlying
        let chain = build_chain_from_summary("SOL", &usdc_summary(), "2026-06-26", NOW, true, 0.0);
        assert_eq!(chain.underlying, "SOL");
        assert_eq!(chain.underlying_price, Some(74.5));
        let strikes: Vec<f64> = chain.rows.iter().map(|r| r.strike).collect();
        assert_eq!(strikes, [90.0]); // BTC/XRP rows excluded
        assert_eq!(chain.rows[0].put.as_ref().unwrap().bid, Some(17.5)); // not 17.5 * 74.5
        assert_eq!(chain.rows[0].call.as_ref().unwrap().mark, Some(1.1));
    }

    #[test]
    fn chain_carries_exact_instrument_name() {
        // the arm path needs the exact venue id; the _USDC suffix must survive verbatim
        let chain = build_chain_from_summary("BTC", &summary(), "2026-06-27", NOW, false, 0.0);
        let row = &chain.rows[0];
        assert_eq!(
            row.call.as_ref().unwrap().instrument_name.as_deref(),
            Some("BTC-27JUN26-100000-C")
        );
        assert_eq!(
            row.put.as_ref().unwrap().instrument_name.as_deref(),
            Some("BTC-27JUN26-100000-P")
        );
        assert_eq!(
            chain.rows[1].call.as_ref().unwrap().instrument_name.as_deref(),
            Some("BTC-27JUN26-110000-C")
        );
        let sol = build_chain_from_summary("SOL", &usdc_summary(), "2026-06-26", NOW, true, 0.0);
        assert_eq!(
            sol.rows[0].put.as_ref().unwrap().instrument_name.as_deref(),
            Some("SOL_USDC-26JUN26-90-P")
        );
    }

    // ---- kind=chain snapshot recording (opt-in ChainRecorder hook) ----------------------------
    // New additive tests (no Python twin): the fixture chain flattens to `ChainRow`s and lands in
    // the store through `record_chain`, exercised over the DataFusion-free `MemHistStore` double
    // (vike-data `test-support` dev-feature) exactly like client.rs's properties_recorder_tests.

    use std::sync::Arc;
    use vike_data::{ChainRecorder, HistStore, MemHistStore, TsRange};

    #[test]
    fn chain_snapshot_rows_flatten_fixture_chain() {
        let chain = build_chain_from_summary("BTC", &summary(), "2026-06-27", NOW, false, 0.0);
        let rows = chain_snapshot_rows(&chain);
        // 100000 call+put + 110000 call = 3 rows; the 25 Sep row was already expiry-filtered
        assert_eq!(rows.len(), 3);
        assert!(rows.iter().all(|r| r.ts == NOW && r.underlying == "BTC"));
        assert!(rows.iter().all(|r| r.expiry_ms == vike_options::expiry_ms("2026-06-27")));
        let call = &rows[0];
        assert_eq!(call.instrument, "BTC-27JUN26-100000-C", "verbatim venue id");
        assert!(call.is_call);
        assert_eq!(call.strike, 100_000.0);
        assert_eq!(call.bid, Some(0.05 * 104_000.0), "USD-scaled premium copied through");
        assert_eq!(call.iv, Some(0.625), "decimal IV copied through");
        assert!(call.delta.is_some(), "enriched greeks recorded");
        let put = &rows[1];
        assert_eq!(put.instrument, "BTC-27JUN26-100000-P");
        assert!(!put.is_call);
        // the 110000 strike has no put — only 3 rows total, and the lone call carries its strike
        assert_eq!(rows[2].strike, 110_000.0);
    }

    #[test]
    fn record_chain_persists_fixture_and_same_minute_is_one_snapshot() {
        let store = Arc::new(MemHistStore::new());
        let rec = ChainRecorder::new(store.clone(), true);
        let chain = build_chain_from_summary("BTC", &summary(), "2026-06-27", NOW, false, 0.0);
        record_chain(&rec, &chain);
        record_chain(&rec, &chain); // same asof → same minute bucket → store no-op
        let got = store.scan_chain("deribit", "BTC", TsRange::all()).unwrap();
        assert_eq!(got.len(), 3, "recorded once, keyed venue=deribit/symbol=BTC");
        assert_eq!(got, chain_snapshot_rows(&chain));
    }

    #[test]
    fn record_chain_disabled_recorder_writes_nothing() {
        let store = Arc::new(MemHistStore::new());
        let rec = ChainRecorder::new(store.clone(), false);
        let chain = build_chain_from_summary("BTC", &summary(), "2026-06-27", NOW, false, 0.0);
        record_chain(&rec, &chain);
        assert!(store.scan_chain("deribit", "BTC", TsRange::all()).unwrap().is_empty());
    }
}
