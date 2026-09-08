//! Pure grouping of the HistStore inventory (`Vec<(SeriesId, SeriesCoverage)>`) into a
//! venue → symbol → (kind, interval) display tree with per-node rollups, for the Data Manager's
//! "Stored" view. No I/O — the caller fetches `DataFusionHist::inventory()` off-thread and hands
//! the result here.

use vike_data::{SeriesCoverage, SeriesId};

/// Aggregate totals for a subtree (a symbol's series, or a venue's symbols).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RollUp {
    pub rows: u64,
    pub bytes: u64,
    pub series: usize,
    pub first_ts: i64,
    pub last_ts: i64,
}
impl RollUp {
    fn add(&mut self, c: &SeriesCoverage) {
        self.rows += c.rows;
        self.bytes += c.bytes;
        self.series += 1;
        self.first_ts = if self.series == 1 { c.first_ts } else { self.first_ts.min(c.first_ts) };
        self.last_ts = self.last_ts.max(c.last_ts);
    }
}

/// One stored series under a symbol: its data kind + optional bar interval + coverage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeriesRow {
    pub kind: String,
    pub interval: Option<String>,
    pub cov: SeriesCoverage,
}

/// One symbol under a venue: its series (sorted by kind then interval) + rollup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolNode {
    /// The series LABEL — the symbol for a per-symbol series, the group name for a grouped one.
    pub symbol: String,
    /// `true` when this node is a GROUPED series (`group=` on disk). Carried because a grouped and a
    /// per-symbol series can share a label while being genuinely different instruments, and because
    /// `vike_data::InstrumentKey` — which the cross-kind coverage report is keyed by — needs it.
    pub grouped: bool,
    pub series: Vec<SeriesRow>,
    pub total: RollUp,
}

/// One venue: its symbols (sorted) + rollup across them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VenueNode {
    pub venue: String,
    pub symbols: Vec<SymbolNode>,
    pub total: RollUp,
}

/// Group + rollup the flat inventory into a sorted venue → symbol → (kind, interval) tree.
/// Pure — no I/O. Sorted deterministically (BTreeMap keys) for a stable GUI render + test asserts.
pub fn build_tree(inv: Vec<(SeriesId, SeriesCoverage)>) -> Vec<VenueNode> {
    use std::collections::BTreeMap;
    let mut venues: BTreeMap<String, BTreeMap<(String, bool), Vec<SeriesRow>>> = BTreeMap::new();
    for (id, cov) in inv {
        // `label()`, not `symbol`: a GROUPED series carries NO symbol (it holds many, told apart by
        // a row column), so keying on `symbol` would collapse every group in a venue into one
        // blank-named node in the customer's Data Manager. The group name is what identifies it on
        // disk and is what the customer should see.
        // Keyed on (label, GROUPED): a grouped series and a per-symbol one can share a name, and they
        // are genuinely different instruments — different directories, different manifests, and a
        // family mid-migration has history in both. Collapsing them here would make the two
        // indistinguishable in the Data Manager and make an `InstrumentKey` lookup impossible.
        let node = (id.label().to_string(), id.group.is_some());
        venues.entry(id.venue).or_default().entry(node).or_default().push(SeriesRow {
            kind: id.kind,
            interval: id.interval,
            cov,
        });
    }
    venues
        .into_iter()
        .map(|(venue, syms)| {
            let mut vtotal = RollUp::default();
            let symbols = syms
                .into_iter()
                .map(|((symbol, grouped), mut rows)| {
                    rows.sort_by(|a, b| (&a.kind, &a.interval).cmp(&(&b.kind, &b.interval)));
                    let mut stotal = RollUp::default();
                    for r in &rows {
                        stotal.add(&r.cov);
                        vtotal.add(&r.cov);
                    }
                    SymbolNode { symbol, grouped, series: rows, total: stotal }
                })
                .collect();
            VenueNode { venue, symbols, total: vtotal }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_data::{SeriesCoverage, SeriesId};

    fn sid(kind: &str, venue: &str, sym: &str, iv: Option<&str>) -> SeriesId {
        SeriesId::per_symbol(kind, venue, sym, iv.map(Into::into))
    }
    fn cov(rows: u64, bytes: u64, a: i64, b: i64) -> SeriesCoverage {
        SeriesCoverage { first_ts: a, last_ts: b, rows, bytes, parts: 1, dates: 1 }
    }

    /// A GROUPED series shows under its GROUP name, not a blank node.
    ///
    /// `SeriesId.symbol` is deliberately empty for a grouped series — it holds many symbols, told
    /// apart by a row column — so keying the tree on `symbol` collapsed every group in a venue into
    /// one unnamed row. The customer would see a single blank entry with everything under it.
    #[test]
    fn a_grouped_series_shows_under_its_group_name() {
        let g = SeriesId::grouped("quote", "polymarket", "btc-5m");
        let s = SeriesId::per_symbol("quote", "polymarket", "ETHUSDT", None);
        let tree = build_tree(vec![(g, SeriesCoverage::default()), (s, SeriesCoverage::default())]);
        assert_eq!(tree.len(), 1, "one venue");
        let names: Vec<&str> = tree[0].symbols.iter().map(|n| n.symbol.as_str()).collect();
        assert!(names.contains(&"btc-5m"), "the group is named, not blank: {names:?}");
        assert!(names.contains(&"ETHUSDT"), "per-symbol series unaffected: {names:?}");
        assert!(!names.contains(&""), "no blank node: {names:?}");
    }

    #[test]
    fn groups_by_venue_then_symbol_and_rolls_up() {
        let inv = vec![
            (sid("bar", "binance", "BTCUSDT", Some("1m")), cov(100, 10, 1, 9)),
            (sid("trade", "binance", "BTCUSDT", None), cov(50, 5, 3, 7)),
            (sid("bar", "okx", "BTC-USDT", Some("1m")), cov(20, 2, 2, 8)),
        ];
        let tree = build_tree(inv);
        assert_eq!(tree.len(), 2); // binance, okx (sorted)
        assert_eq!(tree[0].venue, "binance");
        assert_eq!(tree[0].symbols.len(), 1);
        let btc = &tree[0].symbols[0];
        assert_eq!(btc.series.len(), 2); // bar/1m + trade
        assert_eq!(btc.total.rows, 150);
        assert_eq!(btc.total.bytes, 15);
        assert_eq!(tree[0].total.rows, 150); // venue rollup
        assert_eq!(tree[0].total.first_ts, 1);
        assert_eq!(tree[0].total.last_ts, 9);
    }

    #[test]
    fn empty_inventory_is_empty_tree() {
        assert!(build_tree(Vec::new()).is_empty());
    }
}
