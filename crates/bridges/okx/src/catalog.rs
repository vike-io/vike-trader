//! Public spot + swap/futures/option instrument catalog — the OKX twin of binance's `catalog`
//! module, exposed as the venue's [`CatalogProvider`] contribution to the chart's cross-venue
//! symbol search. Reads the KEYLESS public `/api/v5/public/instruments` (one UNSIGNED GET per
//! `instType` — no credentials, no signer), mapped to universal [`Instrument`]s. The chart's live
//! bar feed ([`crate::market_feed`]) streams `candle<bar>` on the same dashed instId (`BTC-USDT`),
//! so a searched-and-selected OKX symbol subscribes cleanly. OKX's dashed instIds also naturally
//! sit in a distinct key namespace from binance's concatenated ones, so chart keys never collide.
//!
//! Pure parse (`parse_insts`, fixture-tested) is split from the blocking `fetch_json` so the
//! mapping stays testable without network — mirrors binance and bybit's catalog modules exactly.
//! OKX's `instType`s (SPOT/SWAP/FUTURES/OPTION) each need a separate fetch (the venue has no
//! single "all instruments" endpoint), so `parse_insts` is called once per instType. ⚠ OPTION
//! needs one MORE level than the other three: it refuses a bare `instType=OPTION` listing, so it is
//! fetched once per UNDERLYING off the separate keyless `/public/underlying` endpoint — see
//! [`parse_underlyings`].
//!
//! # One `public/instruments` row is read HERE, once (decision 0126)
//!
//! This module also owns the per-ROW reads both planes share: [`okx_asset_class`] (the venue's
//! `instType` word → class), [`row_grid`] (tickSz/lotSz/minSz/maxMktSz) and [`row_contract`] (the
//! typed [`Contract`] built from the row's own fields: `ctType`, `settleCcy`, `ctValCcy`,
//! `expTime`, `alias`, `optType`, `stk`, `uly`). The exec plane's
//! `crates/bridges/okx/src/perp.rs`'s `parse_okx_perp_instruments` builds its grid from
//! [`row_grid`], so the picker's row and the exec map can never disagree about what an instId is.
//! They live here rather than in `perp.rs` because this module is on the FEEDS side of the
//! exec/feeds seam and `perp.rs` is not.
//!
//! Identity is read from the payload, never from the instId text: the class from `instType`,
//! margining from `ctType` (OKX publishes `"linear"`/`"inverse"` on SWAP, FUTURES AND OPTION rows —
//! a live BTC-USD option row reads `ctType=inverse`), the expiry from `expTime`, the right and
//! strike from `optType`/`stk`. A row that lacks a field its class NEEDS is dropped rather than
//! given a guessed value.

use serde_json::Value;
use vike_bridge_core::json::json_num;
use vike_catalog::{CatalogError, CatalogMode, CatalogProvider};
use vike_marketdata::OptionKind;
use vike_model::{
    AssetClass, Contract, Delivery, ExerciseStyle, FutureFamily, FutureSpec, Instrument, Margining,
    OptionSpec, PerpSpec, SymbolProperties, Tenor,
};

const BASE: &str = "https://www.okx.com/api/v5/public/instruments";

/// OKX's OPTION `instType` is the ONE that refuses a bare listing: `?instType=OPTION` answers HTTP
/// 400 code `50015` ("Either parameter uly or instFamily is required"). The tradable families are
/// published by this separate KEYLESS endpoint, whose `data` is a single-element array WRAPPING
/// the list — `{"data": [["SOL-USD", "BTC-USD", …]]}` — not a flat array of strings.
const UNDERLYING: &str = "https://www.okx.com/api/v5/public/underlying";

