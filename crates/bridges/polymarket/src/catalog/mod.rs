//! `catalog` — a searchable in-memory registry over Gamma markets (the reusable discovery core the
//! future UI panel + the rolling 5-min start both consume). Search resolves a market by name; its
//! `token_ids` feed the existing subscribe seam. (spec: 2026-07-11-gamma-catalog-design.md)
//!
//! Also hosts [`CatalogProviderPolymarket`], the venue's [`vike_catalog::CatalogProvider`] contribution
//! (crate-reorg catalog workstream): wraps the SAME Gamma catalog fetch — one [`Instrument`] per
//! outcome CLOB `token_id` (the tradable unit the subscribe seam and order submission key off of),
//! typed as a [`Contract::PredictionOutcome`] (condition id, outcome, token id, resolution time,
//! neg-risk as sent) with the tick and minimum size from the same Gamma row, so Polymarket markets
//! appear in the Symbol picker. Catalog-only: the signing/exec path reads none of it.

pub mod discovery;
pub mod gamma;
pub mod market_ref;
pub mod neg_risk_set;
pub mod rolling_family;
pub mod rules;
pub mod universe;

use crate::catalog::gamma::{GammaClient, GammaMarket};
use crate::catalog::neg_risk_set::NegRiskSet;
use vike_bridge_core::catalog_read::{CatalogPage, CatalogPageWalk, CatalogPageWalkEnd};
use vike_catalog::{CatalogError, CatalogMode, CatalogProvider};
use vike_model::{
    AssetClass, Contract, ContractPredictionOutcome, Instrument, InstrumentVenueIds,
    SymbolProperties,
};
use vike_venues::venue_urls::{UrlRefusal, VenueUrls};

/// An in-memory registry over fetched Gamma markets, with case-insensitive name search.
pub struct MarketCatalog {
    markets: Vec<GammaMarket>,
}

impl MarketCatalog {
    pub fn from_markets(markets: Vec<GammaMarket>) -> Self {
        Self { markets }
    }

    pub fn markets(&self) -> &[GammaMarket] {
        &self.markets
    }

    /// Case-insensitive substring match over `question` + `slug` — the discovery verb.
    pub fn search(&self, query: &str) -> Vec<&GammaMarket> {
        let q = query.to_lowercase();
        self.markets
            .iter()
            .filter(|m| {
                m.question.to_lowercase().contains(&q) || m.slug.to_lowercase().contains(&q)
            })
            .collect()
    }

    /// Look a market up by its CTF condition id.
    pub fn by_condition(&self, condition_id: &str) -> Option<&GammaMarket> {
        self.markets.iter().find(|m| m.condition_id == condition_id)
    }

    /// Every neg-risk SET in the catalog — the registry's mutually-exclusive-outcome view, grouped
    /// by `negRiskMarketID` and ordered by it (see [`NegRiskSet::group`]). Non-neg-risk markets
    /// contribute nothing.
    ///
    /// ⚠ Sets built from a paged `/markets` browse are frequently INCOMPLETE (the browse is
    /// volume-ordered and splits groups), which is why every returned set carries
    /// [`NegRiskSet::completeness`] and refuses to report a Σ or an edge unless complete. Build the
    /// catalog from [`crate::catalog::gamma::GammaClient::markets_by_event_slug`] when a complete set
    /// matters.
    pub fn neg_risk_sets(&self) -> Vec<NegRiskSet> {
        NegRiskSet::group(&self.markets)
    }

    /// Resolve ONE neg-risk set by its `negRiskMarketID` (case-insensitive — the hex is a
    /// contract address-space value that different Gamma fields have been seen to render with
    /// differing case). `None` when no market in the catalog carries that key.
    pub fn neg_risk_set(&self, neg_risk_market_id: &str) -> Option<NegRiskSet> {
        let want = neg_risk_market_id.to_lowercase();
        NegRiskSet::group(&self.markets).into_iter().find(|s| s.market_id.to_lowercase() == want)
    }

    /// The neg-risk set a given market belongs to, looked up by its CTF condition id — the
    /// "what else is mutually exclusive with this thing I hold?" verb.
    pub fn neg_risk_set_for_condition(&self, condition_id: &str) -> Option<NegRiskSet> {
        let m = self.by_condition(condition_id)?;
        if !m.is_neg_risk_member() {
            return None;
        }
        self.neg_risk_set(&m.neg_risk_market_id)
    }
}

