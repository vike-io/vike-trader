//! **USDⓈ-M perpetual funding from the income history** (`vike_binance::execution::funding`), offline, over the venue's own words:
//! `crates/bridges/binance/tests/fixtures/funding_income_demo.json`, the demo account's 16 `FUNDING_FEE` rows of a
//! BTCUSDT position (late October 2026) and its latest week of every income kind, RECORDED read-only on 2026-11-08
//! (`crates/bridges/binance/tests/binance_smokes/binance_funding_capture.rs`). The venue below serves them as the
//! capture measured: a window `[startTime, endTime]`, oldest first, `limit` rows.
//!
//! What it pins: each payment is ONE event in its own series (`BTCUSDT.P`), in USDT, once across polls, never one
//! stamped before the floor; the stream's funding frame, which has no id, no replay and (cross margin) no symbol, is
//! no longer folded, so a payment during a reconnect is booked once, by the poll; a cross-margin payment of a symbol
//! the engine does not own is not booked to it; and the engine's books agree with the wallet that holds the payments.

use std::cell::{Cell, RefCell};

use serde_json::{Value, json};
use vike_binance::execution::event_mapper::perp::map_binance_perp_stream;
use vike_binance::execution::funding::{BinanceFundingPoller, LOOKBACK_MS, decode_binance_funding};
use vike_binance::family::execution::event_mapper::perp::map_perp_stream;
use vike_execution::recon::{
    ReconcileBalanceCheck, ReconcileBalanceTolerance, ReconcileDivergence, diff_balance,
};
use vike_execution::testing::RecordingClient;
use vike_execution::{
    Account, BalanceMode, Engine, EventBusOutbox, EventHandler, PriceBoardConfig, RiskGate,
};
use vike_model::RiskLimits;
use vike_model::events::{AccountState, Event, FundingEvent};

const FIXTURE: &str = include_str!("../fixtures/funding_income_demo.json");
const HOUR: i64 = 3_600_000;
/// The recorded payments' first and last stamps.
const FIRST: i64 = 1_792_857_600_000;
const LAST: i64 = 1_793_289_600_000;

fn recorded(key: &str) -> Vec<Value> {
    let all: Value = serde_json::from_str(FIXTURE).expect("the income fixture parses");
    all["demo_2026_11_08"][key].as_array().cloned().expect("a recorded page")
}

/// The recorded funding rows, and a SYNTHETIC cross-margin payment of ETHUSDT at the last row's instant (the
/// recorded row with its symbol and id changed): the account held no ETHUSDT position across a funding, but its
/// latest week lists ETHUSDT trades, the symbol a position opened by hand would carry.
fn with_a_foreign_payment() -> Vec<Value> {
    let mut rows = recorded("funding_fee");
    let mut eth = rows.last().cloned().expect("a row");
    eth["symbol"] = json!("ETHUSDT");
    eth["income"] = json!("-0.01250000");
    eth["tranId"] = json!(1_682_176_836_353_885_591_i64);
    rows.push(eth);
    rows
}

/// The venue as the capture measured it: rows whose `time` is inside `[startTime, endTime]`, oldest first, `limit`
/// of them.
fn venue(rows: &[Value], params: &[(&str, String)]) -> Value {
    let get = |k: &str| params.iter().find(|(key, _)| *key == k).map(|(_, v)| v.clone());
    let ms = |k: &str| get(k).and_then(|v| v.parse::<i64>().ok());
    assert_eq!(get("incomeType").as_deref(), Some("FUNDING_FEE"));
    let (from, to) = (ms("startTime").unwrap_or(i64::MIN), ms("endTime").unwrap_or(i64::MAX));
    let limit = ms("limit").unwrap_or(100) as usize;
    let mut hits: Vec<Value> = rows
        .iter()
        .filter(|r| (from..=to).contains(&r["time"].as_i64().unwrap()))
        .cloned()
        .collect();
    hits.sort_by_key(|r| r["time"].as_i64().unwrap());
    json!(hits.into_iter().take(limit).collect::<Vec<_>>())
}

