//! IG market-search catalog — the FIRST `QueryBacked` `CatalogProvider`. IG lists thousands of
//! epics (FX, indices, shares, commodities, crypto CFDs, options); there is no bulk "list every
//! market" endpoint worth caching, so instead of enumerating up front we search IG's server per
//! query (`GET /markets?searchTerm={query}`) — mirrors the credentialed-provider shape of
//! `crates/bridges/oanda/src/catalog.rs` (holds `Option<IgConfig>`, empty when creds are absent)
//! but implements `search_remote` instead of `list_instruments`.
//!
//! Pure parse (`parse_markets`, fixture-tested) is split from the authed fetch (which needs a
//! live `IgSession::login` handshake) so the mapping stays testable without network or a token.
//! `epic` (IG's tradeable id, e.g. `CS.D.EURUSD.MINI.IP`) becomes `raw_symbol` verbatim — IG
//! epics aren't a clean base/quote pair and the search payload carries no currency pair, so
//! `base`/`quote` are left empty. `instrumentName` becomes `description`; `instrumentType` is kept
//! verbatim as the venue word (`Instrument::contract_type`).
//!
//! ## The `Contract` a search row becomes (`contract_of`)
//!
//! Every IG market is an OTC contract with IG as counterparty, so everything but FX is a
//! [`Contract::Cfd`] whose [`CfdUnderlying`] comes from `instrumentType`:
//!
//! | `instrumentType` | contract |
//! |---|---|
//! | `CURRENCIES` | [`Contract::Fx`], every [`FxSpec`] field `None` (the search row carries no pip, lot or swap) |
//! | `INDICES` | `Cfd { underlying_kind: Index }` |
//! | `SHARES` | `Cfd { underlying_kind: Share }` — an IG share is a share CFD (or spread bet), NOT a cash equity; the old `AssetClass::Equity` was wrong |
//! | `COMMODITIES` | `Cfd { underlying_kind: Commodity }` (the payload does not separate metals) |
//! | `CRYPTOCURRENCY` | `Cfd { underlying_kind: Crypto }` |
//! | `BONDS` | `Cfd { underlying_kind: Bond }` |
//! | `OPT_*` | `Cfd { underlying_kind: Other }` — IG's listed options are NOT modelled as [`Contract::Option`]: the search row carries no strike, right or expiry instant, and reading them from the epic or the name is the name-parsing 0126 retires |
//! | anything else (`RATES`, `SECTORS`, `BINARY`, `SPRINT_MARKET`, ...) | `Cfd { underlying_kind: Other }` — `RATES` mixes bond futures with short-rate futures, so it is not `Bond` |
//!
//! `underlying`, `margin_rate` and the swaps stay `None`: none of them is in the search row.
//!
//! ## Product and expiry (`product_and_expiry`)
//!
//! The PAYLOAD's `expiry` field wins whenever it is present and non-empty:
//! `"-"` -> [`CfdExpiry::Rolling`] + [`CfdProduct::Cfd`]; `"DFB"` (Daily Funded Bet, a spread-bet
//! product by definition) -> [`CfdExpiry::Dfb`] + [`CfdProduct::SpreadBet`]; any other word (a
//! dated contract such as `"JUN-26"` or `"26-SEP-26"`) -> [`CfdExpiry::Unknown`] + `Cfd`, because
//! neither this crate nor its docs carry a settlement/last-dealing-day convention that would turn a
//! month into an instant (IG's last dealing day varies per market). Only when the field is ABSENT
//! does the epic decide, by exact dot-separated SEGMENT (never a substring):
//! `DAILY` / `TODAY` -> `Dfb` + `SpreadBet` (IG's daily-funded-bet epic forms); `CFD` / `MINI` ->
//! `Rolling` + `Cfd` (IG's undated cash CFD forms); no such segment -> `Unknown` + `Cfd`.
//! A dated row's real expiry instant is a per-epic `GET /markets/{epic}` detail
//! (`instrument.expiryDetails`), which is not fetched here.

use serde_json::Value;
use vike_catalog::{CatalogError, CatalogMode, CatalogProvider, SearchFilter};
use vike_model::{
    AccountTier, AssetClass, CfdExpiry, CfdProduct, CfdSpec, CfdUnderlying, Contract, FxSpec,
    Instrument,
};
use vike_secrets::account_credentials::AccountCredentials;

use crate::config::{IgConfig, load_ig_config_from};
use crate::rest::IgSession;

/// Parse a `/markets?searchTerm=...` payload (`{"markets": [...]}`) into universal
/// [`Instrument`]s. Entries with an empty/missing `epic` are skipped. Never panics on missing
/// fields.
pub fn parse_markets(payload: &Value) -> Vec<Instrument> {
    let Some(list) = payload.get("markets").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in list {
        let epic = e.get("epic").and_then(|v| v.as_str()).unwrap_or("");
        if epic.is_empty() {
            continue;
        }
        let kind = e.get("instrumentType").and_then(|v| v.as_str()).unwrap_or("");
        let expiry = e.get("expiry").and_then(|v| v.as_str()).filter(|s| !s.is_empty());
        let description =
            e.get("instrumentName").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let mut inst = Instrument::new("ig", epic, "", "", contract_of(kind, epic, expiry))
            .with_description(description);
        if !kind.is_empty() {
            inst = inst.with_venue_word(kind);
        }
        out.push(inst);
    }
    out
}

