//! Per-mount ATTRIBUTION DURABILITY across a restart (multi-mount, gap D) — the end-to-end gates
//! for the two values that now survive the journal:
//!
//! - the per-mount ledgers, captured on every `Snapshot` (`Snapshot.mount_attribution`, journal v13) and re-seeded
//!   through `CoreConfig::mount_attribution`, and
//! - the order client id -> mount ORIGIN map, rebuilt from the journal's EXISTING `StrategySubmit`/
//!   `MintedSubmit` provenance (no new record type) and re-seeded through
//!   `CoreConfig::journal_orders`, so an order still RESTING across the restart books its eventual fill into the mount that placed
//!   it instead of vanishing from every ledger.
//!
//! ...plus the third value that had to reach the journal for the map to be COMPLETE: the per-mount
//! budget latch's FLATTEN now stamps its owning `mount_id` on its `MarginCallLiquidate` record
//! (journal v14), so a restore credits that order — and the realized loss its fill books — to the
//! mount whose breach released it instead of the residual row.
//!
//! Plus the compat gate: a pre-v13 journal (no `mount_attribution` on its Snapshot) still restores, with empty
//! ledgers — the additive-only rule `MIN_READABLE_VERSION` encodes.

use crate::scratch::Scratch;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::time::Duration;

use vike_core::replay::restore_from_journal;
use vike_core::strategy_state::mount_id_with;
use vike_core::{CoreConfig, CoreHandle, JournalConfig, Mount, spawn_core};
use vike_execution::testing::RecordingClient;
use vike_execution::{CoreSnapshot, Engine, MountViewKind};
use vike_journal::{JournalFileConfig, JournalRecord, fnv1a32};
use vike_model::Clock;
use vike_model::events::{Event, FillEvent, TradeId};

use crate::kit::doubles::RestOnce;
use crate::kit::engines::sim_engine;
use crate::kit::events::sim_quote;
use crate::kit::journal::{records, rewrite_journal};

const VENUE: &str = "sim";
const SYMBOL: &str = "BTCUSDT";
const INTERVAL: &str = "1m";

/// A scratch journal directory, removed when the returned guard drops. The journal's own `open`
/// calls `create_dir_all`, so the path is RESERVED rather than created. Hold the guard for the
/// whole test — see `crates/vike-core/src/scratch.rs` for the leak this closed.
fn unique_dir(tag: &str) -> Scratch {
    Scratch::reserved(&format!("mattr-{tag}"))
}

fn mount(done: &Arc<AtomicBool>) -> Mount {
    Mount {
        order_client_id_code: None,
        account: None,
        symbols: Vec::new(),
        controller_id: None,
        underlying_symbol: None,
        venue: VENUE.into(),
        symbol: SYMBOL.into(),
        interval: INTERVAL.into(),
        // ONE mount-minted, still-RESTING order to attribute across the restart.
        strategy: Box::new(RestOnce { done: Arc::clone(done) }),
    }
}

/// A journaling config with `snapshot_every = u64::MAX`: no cadence Snapshot fires, so the ONE Snapshot in
/// the journal is the clean-shutdown exit Snapshot — the restore base, carrying the final ledgers.
fn core_config(dir: &Path, done: &Arc<AtomicBool>) -> CoreConfig {
    let t = Arc::new(AtomicI64::new(0));
    let clock: Box<dyn Clock + Send> = Box::new(move || t.fetch_add(1, Ordering::Relaxed));
    CoreConfig {
        seed_cash: 10_000.0,
        clock,
        order_client_id_session: Some(("cafef00d".into(), 0)),
        strategy: Some(mount(done)),
        journal: Some(JournalConfig {
            dir: dir.to_path_buf(),
            file: JournalFileConfig { segment_bytes: 1024 * 1024, flush_every: 1 },
            snapshot_every: u64::MAX,
        }),
        ..CoreConfig::default()
    }
}

