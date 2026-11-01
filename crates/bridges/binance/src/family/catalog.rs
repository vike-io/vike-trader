//! Public spot + USDⓈ-M perp instrument catalog for the Binance-grammar venues — the searchable
//! symbol universe behind the chart's symbol picker. Shared by vike-binance (`crate::catalog`) and
//! vike-aster (`vike_aster::catalog`).
//!
//! Both venues serve the same `exchangeInfo` grammar (`symbols[]` carrying `symbol`/`baseAsset`/
//! `quoteAsset`/`status`/`filters`, plus `contractType`/`marginAsset`/`deliveryDate` on the futures
//! endpoint), so the mapping lives once here with `venue` a caller-supplied parameter, exactly like
//! the rung-1 mappers.
//!
//! **Every row is a typed [`Contract`] read off the row's own fields** (`docs/decisions/0126-every-
//! instrument-class-is-a-typed-contract-in-vike-model-and-feed-values-live-in-vike-marketdata.md`):
//! a spot row is [`Contract::CryptoSpot`]; a futures row is what its `contractType` says it is —
//! [`fapi_contract`] is the ONE place that word is read, and the exec grid parser
//! (`crates/bridges/binance/src/perp.rs`'s `perp_asset_class`) takes its class from the same
//! function, so the picker and the order path cannot classify one row two ways. The tick/step/
//! min/max/min-notional grid is likewise parsed ONCE ([`spot_properties`], [`perp_grid`]) and the
//! exec-path parsers (`crates/bridges/binance/src/family/order_map.rs`'s `parse_symbol_properties`,
//! `crates/bridges/binance/src/perp.rs`'s `parse_binance_perp_instruments`) delegate to it, so a
//! catalog row carries the same grid an order is rounded to.
//!
//! **What the picker lists is NARROWER than what [`fapi_contract`] can type.** [`parse_perp`] keeps
//! the `PERPETUAL` rows it always listed plus `PERPETUAL_DELIVERING` (a perp with a settlement
//! date, still routed by its `.P` id) and `TRADIFI_PERPETUAL` (an equity or commodity perpetual:
//! the same linear USDT contract on the same host). Dated futures are typed but NOT in the picker:
//! the picker's class list has no `CryptoFuture` for this venue
//! (`crates/vike-catalog/src/addressing.rs`'s `addressing_for`). They ARE in the symbol-facts
//! listing ([`futures_listing`]), spelled with the lane suffix (`BTCUSDT_261225.P`: a bare dated
//! symbol would route to SPOT in `crates/bridges/binance/src/family/exec_loop.rs`'s `split_symbol`,
//! and `.P` is this venue's USDⓈ-M lane marker whatever the row's kind), so an operator can TRADE one by
//! typing it, from the command line, though the window cannot pick it.
//!
//! **The endpoint URLs stay OUT of this module.** They are the one real per-venue delta, passed in
//! as plain `&str` by each venue's provider: Binance holds them as `const`s (`api.binance.com` +
//! `fapi.binance.com/fapi/v1`), Aster builds them per call off `urls::urls_for(AccountTier::Live)`
//! — its picker deliberately lists MAINNET instruments regardless of which environment execution
//! trades against, which is Aster's own concern and invisible here. Nothing in this module decides
//! which network it is on, and nothing here became env-resolved.
//!
//! `CatalogProvider` is deliberately NOT implemented here: each venue owns its own unit struct and
//! its own `venue()`/`mode()`, and only the `list_instruments` body — the part that is identical —
//! delegates here. This module is wire grammar, never a registered venue.
//!
//! Pure parse is split from the blocking `fetch_json` so the mapping stays testable without network
//! (mirrors data.rs's parse/fetch split). Each venue keeps its OWN fixture tests over its OWN
//! wrapper, so this one implementation is proven twice.

use serde_json::Value;
use vike_bridge_core::json::json_num;
use vike_catalog::CatalogError;
use vike_model::{
    AssetClass, Contract, Delivery, FutureFamily, FutureSpec, Instrument, Margining, PerpSpec,
    SymbolProperties, Tenor,
};

/// The asset classes both venues' providers contribute — spot off `/api/v3/exchangeInfo`, USDⓈ-M
/// perp off the `fapi` twin. Each venue's `CatalogProvider::asset_classes` returns this.
pub const ASSET_CLASSES: &[AssetClass] = &[AssetClass::CryptoSpot, AssetClass::CryptoPerp];

