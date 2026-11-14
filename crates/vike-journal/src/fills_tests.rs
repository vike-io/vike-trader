//! `fills_from_journal`: the filter (only bare `Event::Fill` records), the order (journal order,
//! never sorted) and the error (a missing directory is `JournalReadError::Io`).

use vike_execution::{GatewayEvent, Ingest};
use vike_model::EventRingCursor;
use vike_model::events::{FillEvent, PositionSide, TradeId};

use super::*;
use crate::JournalFileConfig;
use crate::testutil::*;

fn fill(trade_id: &str, side: i32, px: f64, ts: i64) -> FillEvent {
    FillEvent {
        trade_id: TradeId::new(trade_id).unwrap(),
        order_client_id: "c1".to_string(),
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        side,
        last_qty: 1.0,
        last_price: px,
        commission: 0.1,
        commission_asset: "USDT".into(),
        liquidity_side: "maker".into(),
        timestamp: ts,
        mark_price: None,
        position_side: PositionSide::Both,
    }
}

#[test]
fn only_fills_come_back_and_in_journal_order_not_time_order() {
    let dir = tmp_dir("fills-filter");
    let config = JournalFileConfig { segment_bytes: 64 * 1024, flush_every: 1 };
    let mut j = Journal::open(&dir, config).unwrap();
    // Journal order: a command (skipped), a LATER-stamped fill, a command, an EARLIER-stamped fill.
    j.append_command(1_000, &ingest(0)).unwrap();
    j.append_command(1_001, &Ingest::Event(Event::Fill(fill("a", 1, 100.0, 30)))).unwrap();
    j.append_command(1_002, &ingest(1)).unwrap();
    j.append_command(1_003, &Ingest::Event(Event::Fill(fill("b", -1, 110.0, 10)))).unwrap();
    drop(j);

    let fills = fills_from_journal(&dir).unwrap();
    let got: Vec<(&str, i64)> = fills.iter().map(|f| (f.trade_id.as_str(), f.timestamp)).collect();
    assert_eq!(got, [("a", 30), ("b", 10)], "journal order, commands dropped, nothing sorted");
}

/// **A fee correction is folded into the fill it restates** (`Event::FeeAdjustment`, decision 0238), so the tearsheet
/// reads the fee the account holds: the LAST correction of an execution wins (each states the total over the fill), a
/// fill that named no fee currency takes the correction's, a fill nobody corrected is untouched, and a correction is no
/// fill of its own (the list holds two fills, not three records).
#[test]
fn a_fee_correction_is_folded_into_the_fill_it_restates() {
    let adjustment = |trade_id: &str, extra: f64| {
        Ingest::Event(Event::FeeAdjustment(vike_model::events::FeeAdjustmentEvent {
            trade_id: TradeId::new(trade_id).unwrap(),
            order_client_id: "c1".to_string(),
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            last_price: 100.0,
            extra_commission: extra,
            commission_asset: "USDT".into(),
            timestamp: 99,
        }))
    };
    let dir = tmp_dir("fills-fee-correction");
    let config = JournalFileConfig { segment_bytes: 64 * 1024, flush_every: 1 };
    let mut j = Journal::open(&dir, config).unwrap();
    let mut unknown = fill("a", 1, 100.0, 30);
    unknown.commission = 0.0;
    unknown.commission_asset = "".into();
    j.append_command(1_000, &Ingest::Event(Event::Fill(unknown))).unwrap();
    j.append_command(1_001, &Ingest::Event(Event::Fill(fill("b", -1, 110.0, 40)))).unwrap();
    j.append_command(1_002, &adjustment("a", 1.25)).unwrap();
    j.append_command(1_003, &adjustment("a", 1.40)).unwrap();
    j.append_command(1_004, &adjustment("b", 0.05)).unwrap();
    drop(j);

    let fills = fills_from_journal(&dir).unwrap();
    assert_eq!(fills.len(), 2);
    assert_eq!((fills[0].commission, fills[0].commission_asset.as_str()), (1.40, "USDT"));
    assert!((fills[1].commission - 0.15).abs() < 1e-12, "{}", fills[1].commission);
    let (both, _, _) = fills_and_ledgers_from_journal(&dir).unwrap();
    assert_eq!(both[0].commission, fills[0].commission, "the one-read twin folds the same");
}

