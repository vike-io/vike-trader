//! Public instrument catalog — the Deribit twin of okx's `catalog` module, exposed as the venue's
//! [`CatalogProvider`] contribution to the chart's cross-venue symbol search. Reads the KEYLESS
//! public `/api/v2/public/get_instruments` (UNSIGNED GETs — no credentials, no signer), mapped to
//! universal [`Instrument`]s.
//!
//! Pure parse (`parse_instruments`, fixture-tested) is split from the blocking `fetch_json` so
//! the mapping stays testable without network — mirrors binance/bybit/okx's catalog modules
//! exactly.
//!
//! # What it lists: four requests, each naming its `kind`
//!
//! MEASURED against the live keyless endpoint, 2026-10-04, active rows:
//!
//! | request | rows | becomes |
//! |---|---|---|
//! | `currency=any&kind=future` | 209 = 134 perpetual + 75 dated (26 `day`, 20 `week`, 29 `month`) | [`AssetClass::CryptoPerp`] / [`AssetClass::CryptoFuture`] |
//! | `currency=any&kind=spot` | 57 | [`AssetClass::CryptoSpot`] |
//! | `currency=BTC&kind=option` | 996 | [`AssetClass::Option`] |
//! | `currency=ETH&kind=option` | 836 | [`AssetClass::Option`] |
//!
//! ⚠ **This module used to say Deribit has no all-currencies endpoint, and that was wrong.**
//! `currency=any` is accepted: with a `kind=` it narrows, and without one it answers the whole venue
//! in a single body (209 futures, 57 spot, 5,276 options, 232 combos; the options alone are 4.6 MB).
//! Until 2026-10-04 the catalog asked `currency=BTC` and `currency=ETH` with no `kind`, so it held
//! the two coin-settled perpetuals out of the venue's 134 and no spot pair at all — the 132
//! USDC-settled perpetuals live under `currency=USDC`, which the old list never named.
//!
//! **Every request names a `kind`, and that is what keeps the four disjoint.** A request with no
//! `kind` returns every kind, so an unfiltered `currency=BTC` also answers the BTC-quoted spot
//! rows (four of them) and both combo kinds, and asked beside `kind=future&currency=any` it would
//! return `BTC-PERPETUAL` twice. Two requests that share a `kind` differ in currency (the two
//! option books), so no instrument can arrive on two of them.
//!
//! ## Perpetuals: one variant, three very different products
//!
//! [`AssetClass::CryptoPerp`] covers all 134, and `docs/decisions/0061-an-instrument-names-its-kind.md`
//! keeps it whole. Two settlement modes sit under it — 2 coin-settled `reversed` (`BTC-PERPETUAL`,
//! `ETH-PERPETUAL`) and 132 USDC-settled `linear` (`BTC_USDC-PERPETUAL`) — carried verbatim in
//! [`Instrument::contract_type`]/[`Instrument::settle_asset`]. ⚠ And not all of them are crypto:
//! the venue's own `underlying_type` says 99 are `crypto` and 35 are something else (`equity` 23,
//! `equity_etf` 5, `commodity` 4, `preipo` 2, `crypto_index` 1 — `AAPL_USDC-PERPETUAL`,
//! `GOLD_USDC-PERPETUAL`). The taxonomy has no variant that says so and this catalog does not carry
//! the field, so the picker files an AAPL perpetual under Perps with the rest.
//!
//! ## Options: BTC and ETH only, on purpose
//!
//! The venue lists 5,276 options over seven underlyings (AVAX, BTC, ETH, HYPE, SOL, TRX, XRP) —
//! about 95% of every row it has. 3,444 of them are USDC-settled and are NOT in either request
//! above (that includes 1,212 on BTC and ETH themselves); the two requests above return the
//! coin-margined books only, 996 + 836 = 1,832, exactly what the catalog held before this change.
//! The Options tool owns the option universe through its own chain provider
//! (`crates/bridges/deribit/src/chain.rs`'s `DeribitOptionsProvider`), and this flat symbol list
//! was not widened to duplicate it. What widening would cost, so the decision can be revisited with
//! numbers: the four requests above total 1.83 MB of response bodies (the two unfiltered requests
//! they replaced were 1.77 MB), and swapping the two option books for one `currency=any&kind=option`
//! request — itself 4.6 MB — would take the total to about 4.8 MB. The catalog would grow by 3,444
//! rows, roughly 1 MB of cache JSON at an estimated 300 bytes a row, which
//! `vike_catalog::save_cache` rewrites WHOLE on every refresh and `vike_catalog::load_cache` parses
//! at every start.
//!
//! ## Spot: classified here, not yet claimed by the data path
//!
//! `kind == "spot"` is [`AssetClass::CryptoSpot`] (`BTC_USDC`, `ETH_USDC`; 48 of the 57 are
//! USDC-quoted), so the picker can offer the pair and a spot row's
//! `SymbolProperties::asset_class` is set. ⚠ **`crates/vike-catalog/src/addressing.rs`'s
//! `addressing_for` row for this venue deliberately does NOT list the class** — a spot instrument
//! is only half addressable, MEASURED 2026-10-04 against the public mainnet endpoints:
//!
//! * REST history addresses it: `get_tradingview_chart_data` answers `BTC_USDC` at resolutions 1, 5,
//!   720 and `1D`, the symbol passing through [`crate::data`] verbatim.
//! * the `book.{instrument}.100ms` and `quote.{instrument}` channels subscribe and stream.
//! * the `trades.{instrument}.100ms` and `chart.trades.{instrument}.{resolution}` channels do NOT:
//!   `public/subscribe` answers an EMPTY result for a spot instrument (`agg2`,
//!   `trades.spot.{currency}.100ms` and every other spelling tried matched nothing either; `raw`
//!   needs authentication). `crates/bridges/deribit/src/market_data.rs`'s `classify_rpc_reply`
//!   reads an empty result as an error, which `crates/bridges/deribit/src/market_feed.rs`'s
//!   `bars_main` and `trades_main` turn into a fatal frame and the shared driver into a reconnect
//!   loop — so a spot chart would seed from REST and then never go live.
//!
//! A linear USDC perpetual has no such gap (REST history and all four live channels answer for
//! `BTC_USDC-PERPETUAL`), which is why the `CryptoPerp` already in that row covers the 132 new
//! linear perpetuals without a table change.

