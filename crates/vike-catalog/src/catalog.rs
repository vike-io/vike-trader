//! The aggregated local index of `Enumerable` instruments + the tiered fuzzy search the picker
//! renders from. Ranking migrated from PR #269's `filter_symbols` (exact/prefix/base/substring
//! tiers + preferred-quote tiebreak), extended with the asset-class `Tab` + venue `SearchFilter`.

use crate::{Instrument, SearchFilter};

/// Lower = more preferred as a tiebreaker among equal-tier hits.
pub(crate) fn quote_rank(quote: &str) -> u8 {
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
///
/// ⚠ The comparison is ASCII-case-INSENSITIVE on the instrument's side. It was a case-sensitive
/// match against an upper-cased query, so a venue symbol with a lower-case letter (Hyperliquid's
/// `kPEPE`) could never be found by ANY query. Byte comparison cannot split a character and
/// allocates nothing per instrument, which matters: a search touches every instrument.
fn tier(it: &Instrument, q: &str) -> u8 {
    let sym = it.raw_symbol.as_str();
    if sym.eq_ignore_ascii_case(q) {
        0
    } else if starts_with_ci(sym, q) {
        1
    } else if starts_with_ci(&it.base, q) {
        2
    } else if contains_ci(sym, q) {
        3
    } else {
        4
    }
}

fn starts_with_ci(hay: &str, needle: &str) -> bool {
    hay.len() >= needle.len()
        && hay.as_bytes()[..needle.len()].eq_ignore_ascii_case(needle.as_bytes())
}

fn contains_ci(hay: &str, needle: &str) -> bool {
    needle.is_empty()
        || hay.as_bytes().windows(needle.len()).any(|w| w.eq_ignore_ascii_case(needle.as_bytes()))
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

#[path = "catalog_tests.rs"]
#[cfg(test)]
mod catalog_tests;
