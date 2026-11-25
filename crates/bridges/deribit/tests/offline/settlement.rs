//! The 08:00 UTC settlement of a futures position the engine still holds
//! (`crates/bridges/deribit/src/reconcile/mod.rs`'s `parse_coin_cash_from_entry`), over the testnet capture
//! `crates/bridges/deribit/tests/fixtures/deribit_settlement_capture.json`: the venue minimum of `BTC-PERPETUAL`
//! (inverse, the BTC book) and of `BTC_USDC-PERPETUAL` (linear, the USDC book) bought at 07:55, held across 08:00
//! (`after`, 08:02) and sold back (`closed`). The capture's own `before` phase was refused (the socket was reset), so
//! the cash before 08:00 is the transaction log's. No venue, no credentials, no `#[ignore]`.
//!
//! What it pins: an engine that marks its position from the ENTRY adopts the cash it held before 08:00 (plus what the
//! session realized since, the funding alone here), where `balance + session_rpl` alone holds the settled PnL a
//! second time; that the summary of the same read states the same settled PnL, so a pass whose two reads straddle a
//! settlement or a fill reports no balance; and a book with no futures position held across a settlement reads the
//! old figure bit for bit.

use std::sync::Arc;

use serde_json::{Value, json};
use tracing::Level;

use vike_deribit::ReconcileClientDeribit;
use vike_deribit::execution::rest::VenueRestDeribit;
use vike_deribit::reconcile::{parse_account_summary_coin_cash, parse_coin_cash_from_entry};
use vike_execution::recon::ReconcileClient;
use vike_log::capture::captured;
use vike_model::SymbolProperties;

use crate::fake_deribit_websocket::{FakeDeribit, Session, rest_against, transport_against};

const CAPTURE: &str = include_str!("../fixtures/deribit_settlement_capture.json");
const CLASSES: &str = include_str!("../fixtures/deribit_classes_testnet_positions.json");
/// One unit of the venue's 8-decimal rounding of `balance`, `session_rpl` and the log rows.
const ROUNDING: f64 = 1e-8;

fn capture(phase: &str, field: &str) -> Value {
    let all: Value = serde_json::from_str(CAPTURE).expect("the capture parses");
    let v = all[phase][field].clone();
    assert!(!v.is_null(), "no {phase}.{field} in the capture");
    v
}

fn number(v: &Value, k: &str) -> f64 {
    v[k].as_f64().unwrap_or_else(|| panic!("no number `{k}` in {v}"))
}

fn close(got: f64, want: f64, tolerance: f64) -> bool {
    (got - want).abs() <= tolerance
}

/// Today's reader: `balance + session_rpl` of the currency's summary.
fn session_cash(phase: &str, currency: &str) -> f64 {
    let summary = capture(phase, &format!("account_summary_{currency}"));
    parse_account_summary_coin_cash(&summary.to_string()).expect("parses").expect("a balance")
}

/// The fix: the same summary beside the same currency's positions.
fn cash_from_entry(phase: &str, currency: &str) -> f64 {
    let summary = capture(phase, &format!("account_summary_{currency}"));
    let positions = capture(phase, &format!("positions_{currency}"));
    parse_coin_cash_from_entry(&summary.to_string(), &positions.to_string())
        .expect("parses")
        .expect("a balance")
}

/// `balance + session_rpl` just before 08:00, from the `after` log (newest first): the balance of the newest row before
/// the settlement, plus what the settlement rows moved that was not a held position's `session_upl` (the session's
/// realized PnL, a flat instrument's included).
fn cash_before_0800(currency: &str) -> f64 {
    let log = capture("after", &format!("transaction_log_{currency}"));
    let rows = log["logs"].as_array().expect("a log page");
    let settlements: Vec<&Value> = rows.iter().filter(|r| r["type"] == "settlement").collect();
    let at = settlements.first().expect("the log holds the settlement")["timestamp"].as_i64();
    let prior = rows.iter().find(|r| r["timestamp"].as_i64() < at).expect("a row before 08:00");
    let rpl: f64 = settlements
        .iter()
        .map(|r| number(r, "change") - r["session_upl"].as_f64().unwrap_or(0.0))
        .sum();
    number(prior, "balance") + rpl
}

/// What the settlement moved into `balance` for the held position: its PnL since the entry less its PnL since 08:00.
fn settled(currency: &str) -> f64 {
    let rows = capture("after", &format!("positions_{currency}"));
    let [position] = rows.as_array().expect("rows").as_slice() else {
        panic!("one {currency} row")
    };
    number(position, "total_profit_loss") - number(position, "floating_profit_loss")
}

