//! `instruments` — **the venue's own answer to "which of binance's three books does this symbol
//! name?"**, read once per process off the keyless COIN-M instrument list.
//!
//! The binance twin of `crates/bridges/bybit/src/instruments.rs`, and it exists for the same
//! reason: `crates/bridges/binance/src/data.rs`'s `route_target` needs a THIRD input, and the only
//! honest one is the venue's own listing. `docs/decisions/0061-an-instrument-names-its-kind.md`
//! forbids the two cheaper answers outright —
//!
//! * a **symbol-shape test** (`ends_with("_PERP")`) is the implicit encoding that record exists to
//!   remove, and
//! * a **caller's `AssetClass` claim** cannot answer it, because 0061 REFUSES to split
//!   [`vike_catalog::AssetClass::CryptoPerp`] into linear and inverse (*"the vocabulary names the
//!   PRODUCT, never the venue's word for the route"*). A COIN-M perp and a USDⓈ-M perp are the same
//!   class. The class claim decides spot-vs-perp; this module decides fapi-vs-dapi. The two inputs
//!   are orthogonal, which is why binance needs both where bybit needed only the second.
//!
//! # ⚠ THE PREDICATE: membership of dapi's OWN list. Not a spelling, and not a guess.
//!
//! **MEASURED against the live keyless lists, 2026-09-16** (and re-derivable without touching this
//! file — [`tests::report_the_live_coin_m_listing`] prints it from these same functions):
//!
//! | list | size | what a symbol there is |
//! |---|---|---|
//! | `dapi/v1/exchangeInfo` | **30** (20 `PERPETUAL` + 5 `CURRENT_QUARTER` + 5 `NEXT_QUARTER`) | COIN-M (coin-margined, inverse) |
//! | `fapi/v1/exchangeInfo` | 897, **zero** `*_PERP` | USDⓈ-M |
//! | `api/v3/exchangeInfo` | spot | spot |
//!
//! **The three sets are DISJOINT on the `symbol` field**, measured: `dapi ∩ fapi` and
//! `dapi ∩ spot(Trading)` are both empty. That is the property that makes `BTCUSD_PERP` an
//! unambiguous name for exactly one book anywhere on binance, and therefore the property that lets
//! `crates/vike-catalog/src/addressing.rs`'s binance row stay `BareSymbol::Unambiguous` while this
//! module admits a third book.
//!
//! ⚠ **The `pair` field is NOT disjoint and must never be used as a name.** dapi's `pair` values
//! collide with four live spot pairs — MEASURED: `BNBUSD`, `BTCUSD`, `ETHUSD`, `SOLUSD` are all
//! `status: "TRADING"` spot symbols. And `BTCUSD.P` does not even work: it strips to `BTCUSD`,
//! routes to fapi, and comes back `-1121 Invalid symbol`. The name this workspace uses is the
//! venue's `symbol` (`BTCUSD_PERP`), never its `pair`.
//!
//! # ⚠ Why THIS list and not fapi's
//!
//! The route could equally be decided by asking fapi *"do you list this?"*. It is not, and the
//! reason is measured rather than aesthetic: **dapi's list is 35,676 bytes and fapi's is
//! 1,113,598** (spot's is 17,610,599). Asking the cheap host is 31x less traffic on a path the live
//! feed's warmup takes, and it needs one host instead of two. Both hosts price the call at
//! `REQUEST_WEIGHT` **1** (MEASURED: two successive dapi calls moved `x-mbx-used-weight-1m` 4 -> 5).
//!
//! ⚠ **Neither host honours a `symbol=` filter**, so the whole list IS the unit of work — MEASURED:
//! `dapi/v1/exchangeInfo?symbol=BTCUSD_PERP` and `?pair=BTCUSD` both return all 30, and
//! `fapi/v1/exchangeInfo?symbol=BTCUSDT` returns all 897. There is no per-symbol probe to reach
//! for, which is also why `crates/bridges/binance/src/exec.rs`'s `fetch_binance_properties`
//! downloads a megabyte to answer one lookup.
//!
//! # ⚠ AN UNREADABLE LIST IS AN ERROR, never "not COIN-M"
//!
//! [`coin_m_contract`] returns `Err` when the list cannot be read, and every caller must treat that
//! as *"not proven USDⓈ-M"* rather than as `Ok(None)`. The two must not reach the same code path,
//! for a reason specific to this venue and worse than bybit's: **fapi SERVES the COIN-M tape**.
//! MEASURED 2026-09-16, `fapi/v1/klines?symbol=BTCUSD_PERP` and `dapi/v1/klines?symbol=BTCUSD_PERP`
//! come back BYTE-IDENTICAL, and so do the two hosts' answers for `BTCUSDT`. So a fall-through to
//! fapi does not fail — it succeeds with the WRONG VOLUME COLUMN (see
//! [`crate::family::klines::VolumeColumn`]), and
//! `docs/decisions/0059-bars-and-ticks-for-every-venue-are-two-asks-not-one.md`'s self-sealing
//! commit key then makes that window unrepairable: a corrected re-fetch reports success and writes
//! nothing. A loud refusal an operator retries is strictly better than a silent unit error nobody
//! re-checks.
//!
//! **The accepted residual, declared rather than left to be discovered:** a dapi outage therefore
//! refuses binance PERP kline work for the first route in a process, including a live feed's warmup
//! seed. That is a real cost and it is taken deliberately. Two things bound it: the failure is NOT
//! cached (a transient outage is retried by the next call rather than poisoning the process), and a
//! BARE symbol — every spot fetch — never consults this module at all, so a spot backfill is
//! untouched.
//!
//! # ⚠ The set is COMPUTED, never hardcoded
//!
//! `["BTCUSD_PERP", …]` as a literal would silently stop covering the twenty-first perpetual the
//! day binance lists one, and a route that quietly narrows is worse than one that never existed,
//! because nobody re-checks it. [`parse_coin_m_contracts`] is the pure map and
//! [`coin_m_contracts_live`] the once-per-process derivation.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use serde_json::Value;

