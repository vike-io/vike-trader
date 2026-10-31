//! A journaled PER-SYMBOL MARGIN RATE (`SymbolAdmission::im_requirement`) replays deterministically.
//!
//! The rate rides the journaled `Command::AdmitSymbol`, writes the symbol's `im_by_symbol` row, and
//! `RiskLimits::im_for` puts that row ahead of the engine-wide rate in the gate's buying power, so a
//! replay that did not re-apply the row would admit or refuse orders differently and fail the fence.
//! The scenario is built so the decisions DISCRIMINATE: the engine-wide rate is 0.25 (4x), the
//! account's equity is 400, and an order of 600 notional passes at 0.25 (margin 150) and is refused
//! (`insufficient-margin`, margin 600) at 1.0.
//!
//! 1. admit `SPOT_A` at 1.0 and `PERP_B` with no rate (it stays at the engine-wide 0.25);
//! 2. buy 6 `SPOT_A` @ 100: REFUSED at its 1x;
//! 3. buy 6 `PERP_B` @ 100: passes at the engine-wide 4x and rests (150 committed);
//! 4. re-admit `SPOT_A` at 0.5 (a repeat admission replaces the row);
//! 5. buy 4 `SPOT_A` @ 100: passes at 0.5 (margin 200 against 250 free) where it would be refused at
//!    its old 1.0 (margin 400);
//! 6. both orders fill.
//!
//! The registry the replay reproduces holds the two orders that passed and not the one refused, and
//! the replayed limits hold exactly `SPOT_A: 0.5`: nothing is written for `PERP_B`, which stated no
//! rate. The fence runs in four shapes, so the base `Snap` falls before the first admission, between
//! the refused order and the re-admission, and after the re-admission (the base then carries the
//! row, because `from_snapshot` restores every limit).

use crate::scratch::Scratch;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use vike_core::{CoreConfig, JournalConfig, spawn_core};
use vike_exec::testing::RecordingClient;
use vike_exec::{Command, ExecutionEngine, OrderIntent, SymbolAdmission};
use vike_journal::{CommandJournal, JournalFileConfig, JournalRecord};
use vike_model::events::{Event, FillEvent, PositionSide, TradeId};
use vike_model::{OrderRequest, OrderType, RiskLimits, SymbolGrid};

use crate::kit::events::sim_bare_fill;

const SPOT_A: &str = "SPOT_A";
const PERP_B: &str = "PERP_B";

/// Engine-wide initial margin 0.25 (4x) on the mounted `BTCUSDT`, tick and lot 0.5.
fn engine() -> ExecutionEngine<RecordingClient> {
    let limits = RiskLimits {
        tick_size: Some(0.5),
        lot_size: Some(0.5),
        im_requirement: Some(0.25),
        ..RiskLimits::new()
    };
    ExecutionEngine::new(
        vike_exec::Account::new(1.0, "sim", None, vike_exec::BalanceMode::Delta),
        vike_exec::RiskGate::new(limits),
        RecordingClient::default(),
        "sim",
        "BTCUSDT",
    )
}

fn admission(symbol: &str, im_requirement: Option<f64>) -> Command {
    Command::AdmitSymbol(Box::new(SymbolAdmission {
        route_key: "sim".into(),
        symbol: symbol.into(),
        grid: SymbolGrid { tick_size: Some(0.5), lot_size: Some(0.5), ..SymbolGrid::default() },
        contract_value: None,
        initial_mark: Some(100.0),
        quote_ccy: None,
        quote_rate: None,
        rate_as_of_ms: None,
        im_requirement,
        release: false,
        margin_mode: None,
    }))
}

fn buy(coid: &str, symbol: &str, qty: f64) -> Command {
    Command::Order(OrderIntent::Submit(Box::new(OrderRequest {
        client_order_id: coid.into(),
        venue: "sim".into(),
        symbol: symbol.into(),
        side: 1,
        qty,
        order_type: OrderType::Limit,
        price: Some(100.0),
        ..Default::default()
    })))
}