fn fill_for(order_client_id: &str, qty: f64, px: f64) -> Event {
    Event::Fill(FillEvent {
        // minted by this helper — same `t-<order client id>` bytes as the `format!` it replaced
        trade_id: TradeId::prefixed("t-", order_client_id),
        order_client_id: order_client_id.to_string(),
        venue: VENUE.into(),
        symbol: SYMBOL.into(),
        side: 1,
        last_qty: qty,
        last_price: px,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".into(),
        timestamp: 5,
        mark_price: None,
        position_side: vike_model::events::PositionSide::Both,
    })
}

/// Poll the published snapshot until `f` holds (the core publishes on its own coalesced cadence, so
/// a live-core test must wait for a publish rather than assume one), for this file's 4 s.
fn wait_for(
    handle: &CoreHandle,
    what: &str,
    f: impl Fn(&CoreSnapshot) -> bool,
) -> Arc<CoreSnapshot> {
    crate::kit::handle::wait_for_within(handle, Duration::from_secs(4), what, f)
}

/// The ownership candidates a restart hands the core, built from the pure provenance fold
/// (`RestoredState::order_client_id_mounts`): ownership only, which is what these gates assert.
fn journal_orders_of(
    order_client_id_mounts: &[(String, String)],
) -> Vec<vike_core::replay::OrderOwnerCandidate> {
    order_client_id_mounts
        .iter()
        .map(|(order_client_id, mount_id)| vike_core::replay::OrderOwnerCandidate {
            order_client_id: order_client_id.clone(),
            mount_id: mount_id.clone(),
            ..vike_core::replay::OrderOwnerCandidate::default()
        })
        .collect()
}

/// The mount rows of a snapshot (the trailing residual row filtered out).
fn mount_rows(snap: &CoreSnapshot) -> Vec<&vike_execution::MountView> {
    snap.mounts.iter().filter(|m| m.kind == MountViewKind::Mount).collect()
}

/// GAP D, the payoff: a mount-minted order still RESTING at shutdown keeps its origin across the
/// restart, so the fill that lands AFTER the restart books into that mount's ledger.
///
/// Pre-fix, `order_client_id_mount` was pure runtime state — a restarted core had an empty map, so the fill
/// folded into `Account.realized_pnl`/position and into NO mount ledger at all.
#[test]
fn order_client_id_mount_rebuilt_from_journal_replay() {
    let dir = unique_dir("order-client-id-mount");

    // ---- run 1: the mount submits one order, which is still resting at a clean shutdown.
    let done = Arc::<AtomicBool>::default();
    let handle = spawn_core(sim_engine(), core_config(&dir, &done));
    handle.tick_sender().quote(sim_quote(100.0, 1)).unwrap();
    let snap = wait_for(&handle, "the mount's order to register", |s| !s.orders.is_empty());
    let order_client_id = snap.orders[0].order_client_id.clone();
    handle.shutdown_and_join();

    // ---- the journal alone reconstructs which mount minted that order client id.
    let state = restore_from_journal(&dir).unwrap().expect("run 1 wrote an exit Snapshot");
    assert_eq!(
        state.order_client_id_mounts,
        vec![(order_client_id.clone(), mount_id_with(None, VENUE, SYMBOL, INTERVAL))],
        "the StrategySubmit/MintedSubmit pair names the minting mount"
    );

    // ---- run 2: restore, then fill the PRE-RESTART order.
    let done2 = Arc::<AtomicBool>::default();
    done2.store(true, Ordering::Relaxed); // never re-submit: only the restored order matters
    let config = CoreConfig {
        seed_cash: state.engines[0].equity_seed,
        order_client_id_session: Some((
            state.order_client_id_session.clone(),
            state.order_client_id_sequence,
        )),
        mount_attribution: state.mount_attribution.clone(),
        journal_orders: journal_orders_of(&state.order_client_id_mounts),
        ..core_config(&dir, &done2)
    };
    let engine2 = Engine::from_snapshot(&state.engines[0], RecordingClient::default());
    let handle = spawn_core(engine2, config);
    handle.event_sender().blocking_send(fill_for(&order_client_id, 1.0, 100.0)).unwrap();
    let snap = wait_for(&handle, "the restored mount to book the fill", |s| {
        mount_rows(s).first().is_some_and(|m| m.position != 0.0)
    });
    let rows = mount_rows(&snap);
    assert_eq!(rows.len(), 1, "one mount row: {:?}", snap.mounts);
    assert_eq!(
        rows[0].position.to_bits(),
        1.0_f64.to_bits(),
        "the post-restart fill booked into the ORIGINATING mount's ledger"
    );
    handle.shutdown_and_join();
    let _ = std::fs::remove_dir_all(&dir);
}

