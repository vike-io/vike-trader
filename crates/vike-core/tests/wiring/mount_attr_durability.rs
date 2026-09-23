//! Per-mount ATTRIBUTION DURABILITY across a restart (multi-mount, gap D) — the end-to-end gates
//! for the two values that now survive the journal:
//!
//! - the per-mount ledgers, captured on every `Snap` (`Snap.mount_attr`, journal v13) and re-seeded
//!   through `CoreConfig::mount_attr`, and
//! - the coid -> mount ORIGIN map, rebuilt from the journal's EXISTING `StrategySubmit`/
//!   `MintedSubmit` provenance (no new record type) and re-seeded through `CoreConfig::coid_mounts`,
//!   so an order still RESTING across the restart books its eventual fill into the mount that placed
//!   it instead of vanishing from every ledger.
//!
//! ...plus the third value that had to reach the journal for the map to be COMPLETE: the per-mount
//! budget latch's FLATTEN now stamps its owning `mount_id` on its `MarginCallLiquidate` record
//! (journal v14), so a restore credits that order — and the realized loss its fill books — to the
//! mount whose breach released it instead of the residual row.
//!
//! Plus the compat gate: a pre-v13 journal (no `mount_attr` on its Snap) still restores, with empty
//! ledgers — the additive-only rule `MIN_READABLE_VERSION` encodes.

use crate::scratch::Scratch;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::time::Duration;

use vike_core::journal::JournalFileConfig;
use vike_core::replay::restore_from_journal;
use vike_core::strategy_state::mount_id_of;
use vike_core::{
    CoreConfig, CoreHandle, CoreSnapshot, JournalConfig, LiveBroker, MountRowKind, StrategyMount,
    spawn_core,
};
use vike_exec::testing::RecordingClient;
use vike_exec::{Account, BalanceMode, ExecutionEngine, QuoteUpdate, RiskGate, RiskLimits};
use vike_model::events::{Event, FillEvent, TradeId};
use vike_model::{Clock, QuoteTick, Strategy};

const VENUE: &str = "sim";
const SYMBOL: &str = "BTCUSDT";
const INTERVAL: &str = "1m";

/// A scratch journal directory, removed when the returned guard drops. The journal's own `open`
/// calls `create_dir_all`, so the path is RESERVED rather than created. Hold the guard for the
/// whole test — see `crates/vike-core/src/scratch.rs` for the leak this closed.
fn unique_dir(tag: &str) -> Scratch {
    Scratch::reserved(&format!("mattr-{tag}"))
}

/// Submits ONE tagged limit on its first quote tick, then never trades again — so the run has
/// exactly one mount-minted, still-RESTING order to attribute across the restart.
struct SubmitOnce {
    done: Arc<AtomicBool>,
}
impl Strategy<LiveBroker> for SubmitOnce {
    fn on_quote_tick(&mut self, broker: &mut LiveBroker, _q: &QuoteTick) {
        if !self.done.swap(true, Ordering::Relaxed) {
            broker.submit_limit_tagged("bid", 1, 1.0, 100.0);
        }
    }
}

fn mount(done: &Arc<AtomicBool>) -> StrategyMount {
    StrategyMount {
        account: None,
        symbols: Vec::new(),
        controller_id: None,
        underlying_symbol: None,
        venue: VENUE.into(),
        symbol: SYMBOL.into(),
        interval: INTERVAL.into(),
        strategy: Box::new(SubmitOnce { done: Arc::clone(done) }),
    }
}