/// Parse the underlying endpoint's doubly-nested `data`. A shapeless payload yields NOTHING rather
/// than failing: a missing family list must skip options, never drop the rest of the catalog.
pub fn parse_underlyings(payload: &Value) -> Vec<String> {
    payload
        .get("data")
        .and_then(|d| d.as_array())
        .into_iter()
        .flatten()
        .filter_map(|inner| inner.as_array())
        .flatten()
        .filter_map(|f| f.as_str())
        .filter(|f| !f.is_empty())
        .map(str::to_string)
        .collect()
}

/// The per-underlying OPTION listing URL. ⚠ `uly`, NOT `instFamily`: the venue's 50015 message
/// offers both, but only `uly` serves every underlying `/public/underlying` lists — see
/// [`parse_underlyings`] and this function's test.
fn option_instruments_url(uly: &str) -> String {
    format!("{BASE}?instType=OPTION&uly={uly}")
}

/// A non-empty string field of one row, `None` when absent, empty (OKX's "not applicable") or not
/// a string.
fn text(entry: &Value, key: &str) -> Option<String> {
    entry.get(key).and_then(|v| v.as_str()).filter(|s| !s.is_empty()).map(str::to_string)
}

/// A numeric field of one row (OKX sends every number as a STRING), absent/unreadable = `0.0` —
/// `SymbolProperties`' absent convention.
fn num(entry: &Value, key: &str) -> f64 {
    entry.get(key).and_then(json_num).unwrap_or(0.0)
}

/// An epoch-ms field (`expTime`, a STRING of digits); `None` when absent, empty or not positive.
fn epoch_ms(entry: &Value, key: &str) -> Option<i64> {
    let v = entry.get(key)?;
    v.as_str().and_then(|s| s.parse::<i64>().ok()).or_else(|| v.as_i64()).filter(|ms| *ms > 0)
}

/// The asset class of ONE `public/instruments` `data[]` row, from the venue's own `instType` — the
/// field OKX publishes to say WHICH universe a row belongs to. Read per ROW rather than re-derived
/// from the `instId` text (`-SWAP`/`-250926` tails are exactly the implicit encoding decision 0126
/// removes), and the ONE word→class map both [`row_contract`] and the exec plane's grid use.
///
/// An unrecognised non-empty `instType` is `None`, NOT the neighbouring variant. That is where this
/// deliberately differs from `crates/bridges/binance/src/perp.rs`'s `perp_asset_class`, whose
/// `contractType` is a two-way distinction (perpetual, or a dated contract) so "everything else" has
/// exactly one meaning. `instType` is an open list of NAMED universes — `MARGIN` is spot-margin,
/// and OKX may add more — so "everything else" here names nothing, and
/// [`vike_model::SymbolProperties::asset_class`]'s doc argues why a missing answer must stay
/// missing rather than become a guess.
pub fn okx_asset_class(entry: &Value) -> Option<AssetClass> {
    match entry.get("instType").and_then(|t| t.as_str()).unwrap_or("") {
        "SWAP" => Some(AssetClass::CryptoPerp),
        "FUTURES" => Some(AssetClass::CryptoFuture),
        "SPOT" => Some(AssetClass::CryptoSpot),
        "OPTION" => Some(AssetClass::Option),
        _ => None,
    }
}

/// The price/size grid of ONE row: `tickSz`, `lotSz`, `minSz`, `maxMktSz` (on a derivative every
/// size counts CONTRACTS) and the class from [`okx_asset_class`]. Absent fields are `0.0`.
///
/// ⚠ `contract_size` is deliberately NOT set here: the exec plane carries its own contracts→base
/// factor (`ct_val`, applied at `perp.rs`'s `to_contracts`) and its grid must stay byte-identical;
/// [`parse_insts`] adds [`row_contract_value`] for the catalog row.
pub fn row_grid(entry: &Value) -> SymbolProperties {
    SymbolProperties {
        tick_size: num(entry, "tickSz"),
        step_size: num(entry, "lotSz"),
        min_qty: num(entry, "minSz"),
        max_qty: num(entry, "maxMktSz"),
        asset_class: okx_asset_class(entry),
        // A flat grid (ONE `tickSz`, no tiers), no min-notional, no venue taker hold. FRU rather
        // than an exhaustive literal so a new `SymbolProperties` field costs this parser nothing.
        ..Default::default()
    }
}