/// The `contractType` words [`parse_perp`] lists in the picker. Every one is a [`Contract::Perp`]
/// routed by its `.P` id; see the module doc for why the dated words are not listed.
///
/// `TRADIFI_PERPETUAL` joined in 2026-10-30: an equity or commodity perpetual is a linear USDT perpetual
/// on the same host, grid and `.P` route as any other (MEASURED on the demo host:
/// `docs/ops/trading-any-symbol.md`'s "Binance: TradFi perpetuals and dated futures"), so keeping it out of
/// the picker only hid a tradable symbol. ⚠ A shared-home change, made with aster's consumer measured: aster's live list
/// (2026-10-30, 644 rows) carries `PERPETUAL` and an empty word only, so its picker is the same rows.
const PICKER_CONTRACT_TYPES: &[&str] = &["PERPETUAL", "PERPETUAL_DELIVERING", "TRADIFI_PERPETUAL"];

/// The `contractType` words the symbol-FACTS listing ([`parse_futures`], [`futures_listing`]) admits: the
/// picker's, plus the dated USDⓈ-M futures. A dated symbol is spelled with the lane suffix like every other
/// fapi contract (`BTCUSDT_261225.P`: `.P` is this venue's USDⓈ-M LANE marker, the row's typed [`Contract`]
/// says what it is), so the order, fill, position and price paths that route by the suffix need no change.
pub const FUTURES_CONTRACT_TYPES: &[&str] = &[
    "PERPETUAL",
    "PERPETUAL_DELIVERING",
    "TRADIFI_PERPETUAL",
    "CURRENT_QUARTER",
    "NEXT_QUARTER",
    "CURRENT_WEEK",
    "NEXT_WEEK",
];

/// An upper-cased string field of one `symbols[]` row; `None` when absent or empty.
fn field(entry: &Value, key: &str) -> Option<String> {
    entry.get(key).and_then(Value::as_str).map(str::to_uppercase).filter(|s| !s.is_empty())
}

/// The row's `filters[]` entry of type `filter_type`. The LAST one wins on a duplicate, which is
/// what the exec parsers' `IndexMap` collect did before they delegated here.
fn filter<'a>(entry: &'a Value, filter_type: &str) -> Option<&'a Value> {
    entry
        .get("filters")?
        .as_array()?
        .iter()
        .rfind(|f| f.get("filterType").and_then(Value::as_str) == Some(filter_type))
}

/// `filters[filter_type][key]` as a number. Filter fields arrive as decimal STRINGS
/// (`"0.00100000"`); `json_num` `float()`s them like Python. Absent is `0.0`, the
/// [`SymbolProperties`] "venue did not say" value.
fn filter_num(entry: &Value, filter_type: &str, key: &str) -> f64 {
    filter(entry, filter_type).and_then(|v| v.get(key)).and_then(json_num).unwrap_or(0.0)
}

/// ONE spot `/api/v3/exchangeInfo` row's grid — twin of `data/instrument_db.py::
/// parse_symbol_properties`. `crates/bridges/binance/src/family/order_map.rs`'s
/// `parse_symbol_properties` (the exec grid) and [`parse_spot`] (the picker row) both read it here.
#[must_use]
pub fn spot_properties(entry: &Value) -> SymbolProperties {
    let min_notional = filter(entry, "NOTIONAL")
        .or_else(|| filter(entry, "MIN_NOTIONAL"))
        .and_then(|v| v.get("minNotional"))
        .and_then(json_num)
        .unwrap_or(0.0);
    SymbolProperties {
        tick_size: filter_num(entry, "PRICE_FILTER", "tickSize"),
        step_size: filter_num(entry, "LOT_SIZE", "stepSize"),
        min_qty: filter_num(entry, "LOT_SIZE", "minQty"),
        max_qty: filter_num(entry, "LOT_SIZE", "maxQty"),
        min_notional,
        // The ENDPOINT is the venue's answer here, and it is an unambiguous one: `/api/v3/
        // exchangeInfo` (binance spot, or aster's fork of it) lists spot pairs and nothing else —
        // unlike `/fapi`, whose `/exchangeInfo` mixes perpetuals with dated futures and therefore
        // has to read `contractType` per entry ([`fapi_contract`]).
        asset_class: Some(Contract::CryptoSpot.class()),
        // Everything else absent: spot is 1:1 with the base asset (multiplier 1.0), the grid is
        // flat (ONE `tickSize`, no tiers), and no venue taker hold. FRU rather than an exhaustive
        // literal so a new `SymbolProperties` field costs this parser nothing.
        ..Default::default()
    }
}

