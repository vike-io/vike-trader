//! What the `Ingest::GatewayEvent` arm costs the fold, measured PAIRED against the `Ingest::Event`
//! arm it shares a helper with: PRINT-ONLY, `#[ignore]`d, no ceiling.
//!
//! # Why this file exists
//!
//! The three gated variants of `tests/runtime_latency.rs` (`baseline`, `journal`, `journal-snap`)
//! send `Ingest::Event` only, so none of them executes the new arm: a green gate says nothing about
//! it, and a regression there would ship unmeasured. Adding a variant to the gated harness is not
//! the answer (a new label there is "not a budget in waiting", and an edit to that file fires the
//! latency job by itself), so the paired measurement lives here, beside the gate, in a binary CI
//! never runs.
//!
//! # What it measures
//!
//! The same request/response hop `runtime_latency.rs` times: one fill in flight at a time, the
//! sender spinning until the core has DEQUEUED it, the hop being send -> dequeue. Because the core
//! is single-threaded and the next send starts at the previous dequeue, message `i`'s fold lands
//! inside message `i + 1`'s hop, so the hop distribution carries the fold cost of the previous
//! message. The fill is symbol-filtered (`OTHER`), exactly as the harness's, so the engine's own
//! work is nothing and what remains is the dispatch itself:
//!
//! * `event`: `Ingest::Event(Fill)`, the arm the gate covers;
//! * `gateway`: `Ingest::GatewayEvent(Fill)` with a rising cursor on one account: the extra
//!   discriminant, the box moved out and freed on the fold thread, and the cursor update.
//!
//! * `exec-link`: `Ingest::ExecLink` alternating down/up on the one account (the trade-link
//!   transition, W3b): a RARE control message, measured so its arm is a number rather than an
//!   assumption. It is not a fill, so its hop is stamped in the update's own `at_ts_ms` and read
//!   at dequeue; it is NOT journaled, so the journal-ON label shows the same arm beside a live
//!   journal that records nothing for it.
//!
//! each with the journal OFF and with it ON at the production config (`JournalConfig::at`: the
//! `Ingest` is serialized into the record, the cursor included, and a `Snap` falls every 1024
//! records and carries the cursors). Six labels, `REPS` windows each, the order inside a rep
//! ROTATED so a slow drift of the box does not belong to one label. The `event` labels also
//! answer the question the gated variants answer for the whole change group: the same fill through
//! the same arm, now with two more arms (`GatewayEvent`, `ExecLink`) in the `dispatch` matches.
//!
//! # How to read it
//!
//! One line per window on stderr: `GATEWAY-FOLD label=<label> rep=<n> n=<hops> p50_ns= p99_ns=
//! p999_ns= max_ns= over_100us=`. Compare the `gateway` label with the `event` label of the SAME
//! rep (the paired difference), then take the median over reps. The discipline is
//! `crates/vike-core/CLAUDE.md`'s "Measuring a hot-path change honestly": a the latency box lane, cores 28-31
//! through `latency.slice`, `chrt -f 50`, `--test-threads=1`, `--nocapture`, a byte-identical
//! control and a neutral-edit control, and read the p50 (the p99 differences are inside the
//! controls' own noise). Each window sleeps [`GAP`] first, for the reason
//! `tests/common/rt_gap.rs` documents (the kernel's RT throttle); this file copies the idea, not
//! the guard, because nothing here asserts on a window.
//!
//! ```sh
//! sudo systemd-run --scope --slice=latency.slice -p AllowedCPUs=28-31 -- \
//!   chrt -f 50 target/release/deps/gateway_event_fold-<hash> --ignored --test-threads=1 --nocapture
//! ```

use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use vike_core::{CoreConfig, JournalConfig, spawn_core};
use vike_exec::testing::RecordingClient;
use vike_exec::{
    Account, BalanceMode, ExecLinkUpdate, ExecutionEngine, GatewayCursor, GatewayEvent, Ingest,
    RiskGate,
};
use vike_model::RiskLimits;
use vike_model::events::{Event, FillEvent, PositionSide};

// The owned `/tmp` scratch guard every journal directory a test allocates goes through (see
// `tests/journal.rs` for the leak it closed).
#[path = "../src/scratch.rs"]
mod scratch;

/// Hops per window.
const N: usize = 20_000;
/// Windows per label.
const REPS: usize = 8;
/// The RT-throttle gap slept before each window (`tests/common/rt_gap.rs`'s `RT_GAP`).
const GAP: Duration = Duration::from_millis(100);