fn fill(tid: &str, coid: &str, symbol: &str, qty: f64) -> Event {
    Event::Fill(FillEvent {
        trade_id: TradeId::new(tid).expect("test trade ids are non-empty"),
        client_order_id: coid.into(),
        venue: "sim".into(),
        symbol: symbol.into(),
        side: 1,
        last_qty: qty,
        last_px: 100.0,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".into(),
        ts: 1,
        mark_price: Some(100.0),
        position_side: PositionSide::Both,
    })
}

enum Step {
    Cmd(Command),
    Ev(Event),
}

/// The eight journaled records of the scenario, in order.
fn scenario() -> Vec<Step> {
    vec![
        Step::Cmd(admission(SPOT_A, Some(1.0))),
        Step::Cmd(admission(PERP_B, None)),
        Step::Cmd(buy("oA1", SPOT_A, 6.0)),
        Step::Cmd(buy("oB1", PERP_B, 6.0)),
        Step::Cmd(admission(SPOT_A, Some(0.5))),
        Step::Cmd(buy("oA2", SPOT_A, 4.0)),
        Step::Ev(fill("fB1", "oB1", PERP_B, 6.0)),
        Step::Ev(fill("fA2", "oA2", SPOT_A, 4.0)),
    ]
}

/// A journaled core over `dir` with equity 400; the clock hands out `k` for message `k`.
fn journaled(dir: &Path, snapshot_every: u64) -> CoreConfig {
    let t = Arc::new(AtomicI64::new(0));
    CoreConfig {
        seed_cash: 400.0,
        clock: Box::new(move || t.fetch_add(1, Ordering::Relaxed)),
        coid_session: Some(("cafef00d".into(), 0)),
        journal: Some(JournalConfig {
            dir: dir.to_path_buf(),
            file: JournalFileConfig { segment_bytes: 1024 * 1024, flush_every: 8 },
            snapshot_every,
        }),
        ..CoreConfig::default()
    }
}

/// `pad` leading half-unit fills of the mounted symbol at 1.0 (a notional of a fraction of a unit:
/// they trip the cadence `Snap` without taking any margin the scenario's orders count on).
fn run(dir: &Path, pad: usize, snapshot_every: u64) {
    let handle = spawn_core(engine(), journaled(dir, snapshot_every));
    let sender = handle.event_sender();
    for i in 0..pad {
        sender.blocking_send(sim_bare_fill(&format!("pad{i}"), 0.5, 1.0)).unwrap();
    }
    for step in scenario() {
        match step {
            Step::Cmd(c) => handle.send_command(c),
            Step::Ev(e) => sender.blocking_send(e).unwrap(),
        }
    }
    handle.shutdown_and_join();
}

/// The last `Snap`'s engine: what the LIVE run ended as (the exit snapshot every clean stop writes).
fn live_end(dir: &Path) -> vike_exec::EngineSnapshot {
    CommandJournal::read_all(dir)
        .unwrap()
        .into_iter()
        .rev()
        .find_map(|r| match r {
            JournalRecord::Snap { mut engines, .. } => engines.pop(),
            _ => None,
        })
        .expect("the journal holds a Snap")
}

fn assert_replays_to_the_live_run(tag: &str, pad: usize, snapshot_every: u64) {
    let dir = Scratch::reserved(tag);
    run(&dir, pad, snapshot_every);
    let live = live_end(&dir);

    let out = vike_core::replay::replay_offline(&dir).expect("the fence holds");
    assert!(out.records > 0 && out.snaps_compared >= 1);
    let engine = &out.engines[0];

    // The row: SPOT_A's replacement stands, PERP_B (no rate stated) has none, and the replay agrees
    // with the live run on the whole map.
    assert_eq!(
        engine.limits.im_by_symbol.get(SPOT_A).map(|x| x.to_bits()),
        Some(0.5_f64.to_bits())
    );
    assert!(
        !engine.limits.im_by_symbol.contains_key(PERP_B),
        "a symbol that states no rate gets no row"
    );
    assert_eq!(engine.limits.im_by_symbol.len(), 1);
    assert_eq!(engine.limits.im_by_symbol, live.limits.im_by_symbol, "replay == live");
    assert_eq!(engine.limits.im_requirement, Some(0.25), "the engine-wide rate is untouched");

    // The decisions that rode the rows: the refused order never registered, the two that passed did.
    let coids: Vec<&str> = engine.registry.iter().map(|(c, _)| c.as_str()).collect();
    assert_eq!(coids, ["oB1", "oA2"], "oA1 was refused at 1x; oB1 and the 0.5x oA2 passed");
    let sizes: Vec<(&str, f64)> = engine
        .account
        .positions
        .iter()
        .filter(|((_, s, _), _)| s.as_str() == SPOT_A || s.as_str() == PERP_B)
        .map(|((_, s, _), p)| (s.as_str(), p.size))
        .collect();
    assert_eq!(sizes, [(PERP_B, 6.0), (SPOT_A, 4.0)]);
}

