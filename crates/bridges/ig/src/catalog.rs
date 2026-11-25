//! IG market-search catalog — the FIRST `QueryBacked` `CatalogProvider`. IG lists thousands of
//! epics (FX, indices, shares, commodities, crypto CFDs, options); this provider searches IG's server per
//! query (`GET /markets?searchTerm={query}`) instead of enumerating up front.
//!
//! *Corrected 2026-11-16: this doc said IG has no bulk "list every market" endpoint worth caching. IG replaced its
//! navigation tree with `GET /categories` and paged `GET /categories/{id}/instruments` (measured on the demo
//! gateway 2026-11-16: 9 categories, about 17 reads for the whole list), and the kept list is built from them by
//! `crate::market_list` (decision 0062, amended 2026-11-16). This search provider is still constructed nowhere
//! outside its tests.*
//!
//! It mirrors the credentialed-provider shape of
//! `crates/bridges/oanda/src/catalog.rs` (holds `Option<IgConfig>`, empty when creds are absent)
//! but implements `search_remote` instead of `list_instruments`.
//!
//! Pure parse (`parse_markets`, fixture-tested) is split from the authed fetch (which needs a
//! live `IgSession::login` handshake) so the mapping stays testable without network or a token.
//! `epic` (IG's tradeable id, e.g. `CS.D.EURUSD.MINI.IP`) becomes `raw_symbol` verbatim. The search and list
//! payloads carry no currency, so `base`/`quote` are left empty, except on the two FX forms IG names itself (`pair_of`:
//! `CS.D.<PAIR>.CFD.IP` and `.MINI.IP` of type CURRENCIES), whose epic states the pair: `EURUSD` is base `EUR`, quote
//! `USD`, the quote a paper book reads (`vike_paper::foreign_quote_refusal`, refusing a row of no quote in words).
//! `instrumentName` becomes `description`; `instrumentType` is kept verbatim as the venue word
//! (`Instrument::contract_type`).
//!
//! ## The `Contract` a search row becomes (`contract_of`)
//!
//! Every IG market is an OTC contract with IG as counterparty, so everything but FX is a
//! [`Contract::Cfd`] whose [`ContractCfdUnderlying`] comes from `instrumentType`:
//!
//! | `instrumentType` | contract |
//! |---|---|
//! | `CURRENCIES` | [`Contract::Fx`], every [`ContractFx`] field `None` (the search row carries no pip, lot or swap) |
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
//! ⚠ The search row says only `instrumentType`, and IG types Bitcoin and spot gold `CURRENCIES` (measured on the demo
//! gateway on 2026-10-30), so a `Contract::Fx` row may be a coin or a metal. The admission does not read this row: it
//! judges the market details (`crate::instrument`), whose payload checks, not the type word, decide the class.
//!
//! ## Product and expiry (`product_and_expiry`)
//!
//! The PAYLOAD's `expiry` field wins whenever it is present and non-empty:
//! `"-"` -> [`ContractCfdExpiry::Rolling`] + [`ContractCfdProduct::Cfd`]; `"DFB"` (Daily Funded Bet, a spread-bet
//! product by definition) -> [`ContractCfdExpiry::Dfb`] + [`ContractCfdProduct::SpreadBet`]; any other word (a
//! dated contract such as `"JUN-26"` or `"26-SEP-26"`) -> [`ContractCfdExpiry::Unknown`] + `Cfd`, because
//! neither this crate nor its docs carry a settlement/last-dealing-day convention that would turn a
//! month into an instant (IG's last dealing day varies per market). Only when the field is ABSENT
//! does the epic decide, by exact dot-separated SEGMENT (never a substring):
//! `DAILY` / `TODAY` -> `Dfb` + `SpreadBet` (IG's daily-funded-bet epic forms); `CFD` / `MINI` ->
//! `Rolling` + `Cfd` (IG's undated cash CFD forms); no such segment -> `Unknown` + `Cfd`.
//! A dated row's real expiry instant is a per-epic `GET /markets/{epic}` detail
//! (`instrument.expiryDetails`), which is not fetched here.
//!
//! ## The names a row states (`naming_of`; decision 0290 item 6, the owner's ruling of 2026-11-17)
//!
//! IG names itself: a row states its own naming, and the kept list's writer keeps a stated naming instead of spelling
//! one (`vike_catalog::spelled`). Where IG's market name states the pair (`EUR/USD`, `EUR/USD Mini`), IG's mini
//! `CS.D.<PAIR>.MINI.IP` (type CURRENCIES) is the pair (`EURUSD`) and its standard lot `CS.D.<PAIR>.CFD.IP` (type
//! CURRENCIES, ten times the mini's unit) is the pair + `.CFD` (`EURUSD.CFD`); a pair IG lists with no mini has only its
//! `.CFD` name. A share `*.CASH.IP` is the epic's market code + `.CFD` (`UA.D.AAPL.CASH.IP` is `AAPL.CFD`). A knock-out
//! (type KNOCKOUTS, `CFDBULL`/`CFDBEAR`) and a mini that is no pair have NO name, and say so in words, so no user name
//! resolves to them. Every other epic states nothing and is spelled by the shared grammar: IG's metals typed CURRENCIES
//! (`CS.D.COPPER.CFD.IP`, `Copper`; zinc, lead, aluminium) have no name there, because decision 0290's commodity CFD is
//! the base its row states + `.CFD` and an IG row states none. The list row, the search row (`row_of`) and the
//! operator's row (`crate::operator_row`'s `describe`) all state it here, once.

