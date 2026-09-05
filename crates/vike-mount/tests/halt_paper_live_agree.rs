//! PAPER and LIVE must admit and refuse the SAME things under an engaged HALT sentinel.
//!
//! # Why this test is in vike-mount, of all crates
//!
//! Because it is the first crate that can SEE both clients. `vike_paper::PaperExecutionClient` and
//! `vike_bridge_core::exec_actor::ExecActor` deliberately cannot see each other — vike-paper is the
//! backtest's own fill primitive and may not grow the ureq/tungstenite/rustls transport dependency
//! vike-bridge-core owns (which is the whole reason the shared predicate had to move down to
//! `crates/vike-exec/src/halt.rs`). So neither crate's own test suite can compare them, and
//! "paper and live agree" was a claim with nowhere to live. vike-mount depends on both, and mounts
//! both, which makes it the honest home.
//!
//! # The defect this exists for
//!
//! `ExecActor::submit` and `ExecActor::modify` both consult the sentinel; the paper client gained a
//! `submit` arm and NOT a `modify` arm. A halted paper mount therefore refused new risk while still
//! admitting an amend that ADDS it — paper and live disagreeing about what a halt lets out, in the
//! crate whose entire job is that `backtest == paper == live` is checkable. The pre-existing pair had
//! already drifted the same way once (both blocked `submit`, only hyperliquid blocked `submit_batch`
//! explicitly), which is why the DECISION is one shared function; but a shared predicate cannot make
//! a client CALL it, and that is what this table checks.
//!
//! # The shape
//!
//! One [`CASES`] table of (request shape × verb), folded through BOTH clients with the same engaged
//! sentinel, asserting they reach the same verdict AND that the verdict is the expected one. Two
//! asserts, not one: agreement alone is satisfied by both being broken the same way, and this repo
//! has shipped a "gate" whose two sides agreed vacuously before.
//!
//! Each client gets its own sentinel path inside a temp directory the case OWNS — a bound
//! `tempfile::TempDir` ([`unique_sentinel`]), so nothing here touches the process-wide resolution
//! (`vike_bridge_core::halt::halt_path_from_env`), races a sibling test, or survives the run.

use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::time::Duration;

use vike_bridge_core::exec_actor::{ExecActor, ExecCommand, cancel_batch_undeclared};
use vike_exec::{EventSender, ExecutionClient, Ingest, event_channel};
use vike_model::events::{Event, OrderCanceled, OrderModified, OrderSubmitted};
use vike_model::{FeeSchedule, OrderRequest};
use vike_paper::PaperExecutionClient;

const VENUE: &str = "binance";
const SYMBOL: &str = "BTCUSDT";

/// What a client did with one command, reduced to the only distinction a kill switch is about.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum Verdict {
    /// The command was let through — it reached the venue thread (live) or the resting book (paper).
    Admitted,
    /// The command was stopped at the halt boundary.
    Refused,
}

/// The verb under test. `Cancel` is here because "a halt never traps you" is the promise that makes
/// the whole switch usable, and a regression that started gating cancels would be caught by nothing
/// else in this table.
#[derive(Debug, Clone, Copy)]
enum Verb {
    Submit,
    Modify,
    Cancel,
}

struct Case {
    what: &'static str,
    verb: Verb,
    /// `reduce_only` on the request. Irrelevant to `Cancel`.
    reduce_only: bool,
    expect: Verdict,
    /// Why that verdict is the RIGHT one — the row's argument, not a restatement of `expect`.
    why: &'static str,
}

