//! IDENTITY AGAINST `origin/main`: a core that never sees a foreign-quoted symbol and never states a
//! per-symbol margin rate writes the SAME journal and reaches the SAME state hash as it did before
//! the multi-currency ledger.
//!
//! The scenario is ordinary: admit one symbol (a non-default contract unit, a mark), rest an order
//! on it, fill it, rest another and cancel it, fold a fill on the mounted symbol, stop. The engine
//! has an engine-wide initial margin armed, so the margin lane is on and a stray `im_by_symbol` row
//! would show. Every record the journal holds is read back, serialized with `serde_json`, and
//! folded into one FNV-1a64 digest; the replay's final state hash is pinned beside it. The pinned
//! constants were taken from a run of THIS FILE on `origin/main` (twice, to show they are stable),
//! then asserted on the branch: any new field that reaches the canonical JSON of a record or of an
//! engine snapshot while off (a rate, a currency, an fx table, a margin row) moves them.
//!
//! The file is written to compile on BOTH trees: it names only APIs that exist on `origin/main`, and
//! it builds the admission with `serde_json::from_value` of a JSON object rather than a struct
//! literal, because the literal grew fields on the branch. Seq numbers, clocks and scratch paths are
//! deterministic: the clock hands out `k` for message `k`, the coid session is fixed, and the records
//! carry no path.

use crate::scratch::Scratch;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use vike_core::{CoreConfig, JournalConfig, spawn_core};
use vike_exec::testing::RecordingClient;
use vike_exec::{
    Account, BalanceMode, Command, ExecutionEngine, OrderIntent, RiskGate, SymbolAdmission,
};
use vike_journal::{CommandJournal, JournalFileConfig};
use vike_model::events::{Event, FillEvent, PositionSide, TradeId};
use vike_model::{OrderRequest, OrderType, RiskLimits};

/// What the scenario journals, on `origin/main` and on the branch alike.
const RECORDS: usize = 9;
const RECORDS_DIGEST: u64 = 2_018_645_066_537_154_719;
const FINAL_HASH: u64 = 4_021_672_860_585_908_292;

const SECOND: &str = "ETHUSDT";

fn engine() -> ExecutionEngine<RecordingClient> {
    let limits = RiskLimits {
        tick_size: Some(0.1),
        lot_size: Some(0.001),
        im_requirement: Some(0.25),
        ..RiskLimits::new()
    };
    ExecutionEngine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(limits),
        RecordingClient::default(),
        "sim",
        "BTCUSDT",
    )
}

/// An ORDINARY admission, built from JSON so the file compiles whether or not `SymbolAdmission` has
/// the multi-currency fields.
fn admission() -> Command {
    let a: SymbolAdmission = serde_json::from_value(serde_json::json!({
        "route_key": "sim",
        "symbol": SECOND,
        "grid": { "tick_size": 0.01, "lot_size": 0.0001 },
        "contract_value": { "kind": "linear", "per_unit": 10.0 },
        "initial_mark": 3000.0
    }))
    .expect("an ordinary admission deserializes on both trees");
    Command::AdmitSymbol(Box::new(a))
}

fn limit(coid: &str, symbol: &str, qty: f64, px: f64) -> Command {
    Command::Order(OrderIntent::Submit(Box::new(OrderRequest {
        client_order_id: coid.into(),
        venue: "sim".into(),
        symbol: symbol.into(),
        side: 1,
        qty,
        order_type: OrderType::Limit,
        price: Some(px),
        ..Default::default()
    })))
}

fn fill(tid: &str, coid: &str, symbol: &str, qty: f64, px: f64) -> Event {
    Event::Fill(FillEvent {
        trade_id: TradeId::new(tid).expect("test trade ids are non-empty"),
        client_order_id: coid.into(),
        venue: "sim".into(),
        symbol: symbol.into(),
        side: 1,
        last_qty: qty,
        last_px: px,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".into(),
        ts: 1,
        mark_price: Some(px),
        position_side: PositionSide::Both,
    })
}

/// The scenario, with a cadence `Snap` every 3 records so the journal holds several.
fn build(dir: &Path) {
    let t = Arc::new(AtomicI64::new(0));
    let cfg = CoreConfig {
        seed_cash: 10_000.0,
        clock: Box::new(move || t.fetch_add(1, Ordering::Relaxed)),
        coid_session: Some(("cafef00d".into(), 0)),
        journal: Some(JournalConfig {
            dir: dir.to_path_buf(),
            file: JournalFileConfig { segment_bytes: 1024 * 1024, flush_every: 8 },
            snapshot_every: 3,
        }),
        ..CoreConfig::default()
    };
    let handle = spawn_core(engine(), cfg);
    let sender = handle.event_sender();
    handle.send_command(admission());
    handle.send_command(limit("c6-ord1", SECOND, 0.12346, 3000.456));
    sender.blocking_send(fill("c6-f1", "c6-ord1", SECOND, 0.1235, 3000.46)).unwrap();
    handle.send_command(limit("c6-ord2", SECOND, 0.5, 2900.0));
    handle.send_command(Command::Order(OrderIntent::Cancel("c6-ord2".into())));
    sender.blocking_send(fill("c6-f2", "", "BTCUSDT", 1.0, 100.0)).unwrap();
    handle.shutdown_and_join();
}

/// FNV-1a64 over the JSON of every record, newline-separated, in journal order: the bytes the
/// journal's own writer serializes (`serde_json::to_vec` of the record, field order fixed by the
/// derive). Deliberately NOT through `serde_json::Value`: whether a `Value`'s objects are sorted or
/// kept in insertion order is a `serde_json` feature (`preserve_order`) that another crate of a
/// wider build can switch on, which would move the digest without any record changing.
fn digest_of(dir: &Path) -> (usize, u64) {
    let records = CommandJournal::read_all(dir).expect("the journal reads back");
    let mut bytes = Vec::new();
    for r in &records {
        bytes.extend(serde_json::to_vec(r).expect("a record serializes"));
        bytes.push(b'\n');
    }
    (records.len(), vike_util::fnv::fnv1a64(&bytes))
}

#[test]
fn a_core_with_no_foreign_symbol_journals_what_it_journaled_before() {
    let dir = Scratch::reserved("no-foreign-symbol");
    build(&dir);
    let (records, digest) = digest_of(&dir);
    let out = vike_core::replay::replay_offline(&dir).expect("the fence holds");

    // Nothing new reached the engine: no per-symbol margin row for the admitted symbol.
    assert!(
        out.engines[0].limits.im_by_symbol.is_empty(),
        "an admission that states no rate writes no row: {:?}",
        out.engines[0].limits.im_by_symbol
    );
    assert_eq!(
        (records, digest, out.final_hash),
        (RECORDS, RECORDS_DIGEST, FINAL_HASH),
        "the journal's records and the replayed state hash are the ones origin/main produces"
    );
}

/// The same scenario run twice in two directories journals byte-identical canonical JSON: the pinned
/// digest above is a property of the scenario, not of the run that took it.
#[test]
fn the_scenario_journals_the_same_bytes_every_run() {
    let a = Scratch::reserved("no-foreign-symbol-a");
    let b = Scratch::reserved("no-foreign-symbol-b");
    build(&a);
    build(&b);
    assert_eq!(digest_of(&a), digest_of(&b));
}
