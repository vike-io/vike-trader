//! The perpetual FUNDING producer (`vike_deribit::execution::funding`) over Deribit's own words for it, and what an
//! inverse engine does with its events. No venue, no credentials, no `#[ignore]`.
//!
//! The words are `crates/bridges/deribit/tests/fixtures/deribit_funding_log.json`: the BTC transaction log of the
//! TESTNET demo account RECORDED on 2026-11-08 (`crates/bridges/deribit/tests/deribit_smokes/deribit_funding_capture.rs`;
//! every `BTC-PERPETUAL` row reads `interest_pl: 0.0`, the position `realized_funding: 0.0`), and the venue's
//! DOCUMENTED example page of `private/get_transaction_log`, verbatim, whose `settlement` and `LSP transfer` rows of
//! `BTC-PERPETUAL` realize funding (a non-zero row needs a position held across funding, which a read cannot make).
//!
//! What it pins: each payment becomes ONE `FundingEvent` in the settle coin, once across polls, never one stamped
//! before the poller's floor; and an inverse engine books it ONCE in its coin ledger (decision 0258), in the funding
//! total and own PnL (the drawdown latch's PnL term), while the wallet a reconcile pass adopts, which already holds it,
//! neither counts it again nor drifts from the books.

use std::cell::{Cell, RefCell};

use indexmap::IndexMap;
use serde_json::{Value, json};

use vike_bridge_core::exec_actor::funding_poll::FUNDING_LOOKBACK_MS;
use vike_deribit::execution::funding::{
    DeribitFundingPoller, decode_deribit_funding, funding_currency,
};
use vike_deribit::reconcile::{parse_account_summary_coin_cash, reports_session_cash};
use vike_execution::recon::{
    ReconcileBalanceCheck, ReconcileBalanceTolerance, ReconcileDivergence, diff_balance,
};
use vike_execution::testing::RecordingClient;
use vike_execution::{
    Account, BalanceMode, Engine, EventBusOutbox, EventHandler, MarkSource, PriceBoardConfig,
    RiskGate,
};
use vike_model::events::{AccountState, Event, FundingEvent};
use vike_model::{ContractValue, RiskLimits};

const FIXTURE: &str = include_str!("../fixtures/deribit_funding_log.json");
const INSTRUMENTS: &str = include_str!("../fixtures/deribit_classes_testnet_instruments.json");
const SYMBOL: &str = "BTC-PERPETUAL";
/// The documented page's two funding rows, newest first as the venue lists them: the `LSP transfer` (id 61313)
/// and the `settlement` (id 61311).
const LSP: (i64, i64, f64) = (61313, 1_613_659_900_000, 0.000_005_12);
const SETTLEMENT: (i64, i64, f64) = (61311, 1_613_659_544_153, 0.000_009_93);
/// A poll's clock just after the documented page's newest row.
const NOW: i64 = 1_613_660_000_000;

fn fixture(pointer: &str) -> Value {
    let all: Value = serde_json::from_str(FIXTURE).expect("the funding fixture parses");
    all.pointer(pointer).cloned().unwrap_or_else(|| panic!("no {pointer} in the fixture"))
}

fn logs(page: &Value) -> Vec<Value> {
    page["logs"].as_array().cloned().expect("a transaction-log page has `logs`")
}

fn funding(event: &Event) -> &FundingEvent {
    match event {
        Event::Funding(f) => f,
        other => panic!("not a funding event: {other:?}"),
    }
}

/// The documented page served in TWO pages, as the venue pages an account's log: the three newest rows
/// with `continuation` naming the oldest of them, then the rest with none.
fn documented_pages(params: &Value) -> Result<Value, String> {
    let rows = logs(&fixture("/documented"));
    Ok(match params.get("continuation") {
        None => json!({ "logs": rows[..3], "continuation": 61311 }),
        Some(_) => json!({ "logs": rows[3..], "continuation": null }),
    })
}