/// GAP D, the ledger half: a mount's folded attribution rides the `Snapshot` (journal v13) and comes
/// back through `CoreConfig::mount_attribution`, so a restart resumes the mount's position/realized PnL
/// instead of silently starting it at zero while `Account` keeps the same PnL.
#[test]
fn mount_attribute_survives_snapshot_roundtrip() {
    let dir = unique_dir("snap-roundtrip");

    // ---- run 1: submit, fill, fold the ledger, clean shutdown (the exit Snapshot captures it).
    let done = Arc::<AtomicBool>::default();
    let handle = spawn_core(sim_engine(), core_config(&dir, &done));
    handle.tick_sender().quote(sim_quote(100.0, 1)).unwrap();
    let snap = wait_for(&handle, "the mount's order to register", |s| !s.orders.is_empty());
    let order_client_id = snap.orders[0].order_client_id.clone();
    handle.event_sender().blocking_send(fill_for(&order_client_id, 2.0, 100.0)).unwrap();
    let snap = wait_for(&handle, "the mount to book its fill", |s| {
        mount_rows(s).first().is_some_and(|m| m.position != 0.0)
    });
    assert_eq!(mount_rows(&snap)[0].position.to_bits(), 2.0_f64.to_bits());
    handle.shutdown_and_join();

    // ---- the exit Snapshot carries the ledger, keyed by mount id.
    let state = restore_from_journal(&dir).unwrap().expect("run 1 wrote an exit Snapshot");
    assert_eq!(
        state.mount_attribution.len(),
        1,
        "one row per mount slot: {:?}",
        state.mount_attribution
    );
    let row = &state.mount_attribution[0];
    assert_eq!(row.mount_id, mount_id_with(None, VENUE, SYMBOL, INTERVAL));
    assert_eq!(row.size.to_bits(), 2.0_f64.to_bits());
    assert_eq!(row.average_price.to_bits(), 100.0_f64.to_bits());

    // ---- run 2: the restored core resumes that ledger (a bare quote is enough to force a publish).
    let done2 = Arc::<AtomicBool>::default();
    done2.store(true, Ordering::Relaxed);
    let config = CoreConfig {
        seed_cash: state.engines[0].equity_seed,
        order_client_id_session: Some((
            state.order_client_id_session.clone(),
            state.order_client_id_sequence,
        )),
        mount_attribution: state.mount_attribution.clone(),
        journal_orders: journal_orders_of(&state.order_client_id_mounts),
        ..core_config(&dir, &done2)
    };
    let engine2 = Engine::from_snapshot(&state.engines[0], RecordingClient::default());
    let handle = spawn_core(engine2, config);
    handle.tick_sender().quote(sim_quote(100.0, 9)).unwrap();
    let snap = wait_for(&handle, "the restored core to publish its mount view", |s| {
        !mount_rows(s).is_empty()
    });
    assert_eq!(
        mount_rows(&snap)[0].position.to_bits(),
        2.0_f64.to_bits(),
        "the restored mount resumes its attributed position, not zero"
    );
    handle.shutdown_and_join();
    let _ = std::fs::remove_dir_all(&dir);
}