/// ONE `/fapi` `/exchangeInfo` row's GRID, without a class (the caller sets it from
/// [`fapi_contract`]). Perp deltas from spot: the qty cap is `MARKET_LOT_SIZE.maxQty` (the
/// market-order cap) and the min notional is `MIN_NOTIONAL.notional`.
/// `crates/bridges/binance/src/perp.rs`'s `parse_binance_perp_instruments` and [`parse_perp`] both
/// read it here. Everything else is absent: `/fapi` publishes no `contractSize` (a USDⓈ-M contract
/// is quoted in base units), ONE `tickSize` (flat grid), and no venue taker hold.
#[must_use]
pub fn perp_grid(entry: &Value) -> SymbolProperties {
    SymbolProperties {
        tick_size: filter_num(entry, "PRICE_FILTER", "tickSize"),
        step_size: filter_num(entry, "LOT_SIZE", "stepSize"),
        min_qty: filter_num(entry, "LOT_SIZE", "minQty"),
        max_qty: filter_num(entry, "MARKET_LOT_SIZE", "maxQty"),
        min_notional: filter_num(entry, "MIN_NOTIONAL", "notional"),
        ..Default::default()
    }
}

/// Linear / inverse / quanto from the row's OWN assets: settling in the quote asset is linear, in
/// the base asset inverse, in a third asset quanto. MEASURED 2026-10-11: all 924 `fapi` rows settle
/// in their quote asset (`marginAsset == quoteAsset`).
fn margining(base: Option<&str>, quote: Option<&str>, settle: &str) -> Margining {
    if quote == Some(settle) {
        Margining::Linear
    } else if base == Some(settle) {
        Margining::Inverse
    } else {
        Margining::Quanto
    }
}

/// What ONE `/fapi` `/exchangeInfo` row IS, read off the venue's own fields — `contractType` for
/// the kind, `marginAsset` (else `quoteAsset`) for the settle asset, `deliveryDate` for the date.
/// `None` when the row does not say: no settle asset, an unmeasured `contractType`, or a dated row
/// with no `deliveryDate`.
///
/// MEASURED on the live list 2026-10-11 (`crates/bridges/binance/tests/fixtures/exchange_info/`
/// keeps a slice of it): 703 `PERPETUAL`, 217 `TRADIFI_PERPETUAL` (equity/commodity perpetuals),
/// 2 `CURRENT_QUARTER`, 2 `NEXT_QUARTER`. The DEMO host (2026-10-30) adds `CURRENT_WEEK` and `NEXT_WEEK`
/// (one weekly pair, typed [`Tenor::Week`]), and a delivered quarterly keeps its word with a suffix
/// (`CURRENT_QUARTER DELIVERING`, status `DELIVERING`), which no arm below matches: a contract that is
/// being delivered is not typed.
///
/// * `PERPETUAL` and `TRADIFI_PERPETUAL` are a [`Contract::Perp`]. Their `deliveryDate` is the
///   venue's no-expiry sentinel (`4133404800000`, 2100-12-25) and is NOT read: a perp has no expiry.
/// * `PERPETUAL_DELIVERING` is a perpetual binance has scheduled for delivery: still a
///   [`Contract::Perp`], with `deliveryDate` as `settling_ms`.
/// * `CURRENT_QUARTER` / `NEXT_QUARTER` are a [`Contract::Future`] expiring at `deliveryDate`,
///   tenor [`Tenor::Quarter`] (`CURRENT_WEEK` / `NEXT_WEEK`: [`Tenor::Week`]). `delivery` is [`Delivery::Unknown`]: the row carries no field saying
///   how it settles, so "cash" would be a claim from documentation, not from the payload.
/// * Any other word is `None`, not a neighbouring variant: `TRADIFI_PERPETUAL` arrived after this
///   endpoint was described as "perpetual or dated", so the list is open and a new word names
///   nothing until it is measured.
///
/// `funding_interval_ms` is `None`: the interval is `/fapi/v1/fundingInfo`'s, not this payload's.
/// `value_ccy` is `None`: `/fapi` publishes no contract value or its currency.
#[must_use]
pub fn fapi_contract(entry: &Value) -> Option<Contract> {
    let base = field(entry, "baseAsset");
    let quote = field(entry, "quoteAsset");
    let settle = field(entry, "marginAsset").or_else(|| quote.clone())?;
    let margining = margining(base.as_deref(), quote.as_deref(), &settle);
    let delivery_ms = entry.get("deliveryDate").and_then(Value::as_i64);
    let perp = |settling_ms: Option<i64>| {
        Contract::Perp(PerpSpec {
            margining,
            settle: settle.clone(),
            value_ccy: None,
            funding_interval_ms: None,
            settling_ms,
        })
    };
    match entry.get("contractType").and_then(Value::as_str)? {
        "PERPETUAL" | "TRADIFI_PERPETUAL" => Some(perp(None)),
        "PERPETUAL_DELIVERING" => Some(perp(delivery_ms)),
        word @ ("CURRENT_QUARTER" | "NEXT_QUARTER" | "CURRENT_WEEK" | "NEXT_WEEK") => {
            Some(Contract::Future(FutureSpec {
                margining,
                settle: settle.clone(),
                value_ccy: None,
                expiry_ms: delivery_ms?,
                tenor: Some(if word.ends_with("WEEK") { Tenor::Week } else { Tenor::Quarter }),
                delivery: Delivery::Unknown,
                family: FutureFamily::Crypto,
            }))
        }
        _ => None,
    }
}