/// The real wire: every recorded `BTC-PERPETUAL` row carries the fields the decoder reads (an integer `id`, a
/// `timestamp`, the `currency`, a numeric `interest_pl`), and the account paid no funding in those 30 days (the
/// position's `realized_funding` agrees), so neither the decoder nor the poller says one word.
#[test]
fn the_recorded_testnet_log_states_no_funding_and_the_poller_emits_none() {
    let page = fixture("/testnet_2026_11_08/transaction_log");
    let rows = logs(&page);
    assert_eq!(rows.len(), 55, "the recorded page");
    let perp: Vec<&Value> =
        rows.iter().filter(|r| r["instrument_name"].as_str() == Some(SYMBOL)).collect();
    assert!(perp.len() >= 10, "the recorded page holds the perpetual's trades and settlements");
    for r in &perp {
        assert!(r["id"].is_i64() && r["timestamp"].is_i64(), "{r}");
        assert_eq!(r["currency"], "BTC");
        assert_eq!(r["interest_pl"].as_f64(), Some(0.0), "{r}");
    }
    assert_eq!(fixture("/testnet_2026_11_08/position/realized_funding").as_f64(), Some(0.0));
    assert!(decode_deribit_funding(&rows, SYMBOL).is_empty());

    let mut poller = DeribitFundingPoller::new(|_: &Value| Ok(page.clone()), SYMBOL, "BTC", 0);
    assert_eq!(poller.poll(1_793_440_000_000), Ok(Vec::new()));
}

/// The documented page is two funding payments of `BTC-PERPETUAL`, in BTC as stated (received positive, no flip):
/// the transfer, deposit and expiry rows (`interest_pl: null`) and the option trades (`0`) are no funding, and a
/// mount on another instrument reads none of it. The event names the mounted spelling.
#[test]
fn the_documented_page_is_two_funding_payments_in_the_settle_coin() {
    let rows = logs(&fixture("/documented"));
    assert_eq!(rows.len(), 7);
    let events = decode_deribit_funding(&rows, SYMBOL);
    let got: Vec<(i64, f64)> = events.iter().map(|e| (e.timestamp, e.amount)).collect();
    assert_eq!(got, vec![(LSP.1, LSP.2), (SETTLEMENT.1, SETTLEMENT.2)], "page order");
    for e in &events {
        assert_eq!((e.venue.as_str(), e.symbol.as_str()), ("deribit", SYMBOL));
        assert_eq!(e.currency.as_deref(), Some("BTC"), "the settle coin, stated");
        assert_eq!((e.funding_rate, e.mark_price, e.route_key), (0.0, None, None));
    }
    assert!(decode_deribit_funding(&rows, "ETH-PERPETUAL").is_empty());
    let lower = decode_deribit_funding(&rows, "btc-perpetual");
    assert_eq!(lower.len(), 2);
    assert_eq!(lower[0].symbol.as_str(), "btc-perpetual", "the engine's spelling");
}

/// Every PERPETUAL mount is polled, inverse or linear, in its settle coin; a dated future, an option and a spot pair pay
/// no funding. Rows captured from the testnet.
///
/// Base (the inverse producer alone): `BTC_USDC-PERPETUAL` answered `None`, so a linear perpetual booked no funding.
#[test]
fn every_perpetual_mount_reads_funding_in_its_settle_coin() {
    let all: Value = serde_json::from_str(INSTRUMENTS).expect("the captured rows parse");
    let of = |name: &str| funding_currency(&all[name]);
    assert_eq!(of("BTC-PERPETUAL").as_deref(), Some("BTC"));
    assert_eq!(of("BTC_USDC-PERPETUAL").as_deref(), Some("USDC"));
    for name in ["BTC-3NOV26", "BTC_USDC-3NOV26", "BTC-6NOV26-77000-C", "BTC_USDC"] {
        assert_eq!(of(name), None, "{name}");
    }
}