/// An engine of `venue` told its account currency is `currency` (`None` = never told: the constructor's `USDT`).
fn engine_snapshot(venue: &str, currency: Option<&str>) -> vike_execution::EngineSnapshot {
    use vike_execution::testing::RecordingClient;
    use vike_execution::{Account, BalanceMode, Engine, RiskGate};
    use vike_model::RiskLimits;
    let mut e = Engine::new(
        Account::new(1.0, venue, None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        venue,
        "EURUSD",
    );
    if let Some(currency) = currency {
        e.set_account_currency(currency).expect("a currency code");
    }
    e.snapshot_state()
}

/// **The ledgers the journal's snapshots record come back with the fills, in the one read** (decision 0183,
/// item 13): a tearsheet door needs to know which venue books in a currency that is not the dollar. The dollar
/// group (`USD`, `USDT`, an engine nobody told) is not listed, a pair is listed once however many snapshots
/// repeat it, and a journal with no snapshot lists nothing.
#[test]
fn the_non_dollar_ledgers_of_the_snapshots_come_back_beside_the_fills() {
    let dir = tmp_dir("fills-ledgers");
    let config = JournalFileConfig { segment_bytes: 64 * 1024, flush_every: 1 };
    let mut j = Journal::open(&dir, config.clone()).unwrap();
    let engines = vec![
        engine_snapshot("binance", None),
        engine_snapshot("oanda", Some("usd")),
        engine_snapshot("ig", Some("USDT")),
        engine_snapshot("fxcm", Some("eur")),
    ];
    let books = crate::record::JournalSnapshotParts::default;
    j.append_snap(1_000, &engines, "sess", 0, 0, books(), 1).unwrap();
    j.append_command(1_001, &Ingest::Event(Event::Fill(fill("a", 1, 100.0, 30)))).unwrap();
    j.append_snap(1_002, &engines, "sess", 0, 0, books(), 2).unwrap();
    drop(j);

    let (fills, ledgers, contracts) = fills_and_ledgers_from_journal(&dir).unwrap();
    assert_eq!(fills.len(), 1);
    assert_eq!(
        ledgers,
        [("fxcm".to_string(), "EUR".to_string())],
        "once, upper-cased, dollars out"
    );
    assert!(contracts.is_empty(), "no engine here values a contract off the default");

    // no snapshot: nothing to say, and the fills are exactly `fills_from_journal`'s
    let bare = tmp_dir("fills-no-snap");
    let mut j = Journal::open(&bare, config).unwrap();
    j.append_command(1_000, &Ingest::Event(Event::Fill(fill("a", 1, 100.0, 30)))).unwrap();
    drop(j);
    let (fills, ledgers, contracts) = fills_and_ledgers_from_journal(&bare).unwrap();
    assert!(ledgers.is_empty());
    assert!(contracts.is_empty());
    assert_eq!(fills, fills_from_journal(&bare).unwrap());
}

/// An engine of `venue` valuing `symbol` at each `value`.
fn engine_valuing(venue: &str, values: &[(&str, ContractValue)]) -> vike_execution::EngineSnapshot {
    use vike_execution::testing::RecordingClient;
    use vike_execution::{Account, BalanceMode, Engine, RiskGate};
    use vike_model::RiskLimits;
    let grid = values.iter().map(|(s, v)| ((*s).to_string(), *v)).collect();
    Engine::new(
        Account::new(1.0, venue, Some(grid), BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        venue,
        "BTCUSDT",
    )
    .snapshot_state()
}

/// **The contract values a sheet cannot read come back with the fills** (decision 0243, item 6): inverse at any
/// factor and linear off 1.0 are listed, with the default a paper engine's grid states left out, a row once however many
/// snapshots repeat it, and on an engine of a DOLLAR ledger too.
#[test]
fn the_contract_values_off_the_default_come_back_beside_the_fills() {
    let dir = tmp_dir("fills-contracts");
    let config = JournalFileConfig { segment_bytes: 64 * 1024, flush_every: 1 };
    let mut j = Journal::open(&dir, config).unwrap();
    let engines = vec![
        engine_valuing("binance", &[("BTCUSDT", ContractValue::default())]),
        engine_valuing("deribit", &[("BTC-PERPETUAL", ContractValue::inverse(1.0))]),
        engine_valuing("ibkr", &[("ES", ContractValue::linear(50.0))]),
    ];
    let books = crate::record::JournalSnapshotParts::default;
    j.append_snap(1_000, &engines, "sess", 0, 0, books(), 1).unwrap();
    j.append_snap(1_001, &engines, "sess", 0, 0, books(), 2).unwrap();
    drop(j);

    let (_, ledgers, contracts) = fills_and_ledgers_from_journal(&dir).unwrap();
    assert!(ledgers.is_empty());
    assert_eq!(
        contracts,
        [
            ("deribit".to_string(), "BTC-PERPETUAL".to_string(), ContractValue::inverse(1.0)),
            ("ibkr".to_string(), "ES".to_string(), ContractValue::linear(50.0)),
        ]
    );
}

/// **Why a door leaves those fills out, in the row's numbers**: 1,000 USD of inverse `BTC-PERPETUAL` from 60,000 to
/// 66,000 books +100 on the account and folds to +6,000,000 on a sheet, which reads every unit as linear 1.0. Left out,
/// the sheet holds nothing it cannot value.
#[test]
fn an_inverse_round_trip_is_100_on_the_account_and_no_trade_on_the_sheet() {
    use vike_analytics::{
        TearsheetFillOrder, reconstruct_trades, tearsheet_from_fills, without_non_default_contracts,
    };
    use vike_execution::{Account, BalanceMode};

    let symbol = "BTC-PERPETUAL";
    let on_deribit = |id: &str, side: i32, px: f64, ts: i64| FillEvent {
        venue: "deribit".into(),
        symbol: symbol.into(),
        last_qty: 1_000.0,
        commission: 0.0,
        ..fill(id, side, px, ts)
    };
    let fills = vec![on_deribit("a", 1, 60_000.0, 1), on_deribit("b", -1, 66_000.0, 2)];

    let inverse = ContractValue::inverse(1.0);
    let grid = [(symbol.to_string(), inverse)].into_iter().collect();
    let mut account = Account::new(1.0, "deribit", Some(grid), BalanceMode::Delta);
    for f in &fills {
        account.apply_fill(f);
    }
    assert!((account.realized_pnl - 100.0).abs() < 1e-9, "{}", account.realized_pnl);
    assert_eq!(reconstruct_trades(&fills)[0].pnl, 6_000_000.0, "what the linear fold reads");

    let contracts = [("deribit".to_string(), symbol.to_string(), inverse)];
    let (kept, left) = without_non_default_contracts(fills, &contracts);
    assert!(kept.is_empty());
    assert_eq!(left[0].fills, 2);
    let sheet = tearsheet_from_fills(kept, TearsheetFillOrder::AsHeld, 10_000.0, 252.0).0;
    assert_eq!(sheet.metric("n_trades"), Some(0.0));
}

/// A fill that came over a gateway's trade link is journaled as an `Ingest::GatewayEvent` (v18),
/// and it is a fill: a tearsheet that ignored it would silently lose every trade of a tradehub that
/// runs behind a gateway. Journal order holds across the two doors.
#[test]
fn a_fill_that_came_over_a_gateway_is_a_fill() {
    let dir = tmp_dir("fills-gateway");
    let config = JournalFileConfig { segment_bytes: 64 * 1024, flush_every: 1 };
    let mut j = Journal::open(&dir, config).unwrap();
    let over_gateway = |f: FillEvent, seq: u64| {
        Ingest::GatewayEvent(Box::new(GatewayEvent {
            event: Event::Fill(f),
            route_key: "binance".into(),
            cursor: EventRingCursor { gateway_epoch: 3, sequence: seq, timestamp_ms: 0 },
        }))
    };
    j.append_command(1_000, &Ingest::Event(Event::Fill(fill("a", 1, 100.0, 30)))).unwrap();
    j.append_command(1_001, &over_gateway(fill("b", -1, 110.0, 10), 1)).unwrap();
    // a non-fill event over the gateway is skipped like any other
    j.append_command(
        1_002,
        &Ingest::GatewayEvent(Box::new(GatewayEvent {
            event: Event::OrderSubmitted(vike_model::events::OrderSubmitted {
                order_client_id: "c1".into(),
                timestamp: 1,
            }),
            route_key: "binance".into(),
            cursor: EventRingCursor { gateway_epoch: 3, sequence: 2, timestamp_ms: 0 },
        })),
    )
    .unwrap();
    j.append_command(1_003, &over_gateway(fill("c", 1, 120.0, 20), 3)).unwrap();
    drop(j);

    let fills = fills_from_journal(&dir).unwrap();
    let got: Vec<&str> = fills.iter().map(|f| f.trade_id.as_str()).collect();
    assert_eq!(got, ["a", "b", "c"], "both doors, journal order, the lifecycle event skipped");
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
// `vike-report`'s `tearsheet_from_journal(dir, seed, periods_per_year)` was `fills_from_journal` followed by an
// UNSORTED fold, and `crates/vike/src/tearsheet_cli.rs`'s `build_realized` spelled the same four
// steps. Both are now `fills_from_journal` + `vike_analytics::tearsheet_from_fills` with
// `TearsheetFillOrder::AsHeld`. The hashes are the OLD functions' output, recorded on a lane against the old
// code before the move (every catalog row's `f64::to_bits`, FNV-1a; the same constants pin the
// fold in `crates/vike-analytics/src/tearsheet_tests.rs`). Never re-pin one to make a test pass.
//
// ⚠ RE-PINNED ONCE, 2026-11-09, with the analytics pin and for the same owner-ordered change, not to
// pass: `mar_ratio` left the catalog, the schema went to 2 and the curve is one sample per UTC day.
// Was `ORDERED_HASH = 0x02bf_1fef_90ab_7a53`, `AS_HELD_HASH = 0xe3a1_5809_ee35_5d7e`.

mod door_pin {
    use vike_analytics::{Tearsheet, TearsheetFillOrder, tearsheet_from_fills};

    use super::*;

    // RE-PINNED 2026-11-13 for the owner's phase-4 rename (P4.5c): the serialized key and catalog-id
    // words are spelled out; every value is the same (the comparisons beside each pin still pass).
    // Was `0xf478_fbbb_2f33_406e` / `0x81a7_131b_d75d_a061`.
    const ORDERED_HASH: u64 = 0x4afc_2972_8f5a_24dc;
    const AS_HELD_HASH: u64 = 0xb629_eee6_a899_8f7b;

    fn journal_of(tag: &str, fills: &[FillEvent]) -> Scratch {
        let dir = tmp_dir(tag);
        let config = JournalFileConfig { segment_bytes: 64 * 1024, flush_every: 1 };
        let mut j = Journal::open(&dir, config).unwrap();
        for (i, f) in fills.iter().enumerate() {
            j.append_command(1_000 + i as i64, &Ingest::Event(Event::Fill(f.clone()))).unwrap();
        }
        drop(j);
        dir
    }

    fn fnv(sheet: &Tearsheet) -> u64 {
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

    fn door(dir: &Path) -> Tearsheet {
        let fills = fills_from_journal(dir).unwrap();
        tearsheet_from_fills(fills, TearsheetFillOrder::AsHeld, 10_000.0, 252.0).0
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
    fn a_journal_whose_timestamp_is_not_ascending_is_folded_as_read() {
        let mut fills =
            vec![fill("u1", -1, 110.0, 30), fill("u2", 1, 100.0, 10), fill("u3", 1, 105.0, 20)];
        fills[0].last_qty = 2.0;
        let dir = journal_of("door-unordered", &fills);
        let sheet = door(&dir);
        assert_eq!(fnv(&sheet), AS_HELD_HASH);
        assert_eq!(sheet.metric("n_trades"), Some(2.0), "journal order, not time order");
    }
}