/// Parse `/api/v3/exchangeInfo` into [`Contract::CryptoSpot`] [`Instrument`]s, stamped with the
/// caller's `venue`, each carrying its grid ([`spot_properties`]). `status == "TRADING"` only.
pub fn parse_spot(payload: &Value, venue: &str) -> Vec<Instrument> {
    let Some(list) = payload.get("symbols").and_then(Value::as_array) else { return Vec::new() };
    list.iter()
        .filter(|e| e.get("status").and_then(Value::as_str) == Some("TRADING"))
        .filter_map(|e| {
            let symbol = field(e, "symbol")?;
            Some(
                Instrument::new(
                    venue,
                    symbol,
                    field(e, "baseAsset").unwrap_or_default(),
                    field(e, "quoteAsset").unwrap_or_default(),
                    Contract::CryptoSpot,
                )
                .with_properties(spot_properties(e)),
            )
        })
        .collect()
}

/// Parse the `fapi` `exchangeInfo` into [`Contract::Perp`] [`Instrument`]s, stamped with the
/// caller's `venue`: `status == "TRADING"` rows whose `contractType` is in
/// [`PICKER_CONTRACT_TYPES`] and which [`fapi_contract`] types. Each carries its grid
/// ([`perp_grid`]) and the venue's `contractType` word verbatim.
pub fn parse_perp(payload: &Value, venue: &str) -> Vec<Instrument> {
    parse_futures(payload, venue, PICKER_CONTRACT_TYPES)
}

/// [`parse_perp`] over an explicit set of `contractType` `words`: the picker passes
/// [`PICKER_CONTRACT_TYPES`], the symbol-facts lookup [`FUTURES_CONTRACT_TYPES`]. A row is kept when its
/// `status` is `TRADING`, its word is in `words` and [`fapi_contract`] types it; every row is spelled with
/// the lane suffix ([`super::perp_series_symbol`]) and carries the venue's word verbatim.
pub fn parse_futures(payload: &Value, venue: &str, words: &[&str]) -> Vec<Instrument> {
    let Some(list) = payload.get("symbols").and_then(Value::as_array) else { return Vec::new() };
    let mut out = Vec::new();
    for e in list {
        if e.get("status").and_then(Value::as_str) != Some("TRADING") {
            continue;
        }
        let Some(word) =
            e.get("contractType").and_then(Value::as_str).filter(|w| words.contains(w))
        else {
            continue;
        };
        let Some(symbol) = field(e, "symbol") else { continue };
        let Some(contract) = fapi_contract(e) else { continue };
        let properties = SymbolProperties { asset_class: Some(contract.class()), ..perp_grid(e) };
        out.push(
            Instrument::new(
                venue,
                // `.P` suffix (TradingView `BINANCE:BTCUSDT.P` convention): a distinct vike symbol
                // so a perp doesn't collide with its spot twin (both are `BTCUSDT` on the
                // exchange). The venue's market feed strips the `.P` back to the exchange symbol
                // at the fstream/fapi boundary; everywhere else in vike the perp IS `BTCUSDT.P`.
                // Spelled by the SAME function the perp fill/reconcile labels are, so the id an
                // engine starts on and the label its fills carry cannot drift apart.
                super::perp_series_symbol(&symbol),
                field(e, "baseAsset").unwrap_or_default(),
                field(e, "quoteAsset").unwrap_or_default(),
                contract,
            )
            .with_properties(properties)
            .with_venue_word(word),
        );
    }
    out
}