const CASES: &[Case] = &[
    Case {
        what: "opening submit",
        verb: Verb::Submit,
        reduce_only: false,
        expect: Verdict::Refused,
        why: "the whole point of the switch: a halt stops orders that OPEN or ADD risk",
    },
    Case {
        what: "reducing submit",
        verb: Verb::Submit,
        reduce_only: true,
        expect: Verdict::Admitted,
        why: "a halt must NEVER trap an operator in a position. `OrderIntent::Flatten` mints exactly \
              this shape, so `market-exit` has to work with the file engaged — on the paper mount \
              too, or the rehearsal teaches a lie about the real one",
    },
    Case {
        what: "modify of a resting order",
        verb: Verb::Modify,
        reduce_only: false,
        expect: Verdict::Refused,
        why: "THE REGRESSION THIS FILE EXISTS FOR. A modify can add size or chase price, and \
              \"does this reduce?\" cannot be inferred from the request — `reduce_only` describes \
              the ORIGINAL order and `new_qty` is neither reliably a delta nor reliably a \
              remainder. The exit path under a halt is CANCEL, never modify",
    },
    Case {
        what: "modify of an order that WAS reduce_only",
        verb: Verb::Modify,
        reduce_only: true,
        expect: Verdict::Refused,
        why: "no reducing exemption on modify, on EITHER client: the flag describes the order as \
              submitted, and an amend can raise the qty of a reduce_only order past the position it \
              was covering. Both clients must ignore the flag here, and both do — this row pins \
              that they ignore it the SAME way",
    },
    Case {
        what: "cancel of a resting order",
        verb: Verb::Cancel,
        reduce_only: false,
        expect: Verdict::Admitted,
        why: "cancel is never gated anywhere: pulling resting orders is how an operator reduces \
              exposure under a halt",
    },
];

// ── the two clients, each driven to a Verdict ────────────────────────────────────────────────

/// A sentinel path inside a temp directory this case OWNS, plus the guard that removes it.
///
/// ⚠ Returns the `TempDir` ALONGSIDE the path, and the caller must BIND it — dropping the guard
/// deletes the directory the sentinel lives in, out from under the client still consulting it.
///
/// This used to be `temp_dir().join(format!("vike-halt-agree-{pid}-{n}-{tag}"))` followed by a
/// `create_dir_all`, an idiom `crates/vike-ops/tests/temp_path_gate.rs` accepts and which was
/// still wrong twice over. MEASURED on the CI box, 2026-08-25:
///
/// * **17,696 leaked `vike-halt-agree-*` directories in `/tmp`**, still growing by ~84 a day.
///   Nothing ever deleted one — this file removed the sentinel FILE and left its parent — so
///   every CI run and every verification-lane run leaked permanently.
/// * **A PID is REUSED**, and the CI box runs these tests as TWO users (`the CI user` for CI, `the operator`
///   for the verification lanes). When a PID collides with a directory the other user created, the
///   `create_dir_all` SUCCEEDS (it already exists) and the `fs::write` that engages the sentinel
///   fails with PermissionDenied — a live, intermittent flake. The `AtomicU64` uniquified WITHIN a
///   process; it did nothing across users over time, and it leaked either way.
///
/// `tempfile::TempDir` fixes both halves at once: unique by construction, and self-deleting —
/// including on the panic/unwind path, where a leak is least likely to be noticed. The tag stays
/// in the directory PREFIX (`crates/vike-core/src/scratch.rs`'s `Scratch::root` spelling), so a
/// directory seen mid-run is still attributable to the client that owns it.
fn unique_sentinel(tag: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::Builder::new()
        .prefix(&format!("vike-halt-agree-{tag}-"))
        .tempdir()
        .expect("temp sentinel dir");
    let path = dir.path().join("HALT");
    (dir, path)
}

fn order(coid: &str, reduce_only: bool) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.into(),
        venue: VENUE.into(),
        symbol: SYMBOL.into(),
        side: 1,
        qty: 1.0,
        order_type: "limit".into(),
        price: Some(100.0),
        reduce_only,
        ts: 5,
        ..Default::default()
    }
}