/// The value of ONE contract, `ctVal × ctMult`, in the row's `ctValCcy` (which the [`Contract`]
/// carries as `value_ccy`). `0.0` (absent) when either factor is absent — a SPOT row sends both
/// as `""`. A live BTC-USD option is `ctVal 1 × ctMult 0.01` = 0.01 BTC; a BTC-USD inverse swap is
/// `100 × 1` = 100 **USD**, not an amount of the base.
pub fn row_contract_value(entry: &Value) -> f64 {
    let v = num(entry, "ctVal") * num(entry, "ctMult");
    if v.is_finite() && v > 0.0 { v } else { 0.0 }
}

/// `ctType` → [`Margining`]. OKX publishes only `"linear"` and `"inverse"`; anything else is
/// `None` (the row is then dropped, never guessed).
fn margining(entry: &Value) -> Option<Margining> {
    match entry.get("ctType").and_then(|v| v.as_str()) {
        Some("linear") => Some(Margining::Linear),
        Some("inverse") => Some(Margining::Inverse),
        _ => None,
    }
}

/// A FUTURES row's `alias` → [`Tenor`]. Live 2026-10-11 the venue lists `this_month`,
/// `next_month`, `quarter`, `next_quarter`, `third_quarter`, `fourth_quarter` and
/// `this_five_years`; its docs add `this_week`/`next_week`. An unknown non-empty word is
/// `Tenor::Other`; an empty alias is `None` (the venue said nothing).
fn tenor(entry: &Value) -> Option<Tenor> {
    let alias = text(entry, "alias")?;
    Some(match alias.as_str() {
        "this_week" | "next_week" => Tenor::Week,
        "this_month" | "next_month" => Tenor::Month,
        "quarter" | "next_quarter" | "third_quarter" | "fourth_quarter" => Tenor::Quarter,
        _ => Tenor::Other,
    })
}

/// The typed [`Contract`] of ONE row, built from its own fields (see the module doc). `None` when
/// the class is unknown or a field the class NEEDS is absent: a perp needs `ctType` and
/// `settleCcy`, a future also `expTime`, an option also `optType` (`C`/`P`) and a finite positive
/// `stk`.
///
/// - SPOT → [`Contract::CryptoSpot`].
/// - SWAP → [`Contract::Perp`]; no funding interval or settling time (the row carries neither: a
///   swap's `expTime` is a delisting schedule, not a settlement).
/// - FUTURES → [`Contract::Future`] (`family: Crypto`) with `delivery: Unknown` — the row carries no settlement-method
///   field.
/// - OPTION → [`Contract::Option`] with `style: European`. That is VENUE KNOWLEDGE, not a payload
///   field: OKX lists only European options (exercised automatically at expiry) and the row has
///   no style key.
pub fn row_contract(entry: &Value) -> Option<Contract> {
    let class = okx_asset_class(entry)?;
    if class == AssetClass::CryptoSpot {
        return Some(Contract::CryptoSpot);
    }
    let margining = margining(entry)?;
    let settle = text(entry, "settleCcy")?;
    let value_ccy = text(entry, "ctValCcy");
    match class {
        AssetClass::CryptoPerp => Some(Contract::Perp(PerpSpec {
            margining,
            settle,
            value_ccy,
            funding_interval_ms: None,
            settling_ms: None,
        })),
        AssetClass::CryptoFuture => Some(Contract::Future(FutureSpec {
            margining,
            settle,
            value_ccy,
            expiry_ms: epoch_ms(entry, "expTime")?,
            tenor: tenor(entry),
            delivery: Delivery::Unknown,
            family: FutureFamily::Crypto,
        })),
        AssetClass::Option => {
            let strike = num(entry, "stk");
            if !(strike.is_finite() && strike > 0.0) {
                return None;
            }
            Some(Contract::Option(OptionSpec {
                right: OptionKind::from_cp(entry.get("optType").and_then(|v| v.as_str())?)?,
                strike,
                expiry_ms: epoch_ms(entry, "expTime")?,
                style: ExerciseStyle::European,
                margining,
                settle,
                value_ccy,
                underlying: text(entry, "uly"),
            }))
        }
        _ => None,
    }
}