/// Blocking `GET url` → parsed JSON via the shared [`vike_bridge_core::http::get_json`] (no
/// signer/credentials — both exchangeInfo endpoints are keyless public reads); called once per
/// `AssetClass` by [`list_instruments`]. Only the `CatalogError` wrap is ours — the call, the
/// 200-range gate and the per-stage error strings are the shared seam's.
fn fetch_json(url: &str) -> Result<Value, CatalogError> {
    vike_bridge_core::http::get_json(url).map_err(CatalogError)
}

/// The body of each venue's `CatalogProvider::list_instruments`: fetch + parse spot off `spot_url`,
/// then USDⓈ-M perp off `perp_url`, both stamped with `venue`. Per-endpoint TOLERANT (mirrors
/// okx's per-instType accumulate): one endpoint failing must NOT drop the other — a `?` here used
/// to short-circuit, so a spot outage wiped the venue's perps from the Symbol picker too. A miss is
/// warned (never silent) and the surviving endpoint's instruments still populate.
pub fn list_instruments(
    venue: &str,
    spot_url: &str,
    perp_url: &str,
) -> Result<Vec<Instrument>, CatalogError> {
    Ok(list_tolerant(venue, spot_url, perp_url, fetch_json))
}

/// The venue's body code for a symbol its spot book does not carry (`-1121 Invalid symbol`, HTTP 400).
const INVALID_SYMBOL_CODE: i64 = -1121;