/// A fake venue loop that emits a marker ONLY when a command actually reaches the venue thread, so
/// "refused at the boundary" is distinguishable from "delivered". The same double `ExecActor`'s own
/// `crates/vike-bridge-core/tests/exec_actor_halt.rs` uses.
fn fake_run(events: EventSender, rx: Receiver<ExecCommand>) {
    while let Ok(cmd) = rx.recv() {
        match cmd {
            ExecCommand::Submit(req) => {
                let _ = events.blocking_send(Event::OrderSubmitted(OrderSubmitted {
                    client_order_id: req.client_order_id.clone(),
                    ts: req.ts,
                }));
            }
            ExecCommand::Cancel { client_order_id: coid, .. } => {
                let _ = events.blocking_send(Event::OrderCanceled(OrderCanceled {
                    client_order_id: coid,
                    reason: String::new().into(),
                    ts: 0,
                }));
            }
            // Unreachable: this fake declares no bulk lane, so `ExecActor` fans a batch out into
            // the per-id `Cancel`s above. Shaped like the real venue loops' arm.
            ExecCommand::CancelBatch { client_order_ids, .. } => {
                cancel_batch_undeclared(&events, &client_order_ids)
            }
            ExecCommand::Modify { order, .. } => {
                let _ = events.blocking_send(Event::OrderModified(OrderModified {
                    client_order_id: order.client_order_id.clone(),
                    venue_order_id: None,
                    new_qty: None,
                    new_price: None,
                    ts: 0,
                }));
            }
            ExecCommand::Shutdown => break,
        }
    }
}

/// How long a REFUSED live case waits before concluding nothing is coming.
///
/// ⚠ This is paid in full on every refusal (a blocked `modify` produces no event at all, by
/// design), so it is the wall-clock cost of this file. It must still be long enough that "the CI box
/// was busy" cannot masquerade as "refused" — `fake_run` emits its marker synchronously the instant
/// a command lands, so the only thing being waited on is a thread wake-up.
const REFUSAL_WAIT: Duration = Duration::from_secs(3);

/// Drain whatever the actor produced within a bounded wait. A Verdict is decided by what ARRIVED.
fn drain_ingest(rx: &mut tokio::sync::mpsc::Receiver<Ingest>) -> Vec<Event> {
    let rt = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
    let mut out = Vec::new();
    rt.block_on(async {
        // One bounded wait for the FIRST event, then a non-blocking sweep for any others.
        if let Ok(Some(Ingest::Event(e))) = tokio::time::timeout(REFUSAL_WAIT, rx.recv()).await {
            out.push(e);
        }
        while let Ok(Ingest::Event(e)) = rx.try_recv() {
            out.push(e);
        }
    });
    out
}

/// LIVE: drive one case through a real `ExecActor` with the sentinel engaged.
fn live_verdict(case: &Case) -> Verdict {
    // `_dir` is the guard, and it is bound FIRST so it drops LAST — after `client`, which consults
    // the sentinel on every verb. Its `Drop` takes the whole directory, so there is no
    // `remove_file` at the end of this function any more (that one left the directory behind).
    let (_dir, sentinel) = unique_sentinel("live");
    std::fs::write(&sentinel, b"").expect("engage the sentinel");

    let (events, mut rx) = event_channel(16);
    let mut client = ExecActor::spawn("agree-test", events.clone(), move |rx| fake_run(events, rx))
        .with_halt_path(sentinel.clone());

    let req = order("agree-1", case.reduce_only);
    match case.verb {
        Verb::Submit => client.submit(&req),
        Verb::Modify => client.modify(&req, Some(2.0), Some(101.0)),
        Verb::Cancel => client.cancel("agree-1"),
    }

    // ADMITTED iff the command reached the venue thread (the fake emits a marker only then).
    // A synthesized `OrderRejected` — or nothing at all, which is how BOTH clients spell a blocked
    // modify — is REFUSED.
    let events = drain_ingest(&mut rx);
    let admitted = events.iter().any(|e| {
        matches!(e, Event::OrderSubmitted(_) | Event::OrderCanceled(_) | Event::OrderModified(_))
    });
    if admitted { Verdict::Admitted } else { Verdict::Refused }
}