use serde_json::Value;
use vike_catalog::{CatalogError, CatalogMode, CatalogProvider, Instrument};
use vike_model::AssetClass;

const BASE: &str = "https://www.deribit.com/api/v2/public/get_instruments";

/// Deribit's all-currencies selector for `currency=`. Accepted with a `kind=` filter (measured
/// 2026-10-04) and, unfiltered, answers every kind at once — see the module doc.
const ANY_CURRENCY: &str = "any";

/// The `kind=` words fetched across EVERY currency: dated futures and perpetuals, and spot pairs.
const ALL_CURRENCY_KINDS: [&str; 2] = ["future", "spot"];

/// The currencies whose `kind=option` book is fetched. Deliberately NOT every currency — see the
/// module doc's options section. (SOL and the other altcoins have no coin-margined options at all:
/// theirs sit in the USDC-settled book, which neither request returns.)
const OPTION_CURRENCIES: [&str; 2] = ["BTC", "ETH"];

/// One `public/get_instruments` URL. `kind` is mandatory by construction: an unfiltered request
/// returns every kind and would overlap the others (module doc).
fn instruments_url(currency: &str, kind: &str) -> String {
    format!("{BASE}?currency={currency}&kind={kind}")
}

/// The whole request set, in fetch order: the all-currency kinds, then one option book per
/// [`OPTION_CURRENCIES`] entry.
fn request_urls() -> Vec<String> {
    let mut urls: Vec<String> =
        ALL_CURRENCY_KINDS.iter().map(|kind| instruments_url(ANY_CURRENCY, kind)).collect();
    urls.extend(OPTION_CURRENCIES.iter().map(|currency| instruments_url(currency, "option")));
    urls
}