/// **The BTC book (inverse), held across 08:00.** The 10 USD long from 73885.5 had -2.03e-07 BTC settled into `balance`
/// at 73774.78, and still states its entry: `balance + session_rpl` (99.99788633) is 2e-07 under the cash before 08:00,
/// the settled PnL the engine marks a second time from the entry. Taking it back is the cash before 08:00 (99.99788653).
#[test]
fn the_btc_book_held_across_0800_adopts_the_cash_it_held_before() {
    let before = cash_before_0800("BTC");
    assert!(close(before, 99.997_886_53, 1e-12), "{before}");
    assert!(close(settled("BTC"), -2.03e-07, 1e-15), "{}", settled("BTC"));
    let today = session_cash("after", "BTC");
    assert!(close(today - before, settled("BTC"), ROUNDING), "counted twice: {}", today - before);
    let cash = cash_from_entry("after", "BTC");
    assert!(close(cash, before, ROUNDING), "{cash} vs {before}");
}

/// **The USDC book (linear), held across 08:00.** The 0.0001 BTC long from 73895.5 had -0.009069 settled at 73804.81
/// (and SOL's and ALT's flat realized PnL moved from `session_rpl` into `balance` without a jump); since 08:00 the
/// session realized the position's funding alone (2.55e-06). Taking the settled PnL back is the cash before 08:00
/// plus that funding.
#[test]
fn the_usdc_book_held_across_0800_adopts_the_cash_it_held_before_plus_its_funding() {
    let before = cash_before_0800("USDC");
    assert!(close(before, 99_984.046_401_5, 1e-9), "{before}");
    assert!(close(settled("USDC"), -0.009_069, 1e-12), "{}", settled("USDC"));
    let funding = number(&capture("after", "positions_USDC")[0], "realized_funding");
    let since = number(&capture("after", "account_summary_USDC"), "session_rpl");
    assert_eq!(since, funding, "the session since 08:00 is the position's funding");
    let today = session_cash("after", "USDC");
    assert!(close(today - before, settled("USDC") + funding, ROUNDING), "{}", today - before);
    let cash = cash_from_entry("after", "USDC");
    assert!(close(cash, before + funding, ROUNDING), "{cash} vs {}", before + funding);
}

/// **After the close, both books: nothing is held, so the figure is today's bit for bit.** And it is the cash before
/// 08:00 moved by what an engine books for the round trip from its ENTRY: the close's PnL from the entry price, its
/// fee and its funding (the open's fee was paid before 08:00).
#[test]
fn after_the_close_both_books_read_todays_figure() {
    for currency in ["BTC", "USDC"] {
        let positions = capture("closed", &format!("positions_{currency}"));
        assert_eq!(positions[0]["direction"], "zero", "{currency}: flat");
        assert_eq!(
            cash_from_entry("closed", currency).to_bits(),
            session_cash("closed", currency).to_bits()
        );
    }
    let close_row = |currency: &str| {
        capture("closed", &format!("transaction_log_{currency}"))["logs"][0].clone()
    };
    let books =
        |row: &Value, pnl: f64| -number(row, "commission") + number(row, "interest_pl") + pnl;
    let btc = close_row("BTC");
    let want = cash_before_0800("BTC")
        + books(&btc, 10.0 * (1.0 / 73_885.5 - 1.0 / number(&btc, "price")));
    assert!(close(session_cash("closed", "BTC"), want, ROUNDING), "{want}");
    let usdc = close_row("USDC");
    let want =
        cash_before_0800("USDC") + books(&usdc, 0.0001 * (number(&usdc, "price") - 73_895.5));
    assert!(close(session_cash("closed", "USDC"), want, ROUNDING), "{want}");
}