fn funding(event: &Event) -> &FundingEvent {
    match event {
        Event::Funding(f) => f,
        other => panic!("not a funding event: {other:?}"),
    }
}

/// The stream's frame of one recorded payment in cross margin: the wallet row's `bc` is the payment, and it names no
/// symbol (the venue's doc: cross margin pushes the balance alone).
fn stream_frame(row: &Value) -> Value {
    json!({ "e": "ACCOUNT_UPDATE", "T": row["time"], "a": { "m": "FUNDING_FEE", "B": [
        { "a": "USDT", "wb": "4986.90979507", "cw": "4986.90979507", "bc": row["income"] }
    ]}})
}

/// The recorded page is one event per payment, in the row's own series and asset, received positive as stated; the
/// commission and realized-PnL rows of the latest week (which share one `tranId` across kinds) are no funding.
#[test]
fn the_recorded_page_is_one_event_per_payment_in_its_own_series() {
    let rows = recorded("funding_fee");
    assert_eq!(rows.len(), 16);
    let events = decode_binance_funding(&rows);
    assert_eq!(events.len(), 16);
    for (e, r) in events.iter().zip(&rows) {
        assert_eq!((e.venue.as_str(), e.symbol.as_str()), ("binance", "BTCUSDT.P"));
        assert_eq!(e.currency.as_deref(), Some("USDT"));
        assert_eq!(e.timestamp, r["time"].as_i64().unwrap());
        assert_eq!(e.amount, r["income"].as_str().unwrap().parse::<f64>().unwrap());
    }
    let paid: f64 = events.iter().map(|e| e.amount).sum();
    assert!((paid + 0.107_349_05).abs() < 1e-12, "{paid}");
    assert!(decode_binance_funding(&recorded("every_kind_latest_week")).is_empty());
}

/// **The stream leaves a funding frame to the poll.** Binance's stream folds nothing of it (no event, no wallet
/// figure); aster's, the family's, still books it.
///
/// Base: binance's stream folded a `FundingEvent` on the MOUNTED symbol and the frame's wallet figure.
#[test]
fn the_stream_leaves_a_funding_frame_to_the_poll() {
    let frame = stream_frame(&recorded("funding_fee")[0]);
    assert_eq!(map_binance_perp_stream(&frame, "binance", "BTCUSDT.P", false, None), Vec::new());
    let aster = map_perp_stream(&frame, "aster", "BTCUSDT.P", None);
    assert!(
        aster.iter().any(|e| matches!(e, Event::Funding(_))),
        "aster keeps the stream: {aster:?}"
    );
}

/// **A payment during a reconnect is booked once.** The socket is down across the recorded payments (no frame
/// arrives); after it, the stream redelivers nothing and the poll reads the window: every payment once, oldest first,
/// and again nothing on the next poll. Had a frame arrived, binance's stream folds none of it, so nothing is twice.
#[test]
fn a_payment_during_a_reconnect_is_booked_once_by_the_poll() {
    let rows = recorded("funding_fee");
    let mut poller =
        BinanceFundingPoller::new(|p: &[(&str, String)]| Ok(venue(&rows, p)), FIRST - HOUR);
    let first = poller.poll(LAST + HOUR).expect("served");
    let got: Vec<i64> = first.iter().map(|e| funding(e).timestamp).collect();
    let want: Vec<i64> = rows.iter().map(|r| r["time"].as_i64().unwrap()).collect();
    assert_eq!(got, want, "every payment, oldest first");
    assert_eq!(poller.poll(LAST + 2 * HOUR), Ok(Vec::new()), "and once");
    for row in &rows {
        assert!(
            map_binance_perp_stream(&stream_frame(row), "binance", "BTCUSDT.P", false, None)
                .is_empty()
        );
    }
}