/// Pure map: Gamma markets → one [`Instrument`] per outcome CLOB token, each a
/// [`Contract::PredictionOutcome`] built from the Gamma payload FIELDS (never from a name).
///
/// - `raw_symbol` and [`ContractPredictionOutcome::token_id`] are the outcome's CLOB `token_id` verbatim (the
///   venue-native tradable id the subscribe seam and order submission key off of).
/// - [`ContractPredictionOutcome::condition_id`] is `conditionId`; [`ContractPredictionOutcome::outcome`] the outcome
///   label; [`ContractPredictionOutcome::resolution_ms`] is `endDate` via [`GammaMarket::resolution_ts_ms`]
///   (`None` when absent or unparseable).
/// - [`ContractPredictionOutcome::neg_risk`] is `negRisk` as SENT ([`GammaMarket::neg_risk_declared`]):
///   absent stays `None` (unknown), never `false`. It selects the signing domain, but this map is
///   catalog-only: nothing on the exec/signing path reads it.
/// - `properties` carries `tick_size` (`orderPriceMinTickSize`) and `min_qty` (`orderMinSize`, in
///   shares) from the SAME payload, absent = `0.0` (the struct's convention); no extra fetch.
/// - `wire.group` is `negRiskMarketID` when present (the neg-risk SET key). `wire.coin` stays
///   `None`: the outcome label is not a wire identifier here (the token id is, and it is
///   `raw_symbol`).
/// - `description` is `"{question} — {outcome}"` so search over the question text still resolves a
///   specific outcome leg; the quote is USDC.
///
/// `outcomes`/`token_ids` are zipped positionally when both are present (Gamma's Yes/No — or N-way
/// — ordering is shared between the two decoded arrays); if `outcomes` is short/absent, the
/// token_id itself fills the outcome slot so the description never goes blank. A market with no
/// token_ids contributes nothing — never panics on a shape mismatch.
pub fn markets_to_instruments(markets: &[GammaMarket]) -> Vec<Instrument> {
    let mut out = Vec::new();
    for m in markets {
        let properties = SymbolProperties {
            tick_size: non_negative_or_absent(m.tick_size),
            min_qty: m.order_min_size.unwrap_or(0.0),
            ..Default::default()
        };
        let group = m.is_neg_risk_member().then(|| m.neg_risk_market_id.clone());
        for (i, token_id) in m.token_ids.iter().enumerate() {
            let outcome = m.outcomes.get(i).cloned().unwrap_or_else(|| token_id.clone());
            let contract = Contract::PredictionOutcome(ContractPredictionOutcome {
                condition_id: m.condition_id.clone(),
                outcome: outcome.clone(),
                token_id: token_id.clone(),
                resolution_ms: m.resolution_ts_ms(),
                neg_risk: m.neg_risk_declared,
            });
            out.push(
                Instrument::new("polymarket", token_id.clone(), outcome.clone(), "USDC", contract)
                    .with_description(format!("{} — {}", m.question, outcome))
                    .with_properties(properties)
                    .with_wire(InstrumentVenueIds {
                        asset_id: None,
                        coin: None,
                        group: group.clone(),
                    }),
            );
        }
    }
    out
}

/// `orderPriceMinTickSize` folds absent to `0.0` in [`GammaMarket::tick_size`]; a non-finite or
/// negative value is no grid either, so it also lands on the `0.0` absent convention.
fn non_negative_or_absent(x: f64) -> f64 {
    if x.is_finite() && x > 0.0 { x } else { 0.0 }
}

/// The lot of every outcome token, in shares. The CLOB takes a share size to TWO decimals at every tick size
/// (py-clob-client's `ROUNDING_CONFIG` writes `size` with 2 decimals for the 0.1, 0.01, 0.001 and 0.0001 ticks),
/// and the catalog row above carries none, so [`outcome_rows`] states it: the engine's lot snap then keeps a
/// door-admitted order on the grid the venue accepts. ⚠ Read from the client's rounding table, not measured with
/// an order here.
pub const SHARE_STEP: f64 = 0.01;

/// The venue word of a row whose market is listed but takes no order right now: Gamma says it `closed`, not
/// `active`, or `acceptingOrders: false` (the same three tests `crate::catalog::market_ref` refuses a name for). The
/// control door's facts refuse the token naming it, instead of admitting a symbol that only the venue's own
/// rejection would stop.
pub const VENUE_WORD_NOT_ACCEPTING_ORDERS: &str = "notAcceptingOrders";