use serde_json::Value;
use vike_catalog::{CatalogError, CatalogMode, CatalogProvider, CatalogSearchFilter};
use vike_model::{
    AccountTier, AssetClass, Contract, ContractCfd, ContractCfdExpiry, ContractCfdProduct,
    ContractCfdUnderlying, ContractFx, Instrument, InstrumentNaming,
};
use vike_secrets::account_credentials::AccountCredentials;
use vike_venues::venue_urls::{UrlRefusal, VenueUrls};

use crate::config::load_ig_config_from;
use crate::instrument::IgPropertiesSource;

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
        let description = e.get("instrumentName").and_then(|v| v.as_str()).unwrap_or("");
        out.push(row_of(epic, kind, expiry, description));
    }
    out
}

/// **One IG market as a row**: the epic verbatim, the name, the [`Contract`] of `kind` (IG's type word) and
/// `expiry` (`contract_of`), and `kind` as the venue word when there is one. A search row and a list row
/// (`crate::market_list`) are both this.
pub(crate) fn row_of(epic: &str, kind: &str, expiry: Option<&str>, name: &str) -> Instrument {
    let (base, quote) = pair_of(epic, kind).unwrap_or_default();
    let mut instrument = Instrument::new("ig", epic, base, quote, contract_of(kind, epic, expiry))
        .with_description(name);
    instrument.naming = naming_of(epic, kind, name);
    if kind.is_empty() { instrument } else { instrument.with_venue_word(kind) }
}

/// **The pair IG's own epic states**, `(base, quote)`, for the two FX forms whose naming [`naming_of`] states
/// (`CS.D.<PAIR>.CFD.IP`, `CS.D.<PAIR>.MINI.IP` of type CURRENCIES) when the pair's quote half is a currency code
/// (`vike_model::fiat_code`):
/// IG types copper `CURRENCIES` too (`CS.D.COPPER.CFD.IP`, measured in the CI box's list of 2026-11-17), and `PER` is no
/// currency. `None` for every other epic: its row states no quote.
fn pair_of<'a>(epic: &'a str, kind: &str) -> Option<(&'a str, &'a str)> {
    let segments: Vec<&'a str> = epic.split('.').collect();
    let ["CS", "D", pair, "CFD" | "MINI", "IP"] = segments[..] else { return None };
    let named = kind == "CURRENCIES"
        && pair.len() == 6
        && pair.bytes().all(|b| b.is_ascii_uppercase())
        && vike_model::fiat_code(&pair[3..]).is_some();
    named.then(|| pair.split_at(3))
}

/// **IG's own naming of `epic`** of type `kind` and market name `name` (the module doc's "The names a row states"), or
/// `None` where the shared grammar spells it.
pub(crate) fn naming_of(epic: &str, kind: &str, name: &str) -> Option<InstrumentNaming> {
    let named = InstrumentNaming::User;
    let segments: Vec<&str> = epic.split('.').collect();
    // A six-letter segment is a pair only where IG's market name states it as `BASE/QUOTE`, as every one of the 198
    // pair rows of the CI box's `ig.json` does (`EUR/USD`, `EMFX USD/INR ($1 Mini Contract)`; 2026-11-17): IG types copper
    // CURRENCIES too, and `CS.D.COPPER.CFD.IP` is `Copper`, no pair `COP/PER`.
    let is_pair = |pair: &str| {
        pair.len() == 6
            && pair.bytes().all(|b| b.is_ascii_uppercase())
            && name.contains(&format!("{}/{}", &pair[..3], &pair[3..]))
    };
    match (kind, segments.as_slice()) {
        ("KNOCKOUTS", _) => Some(InstrumentNaming::Unnamed(format!(
            "IG's knock-out {epic} has no vike name: knock-outs are not traded (decision 0290)"
        ))),
        ("CURRENCIES", ["CS", "D", pair, "MINI", "IP"]) if is_pair(pair) => {
            Some(named((*pair).to_string()))
        }
        (_, ["CS", "D", _, "MINI", "IP"]) => Some(InstrumentNaming::Unnamed(format!(
            "IG's mini contract {epic} has no vike name (decision 0290)"
        ))),
        ("CURRENCIES", ["CS", "D", pair, "CFD", "IP"]) if is_pair(pair) => {
            Some(named(format!("{pair}{}", vike_catalog::CFD_SUFFIX)))
        }
        ("SHARES", [_, "D", code, "CASH", "IP"])
            if !code.is_empty() && code.bytes().all(|b| b.is_ascii_alphanumeric()) =>
        {
            Some(named(format!("{code}{}", vike_catalog::CFD_SUFFIX)))
        }
        _ => None,
    }
}