/// The whole scenario after the base `Snap` (four pad fills trip it): every admission, order and
/// fill is re-applied by the replay.
#[test]
fn a_margin_rate_admitted_after_the_replay_base_is_re_applied_and_fenced() {
    assert_replays_to_the_live_run("margin-rate-tail", 4, 4);
}

/// The base falls after the two admissions: it already holds the 1.0 row, and the refused order, the
/// re-admission and the rest are the tail.
#[test]
fn a_base_holding_the_row_replays_the_refusal_and_the_replacement() {
    assert_replays_to_the_live_run("margin-rate-base-admits", 0, 2);
}

/// The base falls after the re-admission: the 0.5 row is BEFORE the base and rides the snapshot;
/// only the order at 0.5 and the fills are replayed.
#[test]
fn a_replacement_before_the_replay_base_rides_the_snapshot() {
    assert_replays_to_the_live_run("margin-rate-base-replaced", 0, 5);
}

/// The base falls after the first rested order: the tail re-applies the replacement, and the second
/// order must see 0.5 and not the 1.0 the base holds.
#[test]
fn a_replacement_in_the_tail_replaces_the_row_the_base_holds() {
    assert_replays_to_the_live_run("margin-rate-base-resting", 0, 4);
}

/// A margin figure that is not a number is journaled as `null` (`serde_json` writes a non-finite float
/// that way), which reads back as NO figure. The live engine therefore has to treat it as no figure
/// too, or the fence breaks: an engine that REFUSED the admission (the old `bad-im-requirement`) would
/// not have the symbol, and the replay, reading `null`, would admit it. A first-sight NaN admits the
/// symbol with no row; a NaN after a real figure removes the row, exactly as a `None` does.
#[test]
fn a_margin_rate_that_is_not_a_number_replays_as_the_live_run_took_it() {
    let dir = Scratch::reserved("margin-rate-nan");
    let handle = spawn_core(engine(), journaled(&dir, 1024));
    handle.send_command(admission(SPOT_A, Some(f64::NAN)));
    handle.send_command(admission(PERP_B, Some(0.5)));
    handle.send_command(admission(PERP_B, Some(f64::NAN)));
    handle.shutdown_and_join();
    let live = live_end(&dir);
    assert_eq!(live.extra_symbols, [SPOT_A, PERP_B], "the live run admitted both");
    assert!(live.limits.im_by_symbol.is_empty(), "and holds no row: the 0.5 was removed");

    let out = vike_core::replay::replay_offline(&dir).expect("the fence holds");
    let engine = &out.engines[0];
    assert_eq!(engine.extra_symbols, live.extra_symbols, "replay == live");
    assert_eq!(engine.limits.im_by_symbol, live.limits.im_by_symbol, "replay == live");
}

/// The stamps: a journal whose admissions state a margin rate is v20 (a v19 reader would margin the
/// symbol at the engine-wide rate), while the same admission stating none leaves the segment at the
/// v19 an ordinary admission needs, even with the margin lane armed.
#[test]
fn only_an_admission_that_states_a_rate_raises_the_header_to_20() {
    let stated = Scratch::reserved("margin-rate-v20");
    run(&stated, 0, 1024);
    assert_eq!(CommandJournal::latest_segment_version(&stated).unwrap(), Some(20));

    let none = Scratch::reserved("margin-rate-v19");
    let handle = spawn_core(engine(), journaled(&none, 1024));
    handle.send_command(admission(PERP_B, None));
    handle.shutdown_and_join();
    assert_eq!(CommandJournal::latest_segment_version(&none).unwrap(), Some(19));
}