/// A CADENCE `Snapshot` a mount's fill trips holds that fill in the mount's ledger: the fill reaches the mount before
/// the `Snapshot` is written (`crates/vike-core/src/runtime/publish.rs`'s `write_snap_after_delivery`), so a crash
/// restore from it (the exit `Snapshot` lost) lands the live ledger. Written before the delivery, it held the fill in the
/// account and not in the mount, and the fill's record sits before it, so no restore delivered it again.
#[test]
fn a_cadence_snapshot_a_mounts_fill_trips_holds_the_fill_in_the_mounts_ledger() {
    let dir = unique_dir("cadence-delivery");
    let done = Arc::<AtomicBool>::default();
    let config = CoreConfig {
        // A cadence `Snapshot` at every journaled message's end: the fill's is the first (the quote is not journaled).
        journal: Some(JournalConfig {
            dir: dir.to_path_buf(),
            file: JournalFileConfig { segment_bytes: 1024 * 1024, flush_every: 1 },
            snapshot_every: 1,
        }),
        ..core_config(&dir, &done)
    };
    let handle = spawn_core(sim_engine(), config);
    handle.tick_sender().quote(sim_quote(100.0, 1)).unwrap();
    let snap = wait_for(&handle, "the mount's order to register", |s| !s.orders.is_empty());
    let order_client_id = snap.orders[0].order_client_id.clone();
    handle.event_sender().blocking_send(fill_for(&order_client_id, 1.0, 100.0)).unwrap();
    handle.shutdown_and_join();

    let mut recs = records(&dir);
    let Some(JournalRecord::Snapshot { mount_attribution: live, .. }) = recs.pop() else {
        panic!("the journal ends in its exit Snapshot");
    };
    assert!(
        live.iter().any(|r| r.venue.is_none() && r.size.to_bits() == 1.0_f64.to_bits()),
        "the live mount heard its fill: {live:?}"
    );
    let crashed = unique_dir("cadence-delivery-crash");
    rewrite_journal(&crashed, recs);
    let state = restore_from_journal(&crashed).unwrap().expect("the fill's cadence Snapshot");
    assert_eq!(state.mount_attribution, live, "the restore holds the mount's live ledger");
}

fn bar_at(ts: i64, px: f64) -> vike_execution::BarUpdate {
    vike_execution::BarUpdate {
        venue: VENUE.into(),
        symbol: SYMBOL.into(),
        interval: INTERVAL.into(),
        bar: vike_marketdata::Bar {
            timestamp: ts,
            open: px,
            high: px,
            low: px,
            close: px,
            volume: 1.0,
            funding: None,
            bid: None,
            ask: None,
            symbol: None,
        },
        sides: None,
    }
}

