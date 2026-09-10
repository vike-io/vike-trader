//! Pure row→domain mapper for the pmxt Polymarket-L2 archive (no I/O).
//!
//! `PmxtRow` is the decoded-columns shape of one pmxt Parquet row (Task 2 fills it from Arrow
//! arrays); `map_row` folds it into `vike_model::{BookUpdate, TradeTick}` exactly as the venue's
//! own event stream is folded elsewhere in this workspace — `book` is a full-state snapshot,
//! `price_change` is a single-level delta, `last_trade_price` is a trade, and
//! `tick_size_change` carries no row of its own (it only updates tracked per-asset state that
//! rides on the next book update, mirroring the live Polymarket feed's tick-size handling).
//! `MapState` is the per-asset carry: tracked `tick_size` (default `0.01` until a
//! `tick_size_change` row is seen) and a monotonic book `seq` (trades don't consume it).

use std::collections::HashMap;

use vike_model::{BookUpdate, BookUpdateKind, QuoteTick, TradeTick};

/// One decoded pmxt Parquet row (Task 2 fills this from Arrow arrays — this task takes it as a
/// plain struct so the mapper stays I/O-free and fixture-testable).
#[derive(Debug, Clone)]
pub struct PmxtRow {
    pub event_type: String,
    pub ts_ms: i64,
    pub local_ts_ms: i64,
    pub asset_id: String,
    pub bids: Option<String>,
    pub asks: Option<String>,
    pub price: Option<f64>,
    pub size: Option<f64>,
    pub side: Option<String>,
    pub new_tick_size: Option<f64>,
    /// The archive's own venue-reported top of book, carried on `price_change` rows (null on
    /// `book`/`last_trade_price`). See [`l1_from_row`] for why this is worth keeping.
    pub best_bid: Option<f64>,
    pub best_ask: Option<f64>,
}

/// The venue-reported top of book on a `price_change` row, as an L1 [`QuoteTick`].
///
/// This is NOT a convenience duplicate of the L2 stream — it is the only complete record of the
/// top of book the archive contains, and a delta replay is measurably WRONG without it. Measured
/// on a real April 2026 part (one `btc-updown-5m` token, 168,312 `price_change` rows, 2 published
/// `book` frames): replaying the deltas alone leaves the best ask agreeing with the venue's own
/// `best_ask` column in only 21,940 of 167,874 rows, because Polymarket emits `price_change` for
/// order PLACEMENT and CANCELLATION but not for liquidity removed by a MATCH — so consumed levels
/// linger as ghosts BELOW the true best ask, and any depth read off that book is overstated on the
/// cheap side, which is the worst possible direction for a capacity estimate. Pruning every ask
/// below this column's value lifts that agreement to 158,713 of 167,874 and makes the next
/// published `book` frame reproduce exactly (29 levels, 0 missing, 0 extra).
///
/// `bid_size`/`ask_size` are 0 — the archive publishes the prices only.
pub fn l1_from_row(row: &PmxtRow) -> Option<QuoteTick> {
    if row.event_type != "price_change" {
        return None;
    }
    let (bid, ask) = (row.best_bid.unwrap_or(0.0), row.best_ask.unwrap_or(0.0));
    (bid > 0.0 || ask > 0.0).then(|| QuoteTick {
        ts: row.ts_ms,
        local_ts: row.local_ts_ms,
        bid,
        ask,
        bid_size: 0.0,
        ask_size: 0.0,
        symbol: row.asset_id.clone(),
    })
}

/// Per-asset carry state: tracked tick size, the book-update sequence counter, and the
/// last emitted (clamped) source `ts` — see `map_row`'s ts-clamping doc for why this exists.
#[derive(Debug, Clone)]
struct AssetState {
    tick_size: f64,
    seq: u64,
    last_ts: i64,
}

impl Default for AssetState {
    fn default() -> Self {
        Self { tick_size: 0.01, seq: 0, last_ts: 0 }
    }
}

/// Per-asset mapper state, keyed by `asset_id`. `Default` starts empty — every asset gets its
/// `AssetState` lazily (tick_size 0.01, seq 0) on first use.
#[derive(Debug, Clone, Default)]
pub struct MapState {
    assets: HashMap<String, AssetState>,
}

/// What one pmxt row maps to.
#[derive(Debug, Clone)]
pub enum Mapped {
    Book(BookUpdate),
    Trade(TradeTick),
    /// Row carries no book/trade event of its own (e.g. `tick_size_change` only updates
    /// tracked state; unknown/malformed rows are tolerated the same way).
    None,
}

/// Decode a pmxt `bids`/`asks` JSON column (`[["price","size"], ...]`) into `(price, size)`
/// pairs. Tolerant: a malformed outer array or an unparsable inner pair yields an empty/skipped
/// result rather than panicking — archive rows are not worth failing a whole backfill over.
pub fn parse_levels(json: &str) -> Vec<(f64, f64)> {
    let raw: Vec<Vec<String>> = match serde_json::from_str(json) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    raw.into_iter()
        .filter_map(|pair| {
            let price: f64 = pair.first()?.parse().ok()?;
            let size: f64 = pair.get(1)?.parse().ok()?;
            Some((price, size))
        })
        .collect()
}