/// **Before 08:00 nothing is taken back.** A position opened since the last settlement states `total_profit_loss` equal
/// to `floating_profit_loss`: every row of the classes capture (futures and options, long and short, both currencies)
/// does, so the cash read beside them is `balance + session_rpl` bit for bit. The BTC summary is the one just before
/// 08:00, from the log.
#[test]
fn before_0800_a_position_opened_since_the_last_settlement_takes_nothing_back() {
    let all: Value = serde_json::from_str(CLASSES).expect("the classes capture parses");
    let rows: Vec<Value> = all.as_object().expect("keyed rows").values().cloned().collect();
    let futures = rows.iter().filter(|r| r["kind"] == "future" && number(r, "size") != 0.0).count();
    assert_eq!(futures, 6, "the open futures rows");
    let summary = json!({"balance": 99.997_886_55, "session_rpl": -2e-08}).to_string();
    let today = parse_account_summary_coin_cash(&summary).expect("parses").map(f64::to_bits);
    let cash =
        parse_coin_cash_from_entry(&summary, &Value::Array(rows).to_string()).expect("parses");
    assert_eq!(cash.map(f64::to_bits), today);
    assert_eq!(
        parse_coin_cash_from_entry(&summary, "[]").expect("parses").map(f64::to_bits),
        today
    );
}

/// The cash of a book holding ONE futures position over two settlements, built by the venue's law: each settlement
/// moves the PnL since the previous one (or the entry) into `balance`, the position keeps its entry, and
/// `floating_profit_loss` runs from the last settlement price (the summary's `futures_pl` and `futures_session_upl` are
/// the same two figures). `pnl(a, b)` is the held size's PnL from price `a` to `b`.
fn held_over_two_settlements(pnl: impl Fn(f64, f64) -> f64) {
    let (entry, s1, s2, mark) = (73_895.5, 73_804.81, 74_420.0, 74_100.0);
    let cash0 = 100.0; // `balance + session_rpl` when the position opened
    let read = |balance: f64, last: f64| {
        let summary = json!({"balance": balance, "session_rpl": 0.0,
            "futures_pl": pnl(entry, mark), "futures_session_upl": pnl(last, mark)});
        let summary = summary.to_string();
        let positions = json!([{"kind": "future", "size": 1.0, "average_price": entry,
            "total_profit_loss": pnl(entry, mark), "floating_profit_loss": pnl(last, mark)}]);
        parse_coin_cash_from_entry(&summary, &positions.to_string()).expect("parses").expect("cash")
    };
    assert_eq!(read(cash0, entry), cash0, "before the first settlement: nothing settled");
    let balance1 = cash0 + pnl(entry, s1);
    assert!(close(read(balance1, s1), cash0, 1e-12), "after one: {}", read(balance1, s1));
    let balance2 = balance1 + pnl(s1, s2);
    assert!(close(read(balance2, s2), cash0, 1e-12), "after two: {}", read(balance2, s2));
    assert!((balance2 - cash0 - pnl(entry, s2)).abs() < 1e-12, "the balance holds both days");
}

/// **Held over TWO settlements, `total − floating` states both settled days**, so the cash is the one held before the
/// first, linear and inverse alike (the PnL telescopes: `(s1 − e) + (s2 − s1) = s2 − e`, and `1/e − 1/s2` likewise).
#[test]
fn a_position_held_over_two_settlements_takes_both_days_back() {
    held_over_two_settlements(|a, b| 0.0001 * (b - a)); // BTC_USDC-PERPETUAL, 0.0001 BTC, in USDC
    held_over_two_settlements(|a, b| 10.0 * (1.0 / a - 1.0 / b)); // BTC-PERPETUAL, 10 USD, in BTC
}

/// **A futures mount's recon client reports it, each book from its own currency**: `fetch_balance` reads the summary
/// and then the positions of the mount's settle currency, so the BTC book is corrected by its BTC position alone and
/// the USDC book by its USDC one. A client that is not a futures mount reads `balance` with one request, as before.
///
/// Base: `fetch_balance` sent the summary read alone and reported `balance + session_rpl`, the settled PnL twice
/// (BTC 2e-07 under the cash before 08:00, USDC 0.009069 under).
#[test]
fn a_futures_mounts_reconcile_client_reports_the_cash_from_entry_of_its_own_currency() {
    for (symbol, currency) in [("BTC-PERPETUAL", "BTC"), ("BTC_USDC-PERPETUAL", "USDC")] {
        let fake = FakeDeribit::spawn(vec![Session::Settlement("after")]);
        let rest = VenueRestDeribit::new(
            transport_against(&fake),
            symbol,
            SymbolProperties::default(),
            currency,
        );
        let client = ReconcileClientDeribit::new(Arc::new(rest)).with_coin_cash(true);
        let got = client.fetch_balance().expect("served").expect("a balance");
        let since =
            number(&capture("after", &format!("account_summary_{currency}")), "session_rpl");
        assert!(close(got, cash_before_0800(currency) + since, ROUNDING), "{currency}: {got}");
        assert_eq!(got.to_bits(), cash_from_entry("after", currency).to_bits(), "{currency}");
        assert_eq!(fake.methods(), ["private/get_account_summary", "private/get_positions"]);
        for method in ["private/get_account_summary", "private/get_positions"] {
            assert_eq!(fake.params_of(method), [json!({"currency": currency})], "{currency}");
        }

        let fake = FakeDeribit::spawn(vec![Session::Settlement("after")]);
        let rest = VenueRestDeribit::new(
            transport_against(&fake),
            symbol,
            SymbolProperties::default(),
            currency,
        );
        let plain = ReconcileClientDeribit::new(Arc::new(rest));
        let balance = number(&capture("after", &format!("account_summary_{currency}")), "balance");
        assert_eq!(plain.fetch_balance().expect("served"), Some(balance), "{currency}");
        assert_eq!(fake.methods(), ["private/get_account_summary"]);
    }
}