/// The keyless COIN-M instrument list — the ONLY binance host that lists these symbols at all.
///
/// ⚠ Deliberately NOT a sibling of `crates/bridges/binance/src/catalog.rs`'s `SPOT_URL`/`PERP_URL`:
/// this module reads the list a ROUTE depends on, and that file fetches neither this host nor
/// anything from it. That is the catalog gap this crate's `CLAUDE.md` now names — the picker cannot
/// offer a COIN-M instrument, and this module deliberately does not change that.
const COIN_M_EXCHANGE_INFO_URL: &str = "https://dapi.binance.com/dapi/v1/exchangeInfo";

/// What binance says a COIN-M symbol IS — read off the row's own `contractType`, never inferred
/// from the string.
///
/// The two arms are different PRODUCTS in `vike_catalog::AssetClass` terms (`CryptoPerp` vs
/// `CryptoFuture`), which is why they are separated here rather than collapsed into a `bool`: the
/// [`vike_catalog::PERP_SUFFIX`] marker is a true statement about the first and a FALSE one about
/// the second, and `crates/bridges/binance/src/data.rs`'s `route_target` refuses the disagreement
/// instead of routing it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoinMContract {
    /// `contractType: "PERPETUAL"` — 20 of the 30 rows, MEASURED.
    Perpetual,
    /// `contractType: "CURRENT_QUARTER"` / `"NEXT_QUARTER"` — a dated delivery future (10 rows).
    /// The venue's own word is carried so a refusal can quote it.
    Delivery(&'static str),
}

impl CoinMContract {
    /// The venue's own `contractType` word, for a refusal message that quotes the venue rather than
    /// paraphrasing it.
    #[must_use]
    pub fn venue_word(self) -> &'static str {
        match self {
            Self::Perpetual => "PERPETUAL",
            Self::Delivery(word) => word,
        }
    }
}

/// Pure: a `dapi/v1/exchangeInfo` body -> every TRADING COIN-M symbol and what binance says it is.
///
/// ⚠ **Tradability is spelled `contractStatus` here, NOT `status`.** MEASURED — a dapi row's keys
/// are `…contractSize contractStatus contractType…` and carry no `status` key at all, while
/// `crates/bridges/binance/src/family/catalog.rs`'s `parse_perp` gates on `status == "TRADING"`.
/// A `parse_perp` pointed at this host would therefore drop all thirty rows and return EMPTY —
/// silently, since `list_tolerant` warns on a one-endpoint-down path and then reports success. That
/// is the concrete reason this module parses the list itself instead of reusing the catalog rung,
/// and the concrete work a catalog admission would have to do first.
///
/// A non-TRADING row is dropped: a delisted contract cannot be what a caller reached by accident,
/// and counting one would route a symbol to a host that no longer serves it.
#[must_use]
pub fn parse_coin_m_contracts(payload: &Value) -> BTreeMap<String, CoinMContract> {
    let Some(list) = payload.get("symbols").and_then(Value::as_array) else {
        return BTreeMap::new();
    };
    list.iter()
        .filter(|e| e.get("contractStatus").and_then(Value::as_str) == Some("TRADING"))
        .filter_map(|e| {
            let symbol = e.get("symbol").and_then(Value::as_str)?;
            if symbol.is_empty() {
                return None;
            }
            let kind = match e.get("contractType").and_then(Value::as_str)? {
                "PERPETUAL" => CoinMContract::Perpetual,
                "CURRENT_QUARTER" => CoinMContract::Delivery("CURRENT_QUARTER"),
                "NEXT_QUARTER" => CoinMContract::Delivery("NEXT_QUARTER"),
                // A contractType nobody has measured is deliberately DROPPED rather than guessed
                // into one of the two arms: an unknown row then routes to fapi exactly as it does
                // today, which is the no-change direction, instead of being sent to dapi on a
                // guess about what it settles in.
                _ => return None,
            };
            Some((symbol.to_uppercase(), kind))
        })
        .collect()
}