/// Parse an OKX `{data:[…]}` payload into catalog rows. `state == "live"` only; a row whose
/// [`row_contract`] is `None` is dropped. The class is the ROW's own `instType`, not the fetch's.
///
/// base/quote come from `baseCcy`/`quoteCcy` (SPOT), else the underlying pair the row publishes
/// (`uly`, then `instFamily` — `"BTC-USD"`), and only then the first two dash segments of `instId`.
/// The grid is [`row_grid`] plus `contract_size` = [`row_contract_value`]; on a SPOT row `max_qty`
/// stays absent because OKX states a spot `maxMktSz` in the QUOTE currency, not the base.
/// `contract_type` carries the venue's `ctType` word verbatim (`"linear"`/`"inverse"`).
pub fn parse_insts(payload: &Value) -> Vec<Instrument> {
    let Some(list) = payload.get("data").and_then(|d| d.as_array()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in list {
        if e.get("state").and_then(|v| v.as_str()) != Some("live") {
            continue;
        }
        let inst_id = e.get("instId").and_then(|v| v.as_str()).unwrap_or("").to_uppercase();
        if inst_id.is_empty() {
            continue;
        }
        let Some(contract) = row_contract(e) else {
            continue;
        };
        let pair =
            text(e, "uly").or_else(|| text(e, "instFamily")).unwrap_or_else(|| inst_id.clone());
        let mut halves = pair.split('-');
        let (pair_base, pair_quote) = (halves.next().unwrap_or(""), halves.next().unwrap_or(""));
        let base = text(e, "baseCcy").as_deref().unwrap_or(pair_base).to_uppercase();
        let quote = text(e, "quoteCcy").as_deref().unwrap_or(pair_quote).to_uppercase();
        let mut properties =
            SymbolProperties { contract_size: row_contract_value(e), ..row_grid(e) };
        if matches!(contract, Contract::CryptoSpot) {
            properties.max_qty = 0.0;
        }
        let mut instrument =
            Instrument::new("okx", inst_id, base, quote, contract).with_properties(properties);
        if let Some(word) = text(e, "ctType") {
            instrument = instrument.with_venue_word(word);
        }
        out.push(instrument);
    }
    out
}

/// Blocking `GET url` → parsed JSON via the shared [`vike_bridge_core::http::get_json`] (no
/// signer/credentials — every `public/instruments` instType is a keyless public read); called once
/// per instType. Only the `CatalogError` wrap is venue-local.
fn fetch_json(url: &str) -> Result<Value, CatalogError> {
    vike_bridge_core::http::get_json(url).map_err(CatalogError)
}

/// The okx venue's `CatalogProvider` contribution: spot + swap(perp) + futures + options, each a
/// separate `instType` fetch parsed by [`parse_insts`] (each row's class is its own `instType`).
pub struct OkxCatalog;

impl OkxCatalog {
    /// The SWAP rows ALONE: one keyless GET instead of [`CatalogProvider::list_instruments`]'s three
    /// listings plus one per option underlying. For a reader that wants perpetuals only (the
    /// gatewayhub's symbol facts, which keep a listing ten minutes); a failed fetch is the error, not
    /// a partial list.
    pub fn list_swap_instruments() -> Result<Vec<Instrument>, CatalogError> {
        Ok(parse_insts(&fetch_json(&format!("{BASE}?instType=SWAP"))?))
    }
}

impl CatalogProvider for OkxCatalog {
    fn venue(&self) -> &str {
        "okx"
    }
    fn asset_classes(&self) -> &[AssetClass] {
        &[
            AssetClass::CryptoSpot,
            AssetClass::CryptoPerp,
            AssetClass::CryptoFuture,
            AssetClass::Option,
        ]
    }
    fn mode(&self) -> CatalogMode {
        CatalogMode::Enumerable
    }
    fn list_instruments(&self) -> Result<Vec<Instrument>, CatalogError> {
        // Per-instType TOLERANT: one endpoint failing must NOT drop the others. Accumulate what
        // succeeds; warn (not fail) on each miss so OKX's spot/perp/futures always populate.
        let mut out = Vec::new();
        for inst_type in ["SPOT", "SWAP", "FUTURES"] {
            match fetch_json(&format!("{BASE}?instType={inst_type}")) {
                Ok(v) => out.extend(parse_insts(&v)),
                Err(e) => {
                    tracing::warn!(target: "vike_okx::catalog", "{inst_type} fetch failed (skipped): {e}")
                }
            }
        }
        // OPTION is the one instType with no single-call listing (see `parse_underlyings`): one
        // keyless fetch for the families, then one listing per family. Fetching it the way the
        // others are fetched 400s, which is why OKX shipped an empty option catalog and a WARN on
        // every app start until 2026-08-22.
        match fetch_json(&format!("{UNDERLYING}?instType=OPTION")) {
            Ok(v) => {
                for uly in parse_underlyings(&v) {
                    match fetch_json(&option_instruments_url(&uly)) {
                        Ok(v) => out.extend(parse_insts(&v)),
                        Err(e) => tracing::warn!(
                            target: "vike_okx::catalog",
                            "OPTION fetch failed for underlying {uly} (skipped): {e}"
                        ),
                    }
                }
            }
            Err(e) => tracing::warn!(
                target: "vike_okx::catalog",
                "OPTION underlying fetch failed (skipped, no options in the catalog): {e}"
            ),
        }
        Ok(out)
    }
}

#[cfg(test)]
mod catalog_tests {
    use super::*;

    /// OKX's OPTION instType REFUSES a bare `instType=OPTION` (HTTP 400, code 50015 "Either
    /// parameter uly or instFamily is required"), so options were never in the catalog AT ALL:
    /// `asset_classes()` advertised `AssetClass::Option` while the picker received zero OKX
    /// options and a WARN fired on every app start. The module already knew — it made the loop
    /// tolerant so spot/perp/futures survive — but never fixed the call. This is the family
    /// endpoint's real payload, captured from the live venue 2026-08-22.
    #[test]
    fn option_underlyings_parse_from_the_nested_data_array() {
        let payload = serde_json::json!({
            "code": "0", "msg": "",
            "data": [["SOL-USD", "BTC-USD", "ETH-USD", "XAU-USD"]]
        });
        assert_eq!(parse_underlyings(&payload), ["SOL-USD", "BTC-USD", "ETH-USD", "XAU-USD"]);
    }

    /// …and a shapeless payload yields nothing rather than panicking — one endpoint failing must
    /// never drop the others, which is the tolerance this module already documents.
    #[test]
    fn option_underlyings_tolerate_a_shapeless_payload() {
        assert!(parse_underlyings(&serde_json::json!({})).is_empty());
        assert!(parse_underlyings(&serde_json::json!({ "data": [] })).is_empty());
        assert!(parse_underlyings(&serde_json::json!({ "data": [[]] })).is_empty());
        assert!(parse_underlyings(&serde_json::json!({ "data": [[""]] })).is_empty());
    }

    /// The URL that produced the 400: an OPTION listing MUST name its underlying. ⚠ It must do so
    /// as `uly`, NOT `instFamily` — the 50015 message offers both, but only `uly` works for every
    /// family the `/public/underlying` endpoint lists. Measured against the live venue 2026-08-22:
    /// `instFamily` serves BTC-USD/ETH-USD and answers `51000 "Parameter instFamily error"` for
    /// SOL-USD and XAU-USD, so pairing the underlying endpoint with `instFamily` reinstated the
    /// forever-warning this fix exists to remove, on two families out of four.
    #[test]
    fn the_option_listing_url_names_its_underlying() {
        let url = option_instruments_url("BTC-USD");
        assert!(url.contains("instType=OPTION"), "{url}");
        assert!(url.contains("uly=BTC-USD"), "the bare form is what 400s: {url}");
        assert!(!url.contains("instFamily="), "instFamily 400s on SOL-USD/XAU-USD: {url}");
    }
    #[test]
    fn swap_maps_to_cryptoperp_with_dashed_symbol() {
        let payload = serde_json::json!({ "data": [
            { "instId": "BTC-USDT-SWAP", "instType": "SWAP", "baseCcy": "", "quoteCcy": "",
              "ctType": "linear", "settleCcy": "USDT", "state": "live" }
        ]});
        let out = parse_insts(&payload);
        assert_eq!(out[0].raw_symbol, "BTC-USDT-SWAP");
        assert_eq!(out[0].asset_class(), AssetClass::CryptoPerp);
    }

    #[test]
    fn spot_maps_base_quote_from_ccy_fields() {
        let payload = serde_json::json!({ "data": [
            { "instId": "BTC-USDT", "instType": "SPOT", "baseCcy": "BTC", "quoteCcy": "USDT", "state": "live" }
        ]});
        let out = parse_insts(&payload);
        assert_eq!(out[0].base, "BTC");
        assert_eq!(out[0].quote, "USDT");
    }

    /// A row missing a field its class NEEDS is dropped, never completed with a guess: a swap
    /// without `ctType` (margining unknown), a future without `expTime`, an option without a
    /// usable `optType`/`stk`, and an `instType` no variant claims (`MARGIN`).
    #[test]
    fn a_row_missing_a_defining_field_is_dropped() {
        let payload = serde_json::json!({ "data": [
            { "instId": "A-USDT-SWAP", "instType": "SWAP", "settleCcy": "USDT", "state": "live" },
            { "instId": "B-USD-261030", "instType": "FUTURES", "ctType": "inverse",
              "settleCcy": "B", "expTime": "", "state": "live" },
            { "instId": "C-USD-261012-1-X", "instType": "OPTION", "ctType": "inverse",
              "settleCcy": "C", "expTime": "1791792000000", "optType": "X", "stk": "1", "state": "live" },
            { "instId": "D-USD-261012-0-C", "instType": "OPTION", "ctType": "inverse",
              "settleCcy": "D", "expTime": "1791792000000", "optType": "C", "stk": "", "state": "live" },
            { "instId": "E-USDT", "instType": "MARGIN", "state": "live" }
        ]});
        assert!(parse_insts(&payload).is_empty(), "{:?}", parse_insts(&payload));
    }

    /// Every `alias` word the venue listed live on 2026-10-11, plus its documented weekly pair.
    #[test]
    fn every_future_alias_names_a_tenor() {
        let t = |a: &str| tenor(&serde_json::json!({ "alias": a }));
        assert_eq!(t("this_week"), Some(Tenor::Week));
        assert_eq!(t("next_week"), Some(Tenor::Week));
        assert_eq!(t("this_month"), Some(Tenor::Month));
        assert_eq!(t("next_month"), Some(Tenor::Month));
        for q in ["quarter", "next_quarter", "third_quarter", "fourth_quarter"] {
            assert_eq!(t(q), Some(Tenor::Quarter), "{q}");
        }
        assert_eq!(t("this_five_years"), Some(Tenor::Other));
        assert_eq!(t(""), None, "an empty alias is the venue saying nothing");
    }
}
