//! FX + CFD instrument catalog — the oanda twin of `crates/bridges/alpaca/src/catalog.rs`.
//! OANDA's instrument list (`GET /v3/accounts/{account_id}/instruments`) sits behind the same
//! Bearer token every other OANDA REST call needs (see [`crate::rest::OandaRest`]), so
//! `OandaCatalog` holds an `Option<OandaConfig>` rather than being a zero-arg unit struct, and
//! `list_instruments` returns `Ok(vec![])` (never an error) when credentials are absent —
//! mirrors the bridge-wide "absent credentials is the live gate" rule
//! (`config::load_oanda_config_from`). The credential map is INJECTED by the caller (this repo
//! stores them in the gitignored workspace `.env`, which never reaches process env — `new` must
//! never sweep `std::env::vars()` itself).
//!
//! Pure parse (`parse_instruments`, fixture-tested) is split from the authed fetch so the mapping
//! stays testable without network or a token — mirrors alpaca's `catalog` module exactly.
//!
//! # What one v20 `Instrument` row becomes
//!
//! The [`Contract`] is built from the row's FIELDS, never from its name:
//! - `type == "CURRENCY"` -> [`Contract::Fx`]: `pip_size = 10^pipLocation`,
//!   `lot_size` stays `None`: OANDA trades in UNITS and publishes no standard-lot multiplier, and
//!   `FxSpec::lot_size` means the standard lot in base units, so the unit increment lives in
//!   `SymbolProperties::step_size` (`10^-tradeUnitsPrecision`) instead; `swap_long`/`swap_short` =
//!   `financing.longRate`/`shortRate` VERBATIM (the venue's annualised financing rate, a fraction,
//!   not a per-lot points figure).
//! - every other `type` (`CFD`, `METAL`, and what OANDA carries as a CFD: indices, commodities,
//!   bonds) -> [`Contract::Cfd`]: `underlying_kind` is `Metal` for `type == "METAL"`, else read from
//!   the row's `ASSET_CLASS` tag (`INDEX`/`COMMODITY`/`BOND`/`METAL`/`CURRENCY`), else `Other`;
//!   `underlying` is `None` (v20 names no underlying reference), `product` is `Cfd` (OANDA offers no
//!   spread bet through v20), `expiry` is `Rolling` (v20 CFDs are undated and financed overnight —
//!   that is what `financing` is), `margin_rate = marginRate`, swaps as for FX.
//!
//! An absent or malformed field is `None` in the spec (and the struct's absent-is-`0.0` convention
//! in [`SymbolProperties`]), never a guessed default: the parser must survive any JSON
//! (`crates/bridges/oanda/tests/decoder_never_panics.rs`'s `catalog_survives_structured_json`).
//!
//! The row's [`SymbolProperties`] grid comes off the SAME payload: `tick_size =
//! 10^-displayPrecision`, `step_size = 10^-tradeUnitsPrecision`, `min_qty = minimumTradeSize`,
//! `max_qty = maximumOrderUnits`. `type` is kept verbatim as the venue word. `name` (e.g.
//! `EUR_USD`) becomes `raw_symbol`, split on `_` for base/quote (base = first part, quote = second —
//! also correct for a CFD like `SPX500_USD`); `displayName` becomes `description`.

use serde_json::Value;
use vike_catalog::{CatalogError, CatalogMode, CatalogProvider};
use vike_model::{
    AssetClass, CfdExpiry, CfdProduct, CfdSpec, CfdUnderlying, Contract, FxSpec, Instrument,
    SymbolProperties,
};
use vike_secrets::credentials::Environment;

use crate::config::{OandaConfig, load_oanda_config_from};
use crate::rest::OandaRest;

/// The widest decimal exponent a v20 precision/location field is believed. Real rows sit in
/// `-5..=1`; the bound only keeps a garbage integer from producing `0.0`/`inf` as a "grid".
const MAX_DECIMAL_EXPONENT: i64 = 15;

/// `10^exp` as the f64 NEAREST to the decimal value (parsed from `1e{exp}`, so `-4` is exactly the
/// literal `0.0001`, which `powi` does not promise). `None` outside [`MAX_DECIMAL_EXPONENT`].
fn pow10(exp: i64) -> Option<f64> {
    if !(-MAX_DECIMAL_EXPONENT..=MAX_DECIMAL_EXPONENT).contains(&exp) {
        return None;
    }
    format!("1e{exp}").parse().ok()
}