/// The process-wide cache. Only ever set from a SUCCESSFUL derivation — a failure returns `Err` and
/// leaves this empty, so the next caller retries instead of inheriting an outage forever.
static COIN_M: OnceLock<BTreeMap<String, CoinMContract>> = OnceLock::new();

/// Every TRADING COIN-M symbol binance lists, derived once per process. Network I/O on the first
/// call only.
///
/// # Errors
///
/// The venue's own error, verbatim, when the list cannot be read. A caller must treat that as "not
/// proven USDⓈ-M" — never as "not COIN-M". See this module's ⚠ on an unreadable list.
pub fn coin_m_contracts_live() -> Result<&'static BTreeMap<String, CoinMContract>, String> {
    coin_m_contracts_with(&|url| vike_bridge_core::http::get_json(url))
}

/// [`coin_m_contracts_live`] over an INJECTED fetcher — the seam a test drives without a socket,
/// and the reason the cache and the parse can be proven apart from the network.
fn coin_m_contracts_with(
    fetch: &impl Fn(&str) -> Result<Value, String>,
) -> Result<&'static BTreeMap<String, CoinMContract>, String> {
    if let Some(cached) = COIN_M.get() {
        return Ok(cached);
    }
    let payload = fetch(COIN_M_EXCHANGE_INFO_URL)?;
    let derived = parse_coin_m_contracts(&payload);
    if derived.is_empty() {
        // An EMPTY parse from a body that READ is not evidence that binance delists COIN-M; it is
        // evidence that this parser stopped matching the venue's shape (a renamed `contractStatus`,
        // a new envelope). Caching it would silently route every COIN-M symbol back to fapi and the
        // wrong volume column for the life of the process — the fail-permissive direction, reached
        // from a successful HTTP 200. So it is refused and not cached.
        return Err(format!(
            "binance: {COIN_M_EXCHANGE_INFO_URL} answered but named no TRADING COIN-M contract. \
             That is not a delisting — the venue listed 30 (20 PERPETUAL, 10 dated) when this \
             parser was written — so it is read as this parser no longer matching the venue's \
             shape, and it is refused rather than cached. \
             `vike_binance::instruments::parse_coin_m_contracts` is the map; the row key it gates \
             on is `contractStatus`, which is NOT the `status` key every other binance host spells."
        ));
    }
    tracing::info!(
        target: "vike_binance::instruments",
        contracts = derived.len() as u64,
        perpetuals = derived.values().filter(|c| **c == CoinMContract::Perpetual).count() as u64,
        "binance: COIN-M contracts listed on dapi — these route to the COIN-M host, not fapi"
    );
    let _ = COIN_M.set(derived);
    Ok(COIN_M.get().expect("just set"))
}