/// **THE v14 PAYOFF, END TO END.** The per-mount budget latch's FLATTEN keeps its mount across a
/// restart: drive a real latch + flatten through a live core, restore from the journal ALONE, fill
/// the flatten, and the realized loss lands in THAT MOUNT's ledger with the residual row absorbing
/// nothing.
///
/// Pre-fix the flatten was journaled as a bare `MarginCallLiquidate` — indistinguishable on disk
/// from the ACCOUNT-wide margin-call sweep's record — so `fold_order_client_id_mounts` closed ownership and
/// left the following `MintedSubmit` unattributed. The restored core then booked the flatten's fill
/// into the RESIDUAL row, and the mount's ledger came back under-reporting precisely the realized
/// loss its budget had just latched on.
#[test]
fn a_budget_latch_flatten_keeps_its_mount_across_a_restart() {
    let dir = unique_dir("latch-flatten");
    let mount_id = mount_id_with(None, VENUE, SYMBOL, INTERVAL);

    // ---- run 1: the mount goes long 1 @ 100, the mark crashes to 40 on a CLOSED BAR, its budget
    // (max_loss 50) latches and flattens.
    let done = Arc::<AtomicBool>::default();
    let mut budgets = std::collections::HashMap::new();
    budgets.insert(
        mount_id.clone(),
        vike_execution::MountBudget {
            max_loss: Some(50.0),
            max_notional: None,
            loss_action: vike_model::MountLossAction::Flatten,
            max_leverage: None,
        },
    );
    let config = CoreConfig { mount_budgets: budgets, ..core_config(&dir, &done) };
    let handle = spawn_core(sim_engine(), config);
    handle.tick_sender().quote(sim_quote(100.0, 1)).unwrap();
    let snap = wait_for(&handle, "the mount's entry to register", |s| !s.orders.is_empty());
    let entry = snap.orders[0].order_client_id.clone();
    handle.event_sender().blocking_send(fill_for(&entry, 1.0, 100.0)).unwrap();
    wait_for(&handle, "the mount to book its entry", |s| {
        mount_rows(s).first().is_some_and(|m| m.position != 0.0)
    });
    // Crash the price on EVERY rung the resolver walks — the quote (bid) rung outranks bar_close,
    // so a bar close alone would still be valued at the stale 100 bid and never breach.
    handle.tick_sender().quote(sim_quote(40.0, 5)).unwrap();
    handle.bar_sender().close(bar_at(60_000, 40.0)).unwrap();
    let snap = wait_for(&handle, "the budget latch to flatten", |s| {
        s.orders.iter().any(|o| o.order_type == vike_model::OrderType::Market)
    });
    let flatten = snap
        .orders
        .iter()
        .find(|o| o.order_type == vike_model::OrderType::Market)
        .expect("the latch submitted a reduce-only MARKET")
        .order_client_id
        .clone();
    assert_ne!(flatten, entry);
    handle.shutdown_and_join();

    // ---- the journal ALONE now credits that flatten to its mount (v14's whole point).
    let state = restore_from_journal(&dir).unwrap().expect("run 1 wrote an exit Snapshot");
    assert!(
        state.order_client_id_mounts.contains(&(flatten.clone(), mount_id.clone())),
        "the latch flatten must restore ATTRIBUTED to its mount: {:?}",
        state.order_client_id_mounts
    );

    // ---- run 2: restore (no budget — nothing must re-latch) and fill the flatten at 40.
    let done2 = Arc::<AtomicBool>::default();
    done2.store(true, Ordering::Relaxed);
    let config2 = CoreConfig {
        seed_cash: state.engines[0].equity_seed,
        order_client_id_session: Some((
            state.order_client_id_session.clone(),
            state.order_client_id_sequence,
        )),
        mount_attribution: state.mount_attribution.clone(),
        journal_orders: journal_orders_of(&state.order_client_id_mounts),
        ..core_config(&dir, &done2)
    };
    let engine2 = Engine::from_snapshot(&state.engines[0], RecordingClient::default());
    let handle = spawn_core(engine2, config2);
    handle
        .event_sender()
        .blocking_send(Event::Fill(FillEvent {
            trade_id: "t-flatten".into(),
            order_client_id: flatten,
            venue: VENUE.into(),
            symbol: SYMBOL.into(),
            side: -1, // sell to close the long
            last_qty: 1.0,
            last_price: 40.0,
            commission: 0.0,
            commission_asset: String::new().into(),
            liquidity_side: "taker".into(),
            timestamp: 9,
            mark_price: None,
            position_side: vike_model::events::PositionSide::Both,
        }))
        .unwrap();
    let snap = wait_for(&handle, "the restored mount to realize the flatten", |s| {
        mount_rows(s).first().is_some_and(|m| m.realized_pnl != 0.0)
    });
    let rows = mount_rows(&snap);
    assert_eq!(rows.len(), 1, "one mount row: {:?}", snap.mounts);
    assert!(
        (rows[0].realized_pnl + 60.0).abs() < 1e-9,
        "the -60 belongs to the MOUNT whose budget breach released the flatten, got {}",
        rows[0].realized_pnl
    );
    let residual = snap.mounts.last().expect("the residual row is published");
    assert_eq!(residual.kind, MountViewKind::Residual);
    assert!(
        residual.realized_pnl.abs() < 1e-9,
        "the residual row must absorb NOTHING — pre-fix it held the whole -60, got {}",
        residual.realized_pnl
    );
    handle.shutdown_and_join();
    let _ = std::fs::remove_dir_all(&dir);
}

