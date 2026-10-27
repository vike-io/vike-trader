//! FX + CFD instrument catalog — the oanda twin of `crates/bridges/alpaca/src/catalog.rs`. OANDA's
//! instrument list (`GET /v3/accounts/{account_id}/instruments`) sits behind the same Bearer token
//! every other OANDA REST call needs (see [`crate::rest::OandaRest`]), so `OandaCatalog` holds an
//! `Option<OandaConfig>` rather than being a zero-arg unit struct, and `list_instruments` returns
//! `Ok(vec![])` (never an error) when credentials are absent — mirrors the bridge-wide "absent
//! credentials is the live gate" rule (`config::load_oanda_config_from`). The account's credentials
//! are INJECTED by the caller (an account's fields, decision 0156 item 6 — they never reach process
//! env, and `new` must never sweep `std::env::vars()` itself).
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

use std::collections::BTreeMap;

use serde_json::Value;
use vike_catalog::{CatalogError, CatalogMode, CatalogProvider};
use vike_model::{
    AccountTier, AssetClass, CfdExpiry, CfdProduct, CfdSpec, CfdUnderlying, Contract, FxSpec,
    Instrument, SymbolProperties, now_ms,
};
use vike_secrets::account_credentials::AccountCredentials;

use crate::config::{OandaConfig, load_oanda_config_from};
use crate::home_conversion::{HomeConversions, RateError, quote_currencies_of};
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
pub(crate) fn decimal_field(v: Option<&Value>) -> Option<f64> {
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

/// The account's home currency out of a `/v3/accounts/{id}/summary` body (`account.currency`,
/// upper-cased). `None` when the field is absent or empty - never a guessed `USD`.
fn parse_home_currency(payload: &Value) -> Option<String> {
    let currency = payload.pointer("/account/currency")?.as_str()?.trim();
    (!currency.is_empty()).then(|| currency.to_ascii_uppercase())
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
    /// Resolves the demo config (same hosts the exec/data clients use for practice trading) from
    /// the caller-supplied account credentials — NOT the process environment (which never holds
    /// these credentials in this repo). `None` when credentials are absent — the live gate.
    pub fn new(creds: &AccountCredentials<'_>) -> Self {
        Self { config: load_oanda_config_from(AccountTier::Demo, creds) }
    }

    /// **A non-reversible fingerprint of the ACCOUNT these credentials name** (a 64-bit hash of the
    /// account id), `None` when the account holds no usable credentials. The gatewayhub keys its
    /// per-account instrument/currency cache by it, so an operator who swaps the credentials under the
    /// same label (another OANDA account, with its own list and its own currency) is never answered from
    /// the old account's cache. The account id is hashed here and nothing else leaves: no token, and no
    /// id to log. In-process only (the hash is not stable across Rust releases and is never stored).
    #[must_use]
    pub fn account_fingerprint(&self) -> Option<u64> {
        use std::hash::{DefaultHasher, Hash, Hasher};
        let config = self.config.as_ref()?;
        let mut hasher = DefaultHasher::new();
        config.account_id.hash(&mut hasher);
        Some(hasher.finish())
    }

    /// **The account's own home currency**, one authed `GET /v3/accounts/{account_id}/summary`. The
    /// instrument list names a quote currency per row and the account books money in this one; the
    /// gatewayhub's any-symbol facts admit an instrument only when the two are equal. Unlike
    /// [`CatalogProvider::list_instruments`] this ERRORS when the account holds no credentials: an
    /// unknown currency must never read as "any currency will do".
    pub fn home_currency(&self) -> Result<String, CatalogError> {
        let config = self.require_config()?;
        let rest = OandaRest::new(config.api_token.clone());
        let summary = rest
            .get(&config.rest_base, &format!("/v3/accounts/{}/summary", config.account_id), "")
            .map_err(|e| CatalogError(format!("oanda /v3/summary: {e}")))?;
        parse_home_currency(&summary).ok_or_else(|| {
            CatalogError("oanda /v3/summary: the account summary carries no currency".into())
        })
    }

    /// The account's config, or the error the money reads share: no credentials is an ERROR, never an
    /// empty answer (an empty answer reads as "nothing to convert", and a missing rate must refuse).
    fn require_config(&self) -> Result<&OandaConfig, CatalogError> {
        self.config.as_ref().ok_or_else(|| {
            CatalogError(
                "oanda: this account holds no usable credentials (API_KEY and ACCOUNT_ID)".into(),
            )
        })
    }

    /// **The quote currency of every instrument in the account's OWN list** (`instrument` -> `USD`,
    /// from the `BASE_QUOTE` name: [`crate::quote_currency_of`]). An instrument whose name is not exactly
    /// `BASE_QUOTE` is absent from the map, and absent means UNKNOWN to the caller, never the account's
    /// currency. Errors with no credentials, like [`Self::home_currency`].
    pub fn quote_currencies(&self) -> Result<BTreeMap<String, String>, CatalogError> {
        self.require_config()?;
        Ok(quote_currencies_of(&self.list_instruments()?))
    }

    /// **The rate source for the engine's multi-currency money**: one authed
    /// `GET /v3/accounts/{id}/pricing?instruments=…&includeHomeConversions=true` plus the account's
    /// home currency (`GET /summary`), as a [`HomeConversions`] stamped with the time it was ASKED
    /// (the conservative moment: the rate is no newer than that). `instruments` names the pairs whose
    /// base and quote currencies are wanted (`["USD_JPY"]` for the yen); a canonical `EURUSD` is
    /// spelled `EUR_USD`. Any failure is an `Err`, and no `Err` has a rate in it.
    ///
    /// The pricing REQUEST is used, not a second pricing STREAM: the stream-session cap per account is
    /// unmeasured (`crates/bridges/oanda/CLAUDE.md`, any symbol), and where a stream line carries
    /// `homeConversions` is not shown on OANDA's pages. [`HomeConversions::from_pricing_body`] parses
    /// the same top-level array wherever a body has one.
    pub fn home_conversions(&self, instruments: &[&str]) -> Result<HomeConversions, RateError> {
        self.home_conversions_assuming(instruments, None)
    }

    /// [`Self::home_conversions`] for an account whose summary may not state a home currency (owner ruling
    /// 2026-10-19: "the account currency is what the venue API states; if the API does not state it, or the read
    /// fails: USD"): `assumed_home` (the gatewayhub's USD) is converted into when the summary read comes back
    /// without one, and `None` keeps the refusal. A home currency the summary DOES state is used as stated,
    /// whatever `assumed_home` says: only silence is filled in. OANDA's pricing body converts into the REAL home
    /// currency, so an assumption that is wrong is refused by `HomeConversions::from_pricing_body`'s identity check
    /// (the venue's entry for the home currency itself must say 1), never booked.
    pub fn home_conversions_assuming(
        &self,
        instruments: &[&str],
        assumed_home: Option<&str>,
    ) -> Result<HomeConversions, RateError> {
        let config = self.require_config().map_err(|e| RateError::Unavailable(e.0))?;
        let names: Vec<String> =
            instruments.iter().map(|i| vike_catalog::to_oanda_instrument(i)).collect();
        // The query is pre-formatted text: only an instrument-name alphabet may reach it.
        let clean = |n: &String| {
            !n.is_empty()
                && n.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
        };
        if names.is_empty() || !names.iter().all(clean) {
            return Err(RateError::Unavailable(
                "oanda: home conversions need one or more instrument names (EUR_USD)".into(),
            ));
        }
        let home = match (self.home_currency(), assumed_home) {
            (Ok(stated), _) => stated,
            (Err(_), Some(assumed)) => assumed.to_string(),
            (Err(why), None) => return Err(RateError::Unavailable(why.0)),
        };
        let asked_ms = now_ms();
        let body = OandaRest::new(config.api_token.clone())
            .get(
                &config.rest_base,
                &format!("/v3/accounts/{}/pricing", config.account_id),
                &format!("instruments={}&includeHomeConversions=true", names.join(",")),
            )
            .map_err(|e| RateError::Unavailable(format!("oanda /v3/pricing: {e}")))?;
        HomeConversions::from_pricing_body(&home, &body, asked_ms)
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
    fn the_home_currency_is_the_summarys_own_and_never_a_default() {
        let body = |c: serde_json::Value| serde_json::json!({ "account": { "currency": c } });
        assert_eq!(parse_home_currency(&body(serde_json::json!("usd"))), Some("USD".into()));
        assert_eq!(parse_home_currency(&body(serde_json::json!(" EUR "))), Some("EUR".into()));
        for none in [body(serde_json::json!("")), body(serde_json::json!(7)), serde_json::json!({})]
        {
            assert_eq!(parse_home_currency(&none), None, "{none}");
        }
    }

    /// No credentials is an ERROR here (the listing's `Ok(empty)` would read as "no currency to
    /// fence on"), and it is raised before any request is built.
    #[test]
    fn a_catalog_with_no_credentials_has_no_home_currency() {
        let why = OandaCatalog::new(&AccountCredentials::none()).home_currency().expect_err("none");
        assert!(why.0.contains("no usable credentials"), "{}", why.0);
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
    fn catalog_resolves_credentials_from_the_injected_account() {
        let fields = vike_secrets::account_credentials::CredentialFields::from_pairs([
            ("API_KEY", "tok-abc-123"),
            ("ACCOUNT_ID", "101-004-1234567-001"),
        ]);
        let creds = AccountCredentials::new(Some(1), Some(&fields), None);
        assert!(
            OandaCatalog::new(&creds).config.is_some(),
            "injected credentials must resolve without touching process env"
        );
    }

    /// The fingerprint follows the ACCOUNT id: stable for one account whatever its token (a rotated key is the
    /// same account with the same list), different for another account, absent with no credentials, and it
    /// is a number that carries neither the id nor the token.
    #[test]
    fn the_account_fingerprint_follows_the_account_id_and_nothing_else() {
        use vike_secrets::account_credentials::CredentialFields;
        let of = |token: &str, account: &str| {
            let fields =
                CredentialFields::from_pairs([("API_KEY", token), ("ACCOUNT_ID", account)]);
            OandaCatalog::new(&AccountCredentials::new(Some(1), Some(&fields), None))
                .account_fingerprint()
        };
        let first = of("tok-a", "101-004-1234567-001").expect("credentials");
        assert_eq!(
            of("tok-b", "101-004-1234567-001"),
            Some(first),
            "a rotated token is one account"
        );
        assert_ne!(of("tok-a", "101-004-7654321-001"), Some(first), "another account differs");
        assert_eq!(OandaCatalog::new(&AccountCredentials::none()).account_fingerprint(), None);
    }

    /// An account holding nothing means no credentials — never a panic, and never a silent
    /// fallback to process env.
    #[test]
    fn catalog_with_no_credentials_is_configless() {
        assert!(OandaCatalog::new(&AccountCredentials::none()).config.is_none());
    }

    /// A loopback OANDA double: answers the next of `script` (`(delay before replying, status, body)`) to
    /// each request and keeps every request's head with the moment it was read, in epoch ms.
    /// DOCS-SHAPED bodies, like every OANDA fixture here: nothing was captured from a practice account.
    fn double(
        script: Vec<(u64, u16, &'static str)>,
    ) -> (String, std::thread::JoinHandle<Vec<(String, i64)>>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let base = format!("http://{}", listener.local_addr().expect("assigned port"));
        let served = std::thread::spawn(move || {
            let mut heads = Vec::new();
            for (delay_ms, status, body) in script {
                let Ok((mut sock, _)) = listener.accept() else { break };
                let mut buf = Vec::new();
                let mut chunk = [0_u8; 1024];
                while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    let n = sock.read(&mut chunk).unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                }
                heads.push((String::from_utf8_lossy(&buf).to_string(), now_ms()));
                std::thread::sleep(std::time::Duration::from_millis(delay_ms));
                let reply = format!(
                    "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\
                     connection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(reply.as_bytes());
                let _ = sock.flush();
            }
            heads
        });
        (base, served)
    }

    fn catalog_at(base: String) -> OandaCatalog {
        OandaCatalog {
            config: Some(OandaConfig {
                api_token: "tok-secret".into(),
                account_id: "acct-1".into(),
                rest_base: base.clone(),
                stream_base: base,
            }),
        }
    }

    /// **The rate request and its stamp, over a real HTTP exchange**: the summary is read for the home
    /// currency, then ONE pricing request asks for the instrument in OANDA's spelling with the conversions
    /// flag on, under the account's Bearer token; the rate is the entry's `positionValue` (a fraction of a
    /// dollar per yen), and it is stamped with the moment the request was ASKED - before the pricing request
    /// reached the venue, not when its answer came back (the conservative side: the rate is no newer).
    #[test]
    fn home_conversions_ask_the_pricing_endpoint_and_stamp_the_moment_of_the_ask() {
        const SUMMARY: &str = r#"{"account":{"currency":"USD"}}"#;
        const PRICING: &str = r#"{"time":"1666000000.000000000","prices":[],"homeConversions":[
            {"currency":"USD","accountGain":"1","accountLoss":"1","positionValue":"1"},
            {"currency":"JPY","accountGain":"0.0066610","accountLoss":"0.0066634","positionValue":"0.0066622"}]}"#;
        // The pricing answer is held back 40 ms, so a stamp taken when it ARRIVES would be 40 ms later than
        // the moment the venue read the request.
        let (base, served) = double(vec![(0, 200, SUMMARY), (40, 200, PRICING)]);
        let before = now_ms();
        let conversions = catalog_at(base).home_conversions(&["USDJPY"]).expect("a rate");
        let after = now_ms();
        let heads = served.join().expect("the double ran");

        assert_eq!(heads.len(), 2, "the summary, then one pricing request: {heads:?}");
        assert!(
            heads[0].0.starts_with("GET /v3/accounts/acct-1/summary HTTP/1.1\r\n"),
            "{}",
            heads[0].0
        );
        let (pricing, read_at) = &heads[1];
        assert!(
            pricing.starts_with(
                "GET /v3/accounts/acct-1/pricing?instruments=USD_JPY&includeHomeConversions=true \
                 HTTP/1.1\r\n"
            ),
            "{pricing}"
        );
        assert!(pricing.to_ascii_lowercase().contains("authorization: bearer tok-secret\r\n"));

        let rate = conversions.quote_rate("JPY").expect("the yen has a rate");
        assert_eq!(rate.account_per_quote.get(), 0.0066622, "positionValue, per yen");
        assert_eq!((conversions.home(), rate.quote_ccy.as_str()), ("USD", "JPY"));
        assert!(
            (before..=*read_at).contains(&rate.as_of_ms),
            "stamped at the ask: {before} <= {} <= {read_at}",
            rate.as_of_ms
        );
        assert!(rate.as_of_ms <= after);
    }

    /// **The owner's USD default at the pricing read** (2026-10-19: "the account currency is what the venue API
    /// states; if the API does not state it, or the read fails: USD"): a summary that states no home currency, or
    /// fails, is converted into the ASSUMED home the caller hands in; without an assumption the refusal stands; a
    /// home currency the summary DOES state is used as stated (the golden: a EUR account stays EUR whatever the
    /// caller would assume); and an assumption that is WRONG is refused by OANDA's own identity check, never booked.
    #[test]
    fn a_summary_that_states_no_home_currency_is_converted_into_the_assumed_one() {
        const BARE: &str = r#"{"account":{}}"#;
        const SUMMARY_EUR: &str = r#"{"account":{"currency":"EUR"}}"#;
        const PRICING_USD: &str = r#"{"homeConversions":[
            {"currency":"USD","accountGain":"1","accountLoss":"1","positionValue":"1"},
            {"currency":"JPY","accountGain":"0.0066610","accountLoss":"0.0066634","positionValue":"0.0066622"}]}"#;
        const PRICING_EUR: &str = r#"{"homeConversions":[
            {"currency":"USD","accountGain":"0.92","accountLoss":"0.92","positionValue":"0.92"},
            {"currency":"JPY","accountGain":"0.0061","accountLoss":"0.0061","positionValue":"0.0061"}]}"#;

        // silence is filled in: the pricing read converts into USD, a yen is a fraction of a dollar
        let (base, served) = double(vec![(0, 200, BARE), (0, 200, PRICING_USD)]);
        let rates = catalog_at(base)
            .home_conversions_assuming(&["USDJPY"], Some("USD"))
            .expect("priced on the assumed USD");
        served.join().expect("the double ran");
        assert_eq!(rates.home(), "USD");
        let yen = rates.quote_rate("JPY").expect("the yen");
        assert_eq!(yen.account_per_quote.get(), 0.0066622);

        // no assumption, no rate (the old answer, in the old words)
        let (base, served) = double(vec![(0, 200, BARE)]);
        let why = catalog_at(base).home_conversions_assuming(&["USDJPY"], None).expect_err("none");
        served.join().expect("the double ran");
        assert!(why.to_string().contains("carries no currency"), "{why}");

        // the golden: the summary states EUR, and EUR it stays
        let (base, served) = double(vec![(0, 200, SUMMARY_EUR), (0, 200, PRICING_EUR)]);
        let rates = catalog_at(base)
            .home_conversions_assuming(&["USDJPY"], Some("USD"))
            .expect("a stated EUR");
        served.join().expect("the double ran");
        assert_eq!(rates.home(), "EUR", "only silence is filled in");

        // a WRONG assumption (the real home is EUR, the summary said nothing): the venue's own entry for USD says
        // it is not 1, and the read is refused
        let (base, served) = double(vec![(0, 200, BARE), (0, 200, PRICING_EUR)]);
        let wrong = catalog_at(base).home_conversions_assuming(&["USDJPY"], Some("USD"));
        served.join().expect("the double ran");
        assert!(wrong.expect_err("refused").to_string().contains("must convert at 1"));
    }

    /// A pricing request the venue refuses is an error in words and has no rate in it, and the words carry
    /// neither the token nor the account id.
    #[test]
    fn a_refused_pricing_request_is_no_rate_and_names_no_secret() {
        let refused = r#"{"errorMessage":"Insufficient authorization to perform request."}"#;
        let (base, served) =
            double(vec![(0, 200, r#"{"account":{"currency":"USD"}}"#), (0, 401, refused)]);
        let why = catalog_at(base).home_conversions(&["EURUSD"]).expect_err("no rate");
        served.join().expect("the double ran");
        let RateError::Unavailable(words) = &why else {
            panic!("unavailable, not a rate: {why:?}")
        };
        assert!(words.contains("oanda /v3/pricing") && words.contains("401"), "{words}");
        assert!(!words.contains("tok-secret") && !words.contains("acct-1"), "{words}");
    }
}