/// The window: from the floor to now with both ends sent, then `LOOKBACK_MS` before the last good window's end; a
/// payment before the floor is in the wallet already and never emitted; a failed read moves nothing; a full page is
/// read on from its newest row.
#[test]
fn the_window_moves_only_on_a_good_read_and_a_full_page_reads_on() {
    let rows = recorded("funding_fee");
    let asked = RefCell::new(Vec::new());
    let down = Cell::new(true);
    let floor = rows[4]["time"].as_i64().unwrap();
    let mut poller = BinanceFundingPoller::new(
        |p: &[(&str, String)]| {
            asked
                .borrow_mut()
                .push(p.iter().map(|(k, v)| (k.to_string(), v.clone())).collect::<Vec<_>>());
            if down.get() { Err("-1001 disconnected".to_string()) } else { Ok(venue(&rows, p)) }
        },
        floor,
    );
    assert!(poller.poll(LAST + HOUR).is_err());
    down.set(false);
    let events = poller.poll(LAST + HOUR).expect("served");
    assert_eq!(events.len(), 12, "the payments from the floor on (inclusive), none before it");
    assert_eq!(asked.borrow()[0], asked.borrow()[1], "the failed read did not move the window");
    let window = |i: usize| {
        let a = &asked.borrow()[i];
        let v =
            |k: &str| a.iter().find(|(key, _)| key == k).map(|(_, v)| v.parse::<i64>().unwrap());
        (v("startTime"), v("endTime"))
    };
    assert_eq!(window(1), (Some(floor), Some(LAST + HOUR)));
    poller.poll(LAST + 2 * HOUR).expect("served");
    assert_eq!(window(2), (Some(LAST + HOUR - LOOKBACK_MS), Some(LAST + 2 * HOUR)));

    // A full page (the venue's 1000 rows) is read on from its newest row.
    let many: Vec<Value> = (0..1500_i64)
        .map(|i| {
            let mut r = rows[0].clone();
            r["time"] = json!(FIRST + i * 1000);
            r["tranId"] = json!(1_000_000 + i);
            r
        })
        .collect();
    let starts = RefCell::new(Vec::new());
    let mut full = BinanceFundingPoller::new(
        |p: &[(&str, String)]| {
            starts.borrow_mut().push(p.iter().find(|(k, _)| *k == "startTime").unwrap().1.clone());
            Ok(venue(&many, p))
        },
        FIRST,
    );
    assert_eq!(
        full.poll(FIRST + 2 * HOUR).expect("served").len(),
        1500,
        "both pages, each row once"
    );
    assert_eq!(*starts.borrow(), vec![FIRST.to_string(), (FIRST + 999_000).to_string()]);
}

