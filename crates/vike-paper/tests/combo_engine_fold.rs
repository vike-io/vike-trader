//! Combo leg fills THROUGH the OMS, not just the paper book: the paper client emits each leg's
//! bare `Event::Fill` under the LEG's own symbol, and `Engine::on_event` gates a bare
//! fill on `accepts_symbol` OR (combo gate 4) `owns_fill_symbol` — the order-client-id-ownership route that
//! folds a fill whose `order_client_id` names a registered order when the symbol is that order's
//! own or one of its `combo_legs`. Before gate 4, a mount that failed to admit the leg symbols
//! hit the worst shape: order Filled via the order-client-id-routed FSM wraps, Account flat, strategy blind.
//!
//! Two engine-level proofs here, upstream of the mount lowering (which wires leg admission at
//! combo registration): (1) with the leg symbols admitted via `extra_symbols` (the production
//! shape — it also carries leg Funding/PositionLiquidated events), both leg positions land in the
//! Account and both fills reach the strategy seam; (2) with NO `extra_symbols` at all, the gate-4
//! order-client-id-ownership route alone still folds the leg fills — the old failure shape is structurally
//! gone.
//!
//! It also PINS `OrderRecord`'s combo accounting: `filled_qty` is in the combo's own unit (3 for
//! the 1×2 ratio spread with request.qty 3; it summed every leg's qty, 9, before) and `average_fill_price`
//! is the net per combo unit (−88). A future change to those numbers must be deliberate.

use vike_execution::testing::pump;
use vike_execution::{Account, BalanceMode, Engine, EventBusOutbox, ExecutionClient, RiskGate};
use vike_marketdata::test_support::quoted_bar;
use vike_model::OrderStatus;
use vike_model::RiskLimits;
use vike_model::{OrderCombo, OrderComboLeg, TimeInForce, build_combo};
use vike_paper::ExecutionClientPaper;

const VENUE: &str = "deribit";

/// The Account's position-ledger key. Aliased to the REAL type rather than re-spelling the tuple:
/// the hot-path re-key changed it from `(String, String, String)` to interned/`Copy` components,
/// and a local restatement silently rots into a `Borrow` mismatch at every `positions.get()`.
type PosKey = vike_execution::PositionKey;

fn pos_key(symbol: &str) -> PosKey {
    (VENUE.into(), symbol.into(), vike_model::events::PositionSide::Both)
}