/// Whether `symbol` can ride in a query string unencoded: the characters a Binance-grammar symbol is
/// made of. Anything else (a `&`, a space) is refused before a request is built.
fn is_plain_symbol(symbol: &str) -> bool {
    !symbol.is_empty()
        && symbol.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// **ONE spot symbol's row**, `GET {spot_url}?symbol=<SYMBOL>` (a few hundred bytes; the unfiltered
/// spot list is ~17.6 MB, measured 2026-09-16), for the symbol-facts lookup. Keyless.
///
/// `Ok(vec![])` is the venue's "no such TRADING symbol": an HTTP 400 carrying code `-1121`, or a row
/// whose `status` is not `TRADING` ([`parse_spot`] drops it). Any other non-2xx, a transport failure
/// or an unparseable body is an `Err`: a failure is never the empty answer, so the caller does not
/// cache "no such symbol" for a venue it could not read.
pub fn spot_symbol(
    venue: &str,
    spot_url: &str,
    symbol: &str,
) -> Result<Vec<Instrument>, CatalogError> {
    if !is_plain_symbol(symbol) {
        return Err(CatalogError(format!("{venue}: `{symbol}` is not a spot symbol spelling")));
    }
    let url = format!("{spot_url}?symbol={symbol}");
    let agent = vike_bridge_core::http::blocking_agent();
    let resp = vike_bridge_core::http::get_raw(&agent, &url, venue, &[]).map_err(CatalogError)?;
    let json = serde_json::from_str::<Value>(&resp.body);
    if resp.status == 400
        && json.as_ref().ok().and_then(|v| v.get("code")).and_then(Value::as_i64)
            == Some(INVALID_SYMBOL_CODE)
    {
        return Ok(Vec::new());
    }
    if !(200..300).contains(&resp.status) {
        return Err(CatalogError(format!(
            "{venue} {spot_url} HTTP {}: {}",
            resp.status,
            vike_bridge_core::http::body_head(&resp.body)
        )));
    }
    let json = json.map_err(|e| CatalogError(format!("{venue} {spot_url} JSON: {e}")))?;
    Ok(parse_spot(&json, venue))
}

/// **The USDⓈ-M perpetual list ALONE**, one `GET perp_url` (fapi ignores `symbol=`, so this is the
/// whole ~1.1 MB list, measured 2026-10-11), for the symbol-facts lookup of a `.P` symbol. Unlike
/// [`list_instruments`] it is STRICT: a failed fetch is an `Err`, never an empty or partial list, and
/// it does not fetch spot at all (a perpetual's facts never need the spot list).
pub fn perp_listing(venue: &str, perp_url: &str) -> Result<Vec<Instrument>, CatalogError> {
    Ok(parse_perp(&fetch_json(perp_url)?, venue))
}

/// [`perp_listing`] over [`FUTURES_CONTRACT_TYPES`]: the USDⓈ-M futures list WITH its dated contracts, strict
/// like it (one `GET`, a failure is an `Err`). The binance symbol-facts lookup of a `.P` symbol reads this;
/// aster's stays [`perp_listing`].
pub fn futures_listing(venue: &str, perp_url: &str) -> Result<Vec<Instrument>, CatalogError> {
    Ok(parse_futures(&fetch_json(perp_url)?, venue, FUTURES_CONTRACT_TYPES))
}

/// [`list_instruments`]'s fold over an injectable fetch — split out so the one-endpoint-down
/// tolerance is testable without network.
fn list_tolerant(
    venue: &str,
    spot_url: &str,
    perp_url: &str,
    fetch: impl Fn(&str) -> Result<Value, CatalogError>,
) -> Vec<Instrument> {
    let mut out = Vec::new();
    match fetch(spot_url) {
        Ok(v) => out.extend(parse_spot(&v, venue)),
        Err(e) => tracing::warn!(
            target: "vike_binance::family::catalog",
            "{venue} spot fetch failed (skipped): {e}"
        ),
    }
    match fetch(perp_url) {
        Ok(v) => out.extend(parse_perp(&v, venue)),
        Err(e) => tracing::warn!(
            target: "vike_binance::family::catalog",
            "{venue} perp fetch failed (skipped): {e}"
        ),
    }
    out
}

#[cfg(test)]
mod tolerant_tests {
    use super::*;

    /// The (g) robustness contract, mirroring okx's per-instType accumulate: the spot endpoint
    /// failing must not wipe the venue — the perps still come back (and vice versa).
    #[test]
    fn one_failed_endpoint_still_yields_the_other_endpoints_instruments() {
        let spot = serde_json::json!({ "symbols": [
            { "symbol": "BTCUSDT", "baseAsset": "BTC", "quoteAsset": "USDT", "status": "TRADING" }
        ]});
        let perp = serde_json::json!({ "symbols": [
            { "symbol": "BTCUSDT", "baseAsset": "BTC", "quoteAsset": "USDT",
              "status": "TRADING", "contractType": "PERPETUAL" }
        ]});
        // spot down → perps survive
        let out = list_tolerant("binance", "spot://u", "perp://u", |url| {
            if url == "spot://u" {
                Err(CatalogError("HTTP 500: simulated".into()))
            } else {
                Ok(perp.clone())
            }
        });
        assert_eq!(out.len(), 1, "the perp page must survive the spot failure");
        assert_eq!(out[0].raw_symbol, "BTCUSDT.P");
        // perp down → spot survives
        let out = list_tolerant("binance", "spot://u", "perp://u", |url| {
            if url == "perp://u" {
                Err(CatalogError("HTTP 500: simulated".into()))
            } else {
                Ok(spot.clone())
            }
        });
        assert_eq!(out.len(), 1, "the spot page must survive the perp failure");
        assert_eq!(out[0].raw_symbol, "BTCUSDT");
        // both failing yields an EMPTY catalog (warned), never an error
        assert!(
            list_tolerant("binance", "s", "p", |_| Err(CatalogError("down".into()))).is_empty()
        );
    }
}

#[cfg(test)]
#[path = "catalog_symbol_tests.rs"]
mod catalog_symbol_tests;