/// What binance says this WIRE symbol is, if it is a COIN-M contract at all. `Ok(None)` is a
/// POSITIVE answer — the venue's list was read and this symbol is not in it, so it is USDⓈ-M or
/// spot. Network I/O on the first call per process.
///
/// # Errors
///
/// When the COIN-M list cannot be read — which is NOT the same answer as `Ok(None)`. See this
/// module's ⚠.
pub fn coin_m_contract(wire_symbol: &str) -> Result<Option<CoinMContract>, String> {
    Ok(coin_m_contracts_live()?.get(&wire_symbol.to_uppercase()).copied())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(symbol: &str, contract_type: &str, status: &str) -> Value {
        serde_json::json!({
            "symbol": symbol,
            "pair": "BTCUSD",
            "contractType": contract_type,
            "contractStatus": status,
            "contractSize": 100,
            "marginAsset": "BTC",
        })
    }

    fn body(rows: Vec<Value>) -> Value {
        serde_json::json!({ "symbols": rows })
    }

    /// The venue's own words, mapped — and the delivery arm KEEPS the word so a refusal can quote
    /// it.
    #[test]
    fn the_contract_type_is_read_from_the_row_never_from_the_spelling() {
        let payload = body(vec![
            row("BTCUSD_PERP", "PERPETUAL", "TRADING"),
            row("BTCUSD_260925", "CURRENT_QUARTER", "TRADING"),
            row("BTCUSD_261225", "NEXT_QUARTER", "TRADING"),
        ]);
        let map = parse_coin_m_contracts(&payload);
        assert_eq!(map.get("BTCUSD_PERP"), Some(&CoinMContract::Perpetual));
        assert_eq!(
            map.get("BTCUSD_260925"),
            Some(&CoinMContract::Delivery("CURRENT_QUARTER")),
            "a dated future must not be collapsed into the perpetual arm"
        );
        assert_eq!(map.get("BTCUSD_261225"), Some(&CoinMContract::Delivery("NEXT_QUARTER")));
        assert_eq!(map["BTCUSD_260925"].venue_word(), "CURRENT_QUARTER");
    }

    /// ⚠ The key is `contractStatus`, and this is the test that says so. A parser reading `status`
    /// (which is what every OTHER binance host spells, and what `family::catalog`'s `parse_perp`
    /// gates on) sees no key at all on a dapi row and drops all thirty.
    #[test]
    fn tradability_is_contract_status_not_status() {
        let mut live = row("BTCUSD_PERP", "PERPETUAL", "TRADING");
        // The shape `parse_perp` would be handed: a `status` key and NO `contractStatus`.
        let mut as_parse_perp_would_spell_it = live.clone();
        as_parse_perp_would_spell_it["status"] = Value::from("TRADING");
        as_parse_perp_would_spell_it.as_object_mut().expect("object").remove("contractStatus");
        assert!(
            parse_coin_m_contracts(&body(vec![as_parse_perp_would_spell_it])).is_empty(),
            "a row with only `status` must not be admitted — the venue does not send one"
        );
        live["contractStatus"] = Value::from("PENDING_TRADING");
        assert!(
            parse_coin_m_contracts(&body(vec![live])).is_empty(),
            "a non-TRADING contract must be dropped"
        );
    }

    /// An unmeasured `contractType` is DROPPED, not guessed — the row then routes to fapi exactly
    /// as it does today rather than being sent to the COIN-M host on an assumption.
    #[test]
    fn an_unmeasured_contract_type_is_dropped_rather_than_guessed() {
        let map = parse_coin_m_contracts(&body(vec![row("BTCUSD_WEEKLY", "NEXT_WEEK", "TRADING")]));
        assert!(map.is_empty());
    }

    #[test]
    fn a_body_that_is_not_the_venues_shape_maps_to_nothing() {
        assert!(parse_coin_m_contracts(&serde_json::json!({})).is_empty());
        assert!(parse_coin_m_contracts(&serde_json::json!({ "symbols": "nope" })).is_empty());
    }

    /// ⚠ **The fail-permissive direction, closed.** A body that READS but parses to nothing is
    /// refused rather than cached: it is evidence this parser stopped matching the venue, and
    /// caching it would route every COIN-M symbol to fapi's wrong volume column for the whole
    /// process life — from an HTTP 200.
    #[test]
    fn an_empty_parse_from_a_readable_body_is_refused_not_cached() {
        let err = coin_m_contracts_with(&|_| Ok(serde_json::json!({ "symbols": [] })))
            .expect_err("an empty list must not read as `no COIN-M contracts exist`");
        assert!(err.contains("contractStatus"), "the refusal must name the key it gates on: {err}");
        assert!(
            COIN_M.get().is_none_or(|cached| !cached.is_empty()),
            "an empty derivation must never reach the process cache"
        );
    }

    /// A read failure is the venue's own error, verbatim, and does NOT poison the cache.
    #[test]
    fn a_read_failure_is_an_error_and_is_not_cached() {
        let err = coin_m_contracts_with(&|url| Err(format!("{url} GET: connection refused")))
            .expect_err("an unreadable list is an error");
        assert!(err.contains("connection refused"), "the venue's own words must survive: {err}");
        assert!(err.contains("dapi.binance.com"), "and the host it failed on: {err}");
    }

    /// The listing this module's predicate was sized against, printed from the code rather than
    /// restated in prose. `#[ignore]`d: it is network I/O against the live venue.
    ///
    /// ```text
    /// cargo test -p vike-binance --lib -- --ignored --nocapture report_the_live
    /// ```
    #[test]
    #[ignore = "network: live binance public endpoints"]
    fn report_the_live_coin_m_listing() {
        let coin_m = coin_m_contracts_live().expect("dapi lists COIN-M contracts");
        let perps = coin_m.values().filter(|c| **c == CoinMContract::Perpetual).count();
        println!("dapi COIN-M contracts: {} ({perps} PERPETUAL)", coin_m.len());
        assert_eq!(coin_m.get("BTCUSD_PERP"), Some(&CoinMContract::Perpetual));
    }
}