#[test]
fn combo_leg_fills_fold_into_the_account_when_every_leg_symbol_is_admitted() {
    let mut engine = Engine::new(
        Account::new(1.0, VENUE, None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        ExecutionClientPaper::new(VENUE, "COMBO", 0.0, 0.0, 0.0),
        VENUE,
        "COMBO",
    );
    // THE requirement under test: every leg symbol admitted via the Phase D seam. Without these,
    // `accepts_symbol` drops the bare leg fills while the FSM wraps still apply — order Filled,
    // Account flat, strategy blind (the #456 lowering wires this at combo registration).
    engine.extra_symbols = vec!["NEAR".to_string(), "FAR".to_string()];
    // what the runtime sets iff a strategy is mounted — makes `applied_fills` observable here
    engine.collect_applied_fills = true;
    let mut outbox = EventBusOutbox::default();

    // the PR's own 1×2 ratio spread, 3 combo units: buy 1×NEAR, sell 2×FAR.
    // net = 100 − 2×94 = −88 (credit); limit −88 crosses on the marks below.
    let spec = OrderCombo {
        venue: VENUE.into(),
        side: 1,
        qty: 3.0,
        legs: vec![
            OrderComboLeg { symbol: "NEAR".into(), ratio: 1, limit: None },
            OrderComboLeg { symbol: "FAR".into(), ratio: -2, limit: None },
        ],
        net_limit: Some(-88.0),
        time_in_force: TimeInForce::Gtc,
        account: None,
        order_client_id: None,
    };
    let request = build_combo(&spec, "cb").expect("valid combo spec");
    engine.submit_order(&request, 0, &mut outbox);
    pump(&mut engine, &mut outbox);
    assert_eq!(engine.registry["cb"].status, OrderStatus::Accepted, "gated, registered, resting");

    engine.client.on_bar(&quoted_bar(1, "NEAR", 99.0, 100.0));
    engine.client.on_bar(&quoted_bar(1, "FAR", 94.0, 95.0));
    pump(&mut engine, &mut outbox);

    // 1) the Account gained BOTH leg keys, with per-leg (not blended) prices.
    let near = engine.account.positions.get(&pos_key("NEAR")).expect("NEAR position exists");
    assert_eq!((near.size, near.average_price), (3.0, 100.0), "buy 1×3 NEAR at its ask");
    let far = engine.account.positions.get(&pos_key("FAR")).expect("FAR position exists");
    assert_eq!((far.size, far.average_price), (-6.0, 94.0), "sell 2×3 FAR at its bid");

    // 2) the strategy seam saw every leg fill (`Strategy::on_fill` deliveries).
    let seen: Vec<(&str, f64)> =
        engine.applied_fills.iter().map(|a| (a.fill.symbol.as_str(), a.fill.last_qty)).collect();
    assert_eq!(seen, vec![("NEAR", 3.0), ("FAR", 6.0)], "both leg fills applied, in leg order");

    // 3) ONE order downstream: the single order client id reached exactly its terminal state.
    let mo = &engine.registry["cb"];
    assert_eq!(mo.status, OrderStatus::Filled, "one order client id, one terminal");

    // 4) PIN `OrderRecord`'s combo accounting (`OrderRecord::apply`'s combo fold): filled_qty is
    // the request's own unit, 3 (it summed every leg's qty, 3 + 6 = 9, before), and average_fill_price is
    // the NET per combo unit, 1×100 − 2×94 = −88 (a cross-instrument VWAP, 96, before).
    assert_eq!(mo.request.qty, 3.0, "the request says 3 combo units");
    assert_eq!(mo.filled_qty, 3.0, "combo units, the unit of `qty`");
    assert_eq!(mo.average_fill_price, -88.0, "the net per combo unit");

    // and nothing was silently lost on the way through the fold
    assert_eq!(engine.events_dropped_order_unknown, 0);
    assert_eq!(engine.closing_events_dropped_order_still_live, 0);
    assert_eq!(engine.events_dropped_order_already_closed, 0);
}

/// Combo gate 4: the SAME combo lifecycle with NO `extra_symbols` wired at all — the engine's
/// order-client-id-ownership route (`owns_fill_symbol`: registered order client id + a `combo_legs` symbol) folds the
/// bare leg fills by itself. Before gate 4 this exact setup was the documented failure shape
/// (order Filled, Account flat, strategy blind); now it is structurally impossible for an order
/// this engine registered. `extra_symbols` remains the production admission (it also carries leg
/// Funding/PositionLiquidated events) — this pins the fill-lane safety net beneath it.
#[test]
fn combo_leg_fills_fold_via_order_client_id_ownership_without_extra_symbols() {
    let mut engine = Engine::new(
        Account::new(1.0, VENUE, None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        ExecutionClientPaper::new(VENUE, "COMBO", 0.0, 0.0, 0.0),
        VENUE,
        "COMBO",
    );
    // deliberately NO extra_symbols — the gate-4 route is the only thing admitting leg fills
    engine.collect_applied_fills = true;
    let mut outbox = EventBusOutbox::default();

    let spec = OrderCombo {
        venue: VENUE.into(),
        side: 1,
        qty: 3.0,
        legs: vec![
            OrderComboLeg { symbol: "NEAR".into(), ratio: 1, limit: None },
            OrderComboLeg { symbol: "FAR".into(), ratio: -2, limit: None },
        ],
        net_limit: Some(-88.0),
        time_in_force: TimeInForce::Gtc,
        account: None,
        order_client_id: None,
    };
    let request = build_combo(&spec, "cb").expect("valid combo spec");
    engine.submit_order(&request, 0, &mut outbox);
    pump(&mut engine, &mut outbox);
    assert_eq!(engine.registry["cb"].status, OrderStatus::Accepted, "gated, registered, resting");

    engine.client.on_bar(&quoted_bar(1, "NEAR", 99.0, 100.0));
    engine.client.on_bar(&quoted_bar(1, "FAR", 94.0, 95.0));
    pump(&mut engine, &mut outbox);

    // the Account gained BOTH leg keys through order client id ownership alone
    let near = engine.account.positions.get(&pos_key("NEAR")).expect("NEAR position exists");
    assert_eq!((near.size, near.average_price), (3.0, 100.0), "buy 1×3 NEAR at its ask");
    let far = engine.account.positions.get(&pos_key("FAR")).expect("FAR position exists");
    assert_eq!((far.size, far.average_price), (-6.0, 94.0), "sell 2×3 FAR at its bid");
    // the strategy seam saw every leg fill, and the one order client id terminalized exactly once
    let seen: Vec<(&str, f64)> =
        engine.applied_fills.iter().map(|a| (a.fill.symbol.as_str(), a.fill.last_qty)).collect();
    assert_eq!(seen, vec![("NEAR", 3.0), ("FAR", 6.0)], "both leg fills applied, in leg order");
    assert_eq!(
        engine.registry["cb"].status,
        OrderStatus::Filled,
        "one order client id, one terminal"
    );
}