/// **A futures mount, inverse or linear, reports the cash with the session's realized PnL** (`balance +
/// session_rpl`); an option or spot mount keeps `balance`, which their trades move at once. The recorded USDC summary
/// carries both session fields, as the BTC one does.
///
/// Base: only an inverse row reported it, so a linear perpetual or future adopted `balance` alone.
#[test]
fn a_futures_mount_inverse_or_linear_reports_the_session_cash() {
    let all: Value = serde_json::from_str(INSTRUMENTS).expect("the captured rows parse");
    for name in ["BTC-PERPETUAL", "BTC-3NOV26", "BTC_USDC-PERPETUAL", "BTC_USDC-3NOV26"] {
        assert!(reports_session_cash(&all[name]), "{name}");
    }
    for name in ["BTC_USDC-6NOV26-77000-C", "BTC_USDC"] {
        assert!(!reports_session_cash(&all[name]), "{name}");
    }
    let summary = usdc_fixture()["account_summary"].clone();
    assert!(summary["session_rpl"].is_number() && summary["session_upl"].is_number(), "{summary}");
    let cash = parse_account_summary_coin_cash(&summary.to_string()).expect("parses");
    assert_eq!(cash, Some(99_996.039_917_92), "flat after the settlement: the balance");
}

const USDC_FIXTURE: &str = include_str!("../fixtures/deribit_funding_log_usdc.json");
const LINEAR: &str = "BTC_USDC-PERPETUAL";

/// The USDC transaction log and account summary of the testnet demo account, RECORDED on 2026-11-08
/// (`crates/bridges/deribit/tests/deribit_smokes/deribit_funding_capture.rs`'s `deribit_funding_capture_usdc`), the log
/// newest first as the venue lists it.
fn usdc_fixture() -> Value {
    let all: Value = serde_json::from_str(USDC_FIXTURE).expect("the USDC fixture parses");
    all["testnet_2026_11_08_usdc"].clone()
}

fn usdc_rows() -> Vec<Value> {
    logs(&usdc_fixture()["transaction_log"])
}

fn get_ts(row: &Value) -> i64 {
    row["timestamp"].as_i64().expect("a stamped row")
}

fn number(row: &Value, k: &str) -> f64 {
    row[k].as_f64().unwrap_or(0.0)
}

/// **The recorded USDC cash, `balance + session_rpl`, does not jump at the 08:00 settlement; `balance` alone does.**
/// The futures closes of the session (the instruments that have a settlement row) moved `balance` by their fees alone:
/// their realized cashflows and the perpetual's funding (`interest_pl` -1e-08, on a close) are what the venue's doc
/// says `session_rpl` accrues until the settlement. The settlement rows moved `balance` by exactly that sum, and the
/// summary recorded after them reads that balance with `session_rpl` 0.0: so the cash the recon client now reports is
/// the same before 08:00 and after, while `balance` jumped by -0.00743001.
#[test]
fn the_recorded_usdc_cash_does_not_jump_at_the_settlement() {
    let rows = usdc_rows(); // newest first
    assert_eq!(rows.len(), 48, "the recorded page");
    let settled_at = rows.iter().find(|r| r["type"] == "settlement").map(get_ts).expect("08:00");
    let futures: Vec<&str> = rows
        .iter()
        .filter(|r| r["type"] == "settlement")
        .filter_map(|r| r["instrument_name"].as_str())
        .collect();
    assert_eq!(futures, ["GOLD_USDC-PERPETUAL", LINEAR, "BTC_USDC-3NOV26"]);
    let session: Vec<&Value> = rows
        .iter()
        .filter(|r| r["type"] == "trade" && get_ts(r) < settled_at)
        .filter(|r| r["instrument_name"].as_str().is_some_and(|n| futures.contains(&n)))
        .collect();
    for r in &session {
        assert_eq!(
            number(r, "change"),
            -number(r, "commission"),
            "a futures trade moves balance by its fee: {r}"
        );
    }
    let session_rpl: f64 =
        session.iter().map(|r| number(r, "cashflow") + number(r, "interest_pl")).sum();
    let settled: f64 =
        rows.iter().filter(|r| r["type"] == "settlement").map(|r| number(r, "change")).sum();
    assert!(close(session_rpl, settled, 1e-12), "{session_rpl} vs {settled}");
    assert!(close(settled, -0.007_430_01, 1e-12), "{settled}");

    let before =
        rows.iter().find(|r| get_ts(r) < settled_at).map(|r| number(r, "balance")).expect("a row");
    let summary = usdc_fixture()["account_summary"].clone();
    let after =
        parse_account_summary_coin_cash(&summary.to_string()).expect("parses").expect("cash");
    assert_eq!(
        number(&rows[0], "balance"),
        number(&summary, "balance"),
        "the summary follows the log"
    );
    assert!(
        close(before + session_rpl, after, 1e-9),
        "the cash: {} then {after}",
        before + session_rpl
    );
    assert!(
        close(after - before, settled, 1e-9),
        "balance alone jumped by the session: {}",
        after - before
    );
}