/// Classify ONE Deribit instrument row from the VENUE'S OWN words: `kind`, refined for futures by
/// `settlement_period`. `kind == "option"` → [`AssetClass::Option`]; `kind == "spot"` →
/// [`AssetClass::CryptoSpot`]; `kind == "future"` → [`AssetClass::CryptoPerp`] when
/// `settlement_period == "perpetual"`, else [`AssetClass::CryptoFuture`].
///
/// `None` for every other `kind` (`future_combo`, `option_combo`) and for a row carrying no `kind`
/// at all — an honestly ABSENT class, never a guess. The catalog SKIPS such a row (it advertises
/// exactly the four classes above); the grid parser leaves `SymbolProperties::asset_class` unset.
///
/// **This is the venue's ONE classifier.** Both of Deribit's instrument payloads reach it —
/// [`parse_instruments`] below (the plural `public/get_instruments` list) and
/// `crates/bridges/deribit/src/exec.rs`'s `parse_get_instrument` (the singular
/// `public/get_instrument` grid, the row the PIT properties store actually records) — so the two
/// cannot answer differently about the same instrument.
///
/// ⚠ **Never derived from `instrument_name`.** `BTC-PERPETUAL` looks decisive and is exactly the
/// implicit encoding `docs/decisions/0061-an-instrument-names-its-kind.md` exists to remove; the
/// venue publishes both words this reads. Only `kind` decides between a spot pair, an option and
/// a future, and `settlement_period` is consulted for a `future` ALONE — a spot or option row that
/// happens to carry `"perpetual"` stays what its `kind` says.
pub fn asset_class_of(row: &Value) -> Option<AssetClass> {
    match row.get("kind").and_then(|v| v.as_str())? {
        "option" => Some(AssetClass::Option),
        "spot" => Some(AssetClass::CryptoSpot),
        "future" => {
            Some(if row.get("settlement_period").and_then(|v| v.as_str()) == Some("perpetual") {
                AssetClass::CryptoPerp
            } else {
                AssetClass::CryptoFuture
            })
        }
        _ => None,
    }
}

