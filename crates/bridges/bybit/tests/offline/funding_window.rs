//! **The funding poll's WINDOW against the venue's documented reading of it**, offline. Bybit's transaction log
//! reads a lone `startTime` as `[startTime, startTime + 24 h]`, both ends at most 7 days apart, and serves 20 rows a
//! page unless asked for up to 50 (`/v5/account/transaction-log`'s doc). The poller sent its spawn floor alone, so a
//! node up for more than a day kept asking for its first day: every settlement after it was never booked. The venue
//! below plays exactly that doc, on the wall clock (the poll's `poll()` reads it).
//!
//! The rows are SYNTHETIC, in the shape of the demo account's real rows (its spot trades of 2026-11-08: a string `id`,
//! `transactionTime` and `funding`): that account held no linear position and listed no linear row in the week
//! before (`crates/bridges/bybit/tests/bybit_smokes/bybit_funding_capture.rs`), as the inverse mount's capture found
//! for its book. Only public seams, so every test here also runs on the code before the fix; where a test says
//! "base", its assertion FAILED there.

use std::sync::Mutex;

use serde_json::{Value, json};
use vike_bridge_core::signer::SignerJson;
use vike_bridge_core::transport::VenueApiError;
use vike_bridge_core::transport::json::RestTransportJson;
use vike_bybit::execution::funding::{BybitFundingPoller, LOG_PATH};
use vike_bybit::execution::rest::VenueRestBybit;
use vike_bybit::history::Category;
use vike_bybit::signer::BybitV5Signer;
use vike_execution::recon::{
    ReconcileBalanceCheck, ReconcileBalanceTolerance, ReconcileDivergence, diff_balance,
};
use vike_execution::testing::RecordingClient;
use vike_execution::{
    Account, BalanceMode, Engine, EventBusOutbox, EventHandler, PriceBoardConfig, RiskGate,
};
use vike_model::events::{AccountState, Event};
use vike_model::{RiskLimits, SymbolProperties, now_ms};
use vike_secrets::credentials::Credentials;

const HOUR: i64 = 3_600_000;
const DAY: i64 = 24 * HOUR;
/// The symbols whose settlements the account-wide log lists; the mount is the first.
const SYMBOLS: [&str; 8] =
    ["BTCUSDT", "ETHUSDT", "SOLUSDT", "XRPUSDT", "DOGEUSDT", "ADAUSDT", "LINKUSDT", "AVAXUSDT"];

/// The documented log: SETTLEMENT rows, newest first, each window read as the doc reads it.
struct Log {
    rows: Vec<Value>,
    /// `(startTime, endTime)` of every request, as sent.
    asked: Mutex<Vec<(Option<i64>, Option<i64>)>>,
}

impl RestTransportJson for Log {
    fn signed(
        &self,
        _base: &str,
        path: &str,
        _method: &str,
        params: &[(&str, Value)],
        _signer: &dyn SignerJson,
    ) -> Result<Value, VenueApiError> {
        assert_eq!(path, LOG_PATH);
        let get = |k: &str| params.iter().find(|(key, _)| *key == k).map(|(_, v)| v.clone());
        let ms = |k: &str| get(k).and_then(|v| v.as_str().and_then(|s| s.parse::<i64>().ok()));
        let (start, end) = (ms("startTime"), ms("endTime"));
        self.asked.lock().unwrap().push((start, end));
        let (from, to) = match (start, end) {
            (Some(s), Some(e)) if e - s > 7 * DAY => {
                return Ok(json!({"retCode": 10001, "retMsg": "endTime - startTime > 7 days"}));
            }
            (Some(s), Some(e)) => (s, e),
            (Some(s), None) => (s, s + DAY), // "Only startTime is passed": 24 hours from it
            (None, Some(e)) => (e - DAY, e),
            (None, None) => (now_ms() - DAY, now_ms()),
        };
        let limit = get("limit").and_then(|v| v.as_u64()).unwrap_or(20).min(50) as usize;
        let offset: usize = get("cursor").and_then(|c| c.as_str()?.parse().ok()).unwrap_or(0);
        let ts = |r: &Value| r["transactionTime"].as_str().unwrap().parse::<i64>().unwrap();
        let mut hits: Vec<&Value> =
            self.rows.iter().filter(|r| (from..=to).contains(&ts(r))).collect();
        hits.sort_by_key(|r| std::cmp::Reverse(ts(r)));
        let page: Vec<&Value> = hits.iter().skip(offset).take(limit).copied().collect();
        let next =
            if offset + limit < hits.len() { (offset + limit).to_string() } else { String::new() };
        Ok(json!({"retCode": 0, "retMsg": "OK", "result": {"list": page, "nextPageCursor": next}}))
    }
}