/// An integer field (`pipLocation`, `displayPrecision`, `tradeUnitsPrecision`).
fn int_field(e: &Value, key: &str) -> Option<i64> {
    e.get(key).and_then(Value::as_i64)
}

/// A v20 `DecimalNumber` (a JSON STRING on the wire; a bare number accepted too), finite only.
fn decimal_field(v: Option<&Value>) -> Option<f64> {
    let x = match v? {
        Value::String(s) => s.trim().parse::<f64>().ok()?,
        Value::Number(n) => n.as_f64()?,
        _ => return None,
    };
    x.is_finite().then_some(x)
}

/// A non-negative size (`minimumTradeSize`, `maximumOrderUnits`) for the absent-is-`0.0` grid.
fn size_field(e: &Value, key: &str) -> f64 {
    decimal_field(e.get(key)).filter(|x| *x >= 0.0).unwrap_or(0.0)
}

/// The `name` of the row's `{"type": "ASSET_CLASS", "name": ...}` tag, if it carries one.
fn asset_class_tag(e: &Value) -> Option<&str> {
    e.get("tags")?.as_array()?.iter().find_map(|t| {
        (t.get("type").and_then(Value::as_str) == Some("ASSET_CLASS"))
            .then(|| t.get("name").and_then(Value::as_str))
            .flatten()
    })
}

/// What a non-CURRENCY row is a CFD ON: `type == "METAL"` first, then the `ASSET_CLASS` tag.
fn cfd_underlying(kind: &str, e: &Value) -> CfdUnderlying {
    if kind == "METAL" {
        return CfdUnderlying::Metal;
    }
    match asset_class_tag(e).map(str::to_ascii_uppercase).as_deref() {
        Some("METAL") => CfdUnderlying::Metal,
        Some("INDEX") => CfdUnderlying::Index,
        Some("COMMODITY") => CfdUnderlying::Commodity,
        Some("BOND") => CfdUnderlying::Bond,
        Some("CURRENCY") => CfdUnderlying::Fx,
        _ => CfdUnderlying::Other,
    }
}

/// Parse a `/v3/accounts/{id}/instruments` payload (`{"instruments": [...]}`) into universal
/// [`Instrument`]s, one typed [`Contract`] per row (the module doc is the field map).
pub fn parse_instruments(payload: &Value) -> Vec<Instrument> {
    let Some(list) = payload.get("instruments").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in list {
        let name = e.get("name").and_then(|v| v.as_str()).unwrap_or("");
        if name.is_empty() {
            continue;
        }
        let raw_symbol = name.to_uppercase();
        let kind = e.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let mut parts = raw_symbol.splitn(2, '_');
        let base = parts.next().unwrap_or("").to_string();
        let quote = parts.next().unwrap_or("").to_string();
        let description = e.get("displayName").and_then(|v| v.as_str()).unwrap_or("").to_string();

        let unit_step = int_field(e, "tradeUnitsPrecision").and_then(|p| pow10(p.checked_neg()?));
        let financing = e.get("financing");
        let swap_long = decimal_field(financing.and_then(|f| f.get("longRate")));
        let swap_short = decimal_field(financing.and_then(|f| f.get("shortRate")));
        let contract = if kind == "CURRENCY" {
            Contract::Fx(FxSpec {
                pip_size: int_field(e, "pipLocation").and_then(pow10),
                lot_size: None,
                swap_long,
                swap_short,
            })
        } else {
            Contract::Cfd(CfdSpec {
                underlying_kind: cfd_underlying(kind, e),
                underlying: None,
                product: CfdProduct::Cfd,
                expiry: CfdExpiry::Rolling,
                margin_rate: decimal_field(e.get("marginRate")),
                swap_long,
                swap_short,
            })
        };
        let properties = SymbolProperties {
            tick_size: int_field(e, "displayPrecision")
                .and_then(|p| pow10(p.checked_neg()?))
                .unwrap_or(0.0),
            step_size: unit_step.unwrap_or(0.0),
            min_qty: size_field(e, "minimumTradeSize"),
            max_qty: size_field(e, "maximumOrderUnits"),
            ..Default::default()
        };
        let mut inst = Instrument::new("oanda", raw_symbol, base, quote, contract)
            .with_description(description)
            .with_properties(properties);
        if !kind.is_empty() {
            inst = inst.with_venue_word(kind);
        }
        out.push(inst);
    }
    out
}

