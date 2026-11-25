//! A journaled release (`SymbolAdmission::release`, decision 0205) replays deterministically and moves
//! no format version.
//!
//! The statement is an `AdmitSymbol` record with one more key, so the segment is stamped as an admission
//! stamps it (19) and never higher, and `replay_offline` re-pumps the tail through a real core and fences
//! the reproduced `state_hash` against the source's last `Snapshot`. Two shapes, as `admit_symbol_replay.rs`
//! has: the release AFTER the replay base (the replay re-applies it) and BEFORE it (the base `Snapshot`
//! already lacks the symbol).

use crate::scratch::Scratch;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use vike_core::{CoreConfig, JournalConfig, spawn_core};
use vike_execution::testing::RecordingClient;
use vike_execution::{Account, BalanceMode, Command, Engine, Ingest, RiskGate, SymbolAdmission};
use vike_journal::{Journal, JournalFileConfig, JournalRecord};
use vike_model::{Clock, RiskLimits, SymbolGrid};

use crate::kit::events::sim_bare_fill;

const SECOND: &str = "ETHUSDT";
const THIRD: &str = "SOLUSDT";

fn admission(symbol: &str) -> Command {
    Command::AdmitSymbol(Box::new(SymbolAdmission {
        route_key: "sim".into(),
        symbol: symbol.into(),
        grid: SymbolGrid { tick_size: Some(0.01), lot_size: Some(0.0001), ..SymbolGrid::default() },
        contract_value: Some(vike_model::ContractValue::linear(10.0)),
        initial_mark: Some(3000.0),
        initial_mark_ms: None,
        quote_currency: None,
        quote_rate: None,
        rate_as_of_ms: None,
        initial_margin_requirement: Some(0.25),
        release: false,
        margin_mode: None,
    }))
}

fn release(symbol: &str) -> Command {
    Command::AdmitSymbol(Box::new(SymbolAdmission::releasing("sim", symbol)))
}

/// `snapshot_every: 4`: the 4th journaled record trips the cadence `Snapshot` that becomes the replay base.
/// `release_first` puts admit + release among the records that come BEFORE it.
fn build(dir: &Path, release_first: bool) {
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
    if release_first {
        handle.send_command(admission(SECOND)); // record 1
        handle.send_command(release(SECOND)); // record 2
        sender.blocking_send(sim_bare_fill("t0", 1.0, 100.0)).unwrap();
        sender.blocking_send(sim_bare_fill("t1", 1.0, 100.0)).unwrap(); // record 4: the base Snapshot
        handle.send_command(admission(THIRD));
    } else {
        for tid in ["t0", "t1", "t2", "t3"] {
            sender.blocking_send(sim_bare_fill(tid, 1.0, 100.0)).unwrap(); // record 4: the base Snapshot
        }
        handle.send_command(admission(SECOND));
        handle.send_command(admission(THIRD));
        handle.send_command(release(SECOND));
    }
    handle.shutdown_and_join();
}

fn replays_to_the_same_state(tag: &str, release_first: bool) {
    let dir = Scratch::reserved(tag);
    build(&dir, release_first);
    let out = vike_core::replay::replay_offline(&dir).expect("the fence holds");
    let engine = &out.engines[0];
    assert_eq!(engine.extra_symbols, vec![THIRD.to_string()], "SECOND was given back, THIRD stays");
    assert!(engine.limits.grid_by_symbol.get(SECOND).is_none(), "no row of the released symbol");
    assert!(
        engine.limits.initial_margin_by_symbol.get(SECOND).is_none(),
        "no margin row of it either"
    );
    assert!(engine.limits.grid_by_symbol.get(THIRD).is_some_and(|row| row.on_demand));
    assert!(out.records > 0 && out.snaps_compared >= 1);

    // The release is an `AdmitSymbol` record, so the segment carries what the admissions in it need (20: they
    // state a margin rate) and the release adds nothing, never 21 (the format that closes the rollback door
    // once more). A build from before the key reads it: `record_compat_tests.rs` proves that on its shapes.
    assert_eq!(Journal::latest_segment_version(&dir).unwrap(), Some(34), "{tag}");
    let releases = Journal::read_all(&dir)
        .unwrap()
        .into_iter()
        .filter(|r| {
            matches!(r, JournalRecord::Command {
                message: Ingest::Command(Command::AdmitSymbol(a)), ..
            } if a.release && a.symbol == SECOND)
        })
        .count();
    assert_eq!(releases, 1, "{tag}: the statement is journaled once");
}

#[test]
fn a_release_after_the_replay_base_is_re_applied_and_fenced() {
    replays_to_the_same_state("unadmit-replay-tail", false);
}

#[test]
fn a_release_before_the_replay_base_rides_the_snapshot() {
    replays_to_the_same_state("unadmit-replay-base", true);
}
