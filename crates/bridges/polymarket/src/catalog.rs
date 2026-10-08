//! `catalog` — a searchable in-memory registry over Gamma markets (the reusable discovery core the
//! future UI panel + the rolling 5-min mount both consume). Search resolves a market by name; its
//! `token_ids` feed the existing subscribe seam. (spec: 2026-07-11-gamma-catalog-design.md)
//!
//! Also hosts [`PolymarketCatalog`], the venue's [`vike_catalog::CatalogProvider`] contribution
//! (crate-reorg catalog workstream): wraps the SAME Gamma catalog fetch — one [`Instrument`] per
//! outcome CLOB `token_id` (the tradable unit the subscribe seam and order submission key off of),
//! tagged `AssetClass::PredictionMarket`, so Polymarket markets appear in the Symbol picker.

use crate::gamma::{GammaClient, GammaMarket};
use crate::neg_risk_set::NegRiskSet;
use vike_catalog::{CatalogError, CatalogMode, CatalogProvider, Instrument};
use vike_model::AssetClass;

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
    /// catalog from [`crate::gamma::GammaClient::markets_by_event_slug`] when a complete set
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

/// Pure map: Gamma markets → one [`Instrument`] per outcome CLOB token. `raw_symbol` is the
/// outcome's `token_id` verbatim (the venue-native tradable id the subscribe seam and order
/// submission key off of); `description` is `"{question} — {outcome}"` so search over the
/// question text still resolves a specific outcome leg. `outcomes`/`token_ids` are zipped
/// positionally when both are present (Gamma's Yes/No — or N-way — ordering is shared between the
/// two decoded arrays); if `outcomes` is short/absent, the token_id itself fills the outcome slot
/// so the description never goes blank. A market with no token_ids contributes nothing — never
/// panics on a shape mismatch.
pub fn markets_to_instruments(markets: &[GammaMarket]) -> Vec<Instrument> {
    let mut out = Vec::new();
    for m in markets {
        for (i, token_id) in m.token_ids.iter().enumerate() {
            let outcome = m.outcomes.get(i).cloned().unwrap_or_else(|| token_id.clone());
            out.push(Instrument {
                venue: "polymarket".into(),
                raw_symbol: token_id.clone(),
                asset_class: AssetClass::PredictionMarket,
                base: outcome.clone(),
                quote: "USDC".into(),
                description: format!("{} — {}", m.question, outcome),
                properties: Default::default(),
                contract_type: None,
                settle_asset: None,
            });
        }
    }
    out
}

/// One Gamma `/markets` page (the `limit=500` ceiling the crate's other browses already use).
const GAMMA_PAGE_LIMIT: usize = 500;

/// Defensive ceiling on pages fetched per catalog load — 40 pages × 500 = 20k active markets,
/// comfortably above the live active universe, so the loop can never crawl unboundedly if Gamma
/// ever stops sending a short final page.
const GAMMA_MAX_PAGES: usize = 40;

/// Page the volume-ordered active-market browse until a SHORT page (the venue's end-of-listing
/// signal), bounded by [`GAMMA_MAX_PAGES`]. Split from the provider over an injectable
/// per-page fetch (`fetch(limit, offset)`) so the paging is testable without network.
///
/// Failure shape: a FIRST-page failure is the venue being unreachable → `Err` (exactly the
/// previous single-page behavior); a LATER page failing degrades to the pages already fetched
/// (warned, never silent) — a mid-crawl hiccup must not wipe the venue from the Symbol picker
/// (the same warn-and-accumulate contract as okx/deribit/bybit's catalogs).
fn paged_markets(
    fetch: impl Fn(usize, usize) -> Result<Vec<GammaMarket>, String>,
) -> Result<Vec<GammaMarket>, CatalogError> {
    let mut out: Vec<GammaMarket> = Vec::new();
    for page in 0..GAMMA_MAX_PAGES {
        match fetch(GAMMA_PAGE_LIMIT, page * GAMMA_PAGE_LIMIT) {
            Ok(batch) => {
                let n = batch.len();
                out.extend(batch);
                if n < GAMMA_PAGE_LIMIT {
                    break; // short page = end of the listing
                }
            }
            Err(e) if page == 0 => return Err(CatalogError(e)),
            Err(e) => {
                tracing::warn!(
                    target: "vike_polymarket::catalog",
                    "gamma page {page} fetch failed (kept {} markets from earlier pages): {e}",
                    out.len()
                );
                break;
            }
        }
    }
    Ok(out)
}

/// The polymarket venue's `CatalogProvider` contribution: prediction markets, bulk-enumerable off
/// the existing [`GammaClient::list`] fetch (geo-routed via `egress::agent()`'s proxy, same as every
/// other Polymarket read) and mapped via [`markets_to_instruments`]. Pages via [`paged_markets`]
/// (the previous single `limit=500` page truncated the picker to the top-500-by-volume, hiding
/// every just-listed ~$0-volume recurring window). Gamma is not under the CLOB per-signer rate
/// budget (`rate_budget` mirrors `Poly-RateLimit-*` on CLOB responses only), and these are keyless
/// public reads — a bounded sequential crawl of ≤[`GAMMA_MAX_PAGES`] pages is well within its
/// public posture.
pub struct PolymarketCatalog;

impl CatalogProvider for PolymarketCatalog {
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
        let markets = paged_markets(|limit, offset| GammaClient::list(true, limit, offset))?;
        Ok(markets_to_instruments(&markets))
    }
}

#[path = "catalog_tests.rs"]
#[cfg(test)]
mod catalog_tests;

#[path = "catalog_catalog_tests.rs"]
#[cfg(test)]
mod catalog_catalog_tests;