/// The summary's own statement of the settled PnL in one read: `futures_pl − futures_session_upl`.
fn stated(phase: &str, currency: &str) -> f64 {
    let summary = capture(phase, &format!("account_summary_{currency}"));
    number(&summary, "futures_pl") - number(&summary, "futures_session_upl")
}

/// The positions' statement of it in one read: `Σ (total_profit_loss − floating_profit_loss)` over the open rows.
fn summed(phase: &str, currency: &str) -> f64 {
    let rows = capture(phase, &format!("positions_{currency}"));
    let rows = rows.as_array().expect("rows");
    let open = rows.iter().filter(|r| number(r, "size") != 0.0);
    open.map(|r| number(r, "total_profit_loss") - number(r, "floating_profit_loss")).sum()
}

/// **Agreement: in one read the summary states the settled PnL the positions do**, both books, after 08:00 and after
/// the close, within one BTC unit even (BTC after: the summary's -2e-07, the position's -2.03e-07), so a pass whose two
/// reads saw the same book reports its cash.
#[test]
fn the_summary_and_the_positions_of_one_book_state_one_settled_pnl() {
    for phase in ["after", "closed"] {
        for currency in ["BTC", "USDC"] {
            let (a, b) = (stated(phase, currency), summed(phase, currency));
            assert!(close(a, b, 1e-8), "{phase} {currency}: {a} vs {b}");
            let summary = capture(phase, &format!("account_summary_{currency}")).to_string();
            let positions = capture(phase, &format!("positions_{currency}")).to_string();
            assert!(parse_coin_cash_from_entry(&summary, &positions).is_ok(), "{phase} {currency}");
        }
    }
    assert!(close(stated("after", "BTC"), -2e-07, 1e-15), "{}", stated("after", "BTC"));
    assert!(close(stated("after", "USDC"), -0.009_069, 1e-12), "{}", stated("after", "USDC"));
}

/// **Disagreement: a close landing between the two reads.** The summary of one phase beside the positions of the other
/// (either way round, both books) differ by the whole settled PnL, so the pass reports NO balance (an `Err`; the next
/// pass reads again) and logs one WARN naming both numbers. Without the check the cash would be off by that PnL.
#[test]
fn reads_that_straddle_a_close_report_no_balance_and_one_warn() {
    for currency in ["BTC", "USDC"] {
        for (summary_phase, positions_phase) in [("after", "closed"), ("closed", "after")] {
            let summary =
                capture(summary_phase, &format!("account_summary_{currency}")).to_string();
            let positions = capture(positions_phase, &format!("positions_{currency}")).to_string();
            let (got, events) = captured(|| parse_coin_cash_from_entry(&summary, &positions));
            let err = got.expect_err("no balance this pass");
            assert!(err.contains("between the two reads"), "{err}");
            let warns: Vec<_> = events.iter().filter(|e| e.level == Level::WARN).collect();
            let [warn] = warns.as_slice() else { panic!("{currency}: one WARN, got {events:?}") };
            assert_eq!(warn.target, "vike_deribit::recon");
            assert_eq!(warn.fields["currency"], currency);
            let field = |k: &str| warn.fields[k].parse::<f64>().expect("a number");
            assert_eq!(field("positions"), summed(positions_phase, currency), "{currency}");
            assert_eq!(field("summary"), stated(summary_phase, currency), "{currency}");
        }
    }
}