/// The [`Contract`] of one search row — the table in the module doc is the authority.
fn contract_of(instrument_type: &str, epic: &str, expiry: Option<&str>) -> Contract {
    let underlying_kind = match instrument_type {
        "CURRENCIES" => {
            return Contract::Fx(FxSpec {
                pip_size: None,
                lot_size: None,
                swap_long: None,
                swap_short: None,
            });
        }
        "INDICES" => CfdUnderlying::Index,
        "SHARES" => CfdUnderlying::Share,
        "COMMODITIES" => CfdUnderlying::Commodity,
        "CRYPTOCURRENCY" => CfdUnderlying::Crypto,
        "BONDS" => CfdUnderlying::Bond,
        _ => CfdUnderlying::Other,
    };
    let (product, expiry) = product_and_expiry(epic, expiry);
    Contract::Cfd(CfdSpec {
        underlying_kind,
        underlying: None,
        product,
        expiry,
        margin_rate: None,
        swap_long: None,
        swap_short: None,
    })
}

/// Product and expiry of a CFD row: the payload's `expiry` word first, the epic's segments only
/// when that word is absent (module doc, "Product and expiry").
fn product_and_expiry(epic: &str, expiry: Option<&str>) -> (CfdProduct, CfdExpiry) {
    match expiry {
        Some("-") => (CfdProduct::Cfd, CfdExpiry::Rolling),
        Some("DFB") => (CfdProduct::SpreadBet, CfdExpiry::Dfb),
        Some(_) => (CfdProduct::Cfd, CfdExpiry::Unknown),
        None => {
            let has = |seg: &str| epic.split('.').any(|s| s == seg);
            if has("DAILY") || has("TODAY") {
                (CfdProduct::SpreadBet, CfdExpiry::Dfb)
            } else if has("CFD") || has("MINI") {
                (CfdProduct::Cfd, CfdExpiry::Rolling)
            } else {
                (CfdProduct::Cfd, CfdExpiry::Unknown)
            }
        }
    }
}

/// Log in, then authed `GET /markets?searchTerm={query}` -> parsed JSON. Mints its own
/// [`IgSession`] per search (a catalog search is a one-shot, not a long-lived actor — mirrors
/// `OandaCatalog::fetch_instruments` minting its own `OandaRest`).
fn fetch_markets(config: &IgConfig, query: &str) -> Result<Value, CatalogError> {
    let session =
        IgSession::login(config).map_err(|e| CatalogError(format!("ig /session login: {e}")))?;
    let q = format!("searchTerm={}", vike_util::percent::percent_encode(query));
    session.get("/markets", "1", &q).map_err(|e| CatalogError(format!("ig /markets: {e}")))
}

/// The ig venue's `CatalogProvider` contribution: `QueryBacked` — thousands of epics, searched
/// live per query rather than enumerated. Credential-gated: [`IgCatalog::new`] loads the demo
/// config once (absent credentials -> `None`, held for the life of the provider); `search_remote`
/// returns an empty result rather than erroring when there's nothing to authenticate with.
pub struct IgCatalog {
    config: Option<IgConfig>,
}

impl IgCatalog {
    /// Resolves the demo config (same gateway the exec/data clients use for practice trading) from
    /// the caller-supplied account credentials — NOT the process environment (which never holds
    /// these credentials in this repo). `None` when credentials are absent — the live gate.
    pub fn new(creds: &AccountCredentials<'_>) -> Self {
        Self { config: load_ig_config_from(AccountTier::Demo, creds) }
    }
}

impl CatalogProvider for IgCatalog {
    fn venue(&self) -> &str {
        "ig"
    }
    fn asset_classes(&self) -> &[AssetClass] {
        // Every row is `Contract::Fx` or `Contract::Cfd` (module doc): IG lists no cash equity or
        // index instrument.
        &[AssetClass::Fx, AssetClass::Cfd]
    }
    fn mode(&self) -> CatalogMode {
        CatalogMode::QueryBacked
    }
    // list_instruments: NOT overridden — QueryBacked venues use the trait default (empty).
    fn search_remote(
        &self,
        query: &str,
        _filter: &SearchFilter,
    ) -> Result<Vec<Instrument>, CatalogError> {
        let Some(config) = &self.config else { return Ok(Vec::new()) };
        Ok(parse_markets(&fetch_markets(config, query)?))
    }
}

#[path = "catalog_tests.rs"]
#[cfg(test)]
mod catalog_tests;
