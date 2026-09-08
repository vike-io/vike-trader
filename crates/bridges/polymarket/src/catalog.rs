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
use vike_catalog::{AssetClass, CatalogError, CatalogMode, CatalogProvider, Instrument};

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gamma::GammaMarket;

    fn mk(q: &str, slug: &str, cid: &str, toks: &[&str]) -> GammaMarket {
        GammaMarket {
            id: "1".into(),
            question: q.into(),
            condition_id: cid.into(),
            slug: slug.into(),
            end_date: "".into(),
            volume: 0.0,
            liquidity: 0.0,
            active: true,
            closed: false,
            neg_risk: false,
            tick_size: 0.01,
            outcomes: vec![],
            token_ids: toks.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn search_is_case_insensitive_over_question_and_slug() {
        let cat = MarketCatalog::from_markets(vec![
            mk("Will BTC be above $100k?", "btc-100k", "0x1", &["a", "b"]),
            mk("ETH flippening", "eth-flip", "0x2", &["c"]),
        ]);
        let r = cat.search("btc");
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].condition_id, "0x1");
        // matches slug too:
        assert_eq!(cat.search("FLIP").len(), 1);
        // no match:
        assert!(cat.search("dogecoin").is_empty());
    }

    #[test]
    fn by_condition_and_token_ids() {
        let cat = MarketCatalog::from_markets(vec![mk("q", "s", "0xabc", &["t1", "t2"])]);
        let m = cat.by_condition("0xabc").expect("found");
        assert_eq!(m.token_ids, vec!["t1".to_string(), "t2".to_string()]);
        assert!(cat.by_condition("0xnope").is_none());
    }

    const MID: &str = "0x55ab76d092f682bf5cbb7e14f13ee12f8410ce7cc1b7906f23b8fb56c11f6500";

    fn nr(idx: u32, title: &str, cid: &str, yes: f64) -> GammaMarket {
        GammaMarket {
            question: format!("Will {title} win?"),
            condition_id: cid.into(),
            active: true,
            neg_risk: true,
            outcomes: vec!["Yes".into(), "No".into()],
            token_ids: vec![format!("y{idx}"), format!("n{idx}")],
            outcome_prices: vec![yes, 1.0 - yes],
            neg_risk_market_id: MID.into(),
            group_item_title: title.into(),
            group_item_index: Some(idx),
            event_slug: "next-prime-minister-of-ethiopia".into(),
            ..Default::default()
        }
    }

    #[test]
    fn catalog_surfaces_neg_risk_sets_and_resolves_one_by_id() {
        let cat = MarketCatalog::from_markets(vec![
            nr(0, "A", "0xa", 0.50),
            mk("plain binary", "bin", "0xbin", &["t"]), // non-neg-risk: contributes no set
            nr(1, "B", "0xb", 0.30),
            nr(2, "C", "0xc", 0.15),
        ]);
        let sets = cat.neg_risk_sets();
        assert_eq!(sets.len(), 1, "the binary market forms no set");
        assert_eq!(sets[0].len(), 3);
        assert_eq!(sets[0].event_slug, "next-prime-minister-of-ethiopia");
        assert_eq!(sets[0].yes_token_ids(), vec!["y0", "y1", "y2"]);
        // Σ = 0.95 over a complete set → a 5c gross screen edge
        assert!((sets[0].yes_price_sum().unwrap() - 0.95).abs() < 1e-12);

        // resolve by the group key, case-insensitively
        let s = cat.neg_risk_set(MID).expect("resolved");
        assert_eq!(s.market_id, MID);
        assert_eq!(cat.neg_risk_set(&MID.to_uppercase()).unwrap().len(), 3);
        assert!(cat.neg_risk_set("0xnope").is_none());
    }

    #[test]
    fn neg_risk_set_for_condition_finds_the_siblings_of_a_held_market() {
        let cat = MarketCatalog::from_markets(vec![
            nr(0, "A", "0xa", 0.5),
            nr(1, "B", "0xb", 0.5),
            mk("plain binary", "bin", "0xbin", &["t"]),
        ]);
        let s = cat.neg_risk_set_for_condition("0xb").expect("member of a set");
        assert_eq!(s.len(), 2);
        assert!(s.members.iter().any(|m| m.condition_id == "0xa"), "sees its sibling");
        // a non-neg-risk market has no set — NOT a degenerate one-member one
        assert!(cat.neg_risk_set_for_condition("0xbin").is_none());
        assert!(cat.neg_risk_set_for_condition("0xunknown").is_none());
    }
}

#[cfg(test)]
mod catalog_tests {
    use super::*;
    use crate::gamma::GammaMarket;
    use vike_catalog::AssetClass;

