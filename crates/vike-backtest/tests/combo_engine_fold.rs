//! Combo leg fills THROUGH the OMS, not just the paper book: the paper client emits each leg's
//! bare `Event::Fill` under the LEG's own symbol, and `ExecutionEngine::on_event` gates a bare
//! fill on `accepts_symbol` OR (combo gate 4) `owns_fill_symbol` — the coid-ownership route that
//! folds a fill whose `client_order_id` names a registered order when the symbol is that order's
//! own or one of its `combo_legs`. Before gate 4, a mount that failed to admit the leg symbols
//! hit the worst shape: order Filled via the coid-routed FSM wraps, Account flat, strategy blind.
//!
//! Two engine-level proofs here, upstream of the mount lowering (which wires leg admission at
//! combo registration): (1) with the leg symbols admitted via `extra_symbols` (the production
//! shape — it also carries leg Funding/PositionLiquidated events), both leg positions land in the
//! Account and both fills reach the strategy seam; (2) with NO `extra_symbols` at all, the gate-4
//! coid-ownership route alone still folds the leg fills — the old failure shape is structurally
//! gone.
//!
//! It also PINS the cross-instrument aggregate semantics of `ManagedOrder` for a combo (the
//! documented display-only caveat): `filled_qty` sums |ratio|×qty across legs (9 for the 1×2
//! ratio spread with request.qty 3 — 300% of the requested combo units) and `avg_fill_px` is a
//! cross-instrument VWAP (96.0 — a price on NO instrument). A future change to those numbers must
//! be deliberate, not accidental.

use vike_backtest::paper::PaperExecutionClient;
use vike_exec::{
    Account, BalanceMode, EventHandler, ExecutionClient, ExecutionEngine, OrderStatus, Outbox,
    RiskGate, RiskLimits,
};
use vike_model::{build_combo, Bar, ComboLeg, ComboSpec, TimeInForce};

const VENUE: &str = "deribit";

/// The Account's position-ledger key. Aliased to the REAL type rather than re-spelling the tuple:
/// the hot-path re-key changed it from `(String, String, String)` to interned/`Copy` components,
/// and a local restatement silently rots into a `Borrow` mismatch at every `positions.get()`.
type PosKey = vike_exec::PositionKey;

fn pos_key(symbol: &str) -> PosKey {
    (VENUE.into(), symbol.into(), vike_model::events::PositionSide::Both)
}

/// A bar carrying an explicit bid/ask for `symbol` (the combo book prices legs off the side it
/// would actually trade).
fn quoted(ts: i64, symbol: &str, bid: f64, ask: f64) -> Bar {
    Bar {
        ts,
        open: bid,
        high: ask,
        low: bid,
        close: (bid + ask) / 2.0,
        volume: 0.0,
        funding: None,
        bid: Some(bid),
        ask: Some(ask),
        symbol: Some(symbol.to_string()),
    }
}

/// Drain the client's queued venue events through the engine's fold (what the core runtime does).
fn pump(engine: &mut ExecutionEngine<PaperExecutionClient>, outbox: &mut Outbox) {
    while let Some(event) = engine.client.poll_events() {
        engine.on_event(&event, outbox);
    }
}

