//! Pure, header-name-driven Databento CSV parsers (no I/O, no clock — fixture-tested).
//! Raw CSV: prices are 1e-9 fixed-point integers (`÷PX_SCALE`), timestamps are nanoseconds
//! (`/NS_PER_MS` → ms). `side` is the aggressor: `A` = sell aggressor (buyer is maker), `B` = buy
//! aggressor. Columns are resolved by NAME via `Header`, so schema column-order drift is tolerated.

use vike_model::{Bar, BookUpdate, BookUpdateKind, QuoteTick, TradeTick};

use crate::csvutil::{f64_of, header_and_rows, i64_of, split_line, ts_ms as csv_ts_ms, Header};

pub const PX_SCALE: f64 = 1e9;
pub const NS_PER_MS: i64 = 1_000_000;

/// Databento's raw-CSV "undefined price" sentinel (`INT64_MAX`), used for unpopulated book levels
/// (`mbp-10` rows with fewer than 10 levels a side) and the absent side of a one-sided `mbp-1`
/// quote. It is NOT zero — a genuine price of 0 is valid and must be kept.
pub const UNDEF_PRICE: i64 = i64::MAX;

/// Scaled price: raw 1e-9 fixed-point integer → f64 dollars. `None` on parse failure or the
/// [`UNDEF_PRICE`] sentinel (so an empty book level / absent quote side is skipped). A real 0 price
/// is preserved — only `INT64_MAX` means "no price".
fn px(row: &[&str], h: &Header, name: &str) -> Option<f64> {
    let raw = i64_of(row, h, name)?;
    (raw != UNDEF_PRICE).then_some(raw as f64 / PX_SCALE)
}

/// ns column → ms (the shared [`crate::csvutil::ts_ms`] with Databento's `NS_PER_MS` divisor).
fn ts_ms(row: &[&str], h: &Header, name: &str) -> Option<i64> {
    csv_ts_ms(row, h, name, NS_PER_MS)
}

pub fn parse_trades(csv: &str) -> Vec<TradeTick> {
    let Some((h, rows)) = header_and_rows(csv) else { return Vec::new() };
    let mut out = Vec::new();
    for line in rows {
        let row = split_line(line);
        let (Some(ts), Some(price), Some(size)) =
            (ts_ms(&row, &h, "ts_event"), px(&row, &h, "price"), f64_of(&row, &h, "size"))
        else {
            continue;
        };
        out.push(TradeTick {
            ts,
            local_ts: ts_ms(&row, &h, "ts_recv").unwrap_or(0),
            price,
            size,
            is_buyer_maker: h.get(&row, "side") == Some("A"),
            symbol: h.get(&row, "symbol").unwrap_or("").to_string(),
        });
    }
    out
}

pub fn parse_ohlcv(csv: &str) -> Vec<Bar> {
    let Some((h, rows)) = header_and_rows(csv) else { return Vec::new() };
    let mut out = Vec::new();
    for line in rows {
        let row = split_line(line);
        let (Some(ts), Some(open), Some(high), Some(low), Some(close)) = (
            ts_ms(&row, &h, "ts_event"),
            px(&row, &h, "open"),
            px(&row, &h, "high"),
            px(&row, &h, "low"),
            px(&row, &h, "close"),
        ) else {
            continue;
        };
        out.push(Bar {
            ts,
            open,
            high,
            low,
            close,
            volume: f64_of(&row, &h, "volume").unwrap_or(0.0),
            funding: None,
            bid: None,
            ask: None,
            symbol: h.get(&row, "symbol").map(str::to_string),
        });
    }
    out
}

pub fn parse_quotes_mbp1(csv: &str) -> Vec<QuoteTick> {
    let Some((h, rows)) = header_and_rows(csv) else { return Vec::new() };
    let mut out = Vec::new();
    for line in rows {
        let row = split_line(line);
        let (Some(ts), Some(bid), Some(ask)) =
            (ts_ms(&row, &h, "ts_event"), px(&row, &h, "bid_px_00"), px(&row, &h, "ask_px_00"))
        else {
            continue;
        };
        out.push(QuoteTick {
            ts,
            local_ts: ts_ms(&row, &h, "ts_recv").unwrap_or(0),
            bid,
            ask,
            bid_size: f64_of(&row, &h, "bid_sz_00").unwrap_or(0.0),
            ask_size: f64_of(&row, &h, "ask_sz_00").unwrap_or(0.0),
            symbol: h.get(&row, "symbol").unwrap_or("").to_string(),
        });
    }
    out
}