/// **A linear perpetual's funding is booked at the row that realizes it, and the books stay on the cash all day.** The
/// poll emits the close's `interest_pl` (-1e-08, in USDC) once and nothing at the settlement row (its slice is 0.0).
/// The cash holds the slice from that row on (`session_rpl`), so a reconcile pass finds no drift then, and none after
/// 08:00, when the slice moves from `session_rpl` into `balance` and the cash does not move.
#[test]
fn a_linear_engine_books_funding_at_the_row_and_the_session_cash_agrees_all_day() {
    let rows = usdc_rows();
    let served = rows.clone();
    let mut poller =
        DeribitFundingPoller::new(|_: &Value| Ok(json!({ "logs": served })), LINEAR, "USDC", 0);
    let events = poller.poll(1_793_440_000_000).expect("served");
    let got: Vec<(i64, f64, Option<&str>)> = events
        .iter()
        .map(funding)
        .map(|e| (e.timestamp, e.amount, e.currency.as_deref()))
        .collect();
    assert_eq!(got, vec![(1_793_412_443_055, -1e-08, Some("USDC"))], "the close's slice, once");
    assert_eq!(poller.poll(1_793_440_060_000), Ok(Vec::new()));

    let engine = || {
        Engine::new(
            Account::new(1.0, "deribit", None, BalanceMode::Delta),
            RiskGate::new(RiskLimits::new()),
            RecordingClient::default(),
            "deribit",
            LINEAR,
        )
    };
    let drift = |e: &Engine<RecordingClient>, cash: f64| {
        let tolerance = ReconcileBalanceTolerance { abs_floor: 1e-9, rel_frac: 0.0 };
        match diff_balance(&e.local_view().as_view(), Some(cash), &e.quote_asset, tolerance, 0) {
            ReconcileBalanceCheck::Anchored { drift: None, .. } => 0.0,
            ReconcileBalanceCheck::Anchored {
                drift: Some(ReconcileDivergence::BalanceDrift { local, venue_balance, .. }),
                ..
            } => venue_balance - local,
            other => panic!("an anchored check: {other:?}"),
        }
    };
    // The engine's share of the cash: what it was before the close, then with the slice in `session_rpl`.
    let cash0 = 99_996.0;
    let slice = -1e-08;
    let config = PriceBoardConfig::default();
    let mut e = engine();
    adopt(&mut e, cash0);
    let own0 = e.resolved_own_pnl(&config);
    let mut ob = EventBusOutbox::default();
    for event in &events {
        e.on_event(event, &mut ob);
    }
    assert_eq!(e.account.funding_paid, slice, "booked once, at the row");
    assert!(close(e.resolved_own_pnl(&config), own0 + slice, 1e-9), "in own PnL");
    // Mid-session the slice is in `session_rpl`; at 08:00 `balance` takes it and `session_rpl` resets, and the cash is
    // the same figure (`the_recorded_usdc_cash_does_not_jump_at_the_settlement`): one check holds for both.
    assert_eq!(drift(&e, cash0 + slice), 0.0, "the books are the cash, before 08:00 and after");
    let mut blind = engine();
    adopt(&mut blind, cash0);
    assert!(
        close(drift(&blind, cash0 + slice), slice, 1e-10),
        "without the event the cash is off by it"
    );
}

/// A floor just before the documented page's older row: both rows are the poller's to emit.
const FLOOR: i64 = SETTLEMENT.1 - 1;