    fn mk_with_outcomes(q: &str, cid: &str, outcomes: &[&str], toks: &[&str]) -> GammaMarket {
        GammaMarket {
            id: "1".into(),
            question: q.into(),
            condition_id: cid.into(),
            slug: "s".into(),
            end_date: "".into(),
            volume: 0.0,
            liquidity: 0.0,
            active: true,
            closed: false,
            neg_risk: false,
            tick_size: 0.01,
            outcomes: outcomes.iter().map(|s| s.to_string()).collect(),
            token_ids: toks.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn one_instrument_per_outcome_token_tagged_prediction_market() {
        let markets = vec![
            mk_with_outcomes("Will BTC be above $100k?", "0x1", &["Yes", "No"], &["t1", "t2"]),
            mk_with_outcomes("ETH flippening?", "0x2", &["Yes", "No"], &["t3", "t4"]),
        ];
        let out = markets_to_instruments(&markets);
        assert_eq!(out.len(), 4);
        for inst in &out {
            assert_eq!(inst.venue, "polymarket");
            assert_eq!(inst.asset_class, AssetClass::PredictionMarket);
            assert_eq!(inst.quote, "USDC");
        }
        assert_eq!(out[0].raw_symbol, "t1");
        assert_eq!(out[0].base, "Yes");
        assert_eq!(out[0].description, "Will BTC be above $100k? — Yes");
        assert_eq!(out[1].raw_symbol, "t2");
        assert_eq!(out[1].base, "No");
        assert_eq!(out[1].description, "Will BTC be above $100k? — No");
        assert_eq!(out[2].raw_symbol, "t3");
        assert_eq!(out[2].description, "ETH flippening? — Yes");
        assert_eq!(out[3].raw_symbol, "t4");
        assert_eq!(out[3].description, "ETH flippening? — No");
    }

    #[test]
    fn missing_outcomes_falls_back_to_token_id_never_panics() {
        // outcomes shorter than token_ids (or absent entirely) — description must never go blank.
        let markets = vec![mk_with_outcomes("no outcome labels", "0x3", &[], &["tokA", "tokB"])];
        let out = markets_to_instruments(&markets);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].raw_symbol, "tokA");
        assert_eq!(out[0].base, "tokA");
        assert_eq!(out[0].description, "no outcome labels — tokA");
        assert_eq!(out[1].raw_symbol, "tokB");
        assert_eq!(out[1].description, "no outcome labels — tokB");
    }

    #[test]
    fn no_token_ids_contributes_nothing() {
        let markets = vec![mk_with_outcomes("no tokens", "0x4", &["Yes", "No"], &[])];
        assert!(markets_to_instruments(&markets).is_empty());
    }

    #[test]
    fn provider_declares_venue_asset_class_and_enumerable_mode() {
        let p = PolymarketCatalog;
        assert_eq!(p.venue(), "polymarket");
        assert_eq!(p.asset_classes(), &[AssetClass::PredictionMarket]);
        assert_eq!(p.mode(), CatalogMode::Enumerable);
    }

    /// A page of `n` distinct markets whose ids encode `(offset, i)` so the tests can prove which
    /// offsets were fetched and that ordering/accumulation is intact.
    fn page(offset: usize, n: usize) -> Vec<GammaMarket> {
        (0..n)
            .map(|i| GammaMarket {
                id: format!("{}", offset + i),
                question: "q".into(),
                condition_id: format!("0x{:x}", offset + i),
                active: true,
                token_ids: vec![format!("t{}", offset + i)],
                ..Default::default()
            })
            .collect()
    }

    #[test]
    fn pages_until_a_short_page_advancing_the_offset() {
        use std::cell::RefCell;
        let calls: RefCell<Vec<(usize, usize)>> = RefCell::new(Vec::new());
        let out = paged_markets(|limit, offset| {
            calls.borrow_mut().push((limit, offset));
            // two full pages then a short one
            Ok(if offset < 2 * GAMMA_PAGE_LIMIT { page(offset, limit) } else { page(offset, 3) })
        })
        .unwrap();
        assert_eq!(out.len(), 2 * GAMMA_PAGE_LIMIT + 3, "all pages accumulated");
        assert_eq!(
            *calls.borrow(),
            vec![
                (GAMMA_PAGE_LIMIT, 0),
                (GAMMA_PAGE_LIMIT, GAMMA_PAGE_LIMIT),
                (GAMMA_PAGE_LIMIT, 2 * GAMMA_PAGE_LIMIT)
            ],
            "offset advances by the page limit; the short page ends the crawl"
        );
        assert_eq!(out[0].id, "0");
        assert_eq!(out.last().unwrap().id, format!("{}", 2 * GAMMA_PAGE_LIMIT + 2));
    }

    #[test]
    fn a_short_first_page_makes_exactly_one_request() {
        use std::cell::Cell;
        let calls = Cell::new(0usize);
        let out = paged_markets(|_, offset| {
            calls.set(calls.get() + 1);
            Ok(page(offset, 7))
        })
        .unwrap();
        assert_eq!(out.len(), 7);
        assert_eq!(calls.get(), 1, "a short first page must not trigger a second fetch");
    }

    #[test]
    fn an_empty_venue_yields_an_empty_catalog() {
        let out = paged_markets(|_, _| Ok(Vec::new())).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn the_page_ceiling_bounds_a_never_short_listing() {
        use std::cell::Cell;
        let calls = Cell::new(0usize);
        let out = paged_markets(|limit, offset| {
            calls.set(calls.get() + 1);
            Ok(page(offset, limit)) // every page full — Gamma "never ends"
        })
        .unwrap();
        assert_eq!(calls.get(), GAMMA_MAX_PAGES, "the defensive ceiling stops the crawl");
        assert_eq!(out.len(), GAMMA_MAX_PAGES * GAMMA_PAGE_LIMIT);
    }

    #[test]
    fn first_page_failure_is_the_venue_being_down_but_a_later_failure_degrades() {
        // first page failing → Err (the previous single-page behavior)
        assert!(paged_markets(|_, _| Err("network: simulated".into())).is_err());
        // a later page failing → the earlier pages are KEPT (warned), never an error
        let out = paged_markets(|limit, offset| {
            if offset == 0 { Ok(page(0, limit)) } else { Err("network: simulated".into()) }
        })
        .unwrap();
        assert_eq!(out.len(), GAMMA_PAGE_LIMIT, "page 0 survives the page-1 failure");
    }
}
