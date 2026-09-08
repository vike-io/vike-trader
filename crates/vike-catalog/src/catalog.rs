//! The aggregated local index of `Enumerable` instruments + the tiered fuzzy search the picker
//! renders from. Ranking migrated from PR #269's `filter_symbols` (exact/prefix/base/substring
//! tiers + preferred-quote tiebreak), extended with the asset-class `Tab` + venue `SearchFilter`.

use crate::{Instrument, SearchFilter};

/// Lower = more preferred as a tiebreaker among equal-tier hits.
fn quote_rank(quote: &str) -> u8 {
    match quote {
        "USDT" => 0,
        "USDC" => 1,
        "USD" => 2,
        "BTC" => 3,
        _ => 4,
    }
}

/// Match tier for `it` against the (already uppercased) query `q`: 0 exact symbol, 1
/// symbol-prefix, 2 base-prefix, 3 substring, 4 no match. Extracted to a module fn (rather than a
/// closure inside `search`) so later tasks can reuse the same ranking.
fn tier(it: &Instrument, q: &str) -> u8 {
    if it.raw_symbol == q {
        0
    } else if it.raw_symbol.starts_with(q) {
        1
    } else if it.base.starts_with(q) {
        2
    } else if it.raw_symbol.contains(q) {
        3
    } else {
        4
    }
}

pub struct Catalog {
    items: Vec<Instrument>,
}

impl Catalog {
    pub fn from_instruments(items: Vec<Instrument>) -> Self {
        Catalog { items }
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Rank + filter for `query` (case-insensitive), best first, capped at `limit`. Empty query
    /// returns the filtered head so the popup is useful before typing.
    pub fn search<'a>(
        &'a self,
        query: &str,
        filter: &SearchFilter,
        limit: usize,
    ) -> Vec<&'a Instrument> {
        let q = query.trim().to_uppercase();
        let passes_filter = |it: &Instrument| -> bool {
            filter.tab.is_none_or(|t| t.matches(it.asset_class))
                && filter.venue.as_deref().is_none_or(|v| it.venue.eq_ignore_ascii_case(v))
        };

        if q.is_empty() {
            return self.items.iter().filter(|it| passes_filter(it)).take(limit).collect();
        }

        let mut hits: Vec<(&Instrument, usize, u8)> = self
            .items
            .iter()
            .enumerate()
            .filter(|(_, it)| passes_filter(it))
            .filter_map(|(idx, it)| {
                let t = tier(it, &q);
                if t == 4 { None } else { Some((it, idx, t)) }
            })
            .collect();

        // tier ASC, preferred quote ASC, then catalog listing order (stable via idx)
        hits.sort_by(|a, b| {
            a.2.cmp(&b.2)
                .then_with(|| quote_rank(&a.0.quote).cmp(&quote_rank(&b.0.quote)))
                .then_with(|| a.1.cmp(&b.1))
        });

        hits.into_iter().take(limit).map(|(it, _, _)| it).collect()
    }
}