/// The poll: one window from the floor to now, paged through `continuation`, its events oldest first; the same
/// rows on the next poll are no new events; the next window starts `FUNDING_LOOKBACK_MS` before the last good one
/// ended.
#[test]
fn a_poll_pages_the_window_and_emits_each_row_once_oldest_first() {
    let asked = RefCell::new(Vec::new());
    let mut poller = DeribitFundingPoller::new(
        |p: &Value| {
            asked.borrow_mut().push(p.clone());
            documented_pages(p)
        },
        SYMBOL,
        "BTC",
        FLOOR,
    );
    let first = poller.poll(NOW).expect("served");
    let got: Vec<(i64, f64)> = first.iter().map(funding).map(|f| (f.timestamp, f.amount)).collect();
    assert_eq!(got, vec![(SETTLEMENT.1, SETTLEMENT.2), (LSP.1, LSP.2)], "oldest first");
    assert_eq!(
        *asked.borrow(),
        vec![
            json!({ "currency": "BTC", "start_timestamp": FLOOR, "end_timestamp": NOW, "count": 1000 }),
            json!({ "currency": "BTC", "start_timestamp": FLOOR, "end_timestamp": NOW, "count": 1000, "continuation": 61311 }),
        ],
        "the second page is the older one the first named"
    );
    asked.borrow_mut().clear();
    // The venue lists the same rows again (it ignores the window here): nothing is new.
    assert_eq!(poller.poll(NOW + 60_000), Ok(Vec::new()));
    assert_eq!(asked.borrow()[0]["start_timestamp"], json!(NOW - FUNDING_LOOKBACK_MS));
}

/// The restart law: a row stamped before the poller's floor is already in the wallet the first pass adopts, so it
/// is never emitted, whatever the venue lists.
#[test]
fn a_row_stamped_before_the_floor_is_never_emitted() {
    let floor = SETTLEMENT.1 + 1;
    let mut poller = DeribitFundingPoller::new(documented_pages, SYMBOL, "BTC", floor);
    let events = poller.poll(NOW).expect("served");
    let got: Vec<i64> = events.iter().map(|e| funding(e).timestamp).collect();
    assert_eq!(got, vec![LSP.1], "the settlement before the floor stays in the wallet");
}

/// A failed read is an `Err` that moves nothing: the next good poll reads the same window and emits what the failed
/// one would have. A venue repeating its `continuation` cannot hold the poll in a loop.
#[test]
fn a_failed_read_loses_nothing_and_a_repeated_token_ends_the_paging() {
    let down = Cell::new(true);
    let asked = RefCell::new(Vec::new());
    let mut poller = DeribitFundingPoller::new(
        |p: &Value| {
            asked.borrow_mut().push(p["start_timestamp"].clone());
            if down.get() {
                Err("[10000] closed mid-request".to_string())
            } else {
                documented_pages(p)
            }
        },
        SYMBOL,
        "BTC",
        FLOOR,
    );
    assert!(poller.poll(NOW).is_err());
    down.set(false);
    assert_eq!(poller.poll(NOW + 60_000).expect("served").len(), 2, "both rows, one poll late");
    assert_eq!(asked.borrow()[0], asked.borrow()[1], "the failed poll did not move the window");

    let calls = Cell::new(0);
    let mut stuck = DeribitFundingPoller::new(
        |_: &Value| {
            calls.set(calls.get() + 1);
            Ok(json!({ "logs": logs(&fixture("/documented")), "continuation": 61282 }))
        },
        SYMBOL,
        "BTC",
        0,
    );
    assert_eq!(stuck.poll(NOW).expect("served").len(), 2);
    assert_eq!(calls.get(), 2, "the second page repeated the token, so the paging stopped");
}