fn engine(symbol: &str) -> Engine<RecordingClient> {
    Engine::new(
        Account::new(1.0, "binance", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "binance",
        symbol,
    )
}

fn adopt(e: &mut Engine<RecordingClient>, cash: f64) {
    let label = e.quote_asset.clone();
    e.account.apply_account_state(
        &AccountState {
            venue: "binance".into(),
            balances: vec![(label.clone(), cash)],
            timestamp: 0,
            route_key: None,
        },
        &label,
    );
}

/// **A cross-margin payment of a symbol the engine does not own is not booked to it.** The poll names each row's own
/// series, so the engine on `BTCUSDT.P` books the recorded BTCUSDT payments and not the ETHUSDT one, which an engine on
/// `ETHUSDT.P` books. The stream's frame of that payment names no symbol: the family's mapper (aster's) labels it with
/// the mounted series, and the BTCUSDT engine would book it.
#[test]
fn a_cross_margin_payment_of_a_foreign_symbol_is_not_booked_to_the_engine() {
    let rows = with_a_foreign_payment();
    let mut poller =
        BinanceFundingPoller::new(|p: &[(&str, String)]| Ok(venue(&rows, p)), FIRST - HOUR);
    let events = poller.poll(LAST + HOUR).expect("served");
    assert_eq!(events.len(), 17);
    let (mut btc, mut eth, mut ob) =
        (engine("BTCUSDT.P"), engine("ETHUSDT.P"), EventBusOutbox::default());
    for event in &events {
        btc.on_event(event, &mut ob);
        eth.on_event(event, &mut ob);
    }
    assert!(
        (btc.account.funding_paid + 0.107_349_05).abs() < 1e-12,
        "{}",
        btc.account.funding_paid
    );
    assert!((eth.account.funding_paid + 0.0125).abs() < 1e-12, "{}", eth.account.funding_paid);

    let foreign = stream_frame(rows.last().unwrap());
    let mut blind = engine("BTCUSDT.P");
    for event in map_perp_stream(&foreign, "binance", "BTCUSDT.P", None) {
        blind.on_event(&event, &mut ob);
    }
    assert!(
        (blind.account.funding_paid + 0.0125).abs() < 1e-12,
        "the stream's frame booked it on the mount"
    );
}

/// **ONCE, against the wallet.** The engine adopted the wallet before the payments; folding the poll's events moves the
/// funding total, own PnL (the drawdown latch's PnL term) and equity by them once, a second poll adds nothing, a
/// reconcile pass finds the books equal to the wallet that holds them (an engine without them drifts by exactly them),
/// and adopting that wallet moves neither equity nor own PnL.
#[test]
fn the_engine_books_the_recorded_payments_once_and_the_wallet_agrees() {
    let rows = recorded("funding_fee");
    let paid = -0.107_349_05;
    let (before, wallet) = (4_986.909_795_07, 4_986.909_795_07 + paid);
    let config = PriceBoardConfig::default();
    let mut e = engine("BTCUSDT.P");
    adopt(&mut e, before);
    let (equity0, own0) = (e.resolved_equity(0.0, &config), e.resolved_own_pnl(&config));
    let mut poller =
        BinanceFundingPoller::new(|p: &[(&str, String)]| Ok(venue(&rows, p)), FIRST - HOUR);
    let mut ob = EventBusOutbox::default();
    for now in [LAST + HOUR, LAST + 2 * HOUR] {
        for event in poller.poll(now).expect("served") {
            e.on_event(&event, &mut ob);
        }
    }
    assert!(
        (e.account.funding_paid - paid).abs() < 1e-9,
        "booked once: {}",
        e.account.funding_paid
    );
    assert!((e.resolved_own_pnl(&config) - (own0 + paid)).abs() < 1e-9, "own PnL moved once");
    let equity1 = e.resolved_equity(0.0, &config);
    assert!((equity1 - (equity0 + paid)).abs() < 1e-9, "equity moved once");
    let own1 = e.resolved_own_pnl(&config);

    let tolerance = ReconcileBalanceTolerance { abs_floor: 1e-9, rel_frac: 0.0 };
    match diff_balance(&e.local_view().as_view(), Some(wallet), &e.quote_asset, tolerance, 0) {
        ReconcileBalanceCheck::Anchored { drift: None, .. } => {}
        other => panic!("the books must agree with the wallet: {other:?}"),
    }
    let mut blind = engine("BTCUSDT.P");
    adopt(&mut blind, before);
    match diff_balance(
        &blind.local_view().as_view(),
        Some(wallet),
        &blind.quote_asset,
        tolerance,
        0,
    ) {
        ReconcileBalanceCheck::Anchored {
            drift: Some(ReconcileDivergence::BalanceDrift { local, venue_balance, .. }),
            ..
        } => {
            assert!((venue_balance - local - paid).abs() < 1e-9, "the wallet is ahead by them")
        }
        other => panic!("an engine that never booked the funding holds a drift: {other:?}"),
    }
    adopt(&mut e, wallet);
    assert!((e.resolved_equity(0.0, &config) - equity1).abs() < 1e-9, "the wallet holds them once");
    assert_eq!(
        e.resolved_own_pnl(&config).to_bits(),
        own1.to_bits(),
        "an adoption writes no own PnL"
    );
}