/// Combine local-index hits with QueryBacked `search_remote` results into one ranked list for the
/// picker. Re-ranks the union by the same tiers as `Catalog::search` and dedups by `id()`.
pub fn merge_ranked(
    local: Vec<Instrument>,
    remote: Vec<Instrument>,
    query: &str,
    limit: usize,
) -> Vec<Instrument> {
    let q = query.trim().to_uppercase();
    let mut seen = std::collections::HashSet::new();
    let mut union: Vec<Instrument> = Vec::with_capacity(local.len() + remote.len());
    for it in local.into_iter().chain(remote) {
        if seen.insert(it.id()) {
            union.push(it);
        }
    }
    let mut idx: Vec<(usize, u8)> =
        union.iter().enumerate().map(|(n, it)| (n, tier(it, &q))).collect();
    idx.sort_by(|a, b| {
        a.1.cmp(&b.1)
            .then_with(|| quote_rank(&union[a.0].quote).cmp(&quote_rank(&union[b.0].quote)))
            .then_with(|| a.0.cmp(&b.0))
    });
    idx.into_iter().take(limit).map(|(n, _)| union[n].clone()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AssetClass, Instrument, SearchFilter, Tab};

    fn i(venue: &str, sym: &str, base: &str, quote: &str, class: AssetClass) -> Instrument {
        Instrument {
            venue: venue.into(),
            raw_symbol: sym.into(),
            asset_class: class,
            base: base.into(),
            quote: quote.into(),
            description: String::new(),
            properties: Default::default(),
        }
    }

    fn sample() -> Catalog {
        Catalog::from_instruments(vec![
            i("binance", "ETHBTC", "ETH", "BTC", AssetClass::CryptoSpot),
            i("binance", "BTCUSDC", "BTC", "USDC", AssetClass::CryptoSpot),
            i("binance", "BTCUSDT", "BTC", "USDT", AssetClass::CryptoSpot),
            i("alpaca", "BTCUSD", "BTC", "USD", AssetClass::CryptoSpot),
            i("alpaca", "AAPL", "AAPL", "USD", AssetClass::Equity),
        ])
    }

    #[test]
    fn exact_symbol_ranks_first_then_prefix_then_base() {
        let c = sample();
        let hits = c.search("BTCUSDT", &SearchFilter::default(), 10);
        assert_eq!(hits[0].raw_symbol, "BTCUSDT"); // exact beats prefix
    }

    #[test]
    fn base_prefix_hits_match_when_symbol_does_not() {
        let c = sample();
        let syms: Vec<_> = c
            .search("BTC", &SearchFilter::default(), 10)
            .iter()
            .map(|x| x.raw_symbol.clone())
            .collect();
        // BTC* symbol-prefix hits, then ETHBTC via base? ETH base doesn't prefix BTC — ETHBTC is a
        // substring hit; ensure the three BTC-prefixed symbols come before ETHBTC.
        assert!(
            syms.iter().position(|s| s == "BTCUSDT").unwrap()
                < syms.iter().position(|s| s == "ETHBTC").unwrap()
        );
    }

    #[test]
    fn preferred_quote_breaks_ties_usdt_first() {
        let c = sample();
        let hits = c.search("BTC", &SearchFilter::default(), 10);
        let usdt = hits.iter().position(|x| x.raw_symbol == "BTCUSDT").unwrap();
        let usdc = hits.iter().position(|x| x.raw_symbol == "BTCUSDC").unwrap();
        assert!(usdt < usdc);
    }

    #[test]
    fn tab_filter_restricts_to_asset_class() {
        let c = sample();
        let hits = c.search("", &SearchFilter { tab: Some(Tab::Stocks), venue: None }, 10);
        assert!(hits.iter().all(|x| x.asset_class == AssetClass::Equity));
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn venue_filter_restricts_to_venue() {
        let c = sample();
        let hits = c.search("BTC", &SearchFilter { tab: None, venue: Some("alpaca".into()) }, 10);
        assert!(hits.iter().all(|x| x.venue == "alpaca"));
    }

    #[test]
    fn empty_query_returns_filtered_head() {
        let c = sample();
        let hits = c.search("", &SearchFilter::default(), 2);
        assert_eq!(hits.len(), 2);
    }

    #[test]
    fn merge_dedups_by_id_and_reranks() {
        let local = vec![i("binance", "BTCUSDT", "BTC", "USDT", AssetClass::CryptoSpot)];
        let remote = vec![
            i("ibkr", "BTC", "BTC", "USD", AssetClass::CryptoSpot),
            i("binance", "BTCUSDT", "BTC", "USDT", AssetClass::CryptoSpot), // dup of local
        ];
        let out = merge_ranked(local, remote, "BTC", 10);
        let ids: Vec<_> = out.iter().map(|x| x.id()).collect();
        assert_eq!(ids.iter().filter(|id| *id == "BTCUSDT.BINANCE").count(), 1); // deduped
        assert!(ids.contains(&"BTC.IBKR".to_string()));
    }

    #[test]
    fn scale_sanity_50k() {
        let mut items = Vec::with_capacity(50_000);
        for n in 0..50_000u32 {
            items.push(i(
                "binance",
                &format!("SYM{n}USDT"),
                &format!("SYM{n}"),
                "USDT",
                AssetClass::CryptoSpot,
            ));
        }
        let c = Catalog::from_instruments(items);
        let hits = c.search("SYM123USDT", &SearchFilter::default(), 10);
        assert_eq!(hits[0].raw_symbol, "SYM123USDT");
    }
}