pub fn parse_book_mbp10(csv: &str) -> Vec<BookUpdate> {
    let Some((h, rows)) = header_and_rows(csv) else { return Vec::new() };
    let mut out = Vec::new();
    let mut seq: u64 = 0;
    for line in rows {
        let row = split_line(line);
        let Some(ts) = ts_ms(&row, &h, "ts_event") else { continue };
        let side = |pxpfx: &str, szpfx: &str| -> Vec<(f64, f64)> {
            (0..10)
                .filter_map(|i| {
                    let p = px(&row, &h, &format!("{pxpfx}_0{i}"))?;
                    let s = f64_of(&row, &h, &format!("{szpfx}_0{i}")).unwrap_or(0.0);
                    Some((p, s))
                })
                .collect()
        };
        out.push(BookUpdate {
            ts,
            local_ts: ts_ms(&row, &h, "ts_recv").unwrap_or(0),
            seq,
            kind: BookUpdateKind::Snapshot,
            tick_size: 0.0,
            bids: side("bid_px", "bid_sz"),
            asks: side("ask_px", "ask_sz"),
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
    fn trades_scales_price_and_ns_to_ms_and_maker_flag() {
        let csv = "ts_recv,ts_event,rtype,publisher_id,instrument_id,action,side,depth,price,size,flags,ts_in_delta,sequence,symbol\n\
                   1700000000123000000,1700000000000000000,1,1,42,T,A,0,4200000000000,7,0,0,1,ESZ4\n";
        let t = parse_trades(csv);
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].ts, 1_700_000_000_000, "ts_event ns → ms");
        assert_eq!(t[0].local_ts, 1_700_000_000_123, "ts_recv ns → ms");
        assert!((t[0].price - 4200.0).abs() < 1e-9, "4200000000000 / 1e9 = 4200.0");
        assert!((t[0].size - 7.0).abs() < 1e-9);
        assert!(t[0].is_buyer_maker, "side A = sell aggressor → buyer is maker");
        assert_eq!(t[0].symbol, "ESZ4");
    }

    #[test]
    fn ohlcv_maps_full_bar() {
        let csv = "ts_event,rtype,publisher_id,instrument_id,open,high,low,close,volume,symbol\n\
                   1700000000000000000,34,1,42,4200000000000,4300000000000,4100000000000,4250000000000,1000,ESZ4\n";
        let b = parse_ohlcv(csv);
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].ts, 1_700_000_000_000);
        assert!((b[0].open - 4200.0).abs() < 1e-9);
        assert!((b[0].high - 4300.0).abs() < 1e-9);
        assert!((b[0].low - 4100.0).abs() < 1e-9);
        assert!((b[0].close - 4250.0).abs() < 1e-9);
        assert!((b[0].volume - 1000.0).abs() < 1e-9, "volume is NOT price-scaled");
        assert_eq!(b[0].symbol.as_deref(), Some("ESZ4"));
    }

    #[test]
    fn mbp1_maps_bbo_quote() {
        let csv = "ts_recv,ts_event,rtype,publisher_id,instrument_id,action,side,depth,price,size,flags,ts_in_delta,sequence,bid_px_00,ask_px_00,bid_sz_00,ask_sz_00,bid_ct_00,ask_ct_00,symbol\n\
                   1700000000123000000,1700000000000000000,1,1,42,T,B,0,4200000000000,7,0,0,1,4199000000000,4201000000000,3,5,1,1,ESZ4\n";
        let q = parse_quotes_mbp1(csv);
        assert_eq!(q.len(), 1);
        assert_eq!(q[0].ts, 1_700_000_000_000);
        assert!((q[0].bid - 4199.0).abs() < 1e-9);
        assert!((q[0].ask - 4201.0).abs() < 1e-9);
        assert!((q[0].bid_size - 3.0).abs() < 1e-9);
        assert!((q[0].ask_size - 5.0).abs() < 1e-9);
        assert_eq!(q[0].symbol, "ESZ4");
    }

    #[test]
    fn mbp10_maps_depth_snapshot_skipping_empty_levels() {
        // 2 populated levels per side; the remaining 8 use Databento's real undefined-price
        // sentinel INT64_MAX (9223372036854775807) — those levels must be skipped, NOT emitted as
        // phantom (9_223_372_036.85, 0.0) levels.
        let mut header = String::from("ts_recv,ts_event,rtype,publisher_id,instrument_id,action,side,depth,price,size,flags,ts_in_delta,sequence");
        for i in 0..10 {
            header += &format!(",bid_px_0{i}");
        }
        for i in 0..10 {
            header += &format!(",ask_px_0{i}");
        }
        for i in 0..10 {
            header += &format!(",bid_sz_0{i}");
        }
        for i in 0..10 {
            header += &format!(",ask_sz_0{i}");
        }
        for i in 0..10 {
            header += &format!(",bid_ct_0{i}");
        }
        for i in 0..10 {
            header += &format!(",ask_ct_0{i}");
        }
        header += ",symbol";
        let mut row = String::from(
            "1700000000123000000,1700000000000000000,1,1,42,T,B,0,4200000000000,7,0,0,1",
        );
        const U: &str = "9223372036854775807"; // INT64_MAX = UNDEF_PRICE
        let bid_px = ["4199000000000", "4198000000000", U, U, U, U, U, U, U, U];
        let ask_px = ["4201000000000", "4202000000000", U, U, U, U, U, U, U, U];
        let bid_sz = ["3", "4", "0", "0", "0", "0", "0", "0", "0", "0"];
        let ask_sz = ["5", "6", "0", "0", "0", "0", "0", "0", "0", "0"];
        for v in bid_px {
            row += &format!(",{v}");
        }
        for v in ask_px {
            row += &format!(",{v}");
        }
        for v in bid_sz {
            row += &format!(",{v}");
        }
        for v in ask_sz {
            row += &format!(",{v}");
        }
        for _ in 0..20 {
            row += ",1";
        } // bid_ct/ask_ct
        row += ",ESZ4";
        let csv = format!("{header}\n{row}\n");
        let b = parse_book_mbp10(&csv);
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].kind, BookUpdateKind::Snapshot);
        assert_eq!(
            b[0].bids,
            vec![(4199.0, 3.0), (4198.0, 4.0)],
            "only populated levels, price-scaled"
        );
        assert_eq!(b[0].asks, vec![(4201.0, 5.0), (4202.0, 6.0)]);
        assert_eq!(b[0].seq, 0);
        assert_eq!(b[0].ts, 1_700_000_000_000);
        assert_eq!(b[0].symbol, "ESZ4");
    }

    #[test]
    fn genuine_zero_price_trade_is_kept_only_int64max_is_undefined() {
        // A real price of 0 (raw "0") must be preserved — only INT64_MAX means "no price".
        let csv = "ts_recv,ts_event,rtype,publisher_id,instrument_id,action,side,depth,price,size,flags,ts_in_delta,sequence,symbol\n\
                   1700000000123000000,1700000000000000000,1,1,42,T,B,0,0,5,0,0,1,SPREAD\n";
        let t = parse_trades(csv);
        assert_eq!(t.len(), 1, "a 0-price trade is a real trade, not skipped");
        assert_eq!(t[0].price, 0.0);
        assert_eq!(t[0].size, 5.0);
    }

    #[test]
    fn mbp1_one_sided_quote_absent_side_is_undef_and_row_skipped() {
        // Absent ask side is INT64_MAX → no valid BBO → the row yields no QuoteTick.
        let csv = "ts_recv,ts_event,rtype,publisher_id,instrument_id,action,side,depth,price,size,flags,ts_in_delta,sequence,bid_px_00,ask_px_00,bid_sz_00,ask_sz_00,bid_ct_00,ask_ct_00,symbol\n\
                   1700000000123000000,1700000000000000000,1,1,42,T,B,0,4200000000000,7,0,0,1,4199000000000,9223372036854775807,3,0,1,0,ESZ4\n";
        assert!(parse_quotes_mbp1(csv).is_empty(), "one-sided quote (undef ask) is skipped");
    }

    #[test]
    fn empty_or_header_only_csv_is_empty() {
        assert!(parse_trades("").is_empty());
        assert!(parse_trades("ts_event,price\n").is_empty());
    }
}
