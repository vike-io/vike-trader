//! Public spot + swap/futures/option instrument catalog — the OKX twin of binance's `catalog`
//! module, exposed as the venue's [`CatalogProvider`] contribution to the chart's cross-venue
//! symbol search. Reads the KEYLESS public `/api/v5/public/instruments` (one UNSIGNED GET per
//! `instType` — no credentials, no signer), mapped to universal [`Instrument`]s. The chart's live
//! bar feed ([`crate::feed`]) streams `candle<bar>` on the same dashed instId (`BTC-USDT`),
//! so a searched-and-selected OKX symbol subscribes cleanly. OKX's dashed instIds also naturally
//! sit in a distinct key namespace from binance's concatenated ones, so chart keys never collide.
//!
//! Pure parse (`parse_instruments`, fixture-tested) is split from the blocking `fetch_json` so the
//! mapping stays testable without network — mirrors binance and bybit's catalog modules exactly.
//! OKX's `instType`s (SPOT/SWAP/FUTURES/OPTION) each need a separate fetch (the venue has no
//! single "all instruments" endpoint), so `parse_instruments` is called once per instType. ⚠ OPTION
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
//! `crates/bridges/okx/src/execution/rest/mod.rs`'s `parse_okx_perp_instruments` builds its grid from
//! [`row_grid`], so the picker's row and the exec map can never disagree about what an instId is.
//! They live here rather than in `execution/rest/mod.rs` because this module is on the FEEDS side of the
//! exec/feeds seam and `execution/rest/mod.rs` is not.
//!
//! Identity is read from the payload, never from the instId text: the class from `instType`,
//! margining from `ctType` (OKX publishes `"linear"`/`"inverse"` on SWAP, FUTURES AND OPTION rows —
//! a live BTC-USD option row reads `ctType=inverse`), the expiry from `expTime`, the right and
//! strike from `optType`/`stk`. A row that lacks a field its class NEEDS is dropped rather than
//! given a guessed value.

use serde_json::Value;
use vike_bridge_core::catalog_read::catalog_read_tolerant;
use vike_bridge_core::json::{get_f64, json_int};
use vike_catalog::{CatalogError, CatalogMode, CatalogProvider};
use vike_marketdata::OptionKind;
use vike_model::{
    AssetClass, Contract, ContractFuture, ContractFutureDelivery, ContractFutureFamily,
    ContractFutureTenor, ContractOption, ContractOptionExerciseStyle, ContractPerp, Instrument,
    Margining, SymbolProperties,
};
use vike_venues::venue_urls::{UrlChannel, UrlProtocol, UrlRefusal, UrlType, VenueUrls};

/// The instruments listing path, on a REST base (the exec plane's `crate::execution::rest::PATH_INSTRUMENTS` is
/// the same path; `perp` is `exec`-only and this module is not).
const PATH_INSTRUMENTS: &str = "/api/v5/public/instruments";

/// OKX's OPTION `instType` is the ONE that refuses a bare listing: `?instType=OPTION` answers HTTP
/// 400 code `50015` ("Either parameter uly or instFamily is required"). The tradable families are
/// published by this separate KEYLESS endpoint, whose `data` is a single-element array WRAPPING
/// the list — `{"data": [["SOL-USD", "BTC-USD", …]]}` — not a flat array of strings.
const PATH_UNDERLYING: &str = "/api/v5/public/underlying";

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

/// The per-underlying OPTION listing URL on `base`. ⚠ `uly`, NOT `instFamily`: the venue's 50015
/// message offers both, but only `uly` serves every underlying `/public/underlying` lists — see
/// [`parse_underlyings`] and this function's test.
fn option_instruments_url(base: &str, uly: &str) -> String {
    format!("{base}{PATH_INSTRUMENTS}?instType=OPTION&uly={uly}")
}

/// A non-empty string field of one row, `None` when absent, empty (OKX's "not applicable") or not
/// a string.
fn text(entry: &Value, key: &str) -> Option<String> {
    entry.get(key).and_then(|v| v.as_str()).filter(|s| !s.is_empty()).map(str::to_string)
}