/// One SETTLEMENT row of `symbol` at `ts`, the documented shape.
fn settlement(n: usize, symbol: &str, ts: i64, funding: f64) -> Value {
    json!({
        "id": format!("{n}_{symbol}_{ts}"), "symbol": symbol, "category": "linear", "side": "Buy",
        "transactionTime": ts.to_string(), "type": "SETTLEMENT", "currency": "USDT",
        "funding": format!("{funding:.8}"), "feeRate": "0.0001", "change": format!("{funding:.8}")
    })
}

/// Every symbol's settlement every 8 hours from `floor + 1 h` to `until`, the mount's paying `-0.25 - 0.01·k`.
fn log_from(floor: i64, until: i64) -> Log {
    let mut rows = Vec::new();
    let mut ts = floor + HOUR;
    let mut k = 0;
    while ts <= until {
        for (i, symbol) in SYMBOLS.iter().enumerate() {
            let funding = if i == 0 { -0.25 - 0.01 * k as f64 } else { 0.1 };
            rows.push(settlement(rows.len(), symbol, ts, funding));
        }
        ts += 8 * HOUR;
        k += 1;
    }
    Log { rows, asked: Mutex::default() }
}

fn rest(log: Log) -> VenueRestBybit<Log> {
    VenueRestBybit {
        category: Category::Linear,
        signer: BybitV5Signer::new(&Credentials::for_test("k", "s", None), || 0),
        transport: log,
        base_url: "https://unused.invalid".to_string(),
        mainnet: false,
        symbol: "BTCUSDT".to_string(),
        properties: SymbolProperties::default(),
        leverage: 2.0,
    }
}

fn mount_rows(log: &Log) -> Vec<(i64, f64)> {
    let mut out: Vec<(i64, f64)> = log
        .rows
        .iter()
        .filter(|r| r["symbol"] == "BTCUSDT")
        .map(|r| {
            (
                r["transactionTime"].as_str().unwrap().parse().unwrap(),
                r["funding"].as_str().unwrap().parse().unwrap(),
            )
        })
        .collect();
    out.sort_unstable_by_key(|(ts, _)| *ts);
    out
}

fn funding_of(events: &[Event], symbol: &str) -> Vec<(i64, f64)> {
    let mut out: Vec<(i64, f64)> = events
        .iter()
        .filter_map(|e| match e {
            Event::Funding(f) if f.symbol == symbol => Some((f.timestamp, f.amount)),
            Event::Funding(_) => None,
            other => panic!("not a funding event: {other:?}"),
        })
        .collect();
    out.sort_unstable_by_key(|(ts, _)| *ts);
    out
}