/// Authed blocking `GET /v3/accounts/{account_id}/instruments` -> parsed JSON. Mints its own
/// [`OandaRest`] (mirrors alpaca's `fetch_assets`) — a catalog fetch is a one-shot, not a
/// long-lived actor, so there's no reason to share the exec/data threads' client.
fn fetch_instruments(config: &OandaConfig) -> Result<Value, CatalogError> {
    let rest = OandaRest::new(config.api_token.clone());
    rest.get(&config.rest_base, &format!("/v3/accounts/{}/instruments", config.account_id), "")
        .map_err(|e| CatalogError(format!("oanda /v3/instruments: {e}")))
}

/// The oanda venue's `CatalogProvider` contribution: FX + CFD, both off the one
/// `/v3/accounts/{id}/instruments` endpoint. Credential-gated: [`OandaCatalog::new`] loads the
/// demo config once (absent credentials -> `None`, held for the life of the provider);
/// `list_instruments` returns an empty catalog rather than erroring when there's nothing to
/// authenticate with.
pub struct OandaCatalog {
    config: Option<OandaConfig>,
}

impl OandaCatalog {
    /// Resolves the demo config (same tier the exec/data clients use for practice trading) from
    /// the caller-supplied var map — the already-loaded workspace `.env`, NOT the process
    /// environment (which never holds these credentials in this repo). `None` when credentials
    /// are absent — the live gate.
    pub fn new(vars: &std::collections::HashMap<String, String>) -> Self {
        Self { config: load_oanda_config_from(Environment::Demo, vars) }
    }
}

impl CatalogProvider for OandaCatalog {
    fn venue(&self) -> &str {
        "oanda"
    }
    fn asset_classes(&self) -> &[AssetClass] {
        &[AssetClass::Fx, AssetClass::Cfd]
    }
    fn mode(&self) -> CatalogMode {
        CatalogMode::Enumerable
    }
    fn list_instruments(&self) -> Result<Vec<Instrument>, CatalogError> {
        let Some(config) = &self.config else { return Ok(Vec::new()) };
        Ok(parse_instruments(&fetch_instruments(config)?))
    }
}

#[cfg(test)]
mod catalog_tests {
    use super::*;

    /// The docs-shaped fixture (per OANDA v20 docs, no captured payload: its `_provenance` key says
    /// so). Replace it with a captured practice-account body when one is recorded.
    fn fixture() -> Vec<Instrument> {
        let body = include_str!("../tests/fixtures/instruments.json");
        parse_instruments(&serde_json::from_str(body).expect("fixture is JSON"))
    }