/// An epoch-ms field (`expTime`, a STRING of digits); `None` when absent, empty or not positive.
fn epoch_ms(entry: &Value, key: &str) -> Option<i64> {
    entry.get(key).and_then(json_int).filter(|ms| *ms > 0)
}

/// The asset class of ONE `public/instruments` `data[]` row, from the venue's own `instType` — the
/// field OKX publishes to say WHICH universe a row belongs to. Read per ROW rather than re-derived
/// from the `instId` text (`-SWAP`/`-250926` tails are exactly the implicit encoding decision 0126
/// removes), and the ONE word→class map both [`row_contract`] and the exec plane's grid use.
///
/// An unrecognised non-empty `instType` is `None`, NOT the neighbouring variant. That is where this
/// deliberately differs from `crates/bridges/binance/src/execution/rest/perp.rs`'s `perp_asset_class`, whose
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
/// factor (`ct_val`, applied at `execution/rest/mod.rs`'s `to_contracts`) and its grid must stay byte-identical;
/// [`parse_instruments`] adds [`row_contract_value`] for the catalog row.
pub fn row_grid(entry: &Value) -> SymbolProperties {
    SymbolProperties {
        tick_size: get_f64(entry, "tickSz"),
        step_size: get_f64(entry, "lotSz"),
        min_qty: get_f64(entry, "minSz"),
        max_qty: get_f64(entry, "maxMktSz"),
        asset_class: okx_asset_class(entry),
        // A flat grid (ONE `tickSz`, no tiers), no min-notional, no venue taker hold. FRU rather
        // than an exhaustive literal so a new `SymbolProperties` field costs this parser nothing.
        ..Default::default()
    }
}

/// The value of ONE contract, `ctVal × ctMult`, in the row's `ctValCcy` (which the [`Contract`]
/// carries as `value_ccy`). `0.0` (absent) when `ctVal` is absent — a SPOT row sends both as `""`; an
/// absent `ctMult` is the usual 1 ([`contract_multiplier`], as the exec mount reads it). A live BTC-USD
/// option is `ctVal 1 × ctMult 0.01` = 0.01 BTC; a BTC-USD inverse swap is `100 × 1` = 100 **USD**, not an
/// amount of the base.
pub fn row_contract_value(entry: &Value) -> f64 {
    let v = get_f64(entry, "ctVal") * contract_multiplier(get_f64(entry, "ctMult"));
    if v.is_finite() && v > 0.0 { v } else { 0.0 }
}