/// The [`Contract`] of one search row — the table in the module doc is the authority.
fn contract_of(instrument_type: &str, epic: &str, expiry: Option<&str>) -> Contract {
    let underlying_kind = match instrument_type {
        "CURRENCIES" => {
            return Contract::Fx(ContractFx {
                pip_size: None,
                lot_size: None,
                swap_long: None,
                swap_short: None,
            });
        }
        "INDICES" => ContractCfdUnderlying::Index,
        "SHARES" => ContractCfdUnderlying::Share,
        "COMMODITIES" => ContractCfdUnderlying::Commodity,
        "CRYPTOCURRENCY" => ContractCfdUnderlying::Crypto,
        "BONDS" => ContractCfdUnderlying::Bond,
        _ => ContractCfdUnderlying::Other,
    };
    let (product, expiry) = product_and_expiry(epic, expiry);
    Contract::Cfd(ContractCfd {
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
pub(crate) fn product_and_expiry(
    epic: &str,
    expiry: Option<&str>,
) -> (ContractCfdProduct, ContractCfdExpiry) {
    match expiry {
        Some("-") => (ContractCfdProduct::Cfd, ContractCfdExpiry::Rolling),
        Some("DFB") => (ContractCfdProduct::SpreadBet, ContractCfdExpiry::Dfb),
        Some(_) => (ContractCfdProduct::Cfd, ContractCfdExpiry::Unknown),
        None => {
            let has = |seg: &str| epic.split('.').any(|s| s == seg);
            if has("DAILY") || has("TODAY") {
                (ContractCfdProduct::SpreadBet, ContractCfdExpiry::Dfb)
            } else if has("CFD") || has("MINI") {
                (ContractCfdProduct::Cfd, ContractCfdExpiry::Rolling)
            } else {
                (ContractCfdProduct::Cfd, ContractCfdExpiry::Unknown)
            }
        }
    }
}

/// The ig venue's `CatalogProvider` contribution: `QueryBacked` — thousands of epics, searched
/// live per query rather than enumerated. Credential-gated: [`CatalogProviderIg::new`] loads the demo
/// config once (absent credentials -> `None`, held for the life of the provider); `search_remote`
/// returns an empty result rather than erroring when there's nothing to authenticate with.
///
/// ⚠ **One KEPT session for every search** (`IgPropertiesSource::search`): a picker asks on every debounced keystroke,
/// and this provider used to log in afresh for each ask (`POST /session` per search, in a process that already
/// holds several IG sessions of the same key). The kept session is re-authenticated in place on a 401 and every
/// search is charged to the source's per-minute read cap, so a typist who outruns the cap is told "not now" instead
/// of being a login storm. Nothing in the workspace constructs this provider outside its tests yet, and the process
/// that could (the gatewayhub, the only one holding credentials) has no wire for a picker to ask it through:
/// `crates/bridges/ig/CLAUDE.md`'s "Three more things" carries what is still unbuilt.
pub struct CatalogProviderIg {
    source: Option<IgPropertiesSource>,
}

impl CatalogProviderIg {
    /// Resolves the demo config (same gateway row the exec/data clients use for practice trading)
    /// from the caller-supplied account credentials and the venue's `urls` — NOT the process
    /// environment (which never holds these credentials in this repo). No source when credentials
    /// are absent — the live gate.
    ///
    /// # Errors
    /// The trio is complete and the demo gateway row was refused.
    pub fn new(creds: &AccountCredentials<'_>, urls: &VenueUrls) -> Result<Self, UrlRefusal> {
        let config = load_ig_config_from(AccountTier::Demo, creds, urls)?;
        Ok(Self { source: config.map(IgPropertiesSource::new) })
    }

    /// A catalog over an already-built `source` (the gatewayhub's own kept one, so the picker's searches and the
    /// facts lookups of one account share ONE login and ONE read cap), and the tests'.
    #[must_use]
    pub fn over(source: IgPropertiesSource) -> Self {
        Self { source: Some(source) }
    }
}

impl CatalogProvider for CatalogProviderIg {
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
        _filter: &CatalogSearchFilter,
    ) -> Result<Vec<Instrument>, CatalogError> {
        let Some(source) = &self.source else { return Ok(Vec::new()) };
        source.search(query).map_err(|e| CatalogError(e.carried()))
    }
}

#[path = "catalog_tests.rs"]
#[cfg(test)]
mod catalog_tests;