    fn row<'a>(rows: &'a [Instrument], sym: &str) -> &'a Instrument {
        rows.iter().find(|i| i.raw_symbol == sym).unwrap_or_else(|| panic!("{sym} present"))
    }

    fn cfd(kind: CfdUnderlying, margin: f64, long: f64, short: f64) -> Contract {
        Contract::Cfd(CfdSpec {
            underlying_kind: kind,
            underlying: None,
            product: CfdProduct::Cfd,
            expiry: CfdExpiry::Rolling,
            margin_rate: Some(margin),
            swap_long: Some(long),
            swap_short: Some(short),
        })
    }

    #[test]
    fn currency_rows_are_fx_with_pip_lot_and_swap_from_the_payload() {
        let rows = fixture();
        assert_eq!(rows.len(), 6);

        let eurusd = row(&rows, "EUR_USD");
        assert_eq!(
            eurusd.contract,
            Contract::Fx(FxSpec {
                pip_size: Some(0.0001),
                lot_size: None,
                swap_long: Some(-0.0507),
                swap_short: Some(0.0284),
            })
        );
        assert_eq!(eurusd.asset_class(), AssetClass::Fx);
        assert_eq!((eurusd.base.as_str(), eurusd.quote.as_str()), ("EUR", "USD"));
        assert_eq!(eurusd.description, "EUR/USD");
        assert_eq!(eurusd.venue, "oanda");
        assert_eq!(eurusd.contract_type.as_deref(), Some("CURRENCY"));
        let p = eurusd.properties;
        assert_eq!((p.tick_size, p.step_size, p.min_qty, p.max_qty), (0.00001, 1.0, 1.0, 1e8));

        // A JPY pair: the pip and the price grid are two decimals coarser — the case a fixed
        // precision gets wrong.
        let usdjpy = row(&rows, "USD_JPY");
        assert_eq!(
            usdjpy.contract,
            Contract::Fx(FxSpec {
                pip_size: Some(0.01),
                lot_size: None,
                swap_long: Some(0.0412),
                swap_short: Some(-0.0661),
            })
        );
        assert_eq!(usdjpy.properties.tick_size, 0.001);
    }

    #[test]
    fn non_currency_rows_are_rolling_cfds_with_the_underlying_read_from_type_and_tags() {
        let rows = fixture();

        let gold = row(&rows, "XAU_USD");
        assert_eq!(gold.contract, cfd(CfdUnderlying::Metal, 0.05, -0.0675, 0.0225));
        assert_eq!(gold.contract_type.as_deref(), Some("METAL"));
        assert_eq!(gold.properties.tick_size, 0.001);
        assert_eq!(gold.properties.max_qty, 500_000.0);

        let spx = row(&rows, "SPX500_USD");
        assert_eq!(spx.contract, cfd(CfdUnderlying::Index, 0.05, -0.0726, 0.0174));
        assert_eq!(spx.asset_class(), AssetClass::Cfd);
        assert_eq!((spx.base.as_str(), spx.quote.as_str()), ("SPX500", "USD"));
        assert_eq!(spx.description, "US SPX 500");
        let p = spx.properties;
        assert_eq!((p.tick_size, p.step_size, p.min_qty, p.max_qty), (0.1, 0.1, 0.1, 10_000.0));

        assert_eq!(row(&rows, "BCO_USD").contract, cfd(CfdUnderlying::Commodity, 0.1, -0.07, 0.02));
        assert_eq!(row(&rows, "USB10Y_USD").contract, cfd(CfdUnderlying::Bond, 0.05, -0.06, 0.01));
    }

    /// A row naming only `name`/`type`/`displayName` (or a `type` the parser does not know) still
    /// parses: every field it does not carry is `None`/`Unknown`-shaped, never a guessed value.
    #[test]
    fn bare_rows_carry_none_rather_than_invented_values() {
        let payload = serde_json::json!({
            "instruments": [
                { "name": "EUR_USD", "type": "CURRENCY", "displayName": "EUR/USD" },
                { "name": "XAU_USD", "type": "METAL", "displayName": "Gold" },
                { "name": "WEIRD_THING", "type": "", "displayName": "Weird",
                  "pipLocation": 9999, "displayPrecision": "5", "minimumTradeSize": "-1",
                  "tags": [{ "type": "KID_ASSET_CLASS", "name": "INDEX" }] }
            ]
        });
        let out = parse_instruments(&payload);
        assert_eq!(out.len(), 3);
        assert_eq!(
            row(&out, "EUR_USD").contract,
            Contract::Fx(FxSpec {
                pip_size: None,
                lot_size: None,
                swap_long: None,
                swap_short: None
            })
        );
        let bare_cfd = |kind| {
            Contract::Cfd(CfdSpec {
                underlying_kind: kind,
                underlying: None,
                product: CfdProduct::Cfd,
                expiry: CfdExpiry::Rolling,
                margin_rate: None,
                swap_long: None,
                swap_short: None,
            })
        };
        assert_eq!(row(&out, "XAU_USD").contract, bare_cfd(CfdUnderlying::Metal));
        let weird = row(&out, "WEIRD_THING");
        assert_eq!(weird.contract, bare_cfd(CfdUnderlying::Other));
        assert_eq!(weird.contract_type, None);
        // `pipLocation` out of range, `displayPrecision` as a string, a negative minimum: all absent.
        assert!(out.iter().all(|i| i.properties == SymbolProperties::default()));
    }

    #[test]
    fn empty_payload_is_empty() {
        assert!(parse_instruments(&serde_json::json!({})).is_empty());
        assert!(parse_instruments(&serde_json::json!({ "instruments": [] })).is_empty());
    }

    /// The catalog must read the credentials it is HANDED, not the process environment.
    /// Before this fix `new()` swept `std::env::vars()`, so a `.env`-only credential set — which
    /// is the ONLY place this repo stores them — resolved to `None` and the venue silently
    /// vanished from the Symbol picker.
    #[test]
    fn catalog_resolves_credentials_from_the_injected_map() {
        let vars: std::collections::HashMap<String, String> = [
            ("OANDA_DEMO_API_KEY", "tok-abc-123"),
            ("OANDA_DEMO_ACCOUNT_ID", "101-004-1234567-001"),
        ]
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        assert!(
            OandaCatalog::new(&vars).config.is_some(),
            "injected credentials must resolve without touching process env"
        );
    }

    /// An empty map means no credentials — never a panic, and never a silent fallback to process
    /// env.
    #[test]
    fn catalog_with_no_credentials_is_configless() {
        let vars = std::collections::HashMap::new();
        assert!(OandaCatalog::new(&vars).config.is_none());
    }
}