/// A symbol-filtered fill (`OTHER`: the engine drops it on the symbol filter, so the dispatch is
/// all that is measured), stamped `ts` for the hop arithmetic.
fn fill(ts: i64) -> FillEvent {
    FillEvent {
        trade_id: "t".into(),
        client_order_id: String::new(),
        venue: "sim".into(),
        symbol: "OTHER".into(),
        side: 1,
        last_qty: 1.0,
        last_px: 100.0,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: String::new().into(),
        ts,
        mark_price: None,
        position_side: PositionSide::Both,
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Door {
    Event,
    Gateway,
    ExecLink,
}

impl Door {
    fn name(self) -> &'static str {
        match self {
            Door::Event => "event",
            Door::Gateway => "gateway",
            Door::ExecLink => "exec-link",
        }
    }
}

struct Stats {
    p50: u64,
    p99: u64,
    p999: u64,
    max: u64,
    over_100us: usize,
}

fn stats(mut hops: Vec<u64>) -> Stats {
    assert!(!hops.is_empty(), "no hop was recorded: the dequeue hook never fired");
    let over_100us = hops.iter().filter(|&&v| v > 100_000).count();
    hops.sort_unstable();
    let pct = |p: f64| hops[((hops.len() as f64 * p) as usize).min(hops.len() - 1)];
    Stats {
        p50: pct(0.50),
        p99: pct(0.99),
        p999: pct(0.999),
        max: hops[hops.len() - 1],
        over_100us,
    }
}

/// One window: `N` paced hops through `door`, the journal on or off.
fn window(door: Door, journal: bool, rep: usize) -> Stats {
    std::thread::sleep(GAP);
    let tag = format!("gwfold-{}-{journal}-{rep}", door.name());
    // Held for the whole window: the guard removes the directory when it drops.
    let dir = journal.then(|| scratch::Scratch::reserved(&tag));
    let base = Instant::now();
    let processed = Arc::new(AtomicU64::new(0));
    let hops = Arc::new(Mutex::new(Vec::<u64>::with_capacity(N)));
    let engine = ExecutionEngine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "sim",
        "BTCUSDT",
    );
    let (processed_hook, hops_hook) = (Arc::clone(&processed), Arc::clone(&hops));
    let cfg = CoreConfig {
        on_dequeued: Some(Box::new(move |msg: &Ingest| {
            let now_ns = base.elapsed().as_nanos() as u64;
            // `venue_event`, so the ONE hook times both fill doors; an `ExecLink` carries its send
            // stamp in its own `at_ts_ms`.
            if let Some(Event::Fill(f)) = msg.venue_event() {
                hops_hook.lock().unwrap().push(now_ns.saturating_sub(f.ts as u64));
            } else if let Ingest::ExecLink(u) = msg {
                hops_hook.lock().unwrap().push(now_ns.saturating_sub(u.at_ts_ms as u64));
            }
            processed_hook.fetch_add(1, Ordering::Release);
        })),
        journal: dir.as_ref().map(|d| JournalConfig::at(d.to_path_buf())),
        ..CoreConfig::default()
    };
    let handle = spawn_core(engine, cfg);
    let sender = handle.event_sender();
    for i in 0..N as u64 {
        let ts = base.elapsed().as_nanos() as i64;
        match door {
            Door::Event => sender.blocking_send(Event::Fill(fill(ts))).unwrap(),
            Door::Gateway => sender
                .blocking_send_gateway(GatewayEvent {
                    event: Event::Fill(fill(ts)),
                    route_key: "sim".into(),
                    cursor: GatewayCursor { gw_epoch: 1, seq: i + 1, ts_ms: ts / 1_000_000 },
                })
                .unwrap(),
            Door::ExecLink => sender
                .exec_link(ExecLinkUpdate {
                    route_key: "sim".into(),
                    venue: "sim".into(),
                    up: i % 2 == 1,
                    at_ts_ms: ts,
                })
                .unwrap(),
        }
        while processed.load(Ordering::Acquire) <= i {
            std::hint::spin_loop();
        }
    }
    handle.shutdown_and_join();
    drop(dir);
    stats(Arc::try_unwrap(hops).unwrap().into_inner().unwrap())
}

#[test]
#[ignore = "release-only latency measurement; print-only (see module doc)"]
fn gateway_event_fold_paired_with_event_fold() {
    let labels = [
        (Door::Event, false),
        (Door::Gateway, false),
        (Door::ExecLink, false),
        (Door::Event, true),
        (Door::Gateway, true),
        (Door::ExecLink, true),
    ];
    let mut err = std::io::stderr();
    for rep in 0..REPS {
        for k in 0..labels.len() {
            // Rotate the order each rep: no label always runs first on a warm or a cold box.
            let (door, journal) = labels[(k + rep) % labels.len()];
            let s = window(door, journal, rep);
            let label = format!("{}{}", door.name(), if journal { "-journal-snap" } else { "" });
            let _ = writeln!(
                err,
                "GATEWAY-FOLD label={label} rep={rep} n={N} p50_ns={} p99_ns={} p999_ns={} \
                 max_ns={} over_100us={}",
                s.p50, s.p99, s.p999, s.max, s.over_100us
            );
        }
    }
}