fn inverse_engine() -> Engine<RecordingClient> {
    let mut grid = IndexMap::new();
    grid.insert(SYMBOL.to_string(), ContractValue::inverse(1.0));
    Engine::new(
        Account::new(1.0, "deribit", Some(grid), BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "deribit",
        SYMBOL,
    )
}

/// The coin cash a reconcile pass adopts, in the frame the node builds (the engine's own quote label).
fn adopt(e: &mut Engine<RecordingClient>, coin: f64) {
    let label = e.quote_asset.clone();
    e.account.apply_account_state(
        &AccountState {
            venue: "deribit".into(),
            balances: vec![(label.clone(), coin)],
            timestamp: 0,
            route_key: None,
        },
        &label,
    );
}

fn mark(e: &mut Engine<RecordingClient>, px: f64) {
    e.price_board.set_mark("deribit", SYMBOL, px, 1);
    e.account.set_mark_from("deribit", SYMBOL, px, MarkSource::VenueMark, 1);
}

fn close(got: f64, want: f64, tolerance: f64) -> bool {
    (got - want).abs() <= tolerance
}

/// **ONCE.** The account's wallet as the recon client reads it (the recorded summary: `balance + session_rpl`) is
/// the wallet after the documented page's two payments; the engine adopted it before them. Folding the polled
/// events moves the funding total by the payments, own PnL (the drawdown latch's PnL term,
/// `crates/vike-core/tests/integration/drawdown_measures_own_pnl.rs`) and equity by the payments at the coin's
/// price, once: a second poll adds nothing. The next pass then finds the books equal to the wallet (no drift with
/// `flags.reconcile_balance` on, where an engine that never heard of the payments holds a drift of exactly them),
/// and adopting the wallet outright (the flag off) moves neither equity nor own PnL: the wallet does not count the
/// funding a second time.
#[test]
fn a_funding_payment_books_once_and_the_adopted_wallet_does_not_count_it_again() {
    let summary = fixture("/testnet_2026_11_08/account_summary");
    let wallet =
        parse_account_summary_coin_cash(&summary.to_string()).expect("parses").expect("a balance");
    assert_eq!(wallet, 99.997_887_15, "the recorded coin cash");
    let mut poller = DeribitFundingPoller::new(documented_pages, SYMBOL, "BTC", 0);
    let events = poller.poll(NOW).expect("served");
    let paid: f64 = events.iter().map(|e| funding(e).amount).sum();
    assert!(close(paid, SETTLEMENT.2 + LSP.2, 1e-18), "{paid}");
    let before = wallet - paid;

    let px = 51_807.07; // the documented settlement price
    let config = PriceBoardConfig::default();
    let mut e = inverse_engine();
    adopt(&mut e, before);
    mark(&mut e, px);
    let (equity0, own0) = (e.resolved_equity(0.0, &config), e.resolved_own_pnl(&config));
    let mut ob = EventBusOutbox::default();
    for event in &events {
        e.on_event(event, &mut ob);
    }
    assert!(
        close(e.account.funding_paid, paid, 1e-18),
        "the funding total: {}",
        e.account.funding_paid
    );
    // Dollar figures: a micro-dollar band, where a second booking would be off by the payments' 0.78 USD.
    assert!(
        close(e.resolved_own_pnl(&config), own0 + paid * px, 1e-6),
        "own PnL moved by the payments once"
    );
    let equity1 = e.resolved_equity(0.0, &config);
    assert!(close(equity1, equity0 + paid * px, 1e-6), "equity moved by the payments once");
    let own1 = e.resolved_own_pnl(&config);

    // The same log again: nothing new, nothing booked.
    for event in poller.poll(NOW + 60_000).expect("served") {
        e.on_event(&event, &mut ob);
    }
    assert!(close(e.account.funding_paid, paid, 1e-18), "no second booking");

    // `flags.reconcile_balance` ON: the pass diffs the wallet against the books, on a band of nothing.
    let tolerance = ReconcileBalanceTolerance { abs_floor: 1e-12, rel_frac: 0.0 };
    match diff_balance(&e.local_view().as_view(), Some(wallet), &e.quote_asset, tolerance, 0) {
        ReconcileBalanceCheck::Anchored { expected, drift: None } => {
            assert!(
                close(expected, wallet, 1e-12),
                "the books are the wallet: {expected} vs {wallet}"
            )
        }
        other => panic!("the books must agree with the wallet: {other:?}"),
    }
    let mut blind = inverse_engine();
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
            assert!(
                close(venue_balance - local, paid, 1e-12),
                "without the events the wallet is ahead by them"
            )
        }
        other => panic!("an engine that never booked the funding holds a drift: {other:?}"),
    }

    // `flags.reconcile_balance` OFF: the pass adopts the wallet outright.
    adopt(&mut e, wallet);
    assert!(
        close(e.resolved_equity(0.0, &config), equity1, 1e-6),
        "the wallet holds the funding once"
    );
    assert_eq!(
        e.resolved_own_pnl(&config).to_bits(),
        own1.to_bits(),
        "an adoption writes no own PnL"
    );
    assert!(close(e.account.funding_paid, paid, 1e-18));
}
