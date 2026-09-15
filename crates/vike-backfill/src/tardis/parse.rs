//! Pure, header-name-driven Tardis CSV parsers (no I/O — fixture-tested). Tardis timestamps are
//! microseconds (`/US_PER_MS` → ms). Trades `side` is `buy`/`sell` (sell taker ⇒ buyer is maker).
//! `incremental_book_L2` rows are single-level: `is_snapshot` ⇒ Snapshot else Delta; `side` bid/ask
//! routes the level; `amount` is the absolute size (0 = level removed). A monotonic per-symbol `seq`
//! is assigned in file order so `scan_book_updates` reproduces file order.

use vike_model::{BookUpdate, BookUpdateKind, QuoteTick, TradeTick};

use crate::csvutil::{Header, f64_of, header_and_rows, split_line, ts_ms as csv_ts_ms};

pub const US_PER_MS: i64 = 1_000;

/// µs column → ms (the shared [`crate::csvutil::ts_ms`] with Tardis's `US_PER_MS` divisor).
fn ts_ms(row: &[&str], h: &Header, name: &str) -> Option<i64> {
    csv_ts_ms(row, h, name, US_PER_MS)
}

pub fn parse_trades(csv: &str) -> Vec<TradeTick> {
    let Some((h, rows)) = header_and_rows(csv) else { return Vec::new() };
    let mut out = Vec::new();
    for line in rows {
        let row = split_line(line);
        let (Some(ts), Some(price), Some(size)) =
            (ts_ms(&row, &h, "timestamp"), f64_of(&row, &h, "price"), f64_of(&row, &h, "amount"))
        else {
            continue;
        };
        out.push(TradeTick {
            ts,
            local_ts: ts_ms(&row, &h, "local_timestamp").unwrap_or(0),
            price,
            size,
            is_buyer_maker: h.get(&row, "side") == Some("sell"),
            symbol: h.get(&row, "symbol").unwrap_or("").to_string(),
        });
    }
    out
}

pub fn parse_quotes(csv: &str) -> Vec<QuoteTick> {
    let Some((h, rows)) = header_and_rows(csv) else { return Vec::new() };
    let mut out = Vec::new();
    for line in rows {
        let row = split_line(line);
        let (Some(ts), Some(bid), Some(ask)) = (
            ts_ms(&row, &h, "timestamp"),
            f64_of(&row, &h, "bid_price"),
            f64_of(&row, &h, "ask_price"),
        ) else {
            continue;
        };
        out.push(QuoteTick {
            ts,
            local_ts: ts_ms(&row, &h, "local_timestamp").unwrap_or(0),
            bid,
            ask,
            bid_size: f64_of(&row, &h, "bid_amount").unwrap_or(0.0),
            ask_size: f64_of(&row, &h, "ask_amount").unwrap_or(0.0),
            symbol: h.get(&row, "symbol").unwrap_or("").to_string(),
        });
    }
    out
}

pub fn parse_book_l2(csv: &str) -> Vec<BookUpdate> {
    let Some((h, rows)) = header_and_rows(csv) else { return Vec::new() };
    let mut out = Vec::new();
    let mut seq: u64 = 0;
    for line in rows {
        let row = split_line(line);
        let (Some(ts), Some(price), Some(amount), Some(side)) = (
            ts_ms(&row, &h, "timestamp"),
            f64_of(&row, &h, "price"),
            f64_of(&row, &h, "amount"),
            h.get(&row, "side"),
        ) else {
            continue;
        };
        let level = (price, amount);
        let (bids, asks) =
            if side == "bid" { (vec![level], Vec::new()) } else { (Vec::new(), vec![level]) };
        let kind = if h.get(&row, "is_snapshot") == Some("true") {
            BookUpdateKind::Snapshot
        } else {
            BookUpdateKind::Delta
        };
        out.push(BookUpdate {
            ts,
            local_ts: ts_ms(&row, &h, "local_timestamp").unwrap_or(0),
            seq,
            kind,
            tick_size: 0.0,
            bids,
            asks,
            symbol: h.get(&row, "symbol").unwrap_or("").to_string(),
        });
        seq += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trades_us_to_ms_and_maker_flag() {
        let csv = "exchange,symbol,timestamp,local_timestamp,id,side,price,amount\n\
                   deribit,BTC-PERPETUAL,1700000000123456,1700000000200000,42,sell,35000.5,3\n";
        let t = parse_trades(csv);
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].ts, 1_700_000_000_123, "µs → ms (truncating)");
        assert_eq!(t[0].local_ts, 1_700_000_000_200);
        assert!((t[0].price - 35000.5).abs() < 1e-9);
        assert!((t[0].size - 3.0).abs() < 1e-9);
        assert!(t[0].is_buyer_maker, "sell taker → buyer is maker");
        assert_eq!(t[0].symbol, "BTC-PERPETUAL");
    }

    #[test]
    fn quotes_maps_bbo() {
        let csv = "exchange,symbol,timestamp,local_timestamp,ask_amount,ask_price,bid_price,bid_amount\n\
                   deribit,BTC-PERPETUAL,1700000000000000,1700000000050000,5,35001,34999,3\n";
        let q = parse_quotes(csv);
        assert_eq!(q.len(), 1);
        assert_eq!(q[0].ts, 1_700_000_000_000);
        assert!((q[0].bid - 34999.0).abs() < 1e-9);
        assert!((q[0].ask - 35001.0).abs() < 1e-9);
        assert!((q[0].bid_size - 3.0).abs() < 1e-9);
        assert!((q[0].ask_size - 5.0).abs() < 1e-9);
        assert_eq!(q[0].symbol, "BTC-PERPETUAL");
    }

    #[test]
    fn book_l2_snapshot_delta_side_and_removal() {
        let csv = "exchange,symbol,timestamp,local_timestamp,is_snapshot,side,price,amount\n\
                   deribit,BTC-PERPETUAL,1700000000000000,1700000000000001,true,bid,34999,10\n\
                   deribit,BTC-PERPETUAL,1700000000001000,1700000000001002,false,ask,35001,0\n";
        let b = parse_book_l2(csv);
        assert_eq!(b.len(), 2);
        assert_eq!(b[0].kind, BookUpdateKind::Snapshot);
        assert_eq!(b[0].bids, vec![(34999.0, 10.0)]);
        assert_eq!(b[0].asks, vec![]);
        assert_eq!(b[0].seq, 0);
        assert_eq!(b[1].kind, BookUpdateKind::Delta);
        assert_eq!(b[1].asks, vec![(35001.0, 0.0)], "amount 0 = level removed (kept as size 0)");
        assert_eq!(b[1].bids, vec![]);
        assert_eq!(b[1].seq, 1, "monotonic per-symbol seq in file order");
    }

    #[test]
    fn empty_csv_is_empty() {
        assert!(parse_trades("").is_empty());
        assert!(parse_book_l2("exchange,symbol\n").is_empty());
    }
}