/// **The facts row of ONE outcome token, for the control door** (`TradeUp::SymbolProperties`): Gamma's point lookup by
/// `clob_token_ids` (one request, keyless, through the venue's egress) mapped by [`markets_to_instruments`]: the
/// market's tick (`orderPriceMinTickSize`), its minimum size in shares (`orderMinSize`), the neg-risk flag as sent
/// ([`ContractPredictionOutcome::neg_risk`]) and the outcome label, plus the share lot ([`SHARE_STEP`]) and the venue word
/// ([`VENUE_WORD_NOT_ACCEPTING_ORDERS`]) of a market that takes no orders. An empty answer is "Gamma lists no
/// open market with this token" (a resolved market is hidden from `/markets`); a failed read is an `Err`.
///
/// The read is the venue's Gamma row among `urls`.
///
/// # Errors
/// The Gamma read's failure, or the missing row.
pub fn outcome_rows(urls: &VenueUrls, token_id: &str) -> Result<Vec<Instrument>, String> {
    let gamma = GammaClient::from_urls(urls).map_err(|e| e.to_string())?;
    Ok(gamma
        .by_token(token_id)?
        .map(|market| outcome_rows_of(&market, token_id))
        .unwrap_or_default())
}

/// **The facts row of ONE outcome token as a recorder keeps it beside the token's recorded rows** (PM-2): the
/// [`outcome_rows`] row (tick, minimum size, share step) with the market's declared venue hold
/// (`crate::instrument::taker_hold::fetch_taker_hold_ms`, `0` when it cannot be read: no hold, never a guess), so a
/// replay reads all of them from the store. One Gamma request, then one or two CLOB ones. `Ok(empty)`: Gamma lists
/// no open market with this token.
///
/// # Errors
/// The Gamma read's failure, or a missing Gamma or CLOB row.
pub fn recorded_token_rows(urls: &VenueUrls, token_id: &str) -> Result<Vec<Instrument>, String> {
    let gamma = GammaClient::from_urls(urls).map_err(|e| e.to_string())?;
    let Some(market) = gamma.by_token(token_id)? else { return Ok(Vec::new()) };
    let clob = crate::config::clob_base(urls).map_err(|e| e.to_string())?;
    let transport = vike_bridge_core::transport::UreqTransport::with_agent(
        crate::feed::VENUE,
        crate::transport::agent(),
    );
    let hold =
        crate::instrument::taker_hold::fetch_taker_hold_ms(&transport, clob, &market.condition_id);
    Ok(outcome_rows_of(&market, token_id)
        .into_iter()
        .map(|mut row| {
            row.properties.taker_hold_ms = hold;
            row
        })
        .collect())
}

/// [`outcome_rows`] over a market already read (the pure half, for tests).
fn outcome_rows_of(market: &GammaMarket, token_id: &str) -> Vec<Instrument> {
    let taking_orders = market.active && !market.closed && market.accepting_orders != Some(false);
    markets_to_instruments(std::slice::from_ref(market))
        .into_iter()
        .filter(|row| row.raw_symbol == token_id)
        .map(|mut row| {
            row.properties.step_size = SHARE_STEP;
            if taking_orders { row } else { row.with_venue_word(VENUE_WORD_NOT_ACCEPTING_ORDERS) }
        })
        .collect()
}

/// One Gamma `/markets/keyset` page, as ASKED (the `limit=500` the crate's other browses already use).
/// Gamma lists at most 100 (`crate::catalog::rules`' module doc), so a page's size never ends the walk.
const GAMMA_PAGE_LIMIT: usize = 500;

/// Defensive ceiling on pages fetched per catalog load: at most 40 × 500 = 20k markets, 4,000 at
/// Gamma's 100 a page (MEASURED 2026-11-17: more than 12,000 markets are active, so a load stops here,
/// the top 4,000 by volume, with its warn line), and the loop never crawls unboundedly.
const GAMMA_MAX_PAGES: usize = 40;

