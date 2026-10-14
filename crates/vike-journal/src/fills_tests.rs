//! `fills_from_journal`: the filter (only bare `Event::Fill` records), the order (journal order,
//! never sorted) and the error (a missing directory is `JournalReadError::Io`).

use vike_model::events::{FillEvent, PositionSide, TradeId};

use super::*;
use crate::JournalFileConfig;
use crate::testutil::*;

fn fill(trade_id: &str, side: i32, px: f64, ts: i64) -> FillEvent {
    FillEvent {
        trade_id: TradeId::new(trade_id).unwrap(),
        client_order_id: "c1".to_string(),
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        side,
        last_qty: 1.0,
        last_px: px,
        commission: 0.1,
        commission_asset: "USDT".into(),
        liquidity_side: "maker".into(),
        ts,
        mark_price: None,
        position_side: PositionSide::Both,
    }
}

#[test]
fn only_fills_come_back_and_in_journal_order_not_time_order() {
    let dir = tmp_dir("fills-filter");
    let cfg = JournalFileConfig { segment_bytes: 64 * 1024, flush_every: 1 };
    let mut j = CommandJournal::open(&dir, cfg).unwrap();
    // Journal order: a command (skipped), a LATER-stamped fill, a command, an EARLIER-stamped fill.
    j.append_cmd(1_000, &ingest(0)).unwrap();
    j.append_cmd(1_001, &Ingest::Event(Event::Fill(fill("a", 1, 100.0, 30)))).unwrap();
    j.append_cmd(1_002, &ingest(1)).unwrap();
    j.append_cmd(1_003, &Ingest::Event(Event::Fill(fill("b", -1, 110.0, 10)))).unwrap();
    drop(j);

    let fills = fills_from_journal(&dir).unwrap();
    let got: Vec<(&str, i64)> = fills.iter().map(|f| (f.trade_id.as_str(), f.ts)).collect();
    assert_eq!(got, [("a", 30), ("b", 10)], "journal order, commands dropped, nothing sorted");
}

#[test]
fn a_missing_directory_is_an_io_error_naming_the_journal() {
    let dir = tmp_dir("fills-missing");
    let err = fills_from_journal(&dir.join("absent")).expect_err("no such journal");
    assert!(matches!(err, JournalReadError::Io(_)));
    assert!(err.to_string().starts_with("reading command journal: "), "{err}");
}

// ---- The journal door's OLD output, pinned through the new composition ---------------------------
//
// `vike-report`'s `tearsheet_from_journal(dir, seed, ppy)` was `fills_from_journal` followed by an
// UNSORTED fold, and `crates/vike/src/tearsheet_cli.rs`'s `build_realized` spelled the same four
// steps. Both are now `fills_from_journal` + `vike_analytics::tearsheet_from_fills` with
// `FillOrder::AsHeld`. The hashes are the OLD functions' output, recorded on a lane against the old
// code before the move (every catalog row's `f64::to_bits`, FNV-1a; the same constants pin the
// fold in `crates/vike-analytics/src/tearsheet_tests.rs`). Never re-pin one to make a test pass.

mod door_pin {
    use vike_analytics::{FillOrder, LiveTearsheet, tearsheet_from_fills};

    use super::*;

    const ORDERED_HASH: u64 = 0x02bf_1fef_90ab_7a53;
    const AS_HELD_HASH: u64 = 0xe3a1_5809_ee35_5d7e;

    fn journal_of(tag: &str, fills: &[FillEvent]) -> Scratch {
        let dir = tmp_dir(tag);
        let cfg = JournalFileConfig { segment_bytes: 64 * 1024, flush_every: 1 };
        let mut j = CommandJournal::open(&dir, cfg).unwrap();
        for (i, f) in fills.iter().enumerate() {
            j.append_cmd(1_000 + i as i64, &Ingest::Event(Event::Fill(f.clone()))).unwrap();
        }
        drop(j);
        dir
    }

    fn fnv(sheet: &LiveTearsheet) -> u64 {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        let rows = sheet.rows().map(|(s, v)| (s.id, v.map_or(u64::MAX, f64::to_bits)));
        for (id, b) in rows.chain([("schema", u64::from(sheet.schema))]) {
            for byte in id.bytes().chain(b.to_le_bytes()) {
                h ^= u64::from(byte);
                h = h.wrapping_mul(0x0100_0000_01b3);
            }
        }
        h
    }

    fn door(dir: &Path) -> LiveTearsheet {
        let fills = fills_from_journal(dir).unwrap();
        tearsheet_from_fills(fills, FillOrder::AsHeld, 10_000.0, 252.0).0
    }

    #[test]
    fn a_time_ordered_journal_folds_to_the_old_tearsheet() {
        let fills = vec![
            fill("t1", 1, 100.0, 10),
            fill("t2", 1, 130.0, 20),
            fill("t3", -1, 150.0, 30),
            fill("t4", -1, 140.0, 40),
            fill("t5", -1, 120.0, 50),
            fill("t6", 1, 125.0, 60),
        ];
        // Same fixture as the analytics pin, whose quantities differ per fill: rebuild them here.
        let qty = [2.0, 1.0, 1.0, 2.0, 1.0, 1.0];
        let fills: Vec<FillEvent> =
            fills.into_iter().zip(qty).map(|(f, q)| FillEvent { last_qty: q, ..f }).collect();
        let dir = journal_of("door-ordered", &fills);
        assert_eq!(fnv(&door(&dir)), ORDERED_HASH);
    }

    #[test]
    fn a_journal_whose_ts_is_not_ascending_is_folded_as_read() {
        let mut fills =
            vec![fill("u1", -1, 110.0, 30), fill("u2", 1, 100.0, 10), fill("u3", 1, 105.0, 20)];
        fills[0].last_qty = 2.0;
        let dir = journal_of("door-unordered", &fills);
        let sheet = door(&dir);
        assert_eq!(fnv(&sheet), AS_HELD_HASH);
        assert_eq!(sheet.metric("n_trades"), Some(2.0), "journal order, not time order");
    }
}