/// Parse a Deribit `{result:[…]}` payload (one request's instruments) into universal
/// [`Instrument`]s. `is_active == false` rows are skipped, and so is every row
/// [`asset_class_of`] cannot classify (`future_combo`/`option_combo`, or a `kind`-less row).
pub fn parse_instruments(payload: &Value) -> Vec<Instrument> {
    let Some(list) = payload.get("result").and_then(|r| r.as_array()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in list {
        if e.get("is_active").and_then(|v| v.as_bool()) == Some(false) {
            continue;
        }
        let Some(class) = asset_class_of(e) else { continue };
        let name = e.get("instrument_name").and_then(|v| v.as_str()).unwrap_or("").to_uppercase();
        if name.is_empty() {
            continue;
        }
        let base = e.get("base_currency").and_then(|v| v.as_str()).unwrap_or("").to_uppercase();
        let quote = e.get("quote_currency").and_then(|v| v.as_str()).unwrap_or("").to_uppercase();
        out.push(Instrument {
            venue: "deribit".into(),
            raw_symbol: name,
            asset_class: class,
            base,
            quote,
            description: String::new(),
            // ⚠ **The venue where one `AssetClass::CryptoPerp` covers BOTH settlement modes**, which
            // `docs/decisions/0061` names as the two-venues-one-word problem: `BTC-PERPETUAL` is
            // coin-settled and a USDC-quoted perpetual is linear, and until now the ONLY signal was
            // the shape of the instrument name. Deribit publishes its own word for it
            // (`instrument_type`: `"reversed"` / `"linear"`) plus the settling currency, and both are
            // carried VERBATIM and UNINTERPRETED. Nothing routes on either — see
            // `vike_catalog::Instrument::contract_type`'s doc for why that is a decision rather
            // than a refactor.
            //
            // A SPOT row publishes `instrument_type: "linear"` too and no settlement currency, so it
            // carries that word and no `settle_asset` — the venue's own answer, kept as given rather
            // than withheld for being odd on a pair that is not a contract.
            properties: Default::default(),
            contract_type: e.get("instrument_type").and_then(|v| v.as_str()).map(str::to_string),
            settle_asset: e
                .get("settlement_currency")
                .and_then(|v| v.as_str())
                .map(|s| s.to_uppercase()),
        });
    }
    out
}

/// Blocking `GET url` → parsed JSON via the shared [`vike_bridge_core::http::get_json`] (no
/// signer/credentials — `public/get_instruments` is a keyless public read); called once per
/// request. Only the `CatalogError` wrap is venue-local.
fn fetch_json(url: &str) -> Result<Value, CatalogError> {
    vike_bridge_core::http::get_json(url).map_err(CatalogError)
}

/// The body of `list_instruments` over an injectable fetch — per-REQUEST TOLERANT: one request
/// failing must NOT drop the others (okx's catalog documents the bug shape this prevents: a `?` on
/// one endpoint wiped the whole venue from the Symbol picker). Accumulate what succeeds; warn (not
/// fail) on each miss. Split from the provider so the tolerance is testable without network.
fn list_tolerant(fetch: impl Fn(&str) -> Result<Value, CatalogError>) -> Vec<Instrument> {
    let mut out = Vec::new();
    for url in request_urls() {
        match fetch(&url) {
            Ok(v) => out.extend(parse_instruments(&v)),
            Err(e) => {
                let query = url.strip_prefix(BASE).unwrap_or(&url);
                tracing::warn!(target: "vike_deribit::catalog", "{query} fetch failed (skipped): {e}")
            }
        }
    }
    out
}

/// The deribit venue's `CatalogProvider` contribution: perpetuals + dated futures + spot pairs of
/// EVERY currency, and the BTC and ETH option books — four requests, tagged via
/// [`parse_instruments`]. Keyless. Per-request tolerant (see [`list_tolerant`]) — mirrors okx's
/// per-instType accumulate.
pub struct DeribitCatalog;

impl CatalogProvider for DeribitCatalog {
    fn venue(&self) -> &str {
        "deribit"
    }
    /// The classes [`Self::list_instruments`] can return — exactly [`asset_class_of`]'s four
    /// outputs, and `a_full_pass_mints_exactly_the_advertised_classes` holds the two lists equal in
    /// both directions (okx advertised `Option` for a long while without ever minting one).
    fn asset_classes(&self) -> &[AssetClass] {
        &[
            AssetClass::Option,
            AssetClass::CryptoFuture,
            AssetClass::CryptoPerp,
            AssetClass::CryptoSpot,
        ]
    }
    fn mode(&self) -> CatalogMode {
        CatalogMode::Enumerable
    }
    fn list_instruments(&self) -> Result<Vec<Instrument>, CatalogError> {
        Ok(list_tolerant(fetch_json))
    }
}

#[cfg(test)]
mod catalog_tests {
    use super::*;

    /// REAL `public/get_instruments` payloads, captured keyless from the live mainnet endpoint
    /// 2026-10-04 and trimmed to a handful of rows each — every row is verbatim venue output, with
    /// ONE exception: `BTC-26MAR27` in `future` carries `is_active: false` / `state: "closed"`
    /// because the live book held no inactive row to capture. Keys: `future` is the
    /// `currency=any&kind=future` envelope, `spot` is `currency=any&kind=spot`, `option_btc` and
    /// `option_eth` are the two option books, and `unfiltered_btc` is what the OLD `currency=BTC`
    /// request returned in shape — every kind mixed, combos included.
    const FIXTURE: &str = include_str!("../tests/fixtures/deribit_get_instruments.json");

    fn envelope(key: &str) -> Value {
        let all: Value = serde_json::from_str(FIXTURE).expect("the fixture is valid JSON");
        all.get(key).unwrap_or_else(|| panic!("fixture has no `{key}` envelope")).clone()
    }

    /// Which fixture envelope answers a given request URL — the injectable fetch the tolerance
    /// tests drive, keyed off the REAL request set so a changed request set cannot go unanswered.
    fn envelope_for(url: &str) -> Option<Value> {
        let key = match url.strip_prefix(BASE)? {
            "?currency=any&kind=future" => "future",
            "?currency=any&kind=spot" => "spot",
            "?currency=BTC&kind=option" => "option_btc",
            "?currency=ETH&kind=option" => "option_eth",
            _ => return None,
        };
        Some(envelope(key))
    }

    fn names(instruments: &[Instrument]) -> Vec<&str> {
        instruments.iter().map(|i| i.raw_symbol.as_str()).collect()
    }

    fn by_name<'a>(instruments: &'a [Instrument], name: &str) -> &'a Instrument {
        instruments
            .iter()
            .find(|i| i.raw_symbol == name)
            .unwrap_or_else(|| panic!("{name} is not in {:?}", names(instruments)))
    }

    fn row(
        name: &str,
        kind: &str,
        settlement_period: &str,
        is_active: bool,
        base: &str,
        quote: &str,
    ) -> Value {
        serde_json::json!({
            "instrument_name": name,
            "kind": kind,
            "settlement_period": settlement_period,
            "is_active": is_active,
            "base_currency": base,
            "quote_currency": quote,
        })
    }

    #[test]
    fn maps_option_perp_future_and_skips_inactive() {
        let payload = serde_json::json!({ "result": [
            row("BTC-27JUN25-60000-C", "option", "month", true, "BTC", "BTC"),
            row("BTC-PERPETUAL", "future", "perpetual", true, "BTC", "USD"),
            row("BTC-27JUN25", "future", "month", true, "BTC", "USD"),
            row("ETH-27JUN25-3000-P", "option", "month", false, "ETH", "ETH"),
        ]});
        let out = parse_instruments(&payload);
        assert_eq!(out.len(), 3);

        assert_eq!(out[0].raw_symbol, "BTC-27JUN25-60000-C");
        assert_eq!(out[0].asset_class, AssetClass::Option);
        assert_eq!(out[0].base, "BTC");
        assert_eq!(out[0].quote, "BTC");

        assert_eq!(out[1].raw_symbol, "BTC-PERPETUAL");
        assert_eq!(out[1].asset_class, AssetClass::CryptoPerp);

        assert_eq!(out[2].raw_symbol, "BTC-27JUN25");
        assert_eq!(out[2].asset_class, AssetClass::CryptoFuture);
    }

    /// The `currency=any&kind=future` shape, row for row: a coin-settled perpetual, a USDC-settled
    /// linear perpetual, a perpetual on an EQUITY, and a dated future of each venue period — and
    /// the one inactive row skipped. Perpetual vs dated is the venue's `settlement_period`, never
    /// the name.
    #[test]
    fn a_real_future_payload_splits_perpetuals_from_dated_futures() {
        let out = parse_instruments(&envelope("future"));
        assert_eq!(
            names(&out),
            [
                "BTC-PERPETUAL",
                "BTC_USDC-PERPETUAL",
                "AAPL_USDC-PERPETUAL",
                "AVAX_USDC-5OCT26",
                "BTC-9OCT26",
                "BTC-30OCT26",
            ],
            "BTC-26MAR27 is inactive and must be skipped"
        );
        // (name, class, base, quote, instrument_type, settlement currency)
        let expected = [
            ("BTC-PERPETUAL", AssetClass::CryptoPerp, "BTC", "USD", "reversed", "BTC"),
            ("BTC_USDC-PERPETUAL", AssetClass::CryptoPerp, "BTC", "USDC", "linear", "USDC"),
            ("AAPL_USDC-PERPETUAL", AssetClass::CryptoPerp, "AAPL", "USDC", "linear", "USDC"),
            ("AVAX_USDC-5OCT26", AssetClass::CryptoFuture, "AVAX", "USDC", "linear", "USDC"),
            ("BTC-9OCT26", AssetClass::CryptoFuture, "BTC", "USD", "reversed", "BTC"),
            ("BTC-30OCT26", AssetClass::CryptoFuture, "BTC", "USD", "reversed", "BTC"),
        ];
        for (name, class, base, quote, contract_type, settle) in expected {
            let i = by_name(&out, name);
            assert_eq!(i.asset_class, class, "{name}");
            assert_eq!((i.base.as_str(), i.quote.as_str()), (base, quote), "{name}");
            assert_eq!(i.contract_type.as_deref(), Some(contract_type), "{name}");
            assert_eq!(i.settle_asset.as_deref(), Some(settle), "{name}");
            assert_eq!(i.venue, "deribit");
        }
    }

    /// The `currency=any&kind=spot` shape: a spot pair is `CryptoSpot` from the venue's `kind`
    /// alone, it carries its own base and quote, and — the venue publishes no settlement currency
    /// on a spot row — no `settle_asset`. The venue's `instrument_type: "linear"` rides along
    /// verbatim, like every other kind's; nothing routes on it.
    #[test]
    fn a_real_spot_payload_is_crypto_spot_with_no_settlement_asset() {
        let out = parse_instruments(&envelope("spot"));
        assert_eq!(names(&out), ["BTC_USDC", "ETH_USDC"]);
        for (name, base) in [("BTC_USDC", "BTC"), ("ETH_USDC", "ETH")] {
            let i = by_name(&out, name);
            assert_eq!(i.asset_class, AssetClass::CryptoSpot, "{name}");
            assert_eq!((i.base.as_str(), i.quote.as_str()), (base, "USDC"), "{name}");
            assert_eq!(i.settle_asset, None, "{name}: a spot row publishes no settlement currency");
            assert_eq!(i.contract_type.as_deref(), Some("linear"), "{name}: the venue's own word");
        }
    }

    /// The two option books keep today's class, base and quote (the coin the premium is quoted in)
    /// and the coin settlement the venue publishes.
    #[test]
    fn a_real_option_payload_stays_an_option() {
        for (key, name, coin) in [
            ("option_btc", "BTC-5OCT26-74000-C", "BTC"),
            ("option_eth", "ETH-5OCT26-2300-C", "ETH"),
        ] {
            let out = parse_instruments(&envelope(key));
            assert_eq!(names(&out), [name]);
            assert_eq!(out[0].asset_class, AssetClass::Option);
            assert_eq!((out[0].base.as_str(), out[0].quote.as_str()), (coin, coin));
            assert_eq!(out[0].contract_type.as_deref(), Some("reversed"));
            assert_eq!(out[0].settle_asset.as_deref(), Some(coin));
        }
    }

    /// What the OLD unfiltered `currency=BTC` request returned in shape — every kind mixed. The
    /// option, the perpetual and the spot pair are kept; the `future_combo` and the `option_combo`
    /// the venue also lists are skipped, because [`asset_class_of`] has no class for either.
    #[test]
    fn a_mixed_payload_skips_both_combo_kinds_and_keeps_the_rest() {
        let payload = envelope("unfiltered_btc");
        let kinds: Vec<&str> = payload["result"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["kind"].as_str().unwrap())
            .collect();
        assert_eq!(
            kinds,
            ["option", "future", "future_combo", "option_combo", "spot"],
            "the fixture must carry both combo kinds or this test proves nothing"
        );
        let out = parse_instruments(&payload);
        assert_eq!(names(&out), ["BTC-5OCT26-74000-C", "BTC-PERPETUAL", "BTC_USDC"]);
        assert_eq!(
            out.iter().map(|i| i.asset_class).collect::<Vec<_>>(),
            [AssetClass::Option, AssetClass::CryptoPerp, AssetClass::CryptoSpot]
        );
    }

    /// The shared classifier, tested on ITS OWN terms rather than only through the catalog's row
    /// loop — `crates/bridges/deribit/src/exec.rs`'s `parse_get_instrument` reaches it directly, so
    /// its `None` arms are a contract two callers now depend on rather than a `continue` detail.
    ///
    /// Every product the venue sells is told apart here by `kind` + `settlement_period` alone: an
    /// option, a perpetual, a dated future of each period the venue uses, and a spot pair.
    #[test]
    fn the_classifier_distinguishes_every_product_by_kind_and_settlement_period() {
        let class = |kind: &str, period: &str| {
            asset_class_of(&serde_json::json!({
                "instrument_name": "BTC-PERPETUAL", "kind": kind, "settlement_period": period
            }))
        };
        assert_eq!(class("option", "month"), Some(AssetClass::Option));
        assert_eq!(class("option", "perpetual"), Some(AssetClass::Option), "kind wins for options");
        assert_eq!(class("spot", "perpetual"), Some(AssetClass::CryptoSpot), "kind wins for spot");
        assert_eq!(class("future", "perpetual"), Some(AssetClass::CryptoPerp));
        for dated in ["day", "week", "month"] {
            assert_eq!(class("future", dated), Some(AssetClass::CryptoFuture), "{dated}");
        }
        // A future whose `settlement_period` the venue omitted is a DATED future, not a perp: only
        // the literal word `perpetual` buys `CryptoPerp`.
        assert_eq!(
            asset_class_of(&serde_json::json!({"kind": "future"})),
            Some(AssetClass::CryptoFuture)
        );
        // A real spot row carries no `settlement_period` key at all and is a spot pair regardless.
        assert_eq!(
            asset_class_of(&serde_json::json!({"kind": "spot", "instrument_name": "BTC_USDC"})),
            Some(AssetClass::CryptoSpot)
        );
        // The two combo kinds, and a row with no `kind`, are UNCLASSIFIED — never guessed.
        for kind in ["future_combo", "option_combo", ""] {
            assert_eq!(class(kind, "perpetual"), None, "{kind}");
        }
        assert_eq!(asset_class_of(&serde_json::json!({})), None);
        assert_eq!(asset_class_of(&serde_json::json!({"kind": 7})), None, "non-string kind");
    }

    /// ⚠ The name is NOT evidence (0061). An instrument NAMED like a perpetual but whose venue
    /// `kind` is `option` is an option, one the venue calls `spot` is spot, and a `future` whose
    /// name reads like an option or a spot pair is still decided by its `settlement_period`.
    #[test]
    fn the_instrument_name_is_never_consulted() {
        let class = |name: &str, kind: &str, period: Option<&str>| {
            let mut r = serde_json::json!({"instrument_name": name, "kind": kind});
            if let Some(p) = period {
                r["settlement_period"] = serde_json::json!(p);
            }
            asset_class_of(&r)
        };
        // Named like a perp, but an option / a spot pair.
        assert_eq!(class("BTC-PERPETUAL", "option", Some("perpetual")), Some(AssetClass::Option));
        assert_eq!(class("BTC-PERPETUAL", "option", None), Some(AssetClass::Option));
        assert_eq!(class("BTC-PERPETUAL", "spot", Some("perpetual")), Some(AssetClass::CryptoSpot));
        // Named like an option or a spot pair, but a future of the period the venue states.
        assert_eq!(
            class("BTC-5OCT26-74000-C", "future", Some("month")),
            Some(AssetClass::CryptoFuture)
        );
        assert_eq!(class("BTC_USDC", "future", Some("perpetual")), Some(AssetClass::CryptoPerp));
        // Named like nothing at all, still a perp when the venue says so.
        assert_eq!(class("", "future", Some("perpetual")), Some(AssetClass::CryptoPerp));
        // And carried through the catalog parse too: a row named `BTC-PERPETUAL` of kind option is
        // an OPTION in the catalog, never a perpetual.
        let payload = serde_json::json!({ "result": [
            row("BTC-PERPETUAL", "option", "perpetual", true, "BTC", "BTC"),
        ]});
        assert_eq!(parse_instruments(&payload)[0].asset_class, AssetClass::Option);
    }

    /// ⚠ **The two-venues-one-word case, made answerable.** `BTC-PERPETUAL` and a USDC-settled
    /// perpetual are both `AssetClass::CryptoPerp` — 0061 keeps that variant WHOLE — so until the
    /// venue's own words were carried, the ONLY thing separating a coin-settled perp from a linear
    /// one anywhere in this tree was the shape of the instrument name.
    ///
    /// Nothing routes on either field; this asserts only that they arrive.
    #[test]
    fn the_venues_own_settlement_words_are_carried_verbatim() {
        let payload = serde_json::json!({ "result": [
            { "instrument_name": "BTC-PERPETUAL", "kind": "future", "settlement_period": "perpetual",
              "is_active": true, "base_currency": "BTC", "quote_currency": "USD",
              "instrument_type": "reversed", "settlement_currency": "BTC" },
            { "instrument_name": "BTC_USDC-PERPETUAL", "kind": "future",
              "settlement_period": "perpetual", "is_active": true, "base_currency": "BTC",
              "quote_currency": "USDC", "instrument_type": "linear",
              "settlement_currency": "usdc" },
        ]});
        let out = parse_instruments(&payload);
        assert_eq!(out[0].asset_class, out[1].asset_class, "one variant covers both, by 0061");
        assert_eq!(out[0].contract_type.as_deref(), Some("reversed"));
        assert_eq!(out[0].settle_asset.as_deref(), Some("BTC"));
        assert_eq!(out[1].contract_type.as_deref(), Some("linear"));
        assert_eq!(out[1].settle_asset.as_deref(), Some("USDC"));
    }

    /// A payload without the two fields leaves both ABSENT — byte-identical to a world without them.
    #[test]
    fn a_payload_without_the_fields_leaves_them_absent() {
        let payload = serde_json::json!({ "result": [
            row("BTC-PERPETUAL", "future", "perpetual", true, "BTC", "USD"),
        ]});
        let out = parse_instruments(&payload);
        assert_eq!(out[0].contract_type, None);
        assert_eq!(out[0].settle_asset, None);
    }

    #[test]
    fn venue_and_uppercases_raw_symbol() {
        let payload = serde_json::json!({ "result": [
            row("btc-perpetual", "future", "perpetual", true, "BTC", "USD"),
        ]});
        let out = parse_instruments(&payload);
        assert_eq!(out[0].venue, "deribit");
        assert_eq!(out[0].raw_symbol, "BTC-PERPETUAL");
    }

    #[test]
    fn empty_or_missing_result_yields_empty() {
        assert!(parse_instruments(&serde_json::json!({})).is_empty());
        assert!(parse_instruments(&serde_json::json!({"result": []})).is_empty());
    }

    /// The request set, spelled out: perpetuals, dated futures and spot across EVERY currency, and
    /// the option books for BTC and ETH only. Every URL names its `kind` — an unfiltered request
    /// would return every kind and overlap the others — and no two URLs are the same query.
    #[test]
    fn the_request_set_widens_perps_and_spot_to_every_currency_and_keeps_options_narrow() {
        let urls = request_urls();
        let b = "https://www.deribit.com/api/v2/public/get_instruments";
        assert_eq!(
            urls,
            [
                format!("{b}?currency=any&kind=future"),
                format!("{b}?currency=any&kind=spot"),
                format!("{b}?currency=BTC&kind=option"),
                format!("{b}?currency=ETH&kind=option"),
            ]
        );
        for url in &urls {
            assert!(url.contains("&kind="), "{url} must name its kind");
        }
        let unique: std::collections::HashSet<_> = urls.iter().collect();
        assert_eq!(unique.len(), urls.len(), "a repeated request would duplicate its rows");
    }

    /// A full pass over the real fixture envelopes — one per request — holds every product kind
    /// the venue sells and not one instrument twice (the four requests are disjoint).
    #[test]
    fn a_full_pass_yields_every_product_once() {
        let out = list_tolerant(|url| envelope_for(url).ok_or_else(|| CatalogError(url.into())));
        let mut sorted = names(&out);
        sorted.sort_unstable();
        let before = sorted.len();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            before,
            "an instrument arrived on two requests: {:?}",
            names(&out)
        );
        assert_eq!(out.len(), 6 + 2 + 1 + 1, "futures + spot + one option per book");
        let count = |class| out.iter().filter(|i| i.asset_class == class).count();
        assert_eq!(count(AssetClass::CryptoPerp), 3);
        assert_eq!(count(AssetClass::CryptoFuture), 3);
        assert_eq!(count(AssetClass::CryptoSpot), 2);
        assert_eq!(count(AssetClass::Option), 2);
    }

    /// `asset_classes()` is what the picker is told this venue sells, so it must be EXACTLY what a
    /// pass can mint — in both directions. A class advertised and never minted is the okx bug (it
    /// advertised `Option` while the picker received none); a class minted and never advertised is
    /// invisible to anything that filters on the advertisement.
    #[test]
    fn a_full_pass_mints_exactly_the_advertised_classes() {
        let out = list_tolerant(|url| envelope_for(url).ok_or_else(|| CatalogError(url.into())));
        let minted: std::collections::HashSet<AssetClass> =
            out.iter().map(|i| i.asset_class).collect();
        let advertised: std::collections::HashSet<AssetClass> =
            DeribitCatalog.asset_classes().iter().copied().collect();
        assert_eq!(minted, advertised);
        assert_eq!(
            DeribitCatalog.asset_classes().len(),
            advertised.len(),
            "a class is advertised twice"
        );
        assert!(
            advertised.contains(&AssetClass::CryptoSpot),
            "spot is advertised since 2026-10-04"
        );
    }

    /// The (g) robustness contract, mirroring okx's per-instType accumulate: ONE request failing
    /// must not wipe the venue. Each of the four requests is failed in turn and the OTHER three
    /// still come back, row for row; every request failing yields an EMPTY catalog (warned), never
    /// an error.
    #[test]
    fn one_failed_request_still_yields_the_other_requests_instruments() {
        let all = list_tolerant(|url| envelope_for(url).ok_or_else(|| CatalogError(url.into())));
        for failing in request_urls() {
            let out = list_tolerant(|url| {
                if url == failing {
                    Err(CatalogError("HTTP 400: simulated".into()))
                } else {
                    envelope_for(url).ok_or_else(|| CatalogError(url.into()))
                }
            });
            let lost = envelope_for(&failing).map(|e| parse_instruments(&e)).unwrap();
            assert!(!lost.is_empty(), "{failing}: the fixture envelope must hold rows");
            assert_eq!(
                out.len(),
                all.len() - lost.len(),
                "{failing} failing must cost exactly its own rows, no more"
            );
            for i in &lost {
                assert!(!names(&out).contains(&i.raw_symbol.as_str()), "{failing}");
            }
        }
        // all requests failing yields an EMPTY catalog (warned), never an error
        assert!(list_tolerant(|_| Err(CatalogError("down".into()))).is_empty());
    }

    /// The old per-currency shape of the same contract, kept: a failing BTC option book still leaves
    /// the ETH one (and everything else).
    #[test]
    fn a_failed_btc_option_book_still_yields_the_eth_one() {
        let out = list_tolerant(|url| {
            if url.contains("currency=BTC") {
                Err(CatalogError("HTTP 400: simulated".into()))
            } else {
                envelope_for(url).ok_or_else(|| CatalogError(url.into()))
            }
        });
        assert!(names(&out).contains(&"ETH-5OCT26-2300-C"), "the ETH book must survive");
        assert!(!names(&out).contains(&"BTC-5OCT26-74000-C"));
        assert!(names(&out).contains(&"BTC_USDC"), "spot must survive an option-book failure");
    }
}