/// The additive-bump compat gate: a pre-v13 journal — whose Snapshots carry NO `mount_attribution` key and
/// whose header stamps 12 — still restores, with EMPTY ledgers and everything else intact.
/// `MIN_READABLE_VERSION` stays where it is precisely because this holds.
#[test]
fn journal_v12_still_readable() {
    let dir = unique_dir("v12-compat");
    let done = Arc::<AtomicBool>::default();
    let handle = spawn_core(sim_engine(), core_config(&dir, &done));
    handle.tick_sender().quote(sim_quote(100.0, 1)).unwrap();
    let snap = wait_for(&handle, "the mount's order to register", |s| !s.orders.is_empty());
    let order_client_id = snap.orders[0].order_client_id.clone();
    handle.shutdown_and_join();

    // Byte-surgery every segment into the v12 shape: walk the [len][crc][payload] frames, strip
    // `mount_attribution` from each Snapshot payload, re-frame, stamp the header at the floor (format 31
    // refuses a v12 header itself; the stripped key is what a pre-v13 Snapshot lacked).
    const HEADER: usize = 16; // magic u32 | version u32 | first_sequence u64 (stable format)
    let mut stripped = 0;
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|s| s.to_str()) != Some("vjl") {
            continue;
        }
        let bytes = std::fs::read(&path).unwrap();
        let mut out = bytes[..HEADER].to_vec();
        out[4..8].copy_from_slice(&34u32.to_le_bytes());
        let mut current = HEADER;
        while current + 8 <= bytes.len() {
            let len = u32::from_le_bytes(bytes[current..current + 4].try_into().unwrap()) as usize;
            if len == 0 || current + 8 + len > bytes.len() {
                break;
            }
            let payload = &bytes[current + 8..current + 8 + len];
            let mut v = vike_journal::payload_as_value(payload);
            if let Some(snap) = v.get_mut("Snapshot").and_then(|s| s.as_object_mut())
                && snap.remove("mount_attribution").is_some()
            {
                stripped += 1;
            }
            let doctored = vike_journal::payload_of_value(&v);
            out.extend_from_slice(&(doctored.len() as u32).to_le_bytes());
            out.extend_from_slice(&fnv1a32(&doctored).to_le_bytes());
            out.extend_from_slice(&doctored);
            current += 8 + len;
        }
        out.resize(bytes.len().max(out.len()), 0);
        std::fs::write(&path, out).unwrap();
    }
    assert!(
        stripped > 0,
        "the run really did write a Snapshot carrying mount_attribution to doctor"
    );

    let state = restore_from_journal(&dir).unwrap().expect("a v12 journal still restores");
    assert!(
        state.mount_attribution.is_empty(),
        "a pre-v13 Snapshot carries no ledgers: restored EMPTY"
    );
    assert_eq!(state.engines.len(), 1, "the rest of the restore is untouched");
    assert_eq!(state.order_client_id_session, "cafef00d");
    // ...and the order client id -> mount fold is version-independent (it reads StrategySubmit/MintedSubmit,
    // records that exist since v3/v4), so it still resolves the resting order's origin.
    assert_eq!(
        state.order_client_id_mounts,
        vec![(order_client_id, mount_id_with(None, VENUE, SYMBOL, INTERVAL))]
    );
    let _ = std::fs::remove_dir_all(&dir);
}