/// **A node up three days books every settlement since its start, once.** 72 rows (eight symbols, nine settlements)
/// over two pages of 50; the mount's nine reach the core, the next poll adds nothing, and the window asked runs from
/// the floor to now with both ends sent.
///
/// Base: the poll sent `startTime` = the floor alone, the venue answered the node's first day only, 20 rows of it: the
/// settlements of the last two days were never booked.
#[test]
fn a_node_up_three_days_books_every_settlement_since_its_start_once() {
    let now = now_ms();
    let floor = now - 3 * DAY;
    let rest = rest(log_from(floor, now - HOUR));
    let want = mount_rows(&rest.transport);
    assert_eq!(want.len(), 9);
    let mut poller = BybitFundingPoller::new(&rest, "BTCUSDT", floor);
    let first = poller.poll().expect("the log answered");
    assert_eq!(
        first.len(),
        72,
        "every symbol's settlement since the floor (the engine filters by symbol)"
    );
    assert_eq!(funding_of(&first, "BTCUSDT"), want, "the mount's nine settlements");
    assert!(poller.poll().expect("the log answered").is_empty(), "nothing twice");
    let asked = rest.transport.asked.lock().unwrap().clone();
    assert_eq!(asked[0].0, Some(floor), "the first window starts at the floor");
    assert!(asked[0].1.is_some_and(|e| e >= now), "and ends now: {asked:?}");
}

/// **An outage longer than the venue's 7-day window is read back a week at a time**: the first poll of a node whose
/// floor is ten days back asks seven days (the venue refuses more), the next one the rest; together they book each
/// settlement once.
///
/// Base: each poll asked the floor's first day.
#[test]
fn a_window_wider_than_seven_days_is_read_a_week_at_a_time() {
    let now = now_ms();
    let floor = now - 10 * DAY;
    let rest = rest(log_from(floor, now - HOUR));
    let want = mount_rows(&rest.transport);
    let mut poller = BybitFundingPoller::new(&rest, "BTCUSDT", floor);
    let mut got = poller.poll().expect("the first week");
    got.extend(poller.poll().expect("the rest"));
    assert!(poller.poll().expect("nothing left").is_empty());
    assert_eq!(funding_of(&got, "BTCUSDT"), want, "each of the {} settlements once", want.len());
    let asked = rest.transport.asked.lock().unwrap().clone();
    let (s, e) = (asked[0].0.unwrap(), asked[0].1.unwrap());
    assert_eq!((s, e - s), (floor, 7 * DAY), "the first window is the venue's widest");
}

/// **The engine books the mount's settlements once and the wallet does not count them again.** A linear engine on
/// `BTCUSDT` folds the three days: its funding total, own PnL (the drawdown latch's PnL term) and equity move by the
/// mount's settlements once (the other symbols' rows are not its symbol); the next poll books nothing. A reconcile pass
/// against the wallet that holds them finds no drift (an engine that never heard of them holds a drift of exactly
/// them), and adopting the wallet moves neither equity nor own PnL.
#[test]
fn the_engine_books_the_settlements_once_and_the_wallet_agrees() {
    let now = now_ms();
    let floor = now - 3 * DAY;
    let rest = rest(log_from(floor, now - HOUR));
    let paid: f64 = mount_rows(&rest.transport).iter().map(|(_, f)| f).sum();
    let (before, wallet) = (10_000.0, 10_000.0 + paid);
    let engine = || {
        Engine::new(
            Account::new(1.0, "bybit", None, BalanceMode::Delta),
            RiskGate::new(RiskLimits::new()),
            RecordingClient::default(),
            "bybit",
            "BTCUSDT",
        )
    };
    let adopt = |e: &mut Engine<RecordingClient>, cash: f64| {
        let label = e.quote_asset.clone();
        e.account.apply_account_state(
            &AccountState {
                venue: "bybit".into(),
                balances: vec![(label.clone(), cash)],
                timestamp: 0,
                route_key: None,
            },
            &label,
        );
    };
    let config = PriceBoardConfig::default();
    let mut e = engine();
    adopt(&mut e, before);
    let (equity0, own0) = (e.resolved_equity(0.0, &config), e.resolved_own_pnl(&config));
    let mut poller = BybitFundingPoller::new(&rest, "BTCUSDT", floor);
    let mut ob = EventBusOutbox::default();
    for _ in 0..2 {
        for event in poller.poll().expect("the log answered") {
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
    let mut blind = engine();
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
