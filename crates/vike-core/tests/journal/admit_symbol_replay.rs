//! A journaled `Command::AdmitSymbol` replays deterministically (decision 0173).
//!
//! The command carries the new symbol's grid, so the replay core needs no venue to reproduce an
//! admitted engine: `replay_offline` re-pumps the journaled tail through a real core and fences the
//! reproduced `state_hash` against the source's last `Snapshot`. Two shapes, because the admission can
//! sit on either side of the replay base: AFTER it (the replay re-applies the command) or BEFORE it
//! (the base `Snapshot` already holds the symbol, its grid row and its position, and `from_snapshot`
//! gives them back).

use crate::scratch::Scratch;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use vike_core::{CoreConfig, JournalConfig, spawn_core};
use vike_execution::SymbolAdmission;
use vike_execution::testing::RecordingClient;
use vike_execution::{Account, BalanceMode, Command, Engine, OrderIntent, RiskGate};
use vike_journal::JournalFileConfig;
use vike_model::events::{Event, FillEvent, PositionSide};
use vike_model::{Clock, OrderRequest, RiskLimits, SymbolGrid};

use crate::kit::events::sim_bare_fill;

const SECOND: &str = "ETHUSDT";

fn admission() -> Command {
    Command::AdmitSymbol(Box::new(SymbolAdmission {
        route_key: "sim".into(),
        symbol: SECOND.into(),
        grid: SymbolGrid { tick_size: Some(0.01), lot_size: Some(0.0001), ..SymbolGrid::default() },
        contract_value: Some(vike_model::ContractValue::linear(10.0)),
        initial_mark: Some(3000.0),
        initial_mark_ms: None,
        quote_currency: None,
        quote_rate: None,
        rate_as_of_ms: None,
        initial_margin_requirement: None,
        release: false,
        margin_mode: None,
    }))
}

fn eth_order() -> Command {
    Command::Order(OrderIntent::Submit(Box::new(OrderRequest {
        order_client_id: "ord1".into(),
        venue: "sim".into(),
        symbol: SECOND.into(),
        side: 1,
        qty: 0.12346,
        order_type: vike_model::OrderType::Limit,
        price: Some(3000.456),
        ..Default::default()
    })))
}

fn eth_fill() -> Event {
    Event::Fill(FillEvent {
        trade_id: "e1".into(),
        order_client_id: "ord1".into(),
        venue: "sim".into(),
        symbol: SECOND.into(),
        side: 1,
        last_qty: 0.1235,
        last_price: 3000.46,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".into(),
        timestamp: 1,
        mark_price: Some(3000.46),
        position_side: PositionSide::Both,
    })
}

/// `snapshot_every: 4`: the 4th journaled record trips the cadence `Snapshot` that becomes the replay
/// base. `admit_first` puts the admission among the records that come BEFORE it.
fn build(dir: &Path, admit_first: bool) {
    let limits = RiskLimits { tick_size: Some(0.1), lot_size: Some(0.001), ..RiskLimits::new() };
    let engine = Engine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(limits),
        RecordingClient::default(),
        "sim",
        "BTCUSDT",
    );
    let t = Arc::new(AtomicI64::new(0));
    let clock: Box<dyn Clock + Send> = Box::new(move || t.fetch_add(1, Ordering::Relaxed));
    let config = CoreConfig {
        seed_cash: 10_000.0,
        clock,
        order_client_id_session: Some(("cafef00d".into(), 0)),
        journal: Some(JournalConfig {
            dir: dir.to_path_buf(),
            file: JournalFileConfig { segment_bytes: 1024 * 1024, flush_every: 8 },
            snapshot_every: 4,
        }),
        ..CoreConfig::default()
    };
    let handle = spawn_core(engine, config);
    let sender = handle.event_sender();
    if admit_first {
        handle.send_command(admission()); // record 1
        sender.blocking_send(sim_bare_fill("t0", 1.0, 100.0)).unwrap();
        sender.blocking_send(sim_bare_fill("t1", 1.0, 100.0)).unwrap();
        sender.blocking_send(sim_bare_fill("t2", 1.0, 100.0)).unwrap(); // record 4: the base Snapshot
        handle.send_command(eth_order());
        sender.blocking_send(eth_fill()).unwrap();
    } else {
        for tid in ["t0", "t1", "t2", "t3"] {
            sender.blocking_send(sim_bare_fill(tid, 1.0, 100.0)).unwrap(); // record 4: the base Snapshot
        }
        handle.send_command(admission());
        handle.send_command(eth_order());
        sender.blocking_send(eth_fill()).unwrap();
    }
    handle.shutdown_and_join();
}

fn replays_to_the_same_state(tag: &str, admit_first: bool) {
    let dir = Scratch::reserved(tag);
    build(&dir, admit_first);
    let out = vike_core::replay::replay_offline(&dir).expect("the fence holds");
    let engine = &out.engines[0];
    assert_eq!(engine.extra_symbols, vec![SECOND.to_string()], "the symbol is admitted");
    let row = engine.limits.grid_by_symbol.get(SECOND).expect("its grid row");
    assert_eq!(row.tick_size, Some(0.01));
    assert!(row.on_demand, "the row remembers it came from an admission");
    assert!(
        engine.account.positions.iter().any(|((_, s, _), p)| s.as_str() == SECOND && p.size > 0.0),
        "the ETH fill folded into the ETH position"
    );
    assert!(out.records > 0 && out.snaps_compared >= 1);
}

#[test]
fn an_admission_after_the_replay_base_is_re_applied_and_fenced() {
    replays_to_the_same_state("admit-replay-tail", false);
}

#[test]
fn an_admission_before_the_replay_base_rides_the_snapshot() {
    replays_to_the_same_state("admit-replay-base", true);
}