fn engine() -> ExecutionEngine<RecordingClient> {
    ExecutionEngine::new(
        Account::new(1.0, VENUE, None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        VENUE,
        SYMBOL,
    )
}

/// A journaling config with `snapshot_every = u64::MAX`: no cadence Snap fires, so the ONE Snap in
/// the journal is the clean-shutdown exit Snap — the restore base, carrying the final ledgers.
fn core_config(dir: &Path, done: &Arc<AtomicBool>) -> CoreConfig {
    let t = Arc::new(AtomicI64::new(0));
    let clock: Box<dyn Clock + Send> = Box::new(move || t.fetch_add(1, Ordering::Relaxed));
    CoreConfig {
        seed_cash: 10_000.0,
        clock,
        coid_session: Some(("cafef00d".into(), 0)),
        strategy: Some(mount(done)),
        journal: Some(JournalConfig {
            dir: dir.to_path_buf(),
            file: JournalFileConfig { segment_bytes: 1024 * 1024, flush_every: 1 },
            snapshot_every: u64::MAX,
        }),
        ..CoreConfig::default()
    }
}

fn quote(px: f64, ts: i64) -> QuoteUpdate {
    QuoteUpdate {
        venue: VENUE.into(),
        symbol: SYMBOL.into(),
        quote: QuoteTick {
            ts,
            local_ts: ts,
            bid: px,
            ask: px,
            bid_size: 1.0,
            ask_size: 1.0,
            symbol: SYMBOL.into(),
        },
    }
}

fn fill_for(coid: &str, qty: f64, px: f64) -> Event {
    Event::Fill(FillEvent {
        // minted by this helper — same `t-<coid>` bytes as the `format!` it replaced
        trade_id: TradeId::prefixed("t-", coid),
        client_order_id: coid.to_string(),
        venue: VENUE.into(),
        symbol: SYMBOL.into(),
        side: 1,
        last_qty: qty,
        last_px: px,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".into(),
        ts: 5,
        mark_price: None,
        position_side: "BOTH".into(),
    })
}

/// Poll the published snapshot until `f` holds (the core publishes on its own coalesced cadence, so
/// a live-core test must wait for a publish rather than assume one).
fn wait_for(
    handle: &CoreHandle,
    what: &str,
    f: impl Fn(&CoreSnapshot) -> bool,
) -> Arc<CoreSnapshot> {
    for _ in 0..400 {
        let s = handle.snapshot();
        if f(&s) {
            return s;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("timed out waiting for: {what}");
}

/// The mount rows of a snapshot (the trailing residual row filtered out).
fn mount_rows(snap: &CoreSnapshot) -> Vec<&vike_core::MountView> {
    snap.mounts.iter().filter(|m| m.kind == MountRowKind::Mount).collect()
}

/// GAP D, the payoff: a mount-minted order still RESTING at shutdown keeps its origin across the
/// restart, so the fill that lands AFTER the restart books into that mount's ledger.
///
/// Pre-fix, `coid_mount` was pure runtime state — a restarted core had an empty map, so the fill
/// folded into `Account.realized_pnl`/position and into NO mount ledger at all.
#[test]
fn coid_mount_rebuilt_from_journal_replay() {
    let dir = unique_dir("coid-mount");

    // ---- run 1: the mount submits one order, which is still resting at a clean shutdown.
    let done = Arc::<AtomicBool>::default();
    let handle = spawn_core(engine(), core_config(&dir, &done));
    handle.tick_sender().quote(quote(100.0, 1)).unwrap();
    let snap = wait_for(&handle, "the mount's order to register", |s| !s.orders.is_empty());
    let coid = snap.orders[0].client_order_id.clone();
    handle.shutdown_and_join();

    // ---- the journal alone reconstructs which mount minted that coid.
    let state = restore_from_journal(&dir).unwrap().expect("run 1 wrote an exit Snap");
    assert_eq!(
        state.coid_mounts,
        vec![(coid.clone(), mount_id_of(VENUE, SYMBOL, INTERVAL))],
        "the StrategySubmit/MintedSubmit pair names the minting mount"
    );

    // ---- run 2: restore, then fill the PRE-RESTART order.
    let done2 = Arc::<AtomicBool>::default();
    done2.store(true, Ordering::Relaxed); // never re-submit: only the restored order matters
    let cfg = CoreConfig {
        seed_cash: state.engines[0].equity_seed,
        coid_session: Some((state.coid_session.clone(), state.coid_seq)),
        mount_attr: state.mount_attr.clone(),
        coid_mounts: state.coid_mounts.clone(),
        ..core_config(&dir, &done2)
    };
    let engine2 = ExecutionEngine::from_snapshot(&state.engines[0], RecordingClient::default());
    let handle = spawn_core(engine2, cfg);
    handle.event_sender().blocking_send(fill_for(&coid, 1.0, 100.0)).unwrap();
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

/// GAP D, the ledger half: a mount's folded attribution rides the `Snap` (journal v13) and comes
/// back through `CoreConfig::mount_attr`, so a restart resumes the mount's position/realized PnL
/// instead of silently starting it at zero while `Account` keeps the same PnL.
#[test]
fn mount_attr_survives_snapshot_roundtrip() {
    let dir = unique_dir("snap-roundtrip");

    // ---- run 1: submit, fill, fold the ledger, clean shutdown (the exit Snap captures it).
    let done = Arc::<AtomicBool>::default();
    let handle = spawn_core(engine(), core_config(&dir, &done));
    handle.tick_sender().quote(quote(100.0, 1)).unwrap();
    let snap = wait_for(&handle, "the mount's order to register", |s| !s.orders.is_empty());
    let coid = snap.orders[0].client_order_id.clone();
    handle.event_sender().blocking_send(fill_for(&coid, 2.0, 100.0)).unwrap();
    let snap = wait_for(&handle, "the mount to book its fill", |s| {
        mount_rows(s).first().is_some_and(|m| m.position != 0.0)
    });
    assert_eq!(mount_rows(&snap)[0].position.to_bits(), 2.0_f64.to_bits());
    handle.shutdown_and_join();

    // ---- the exit Snap carries the ledger, keyed by mount id.
    let state = restore_from_journal(&dir).unwrap().expect("run 1 wrote an exit Snap");
    assert_eq!(state.mount_attr.len(), 1, "one row per mount slot: {:?}", state.mount_attr);
    let row = &state.mount_attr[0];
    assert_eq!(row.mount_id, mount_id_of(VENUE, SYMBOL, INTERVAL));
    assert_eq!(row.size.to_bits(), 2.0_f64.to_bits());
    assert_eq!(row.avg_px.to_bits(), 100.0_f64.to_bits());

    // ---- run 2: the restored core resumes that ledger (a bare quote is enough to force a publish).
    let done2 = Arc::<AtomicBool>::default();
    done2.store(true, Ordering::Relaxed);
    let cfg = CoreConfig {
        seed_cash: state.engines[0].equity_seed,
        coid_session: Some((state.coid_session.clone(), state.coid_seq)),
        mount_attr: state.mount_attr.clone(),
        coid_mounts: state.coid_mounts.clone(),
        ..core_config(&dir, &done2)
    };
    let engine2 = ExecutionEngine::from_snapshot(&state.engines[0], RecordingClient::default());
    let handle = spawn_core(engine2, cfg);
    handle.tick_sender().quote(quote(100.0, 9)).unwrap();
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

fn bar_at(ts: i64, px: f64) -> vike_exec::BarUpdate {
    vike_exec::BarUpdate {
        venue: VENUE.into(),
        symbol: SYMBOL.into(),
        interval: INTERVAL.into(),
        bar: vike_model::Bar {
            ts,
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
    }
}

/// **THE v14 PAYOFF, END TO END.** The per-mount budget latch's FLATTEN keeps its mount across a
/// restart: drive a real latch + flatten through a live core, restore from the journal ALONE, fill
/// the flatten, and the realized loss lands in THAT MOUNT's ledger with the residual row absorbing
/// nothing.
///
/// Pre-fix the flatten was journaled as a bare `MarginCallLiquidate` — indistinguishable on disk
/// from the ACCOUNT-wide margin-call sweep's record — so `fold_coid_mounts` closed ownership and
/// left the following `MintedSubmit` unattributed. The restored core then booked the flatten's fill
/// into the RESIDUAL row, and the mount's ledger came back under-reporting precisely the realized
/// loss its budget had just latched on.
#[test]
fn a_budget_latch_flatten_keeps_its_mount_across_a_restart() {
    let dir = unique_dir("latch-flatten");
    let mount_id = mount_id_of(VENUE, SYMBOL, INTERVAL);

    // ---- run 1: the mount goes long 1 @ 100, the mark crashes to 40 on a CLOSED BAR, its budget
    // (max_loss 50) latches and flattens.
    let done = Arc::<AtomicBool>::default();
    let mut budgets = std::collections::HashMap::new();
    budgets.insert(
        mount_id.clone(),
        vike_core::MountBudget {
            max_loss: Some(50.0),
            max_notional: None,
            flatten_on_breach: true,
        },
    );
    let cfg = CoreConfig { mount_budgets: budgets, ..core_config(&dir, &done) };
    let handle = spawn_core(engine(), cfg);
    handle.tick_sender().quote(quote(100.0, 1)).unwrap();
    let snap = wait_for(&handle, "the mount's entry to register", |s| !s.orders.is_empty());
    let entry = snap.orders[0].client_order_id.clone();
    handle.event_sender().blocking_send(fill_for(&entry, 1.0, 100.0)).unwrap();
    wait_for(&handle, "the mount to book its entry", |s| {
        mount_rows(s).first().is_some_and(|m| m.position != 0.0)
    });
    // Crash the price on EVERY rung the resolver walks — the quote (bid) rung outranks bar_close,
    // so a bar close alone would still be valued at the stale 100 bid and never breach.
    handle.tick_sender().quote(quote(40.0, 5)).unwrap();
    handle.bar_sender().close(bar_at(60_000, 40.0)).unwrap();
    let snap = wait_for(&handle, "the budget latch to flatten", |s| {
        s.orders.iter().any(|o| o.order_type == "market")
    });
    let flatten = snap
        .orders
        .iter()
        .find(|o| o.order_type == "market")
        .expect("the latch submitted a reduce-only MARKET")
        .client_order_id
        .clone();
    assert_ne!(flatten, entry);
    handle.shutdown_and_join();

    // ---- the journal ALONE now credits that flatten to its mount (v14's whole point).
    let state = restore_from_journal(&dir).unwrap().expect("run 1 wrote an exit Snap");
    assert!(
        state.coid_mounts.contains(&(flatten.clone(), mount_id.clone())),
        "the latch flatten must restore ATTRIBUTED to its mount: {:?}",
        state.coid_mounts
    );

    // ---- run 2: restore (no budget — nothing must re-latch) and fill the flatten at 40.
    let done2 = Arc::<AtomicBool>::default();
    done2.store(true, Ordering::Relaxed);
    let cfg2 = CoreConfig {
        seed_cash: state.engines[0].equity_seed,
        coid_session: Some((state.coid_session.clone(), state.coid_seq)),
        mount_attr: state.mount_attr.clone(),
        coid_mounts: state.coid_mounts.clone(),
        ..core_config(&dir, &done2)
    };
    let engine2 = ExecutionEngine::from_snapshot(&state.engines[0], RecordingClient::default());
    let handle = spawn_core(engine2, cfg2);
    handle
        .event_sender()
        .blocking_send(Event::Fill(FillEvent {
            trade_id: "t-flatten".into(),
            client_order_id: flatten,
            venue: VENUE.into(),
            symbol: SYMBOL.into(),
            side: -1, // sell to close the long
            last_qty: 1.0,
            last_px: 40.0,
            commission: 0.0,
            commission_asset: String::new().into(),
            liquidity_side: "taker".into(),
            ts: 9,
            mark_price: None,
            position_side: "BOTH".into(),
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
    assert_eq!(residual.kind, MountRowKind::Residual);
    assert!(
        residual.realized_pnl.abs() < 1e-9,
        "the residual row must absorb NOTHING — pre-fix it held the whole -60, got {}",
        residual.realized_pnl
    );
    handle.shutdown_and_join();
    let _ = std::fs::remove_dir_all(&dir);
}

/// FNV-1a32 twin of the journal's frame checksum (the private `journal::fnv1a32`) — needed to
/// re-frame the doctored payloads below.
fn fnv1a32(data: &[u8]) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for &b in data {
        h ^= b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

/// The additive-bump compat gate: a pre-v13 journal — whose Snaps carry NO `mount_attr` key and
/// whose header stamps 12 — still restores, with EMPTY ledgers and everything else intact.
/// `MIN_READABLE_VERSION` stays where it is precisely because this holds.
#[test]
fn journal_v12_still_readable() {
    let dir = unique_dir("v12-compat");
    let done = Arc::<AtomicBool>::default();
    let handle = spawn_core(engine(), core_config(&dir, &done));
    handle.tick_sender().quote(quote(100.0, 1)).unwrap();
    let snap = wait_for(&handle, "the mount's order to register", |s| !s.orders.is_empty());
    let coid = snap.orders[0].client_order_id.clone();
    handle.shutdown_and_join();

    // Byte-surgery every segment into the v12 shape: walk the [len][crc][payload] frames, strip
    // `mount_attr` from each Snap payload, re-frame, restamp the header to 12.
    const HEADER: usize = 16; // magic u32 | version u32 | first_seq u64 (stable format)
    let mut stripped = 0;
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|s| s.to_str()) != Some("vjl") {
            continue;
        }
        let bytes = std::fs::read(&path).unwrap();
        let mut out = bytes[..HEADER].to_vec();
        out[4..8].copy_from_slice(&12u32.to_le_bytes());
        let mut cur = HEADER;
        while cur + 8 <= bytes.len() {
            let len = u32::from_le_bytes(bytes[cur..cur + 4].try_into().unwrap()) as usize;
            if len == 0 || cur + 8 + len > bytes.len() {
                break;
            }
            let payload = &bytes[cur + 8..cur + 8 + len];
            let mut v: serde_json::Value = serde_json::from_slice(payload).unwrap();
            if let Some(snap) = v.get_mut("Snap").and_then(|s| s.as_object_mut())
                && snap.remove("mount_attr").is_some()
            {
                stripped += 1;
            }
            let doctored = serde_json::to_vec(&v).unwrap();
            out.extend_from_slice(&(doctored.len() as u32).to_le_bytes());
            out.extend_from_slice(&fnv1a32(&doctored).to_le_bytes());
            out.extend_from_slice(&doctored);
            cur += 8 + len;
        }
        out.resize(bytes.len().max(out.len()), 0);
        std::fs::write(&path, out).unwrap();
    }
    assert!(stripped > 0, "the run really did write a Snap carrying mount_attr to doctor");

    let state = restore_from_journal(&dir).unwrap().expect("a v12 journal still restores");
    assert!(state.mount_attr.is_empty(), "a pre-v13 Snap carries no ledgers: restored EMPTY");
    assert_eq!(state.engines.len(), 1, "the rest of the restore is untouched");
    assert_eq!(state.coid_session, "cafef00d");
    // ...and the coid -> mount fold is version-independent (it reads StrategySubmit/MintedSubmit,
    // records that exist since v3/v4), so it still resolves the resting order's origin.
    assert_eq!(state.coid_mounts, vec![(coid, mount_id_of(VENUE, SYMBOL, INTERVAL))]);
    let _ = std::fs::remove_dir_all(&dir);
}