/// A USDC book of THREE open futures, each row on the venue's 6-decimal grid, beside a summary whose
/// `futures_pl − futures_session_upl` is the rows' sum plus `off_by` (what rounding the total once, not each row, can
/// leave). Answers the reader's verdict.
fn three_position_usdc_book(off_by: f64) -> Result<Option<f64>, String> {
    let rows = [
        (7.375_119, -0.014_431, -0.005_362),
        (-7.684_06, 0.002_035, 0.001_017),
        (0.5, 0.000_123, -0.000_456),
    ];
    let positions: Vec<Value> = rows
        .iter()
        .map(|&(size, total, floating)| {
            json!({"kind": "future", "size": size, "total_profit_loss": total, "floating_profit_loss": floating})
        })
        .collect();
    let total: f64 = rows.iter().map(|r| r.1).sum();
    let floating: f64 = rows.iter().map(|r| r.2).sum();
    let summary = json!({"currency": "USDC", "balance": 1_000.0, "session_rpl": 0.0,
        "futures_pl": total + off_by, "futures_session_upl": floating});
    parse_coin_cash_from_entry(&summary.to_string(), &Value::Array(positions).to_string())
}

/// **The cross-check's band is `(k + 1)` units of the currency's grid**: three USDC rows (`k` = 3, unit 1e-6) allow
/// 4e-6. A 2e-6 gap between the rounded rows and the rounded total is rounding and PASSES with the position sum as
/// the value (-0.007472 settled, taken back from 1000); a 5e-5 gap is a different book and FAILS.
///
/// Base (a fixed 1e-8): the 2e-6 gap was refused, every pass, on a correct multi-position USDC book.
#[test]
fn a_three_position_usdc_book_passes_a_rounding_gap_and_fails_a_real_one() {
    let cash = three_position_usdc_book(2e-6).expect("rounding passes").expect("a balance");
    assert!(close(cash, 1_000.0 + 0.007_472, 1e-9), "the position sum is the value: {cash}");
    let err = three_position_usdc_book(5e-5).expect_err("a real gap fails");
    assert!(err.contains("between the two reads"), "{err}");
}

/// **A summary that states no `futures_pl` or no `futures_session_upl` skips the cross-check**: the cash is the
/// position sum's alone (the corrected figure, bit for bit what the full summary gives), with ONE DEBUG line and no
/// WARN, even beside positions the summary would have refused.
#[test]
fn a_summary_without_the_futures_fields_skips_the_check_with_one_debug_line() {
    for field in ["futures_pl", "futures_session_upl"] {
        let mut summary = capture("after", "account_summary_BTC");
        summary.as_object_mut().expect("an object").remove(field);
        let positions = capture("after", "positions_BTC").to_string();
        let (got, events) =
            captured(|| parse_coin_cash_from_entry(&summary.to_string(), &positions));
        let cash = got.expect("no check, no refusal").expect("a balance");
        assert_eq!(cash.to_bits(), cash_from_entry("after", "BTC").to_bits(), "without {field}");
        let ours: Vec<_> = events.iter().filter(|e| e.target == "vike_deribit::recon").collect();
        let [line] = ours.as_slice() else { panic!("without {field}: one line, got {events:?}") };
        assert_eq!(line.level, Level::DEBUG, "without {field}");

        // The flat summary of the close beside the held position: refused with the field, reported without it.
        let mut flat = capture("closed", "account_summary_BTC");
        flat.as_object_mut().expect("an object").remove(field);
        let held = parse_coin_cash_from_entry(&flat.to_string(), &positions).expect("not refused");
        assert_eq!(held, Some(session_cash("closed", "BTC") - settled("BTC")), "without {field}");
    }
}

/// **A failed balance read carries deribit's own outcome** (the read the startup credential probe makes): the rate
/// limit is not now, a refused request refused, and a code deribit's table does not map outcome unknown, never a
/// refused key.
///
/// Base: every failure of this read was held as text and refused, as no deribit table existed.
#[test]
fn a_failed_balance_read_carries_deribits_own_outcome() {
    use vike_utility::errors::FailureOutcome;
    for (code, outcome) in [
        (10_028, FailureOutcome::NotNow),
        (-32_602, FailureOutcome::Refused),
        (19_999, FailureOutcome::OutcomeUnknown),
    ] {
        let fake = FakeDeribit::spawn(vec![Session::VenueErrorOn {
            method: "private/get_account_summary",
            code,
            message: "scripted",
            absent: false,
        }]);
        let client = ReconcileClientDeribit::new(rest_against(&fake));
        let failed = client.fetch_balance().expect_err("the venue refused the read");
        assert_eq!(failed.outcome, outcome, "{code}: {failed}");
    }
}