/// Fold one pmxt row into a `BookUpdate`/`TradeTick`, or `Mapped::None` for rows that carry no
/// event of their own (`tick_size_change`, unknown event types, rows missing required fields).
///
/// The archive is ordered by `timestamp_received` (true ingest order, monotonic per asset), but
/// the source `timestamp` column that becomes `ts` occasionally regresses by up to ~500ms within
/// an asset's stream. `seq` is assigned in file-row order (the true order) — so `ts` is clamped
/// to `max(source_ts, prev_ts_for_this_asset)` per asset, keeping true order AND making `ts`
/// monotonic, so `HistStore::scan_book_updates`'s `(ts, seq)` sort reproduces file order instead
/// of reordering around the regression. `local_ts` is the raw ingest time and is already
/// monotonic, so it is passed through unclamped.
pub fn map_row(state: &mut MapState, row: &PmxtRow) -> Mapped {
    let asset = state.assets.entry(row.asset_id.clone()).or_default();

    match row.event_type.as_str() {
        "book" => {
            let (Some(bids), Some(asks)) = (row.bids.as_deref(), row.asks.as_deref()) else {
                return Mapped::None;
            };
            let ts = row.ts_ms.max(asset.last_ts);
            asset.last_ts = ts;
            let update = BookUpdate {
                ts,
                local_ts: row.local_ts_ms,
                seq: asset.seq,
                kind: BookUpdateKind::Snapshot,
                tick_size: asset.tick_size,
                bids: parse_levels(bids),
                asks: parse_levels(asks),
                symbol: row.asset_id.clone(),
            };
            asset.seq += 1;
            Mapped::Book(update)
        }
        "price_change" => {
            let (Some(price), Some(size), Some(side)) = (row.price, row.size, row.side.as_deref())
            else {
                return Mapped::None;
            };
            let level = (price, size);
            let (bids, asks) =
                if side == "BUY" { (vec![level], Vec::new()) } else { (Vec::new(), vec![level]) };
            let ts = row.ts_ms.max(asset.last_ts);
            asset.last_ts = ts;
            let update = BookUpdate {
                ts,
                local_ts: row.local_ts_ms,
                seq: asset.seq,
                kind: BookUpdateKind::Delta,
                tick_size: asset.tick_size,
                bids,
                asks,
                symbol: row.asset_id.clone(),
            };
            asset.seq += 1;
            Mapped::Book(update)
        }
        "last_trade_price" => {
            let (Some(price), Some(size)) = (row.price, row.size) else {
                return Mapped::None;
            };
            let ts = row.ts_ms.max(asset.last_ts);
            asset.last_ts = ts;
            Mapped::Trade(TradeTick {
                ts,
                local_ts: row.local_ts_ms,
                price,
                size,
                is_buyer_maker: row.side.as_deref() == Some("SELL"),
                symbol: row.asset_id.clone(),
            })
        }
        "tick_size_change" => {
            if let Some(new_tick_size) = row.new_tick_size {
                asset.tick_size = new_tick_size;
            }
            Mapped::None
        }
        _ => Mapped::None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_model::BookUpdateKind;

    fn row(ev: &str) -> PmxtRow {
        PmxtRow {
            event_type: ev.into(),
            ts_ms: 1000,
            local_ts_ms: 1001,
            asset_id: "TOK".into(),
            bids: None,
            asks: None,
            price: None,
            size: None,
            side: None,
            new_tick_size: None,
            best_bid: None,
            best_ask: None,
        }
    }

    #[test]
    fn parse_levels_decodes_json_pairs() {
        assert_eq!(
            parse_levels(r#"[["0.51","10.0"],["0.50","5"]]"#),
            vec![(0.51, 10.0), (0.50, 5.0)]
        );
        assert_eq!(parse_levels("[]"), vec![]);
        assert_eq!(parse_levels("garbage"), vec![], "malformed → empty, not panic");
    }

    #[test]
    fn book_row_maps_to_snapshot() {
        let mut st = MapState::default();
        let mut r = row("book");
        r.bids = Some(r#"[["0.5","10"]]"#.into());
        r.asks = Some(r#"[["0.51","8"]]"#.into());
        let Mapped::Book(b) = map_row(&mut st, &r) else { panic!() };
        assert_eq!(b.kind, BookUpdateKind::Snapshot);
        assert_eq!(b.bids, vec![(0.5, 10.0)]);
        assert_eq!(b.asks, vec![(0.51, 8.0)]);
        assert_eq!(b.ts, 1000);
        assert_eq!(b.local_ts, 1001);
        assert_eq!(b.symbol, "TOK");
        assert_eq!(b.tick_size, 0.01, "default tick_size before any tick_size_change");
        assert_eq!(b.seq, 0);
    }

    #[test]
    fn price_change_maps_to_side_correct_delta() {
        let mut st = MapState::default();
        let mut buy = row("price_change");
        buy.price = Some(0.42);
        buy.size = Some(7.0);
        buy.side = Some("BUY".into());
        let Mapped::Book(b) = map_row(&mut st, &buy) else { panic!() };
        assert_eq!(b.kind, BookUpdateKind::Delta);
        assert_eq!(b.bids, vec![(0.42, 7.0)]);
        assert_eq!(b.asks, vec![]);
        let mut sell = row("price_change");
        sell.price = Some(0.60);
        sell.size = Some(0.0);
        sell.side = Some("SELL".into());
        let Mapped::Book(s) = map_row(&mut st, &sell) else { panic!() };
        assert_eq!(s.asks, vec![(0.60, 0.0)], "SELL → asks; size 0 = level removed");
        assert_eq!(s.bids, vec![]);
        assert_eq!(s.seq, 1, "seq increments per asset across book updates");
    }

    #[test]
    fn last_trade_maps_to_trade_tick() {
        let mut st = MapState::default();
        let mut r = row("last_trade_price");
        r.price = Some(0.95);
        r.size = Some(3.0);
        r.side = Some("SELL".into());
        let Mapped::Trade(t) = map_row(&mut st, &r) else { panic!() };
        assert_eq!(t.price, 0.95);
        assert_eq!(t.size, 3.0);
        assert!(t.is_buyer_maker, "SELL taker → buyer was maker");
        assert_eq!(t.ts, 1000);
        assert_eq!(t.symbol, "TOK");
    }

    #[test]
    fn ts_clamped_non_decreasing_per_asset() {
        let mut st = MapState::default();

        let mut first = row("price_change");
        first.ts_ms = 2000;
        first.local_ts_ms = 2001;
        first.price = Some(0.42);
        first.size = Some(7.0);
        first.side = Some("BUY".into());
        let Mapped::Book(b1) = map_row(&mut st, &first) else { panic!() };
        assert_eq!(b1.ts, 2000);
        assert_eq!(b1.local_ts, 2001);

        // Source ts regresses (1500 < 2000) — must clamp up to the prior max, not pass through.
        let mut second = row("price_change");
        second.ts_ms = 1500;
        second.local_ts_ms = 1502;
        second.price = Some(0.43);
        second.size = Some(5.0);
        second.side = Some("BUY".into());
        let Mapped::Book(b2) = map_row(&mut st, &second) else { panic!() };
        assert_eq!(b2.ts, 2000, "ts clamped to prior max for this asset, not the regressed 1500");
        assert_eq!(b2.local_ts, 1502, "local_ts (raw ingest time) passes through unclamped");
        assert_eq!(b2.seq, 1, "seq still assigned in file order");

        // A different asset's clamp state is independent — its own last_ts starts at 0.
        let mut other = row("price_change");
        other.asset_id = "OTHER".into();
        other.ts_ms = 500;
        other.price = Some(0.10);
        other.size = Some(1.0);
        other.side = Some("SELL".into());
        let Mapped::Book(o1) = map_row(&mut st, &other) else { panic!() };
        assert_eq!(o1.ts, 500, "different asset's ts is unaffected by TOK's clamp state");
    }

    /// The L1 lane exists ONLY for `price_change` rows (the only ones the archive stamps with a
    /// top of book) — lifting it off a `book` row would fabricate a quote out of nulls.
    #[test]
    fn l1_is_lifted_from_price_change_rows_only() {
        let mut pc = row("price_change");
        pc.best_bid = Some(0.44);
        pc.best_ask = Some(0.47);
        let q = l1_from_row(&pc).expect("a price_change with a top of book yields an L1 quote");
        assert_eq!((q.bid, q.ask), (0.44, 0.47));
        assert_eq!((q.ts, q.local_ts, q.symbol.as_str()), (1000, 1001, "TOK"));
        assert_eq!((q.bid_size, q.ask_size), (0.0, 0.0), "the archive publishes prices only");

        let mut bk = row("book");
        bk.best_bid = Some(0.44);
        bk.best_ask = Some(0.47);
        assert!(l1_from_row(&bk).is_none(), "a book row is not an L1 source");
        assert!(l1_from_row(&row("price_change")).is_none(), "no top of book → no quote");
    }

    /// One side missing is still a usable quote — the prune only ever reads the ask.
    #[test]
    fn l1_survives_a_one_sided_top_of_book() {
        let mut pc = row("price_change");
        pc.best_ask = Some(0.47);
        let q = l1_from_row(&pc).expect("ask-only is still a quote");
        assert_eq!((q.bid, q.ask), (0.0, 0.47));
    }

    #[test]
    fn tick_size_change_tracks_for_next_book_update() {
        let mut st = MapState::default();
        let mut tsc = row("tick_size_change");
        tsc.new_tick_size = Some(0.001);
        assert!(matches!(map_row(&mut st, &tsc), Mapped::None), "tick_size_change stores no row");
        let mut b = row("book");
        b.bids = Some("[]".into());
        b.asks = Some("[]".into());
        let Mapped::Book(bu) = map_row(&mut st, &b) else { panic!() };
        assert_eq!(bu.tick_size, 0.001, "tracked tick_size applied to the next book update");
    }
}