/// **A row's `ctMult`, an absent one (read as `0.0`, or anything not a positive number) being the usual 1**: the
/// ONE reading of the multiplier, by the catalog's contract value ([`row_contract_value`]) and the exec mount's
/// (`crate::execution::mount_money`), which read 0 and 1 for an absent one before (defect E11).
pub(crate) fn contract_multiplier(ct_mult: f64) -> f64 {
    if ct_mult.is_finite() && ct_mult > 0.0 { ct_mult } else { 1.0 }
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

/// A FUTURES row's `alias` → [`ContractFutureTenor`]. Live 2026-10-11 the venue lists `this_month`,
/// `next_month`, `quarter`, `next_quarter`, `third_quarter`, `fourth_quarter` and
/// `this_five_years`; its docs add `this_week`/`next_week`. An unknown non-empty word is
/// `ContractFutureTenor::Other`; an empty alias is `None` (the venue said nothing).
fn tenor(entry: &Value) -> Option<ContractFutureTenor> {
    let alias = text(entry, "alias")?;
    Some(match alias.as_str() {
        "this_week" | "next_week" => ContractFutureTenor::Week,
        "this_month" | "next_month" => ContractFutureTenor::Month,
        "quarter" | "next_quarter" | "third_quarter" | "fourth_quarter" => {
            ContractFutureTenor::Quarter
        }
        _ => ContractFutureTenor::Other,
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
        AssetClass::CryptoPerp => Some(Contract::Perp(ContractPerp {
            margining,
            settle,
            value_currency: value_ccy,
            funding_interval_ms: None,
            settling_ms: None,
        })),
        AssetClass::CryptoFuture => Some(Contract::Future(ContractFuture {
            margining,
            settle,
            value_currency: value_ccy,
            expiry_ms: epoch_ms(entry, "expTime")?,
            tenor: tenor(entry),
            delivery: ContractFutureDelivery::Unknown,
            family: ContractFutureFamily::Crypto,
        })),
        AssetClass::Option => {
            let strike = get_f64(entry, "stk");
            if !(strike.is_finite() && strike > 0.0) {
                return None;
            }
            Some(Contract::Option(ContractOption {
                right: OptionKind::from_cp(entry.get("optType").and_then(|v| v.as_str())?)?,
                strike,
                expiry_ms: epoch_ms(entry, "expTime")?,
                style: ContractOptionExerciseStyle::European,
                margining,
                settle,
                value_currency: value_ccy,
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
pub fn parse_instruments(payload: &Value) -> Vec<Instrument> {
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

/// Blocking `GET url` → parsed JSON via the shared [`vike_transport::http::get_json`] (no
/// signer/credentials — every `public/instruments` instType is a keyless public read); called once
/// per instType. Only the `CatalogError` wrap is venue-local.
fn fetch_json(url: &str) -> Result<Value, CatalogError> {
    vike_transport::http::get_json(url).map_err(|e| CatalogError(e.into()))
}

/// [`fetch_json`] on the network a `simulated` read names. Demo and mainnet share the host and the demo is the
/// `x-simulated-trading: 1` header (`crate::execution::rest::simulated`), which changes the LIST the public endpoint serves
/// (measured 2026-10-31: 188 SWAP rows against 532, 130 FUTURES rows against 285, and the USDT-linear dated
/// futures that exist only on the demo). `false` is `fetch_json` itself, so a mainnet read is byte-identical.
fn fetch_json_on(url: &str, simulated: bool) -> Result<Value, CatalogError> {
    let headers: &[(&str, &str)] = if simulated { &[("x-simulated-trading", "1")] } else { &[] };
    vike_transport::http::get_json_with_headers(url, headers).map_err(|e| CatalogError(e.into()))
}

/// The REST base a symbol-facts read asks on `urls`: a `simulated` (DEMO) account's own network, the
/// demo `trade` row (a demo `data` ask would fall back to LIVE: a demo path reaching mainnet), else the
/// live `data` row. Demo and mainnet share the host here; the network is the header `fetch_json_on`
/// sends by the same flag.
fn facts_base(urls: &VenueUrls, simulated: bool) -> Result<&str, CatalogError> {
    let (ty, channel) = if simulated {
        (UrlType::Demo, UrlChannel::Trade)
    } else {
        (UrlType::Live, UrlChannel::Data)
    };
    urls.url(ty, channel, UrlProtocol::Rest, "")
        .map_err(|refusal| CatalogError(refusal.to_string().into()))
}

/// The okx venue's `CatalogProvider` contribution: spot + swap(perp) + futures + options, each a
/// separate `instType` fetch parsed by [`parse_instruments`] (each row's class is its own `instType`). It
/// holds the picker's REST base, resolved once by [`CatalogProviderOkx::from_urls`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogProviderOkx {
    base: String,
}

impl CatalogProviderOkx {
    /// The picker's catalog over `urls`' `okx` `live` `data` `rest` row (the picker lists mainnet; it
    /// is not an admission).
    ///
    /// # Errors
    /// The lookup's refusal (no such row).
    pub fn from_urls(urls: &VenueUrls) -> Result<Self, UrlRefusal> {
        let base = urls.url(UrlType::Live, UrlChannel::Data, UrlProtocol::Rest, "")?;
        Ok(CatalogProviderOkx { base: base.to_string() })
    }

    /// The SWAP rows ALONE: one keyless GET instead of [`CatalogProvider::list_instruments`]'s three
    /// listings plus one per option underlying. For a reader that wants perpetuals only (the
    /// gatewayhub's symbol facts, which keep a listing ten minutes); a failed fetch is the error, not
    /// a partial list. `simulated` reads the DEMO network's list (the account's tier is the network,
    /// `fetch_json_on`) at the demo account's own address; `false` is the public mainnet list
    /// (`facts_base`). A refused lookup is the error, in its words.
    pub fn list_swap_instruments(
        urls: &VenueUrls,
        simulated: bool,
    ) -> Result<Vec<Instrument>, CatalogError> {
        let base = facts_base(urls, simulated)?;
        Ok(parse_instruments(&fetch_json_on(
            &format!("{base}{PATH_INSTRUMENTS}?instType=SWAP"),
            simulated,
        )?))
    }

    /// The row of ONE spot pair or dated future (`?instType=&instId=`, a few hundred bytes against the whole
    /// class's listing: 570 spot rows were 610 KB on the demo host). The class is the instId's own shape
    /// ([`crate::instrument::InstrumentType`]); an id of no such shape (a SWAP, which the whole-list read serves, or
    /// an option) is NO rows, and so is an id the venue does not list or lists as anything but `live` (the
    /// venue answers an unknown id `51001` with an empty `data`, HTTP 200, measured 2026-10-30). The gatewayhub's
    /// symbol facts ask it (decision 0232); a failed fetch is the error, never an empty list. `simulated`
    /// asks the DEMO network (the three demo-only USDT dated futures answer `51001` on mainnet), at the
    /// address [`Self::list_swap_instruments`] asks.
    pub fn instrument_rows(
        urls: &VenueUrls,
        inst_id: &str,
        simulated: bool,
    ) -> Result<Vec<Instrument>, CatalogError> {
        let inst_id = inst_id.to_ascii_uppercase();
        let class = match crate::instrument::InstrumentType::of_inst_id(&inst_id) {
            Some(
                class @ (crate::instrument::InstrumentType::Spot
                | crate::instrument::InstrumentType::Futures),
            ) => class,
            _ => return Ok(Vec::new()),
        };
        let base = facts_base(urls, simulated)?;
        Ok(parse_instruments(&fetch_json_on(
            &format!("{base}{PATH_INSTRUMENTS}?instType={}&instId={inst_id}", class.inst_type()),
            simulated,
        )?))
    }
}

impl CatalogProvider for CatalogProviderOkx {
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
        let base = &self.base;
        let mut out = catalog_read_tolerant(
            ["SPOT", "SWAP", "FUTURES"],
            |inst_type| {
                fetch_json(&format!("{base}{PATH_INSTRUMENTS}?instType={inst_type}"))
                    .map(|v| parse_instruments(&v))
            },
            |inst_type, e| tracing::warn!(target: "vike_okx::catalog", "{inst_type} fetch failed (skipped): {e}"),
        );
        // OPTION is the one instType with no single-call listing (see `parse_underlyings`): one
        // keyless fetch for the families, then one listing per family. Fetching it the way the
        // others are fetched 400s, which is why OKX shipped an empty option catalog and a WARN on
        // every app start until 2026-08-22.
        match fetch_json(&format!("{base}{PATH_UNDERLYING}?instType=OPTION")) {
            Ok(v) => out.extend(catalog_read_tolerant(
                parse_underlyings(&v),
                |uly| fetch_json(&option_instruments_url(base, uly)).map(|v| parse_instruments(&v)),
                |uly, e| {
                    tracing::warn!(
                        target: "vike_okx::catalog",
                        "OPTION fetch failed for underlying {uly} (skipped): {e}"
                    )
                },
            )),
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
        let catalog =
            CatalogProviderOkx::from_urls(&VenueUrls::seed("okx")).expect("the seed's data row");
        let url = option_instruments_url(&catalog.base, "BTC-USD");
        assert_eq!(
            url,
            "https://www.okx.com/api/v5/public/instruments?instType=OPTION&uly=BTC-USD"
        );
        assert!(url.contains("instType=OPTION"), "{url}");
        assert!(url.contains("uly=BTC-USD"), "the bare form is what 400s: {url}");
        assert!(!url.contains("instFamily="), "instFamily 400s on SOL-USD/XAU-USD: {url}");
    }

    /// **Where each read asks.** The picker and a LIVE account's facts read the live `data` row; a DEMO
    /// account's facts read the demo `trade` row (its own network: a demo `data` ask would fall back to
    /// live), the same host here because the header is the network. No row is a refusal in the lookup's
    /// words, never a read of somewhere else.
    #[test]
    fn the_facts_read_the_accounts_own_network_and_the_picker_mainnet() {
        let seed = VenueUrls::seed("okx");
        assert_eq!(
            CatalogProviderOkx::from_urls(&seed).expect("picker").base,
            "https://www.okx.com"
        );
        assert_eq!(facts_base(&seed, false).map_err(|e| e.0), Ok("https://www.okx.com"));
        assert_eq!(facts_base(&seed, true).map_err(|e| e.0), Ok("https://www.okx.com"));
        // a table holding ONLY the live rows: the demo facts are refused, never read on the live row
        let live_only = VenueUrls::seed_typed("okx", UrlType::Live);
        let why = facts_base(&live_only, true).expect_err("no demo trade row").0;
        assert!(why.text().contains("no address row") && why.text().contains("demo"), "{why}");
        assert!(facts_base(&live_only, false).is_ok());
        assert!(CatalogProviderOkx::from_urls(&VenueUrls::default()).is_err());
    }
    /// The request a loopback server saw for `fetch_json_on(.., simulated)`: demo and mainnet share the host, so
    /// the header is the WHOLE switch between the two lists. A mainnet read carries no `x-simulated-trading`
    /// (a live account's list is the list it always read), a demo read carries `1`, and both carry this client's own
    /// User-Agent (the agent's, `vike_transport::http::USER_AGENT`).
    fn request_seen_by(simulated: bool) -> String {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let url = format!(
            "http://{}/api/v5/public/instruments?instType=SWAP",
            listener.local_addr().unwrap()
        );
        let server = std::thread::spawn(move || {
            let (mut conn, _) = listener.accept().expect("accept");
            let mut buffer = [0u8; 4096];
            let n = conn.read(&mut buffer).expect("read");
            let body = r#"{"code":"0","data":[]}"#;
            let _ = conn.write_all(
                format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            );
            String::from_utf8_lossy(&buffer[..n]).to_ascii_lowercase()
        });
        let got = fetch_json_on(&url, simulated).expect("a 200 with JSON is read");
        assert_eq!(got["code"], "0");
        server.join().expect("server")
    }

    #[test]
    fn a_demo_read_sends_the_simulated_trading_header_and_a_mainnet_read_does_not() {
        let demo = request_seen_by(true);
        assert!(demo.contains("x-simulated-trading: 1\r\n"), "{demo}");
        let user_agent = format!("user-agent: {}\r\n", vike_transport::http::USER_AGENT);
        assert!(demo.contains(&user_agent), "{demo}");
        let mainnet = request_seen_by(false);
        assert!(mainnet.contains(&user_agent), "{mainnet}");
        assert!(
            !mainnet.contains("x-simulated-trading"),
            "a live account reads the mainnet list: {mainnet}"
        );
    }

    #[test]
    fn swap_maps_to_cryptoperp_with_dashed_symbol() {
        let payload = serde_json::json!({ "data": [
            { "instId": "BTC-USDT-SWAP", "instType": "SWAP", "baseCcy": "", "quoteCcy": "",
              "ctType": "linear", "settleCcy": "USDT", "state": "live" }
        ]});
        let out = parse_instruments(&payload);
        assert_eq!(out[0].raw_symbol, "BTC-USDT-SWAP");
        assert_eq!(out[0].asset_class(), AssetClass::CryptoPerp);
    }

    #[test]
    fn spot_maps_base_quote_from_ccy_fields() {
        let payload = serde_json::json!({ "data": [
            { "instId": "BTC-USDT", "instType": "SPOT", "baseCcy": "BTC", "quoteCcy": "USDT", "state": "live" }
        ]});
        let out = parse_instruments(&payload);
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
        assert!(parse_instruments(&payload).is_empty(), "{:?}", parse_instruments(&payload));
    }

    /// Every `alias` word the venue listed live on 2026-10-11, plus its documented weekly pair.
    #[test]
    fn every_future_alias_names_a_tenor() {
        let t = |a: &str| tenor(&serde_json::json!({ "alias": a }));
        assert_eq!(t("this_week"), Some(ContractFutureTenor::Week));
        assert_eq!(t("next_week"), Some(ContractFutureTenor::Week));
        assert_eq!(t("this_month"), Some(ContractFutureTenor::Month));
        assert_eq!(t("next_month"), Some(ContractFutureTenor::Month));
        for q in ["quarter", "next_quarter", "third_quarter", "fourth_quarter"] {
            assert_eq!(t(q), Some(ContractFutureTenor::Quarter), "{q}");
        }
        assert_eq!(t("this_five_years"), Some(ContractFutureTenor::Other));
        assert_eq!(t(""), None, "an empty alias is the venue saying nothing");
    }

    /// `vike_catalog::is_inverse_contract` reads the USER name (decision 0290: `BTCUSD.P` is `BTC-USD-SWAP`), for a
    /// process that holds no row; this holds it to every real row of the committed books that has a user name (an
    /// option has none off deribit). A name that parted from its row would be a backtest refused or booked wrong.
    #[test]
    fn the_name_rule_agrees_with_every_row() {
        let (mut inverse, mut dated) = (0, 0);
        let mut inverse_rows: Vec<Instrument> = Vec::new();
        for page in [
            include_str!("../tests/fixtures/instruments/swap.json"),
            include_str!("../tests/fixtures/instruments/futures.json"),
            include_str!("../tests/fixtures/instruments/option.json"),
            include_str!("../tests/fixtures/instruments/spot.json"),
        ] {
            let payload: Value = serde_json::from_str(page).expect("a committed fixture");
            // named as the datahub's write stores them, and so the recorded kept list (`vike_catalog::spelled`)
            for row in vike_catalog::spelled(parse_instruments(&payload)) {
                let Ok(user) = vike_catalog::user_name_of_row(&row) else {
                    assert!(
                        matches!(row.contract, vike_model::Contract::Option(_)),
                        "{}",
                        row.raw_symbol
                    );
                    continue;
                };
                let value = vike_model::ContractValue::of(&row.contract, 1.0);
                let by_row = value.map(|v| v.kind) == Some(vike_model::NotionalKind::Inverse);
                let by_name = vike_catalog::is_inverse_contract("okx", &user);
                assert_eq!(by_name, by_row, "{user} ({})", row.raw_symbol);
                // The catalog's whole value of the symbol with its kept row (what a paper book and a backtest
                // read): the linear 1.0 a mount books (it converts contracts to base and states none), and for an
                // inverse row the value its own row gives, `ctVal · ctMult` dollars a contract. Without the row an
                // inverse name has no value: its face is the row's.
                let want = if by_row {
                    vike_model::ContractValue::of(&row.contract, row.properties.contract_size)
                } else {
                    value
                };
                assert_eq!(
                    vike_catalog::kept_contract_value("okx", &user, Some(&row)),
                    want,
                    "{user}"
                );
                let no_row = if by_row { None } else { value };
                assert_eq!(vike_catalog::contract_value("okx", &user), no_row, "{user}");
                // The settle coin the door's one-coin fence and a backtest's coin ledger read off the
                // name is the row's `settleCcy`.
                let coin_by_row =
                    by_row.then(|| row.contract.settle_asset().map(str::to_string)).flatten();
                let coin_by_name = vike_catalog::inverse_settle_coin("okx", &user);
                assert_eq!(coin_by_name, coin_by_row, "{user}");
                // The delivery guard's expiry is the NAME's day at the venue's hour (B3.02): it is the row's `expTime`.
                let expiry =
                    vike_venues::venue_delivery_guard::delivery_guard_for("okx").expiry_of(&user);
                assert_eq!(expiry, row.contract.expiry_ms(), "{user} ({})", row.raw_symbol);
                dated += usize::from(expiry.is_some());
                inverse += usize::from(by_row);
                if by_row {
                    inverse_rows.push(row);
                }
            }
        }
        assert!(inverse >= 2, "BTC-USD-SWAP and BTC-USD-261030 are in the fixtures");
        assert!(dated >= 2, "BTC-USD-261030 and ETH-USDT-261225 are dated");
        // ...and the RECORDED kept list a test that holds no datahub reads a face from is exactly these inverse
        // rows, row for row, both ways (`vike_catalog::recorded_kept_lists`).
        let recorded = vike_catalog::recorded_kept_lists();
        let recorded = recorded.iter().find(|l| l.venue == "okx").expect("a recorded okx list");
        assert_eq!(recorded.instruments, inverse_rows, "the recorded kept list");
    }

    /// A row's number is read bare or quoted, and anything else is absent (`0.0`); an `expTime` is a
    /// whole number, bare or quoted, and positive, or the dated row is dropped. Pinned on the edge
    /// inputs before the two reads moved onto `vike_bridge_core::json`.
    #[test]
    fn a_row_number_is_bare_or_quoted_and_an_expiry_a_positive_whole_number() {
        use serde_json::json;
        let grid = row_grid(&json!({
            "instType": "SWAP", "tickSz": 0.1, "lotSz": "0.01", "minSz": "x", "maxMktSz": true
        }));
        assert_eq!(
            (grid.tick_size, grid.step_size, grid.min_qty, grid.max_qty),
            (0.1, 0.01, 0.0, 0.0)
        );
        let unread = row_grid(&json!({ "minSz": null, "lotSz": " 1" }));
        assert_eq!((unread.min_qty, unread.step_size), (0.0, 0.0), "null, and no trim");
        assert_eq!(row_contract_value(&json!({ "ctVal": "100", "ctMult": 1 })), 100.0);
        assert_eq!(row_contract_value(&json!({ "ctVal": "", "ctMult": "" })), 0.0);
        // an absent `ctMult` is 1, as the exec mount reads it (E11: the catalog read 0 here)
        assert_eq!(row_contract_value(&json!({ "ctVal": "100", "ctMult": "" })), 100.0);
        assert_eq!(row_contract_value(&json!({ "ctVal": "100" })), 100.0);
        let future = |exp: Option<Value>| {
            let mut row = json!({ "instType": "FUTURES", "ctType": "linear", "settleCcy": "USDT" });
            if let Some(exp) = exp {
                row["expTime"] = exp;
            }
            match row_contract(&row) {
                Some(Contract::Future(spec)) => Some(spec.expiry_ms),
                None => None,
                other => panic!("a FUTURES row is a future or nothing: {other:?}"),
            }
        };
        assert_eq!(future(Some(json!("1767600000000"))), Some(1_767_600_000_000));
        assert_eq!(future(Some(json!(1_767_600_000_000_i64))), Some(1_767_600_000_000));
        for refused in [
            json!("0"),
            json!(0),
            json!("-1"),
            json!(-1),
            json!(""),
            json!(" 1"),
            json!("1.5e12"),
            json!(1.5e12),
            json!(true),
            json!(null),
        ] {
            assert_eq!(future(Some(refused.clone())), None, "{refused}");
        }
        assert_eq!(future(None), None, "absent");
    }
}