#[test]
fn combo_leg_fills_fold_into_the_account_when_every_leg_symbol_is_admitted() {
    let mut engine = ExecutionEngine::new(
        Account::new(1.0, VENUE, None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        PaperExecutionClient::new(VENUE, "COMBO", 0.0, 0.0, 0.0),
        VENUE,
        "COMBO",
    );
    // THE requirement under test: every leg symbol admitted via the Phase D seam. Without these,
    // `accepts_symbol` drops the bare leg fills while the FSM wraps still apply — order Filled,
    // Account flat, strategy blind (the #456 lowering wires this at combo registration).
    engine.extra_symbols = vec!["NEAR".to_string(), "FAR".to_string()];
    // what the runtime sets iff a strategy is mounted — makes `applied_fills` observable here
    engine.collect_applied_fills = true;
    let mut outbox = Outbox::default();

    // the PR's own 1×2 ratio spread, 3 combo units: buy 1×NEAR, sell 2×FAR.
    // net = 100 − 2×94 = −88 (credit); limit −88 crosses on the marks below.
    let spec = ComboSpec {
        venue: VENUE.into(),
        side: 1,
        qty: 3.0,
        legs: vec![
            ComboLeg { symbol: "NEAR".into(), ratio: 1 },
            ComboLeg { symbol: "FAR".into(), ratio: -2 },
        ],
        net_limit: Some(-88.0),
        time_in_force: TimeInForce::Gtc,
    };
    let request = build_combo(&spec, "cb").expect("valid combo spec");
    engine.submit_order(&request, 0, &mut outbox);
    pump(&mut engine, &mut outbox);
    assert_eq!(engine.registry["cb"].status, OrderStatus::Accepted, "gated, registered, resting");

    engine.client.on_bar(&quoted(1, "NEAR", 99.0, 100.0));
    engine.client.on_bar(&quoted(1, "FAR", 94.0, 95.0));
    pump(&mut engine, &mut outbox);

    // 1) the Account gained BOTH leg keys, with per-leg (not blended) prices.
    let near = engine.account.positions.get(&pos_key("NEAR")).expect("NEAR position exists");
    assert_eq!((near.size, near.avg_px), (3.0, 100.0), "buy 1×3 NEAR at its ask");
    let far = engine.account.positions.get(&pos_key("FAR")).expect("FAR position exists");
    assert_eq!((far.size, far.avg_px), (-6.0, 94.0), "sell 2×3 FAR at its bid");

    // 2) the strategy seam saw every leg fill (`Strategy::on_fill` deliveries).
    let seen: Vec<(&str, f64)> =
        engine.applied_fills.iter().map(|a| (a.fill.symbol.as_str(), a.fill.last_qty)).collect();
    assert_eq!(seen, vec![("NEAR", 3.0), ("FAR", 6.0)], "both leg fills applied, in leg order");

    // 3) ONE order downstream: the single coid reached exactly its terminal state.
    let mo = &engine.registry["cb"];
    assert_eq!(mo.status, OrderStatus::Filled, "one coid, one terminal");

    // 4) PIN the documented cross-instrument AGGREGATE semantics (display-only caveat at
    // `emit_combo_fills`): filled_qty = Σ|ratio|×qty = 3 + 6 = 9 ≠ request.qty = 3, and
    // avg_fill_px = (3×100 + 6×94)/9 = 96.0 — a VWAP across different instruments, a price on
    // no tradable instrument. If these numbers ever change, change the doc WITH them.
    assert_eq!(mo.request.qty, 3.0, "the request still says 3 combo units");
    assert_eq!(mo.filled_qty, 9.0, "cross-leg aggregate, NOT combo units");
    assert_eq!(mo.avg_fill_px, 96.0, "cross-instrument VWAP, NOT a tradable price");

    // and nothing was silently lost on the way through the fold
    assert_eq!(engine.dropped_unknown_coid, 0);
    assert_eq!(engine.dropped_terminal_on_live, 0);
    assert_eq!(engine.stranded_terminal_drops, 0);
}

/// Combo gate 4: the SAME combo lifecycle with NO `extra_symbols` wired at all — the engine's
/// coid-ownership route (`owns_fill_symbol`: registered coid + a `combo_legs` symbol) folds the
/// bare leg fills by itself. Before gate 4 this exact setup was the documented failure shape
/// (order Filled, Account flat, strategy blind); now it is structurally impossible for an order
/// this engine registered. `extra_symbols` remains the production admission (it also carries leg
/// Funding/PositionLiquidated events) — this pins the fill-lane safety net beneath it.
#[test]
fn combo_leg_fills_fold_via_coid_ownership_without_extra_symbols() {
    let mut engine = ExecutionEngine::new(
        Account::new(1.0, VENUE, None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        PaperExecutionClient::new(VENUE, "COMBO", 0.0, 0.0, 0.0),
        VENUE,
        "COMBO",
    );
    // deliberately NO extra_symbols — the gate-4 route is the only thing admitting leg fills
    engine.collect_applied_fills = true;
    let mut outbox = Outbox::default();

    let spec = ComboSpec {
        venue: VENUE.into(),
        side: 1,
        qty: 3.0,
        legs: vec![
            ComboLeg { symbol: "NEAR".into(), ratio: 1 },
            ComboLeg { symbol: "FAR".into(), ratio: -2 },
        ],
        net_limit: Some(-88.0),
        time_in_force: TimeInForce::Gtc,
    };
    let request = build_combo(&spec, "cb").expect("valid combo spec");
    engine.submit_order(&request, 0, &mut outbox);
    pump(&mut engine, &mut outbox);
    assert_eq!(engine.registry["cb"].status, OrderStatus::Accepted, "gated, registered, resting");

    engine.client.on_bar(&quoted(1, "NEAR", 99.0, 100.0));
    engine.client.on_bar(&quoted(1, "FAR", 94.0, 95.0));
    pump(&mut engine, &mut outbox);

    // the Account gained BOTH leg keys through coid ownership alone
    let near = engine.account.positions.get(&pos_key("NEAR")).expect("NEAR position exists");
    assert_eq!((near.size, near.avg_px), (3.0, 100.0), "buy 1×3 NEAR at its ask");
    let far = engine.account.positions.get(&pos_key("FAR")).expect("FAR position exists");
    assert_eq!((far.size, far.avg_px), (-6.0, 94.0), "sell 2×3 FAR at its bid");
    // the strategy seam saw every leg fill, and the one coid terminalized exactly once
    let seen: Vec<(&str, f64)> =
        engine.applied_fills.iter().map(|a| (a.fill.symbol.as_str(), a.fill.last_qty)).collect();
    assert_eq!(seen, vec![("NEAR", 3.0), ("FAR", 6.0)], "both leg fills applied, in leg order");
    assert_eq!(engine.registry["cb"].status, OrderStatus::Filled, "one coid, one terminal");
}