/// PAPER: drive the same case through an armed `PaperExecutionClient` with the sentinel engaged.
///
/// The paper book emits `OrderSubmitted` unconditionally at the top of `submit` (the emitter split),
/// so admission is read off `OrderAccepted` — the event that means the book TOOK the order — exactly
/// as admission on the live side is read off the command reaching the venue thread.
fn paper_verdict(case: &Case) -> Verdict {
    // Bound FIRST so it drops LAST — see the twin note in [`live_verdict`].
    let (_dir, sentinel) = unique_sentinel("paper");

    let mut client = PaperExecutionClient::with_fee_schedule(VENUE, SYMBOL, 0.0, FeeSchedule::Free)
        .with_halt_path(sentinel.clone());

    // Rest an order BEFORE engaging, so `modify`/`cancel` have something real to act on — a verdict
    // read off a no-op would be the same "nothing happened" as a refusal.
    client.submit(&order("agree-1", false));
    while client.poll_events().is_some() {}

    std::fs::write(&sentinel, b"").expect("engage the sentinel");
    let req = order("agree-1", case.reduce_only);
    match case.verb {
        Verb::Submit => {
            let fresh = order("agree-2", case.reduce_only);
            client.submit(&fresh);
        }
        Verb::Modify => client.modify(&req, Some(2.0), Some(101.0)),
        Verb::Cancel => client.cancel("agree-1"),
    }

    let mut events = Vec::new();
    while let Some(e) = client.poll_events() {
        events.push(e);
    }
    let admitted = events.iter().any(|e| {
        matches!(e, Event::OrderAccepted(_) | Event::OrderCanceled(_) | Event::OrderModified(_))
    });
    if admitted { Verdict::Admitted } else { Verdict::Refused }
}

// ── the gate ─────────────────────────────────────────────────────────────────────────────────

/// THE test. Every row: paper and live reach the same verdict, and it is the expected one.
///
/// ⚠ Mutation proof — delete the `halt_engaged()` arm from `crates/vike-paper/src/lib.rs`'s
/// `modify` and the two `modify` rows go red on both asserts (paper admits, live refuses).
#[test]
fn paper_and_live_admit_and_refuse_the_same_things_under_halt() {
    let mut disagreements = Vec::new();
    let mut wrong = Vec::new();
    for case in CASES {
        let live = live_verdict(case);
        let paper = paper_verdict(case);
        if live != paper {
            disagreements
                .push(format!("{}: live={live:?} paper={paper:?} — {}", case.what, case.why));
        }
        if live != case.expect {
            wrong.push(format!(
                "{} on LIVE: got {live:?}, want {:?} — {}",
                case.what, case.expect, case.why
            ));
        }
        if paper != case.expect {
            wrong.push(format!(
                "{} on PAPER: got {paper:?}, want {:?} — {}",
                case.what, case.expect, case.why
            ));
        }
    }
    assert!(
        disagreements.is_empty(),
        "PAPER and LIVE disagree about what an engaged HALT admits. An operator rehearses the kill \
         switch on the paper mount; a switch that behaves differently there teaches the wrong \
         lesson about the one control that survives a wedged runtime:\n  {}",
        disagreements.join("\n  ")
    );
    assert!(
        wrong.is_empty(),
        "the two clients AGREE but on the wrong answer — which is what agreement alone cannot \
         catch, and why this test asserts the expected verdict as well:\n  {}",
        wrong.join("\n  ")
    );
}

/// The floor + the mutation self-test for the harness itself. Both verdict functions must be able to
/// report BOTH values, or the test above is green for the wrong reason.
///
/// A test whose measurement apparatus only ever returns one answer is the failure mode this repo has
/// shipped three times (`declaration-pinning tests don't gate`).
#[test]
fn the_harness_can_report_both_verdicts() {
    assert!(CASES.len() >= 5, "the table shrank below the shapes a halt has to distinguish");
    let refused = CASES.iter().find(|c| c.expect == Verdict::Refused).expect("a Refused row");
    let admitted = CASES.iter().find(|c| c.expect == Verdict::Admitted).expect("an Admitted row");
    assert_eq!(live_verdict(refused), Verdict::Refused);
    assert_eq!(live_verdict(admitted), Verdict::Admitted);
    assert_eq!(paper_verdict(refused), Verdict::Refused);
    assert_eq!(paper_verdict(admitted), Verdict::Admitted);
}