/// Page the volume-ordered active-market browse ([`GammaClient::keyset_page`] over
/// [`gamma::gamma_keyset_query`]) until Gamma names no next cursor (a page's size never ends it: Gamma
/// lists fewer than it is asked), bounded by [`GAMMA_MAX_PAGES`]. Split from the provider over an
/// injectable per-page fetch (`fetch(after_cursor)`, `""` the first page: its markets and the next
/// page's cursor) so the paging is testable without network.
///
/// Failure shape: a FIRST-page failure is the venue being unreachable → `Err` (exactly the
/// previous single-page behavior); a LATER page failing degrades to the pages already fetched
/// (warned, never silent) — a mid-crawl hiccup must not wipe the venue from the Symbol picker
/// (the same warn-and-accumulate contract as okx/deribit/bybit's catalogs).
///
/// The walk is the shared `CatalogPageWalk`, tolerant (a later page failing keeps the rows), its position
/// the cursor Gamma hands back, a repeated cursor and the ceiling each a warn line.
fn paged_markets(
    fetch: impl Fn(&str) -> Result<(Vec<GammaMarket>, Option<String>), String>,
) -> Result<Vec<GammaMarket>, CatalogError> {
    let walk = CatalogPageWalk { max_pages: GAMMA_MAX_PAGES, strict: false };
    let (out, end) = walk
        .walk(String::new(), |cursor| fetch(cursor).map(|(rows, next)| CatalogPage { rows, next }))
        .map_err(|why| CatalogError(why.into()))?;
    match end {
        CatalogPageWalkEnd::Failed { page, error: e } => tracing::warn!(
            target: "vike_polymarket::catalog",
            "gamma page {page} fetch failed (kept {} markets from earlier pages): {e}",
            out.len()
        ),
        CatalogPageWalkEnd::Ceiling => tracing::warn!(
            target: "vike_polymarket::catalog",
            "gamma still offered a page after {GAMMA_MAX_PAGES}; stopping with {} markets",
            out.len()
        ),
        CatalogPageWalkEnd::Repeated { page } => tracing::warn!(
            target: "vike_polymarket::catalog",
            "gamma named page {page}'s cursor as its next page; stopping with {} markets",
            out.len()
        ),
        CatalogPageWalkEnd::Listed => {}
    }
    Ok(out)
}

/// The polymarket venue's `CatalogProvider` contribution: prediction markets, bulk-enumerable off
/// the [`GammaClient::keyset_page`] browse (geo-routed via `transport::agent()`'s proxy, same as every
/// other Polymarket read) and mapped via [`markets_to_instruments`]. Pages via [`paged_markets`]
/// (the previous single `limit=500` page truncated the picker to the top-500-by-volume, hiding
/// every just-listed ~$0-volume recurring window). Gamma is not under the CLOB per-signer rate
/// budget (`rate_budget` mirrors `Poly-RateLimit-*` on CLOB responses only), and these are keyless
/// public reads — a bounded sequential crawl of ≤[`GAMMA_MAX_PAGES`] pages is well within its
/// public posture.
///
/// It reads the venue's Gamma row; a missing row is the catalog's error, in its words, at list time.
pub struct CatalogProviderPolymarket {
    gamma: Result<GammaClient, UrlRefusal>,
    /// The venue's rows, for a search by token id ([`recorded_token_rows`]).
    urls: VenueUrls,
}

impl CatalogProviderPolymarket {
    /// The catalog over the Gamma row among `urls` (the venue's `venue_url` rows).
    pub fn new(urls: &VenueUrls) -> Self {
        CatalogProviderPolymarket { gamma: GammaClient::from_urls(urls), urls: urls.clone() }
    }
}

impl CatalogProvider for CatalogProviderPolymarket {
    fn venue(&self) -> &str {
        "polymarket"
    }
    fn asset_classes(&self) -> &[AssetClass] {
        &[AssetClass::PredictionMarket]
    }
    fn mode(&self) -> CatalogMode {
        CatalogMode::Enumerable
    }
    fn list_instruments(&self) -> Result<Vec<Instrument>, CatalogError> {
        let gamma = self.gamma.as_ref().map_err(|e| CatalogError(e.to_string().into()))?;
        let query = gamma::gamma_keyset_query(true, GAMMA_PAGE_LIMIT);
        let markets = paged_markets(|cursor| gamma.keyset_page(&query, cursor))?;
        Ok(markets_to_instruments(&markets))
    }

    /// A search by an outcome token id answers that token's facts row ([`recorded_token_rows`]): what the datahub's
    /// recorder keeps beside a recorded token's rows (PM-2). Any other text answers nothing, as before.
    fn search_remote(
        &self,
        query: &str,
        _filter: &vike_catalog::CatalogSearchFilter,
    ) -> Result<Vec<Instrument>, CatalogError> {
        let token = query.trim();
        if !vike_catalog::is_outcome_token(token) {
            return Ok(Vec::new());
        }
        recorded_token_rows(&self.urls, token).map_err(|why| CatalogError(why.into()))
    }
}

#[path = "catalog_tests.rs"]
#[cfg(test)]
mod catalog_tests;

#[path = "catalog_catalog_tests.rs"]
#[cfg(test)]
mod catalog_catalog_tests;
