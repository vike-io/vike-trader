//! The HALT kill switch reaches a PAPER MOUNT — and, just as load-bearing, reaches no simulation.
//!
//! # The defect these tests exist for
//!
//! `grep -rni halt crates/vike-paper/src/` used to return nothing. A daemon running the paper
//! exchange — which is what `vike-tradehub`'s paper variant runs end to end, and what EVERY venue
//! without credentials falls back to under the live gate — observed no HALT file at all. Arming the
//! kill switch on such a mount changed nothing, silently.
//!
//! That is harmless in the narrow sense (a paper book puts nothing on a wire), and dangerous in the
//! sense that matters: the paper mount is where an operator REHEARSES the switch, and a switch that
//! works on some mounts and not others is worse than one that works nowhere, because the operator
//! cannot tell which they have. They `touch` the file, see orders keep flowing, and learn the wrong
//! lesson about the one control that survives a wedged runtime.
//!
//! # …and the OPPOSITE property, which is why this is opt-in
//!
//! `PaperExecutionClient` wears two hats. It is the mounted paper exchange above, and it is the
//! SIMULATION primitive `vike-backtest`'s `tests/r7_gate.rs` drives to prove backtest == paper
//! bit-for-bit. Had the sentinel been read unconditionally, a stray `HALT` file in a developer's
//! `<project>/settings/state/` — or on a CI runner — would silently change backtest fills and redden
//! that gate for a reason nobody would find. So the sentinel is armed BY A MOUNT
//! (`vike_mount::make_engine`'s `paper_client`, `vike_run::build_paper_maker_core_with`) and by
//! nothing else, and [`an_unarmed_book_ignores_a_halt_file_that_exists`] is the regression guard for
//! that half — it is every bit as important as the blocking tests above it.
//!
//! Each test uses its OWN sentinel path inside a temp directory it OWNS — a bound
//! `tempfile::TempDir` ([`sentinel_for`]), so nothing here touches the process-wide resolution
//! (`vike_bridge_core::halt::halt_path_from_env`), races a sibling test, or survives the run.
//!
//! ⚠ **That isolation is also this file's limit, which is why `tests/paper_halt_process_wide.rs`
//! exists.** A private temp sentinel can prove that an ARBITRARY existing file changes nothing; it
//! cannot prove anything about the PROCESS-WIDE one, because on a CI runner or a fresh checkout that
//! path does not exist and so a book that consulted it would pass anyway. The sibling file supplies
//! `VIKE_HALT_FILE` itself, which is what makes that half's mutation proof hold on every box rather
//! than on whichever one has a stray `HALT` lying around.

use std::path::PathBuf;

use vike_exec::ExecutionClient;
use vike_model::events::Event;
use vike_model::{FeeSchedule, OrderRequest};
use vike_paper::PaperExecutionClient;

const VENUE: &str = "binance";
const SYMBOL: &str = "BTCUSDT";

/// A unique, EXISTING directory for this test's sentinel, plus the sentinel path inside it — and
/// the guard that deletes both.
///
/// ⚠ Returns the `TempDir` ALONGSIDE the path, and the caller must BIND it: dropping the guard
/// removes the directory out from under the book still consulting the sentinel inside it.
///
/// This used to be `temp_dir().join(format!("vike-paper-halt-{pid}-{tag}"))` followed by a
/// `create_dir_all`, described here as "no dependency, no shared path, no cleanup ordering to get
/// wrong". The last two were false. MEASURED on the CI box, 2026-08-25:
///
/// * **4,957 leaked `vike-paper-halt-*` directories in `/tmp`**. Nothing ever deleted one — the
///   tests removed the sentinel FILE and left its parent — so every run leaked permanently.
/// * **A PID is REUSED**, and the CI box runs these tests as TWO users (`the CI user` for CI, `the operator`
///   for the verification lanes). When a PID collides with a directory the other user created, the
///   `create_dir_all` SUCCEEDS (it already exists) and the `fs::write` that engages the sentinel
///   fails with PermissionDenied — a live, intermittent flake. pid+tag separates the tests within
///   one run; it separates nothing across users over time, and it leaked either way.
///
/// `tempfile::TempDir` fixes both halves at once: unique by construction, and self-deleting —
/// including on the panic/unwind path, where a leak is least likely to be noticed. The per-test
/// tag stays in the directory PREFIX (`crates/vike-core/src/scratch.rs`'s `Scratch::root`
/// spelling), so a directory seen mid-run still names the test that owns it.
fn sentinel_for(tag: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::Builder::new()
        .prefix(&format!("vike-paper-halt-{tag}-"))
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
        ..Default::default()
    }
}

/// Drain the client's event queue through the `poll_events` seam the live core actually pumps, so
/// these tests assert on the events a mount would book rather than on a private buffer.
fn drain(client: &mut PaperExecutionClient) -> Vec<Event> {
    let mut out = Vec::new();
    while let Some(e) = client.poll_events() {
        out.push(e);
    }
    out
}

fn kinds(events: &[Event]) -> Vec<&'static str> {
    events
        .iter()
        .map(|e| match e {
            Event::OrderSubmitted(_) => "submitted",
            Event::OrderAccepted(_) => "accepted",
            Event::OrderRejected(_) => "rejected",
            Event::Fill(_) => "fill",
            Event::OrderCanceled(_) => "canceled",
            _ => "other",
        })
        .collect()
}

/// THE regression test. An armed mount refuses an OPENING submit while the sentinel exists, and the
/// order reaches a TERMINAL rejection rather than silently vanishing (the venue-adapter contract: "a
/// dead venue path must synthesize a terminal `OrderRejected`").
///
/// ⚠ Mutation proof: delete the `halt_engaged()` arm in `PaperExecutionClient::submit` and this goes
/// red on the `accepted` it then books.
#[test]
fn an_armed_paper_mount_refuses_an_opening_submit_while_the_sentinel_exists() {
    let (_dir, path) = sentinel_for("blocks");
    std::fs::write(&path, b"").expect("engage the sentinel");

    let mut client = PaperExecutionClient::with_fee_schedule(VENUE, SYMBOL, 0.0, FeeSchedule::Free)
        .with_halt_path(path.clone());
    client.submit(&order("opening-1", false));

    let events = drain(&mut client);
    assert_eq!(
        kinds(&events),
        vec!["submitted", "rejected"],
        "an opening submit under a HALT file must be SUBMITTED then terminally REJECTED — never \
         accepted, and never dropped on the floor. Got: {events:?}"
    );
    let Some(Event::OrderRejected(r)) =
        events.iter().find(|e| matches!(e, Event::OrderRejected(_)))
    else {
        panic!("no rejection: {events:?}")
    };
    assert_eq!(
        r.reason.as_str(),
        vike_exec::halt::HALT_REJECT_REASON,
        "the rejection must carry the SHARED halt reason, so an operator (and the GUI) recognizes a \
         halt rejection by the same wording paper and live both use"
    );
}

/// The exemption, and the reason the whole reducing-submit arm exists: a halt must never trap an
/// operator in a position. `OrderIntent::Flatten` mints exactly this shape, so `market-exit` has to
/// work from a halted PAPER mount too — otherwise the rehearsal teaches a lie about the real one.
///
/// ⚠ Mutation proof: change the paper gate to refuse unconditionally (drop the
/// `!halt_admits_submit(..)` half) and this goes red.
#[test]
fn an_armed_paper_mount_still_admits_a_reducing_submit_so_a_halt_cannot_trap_you() {
    let (_dir, path) = sentinel_for("reduce");
    std::fs::write(&path, b"").expect("engage the sentinel");

    let mut client = PaperExecutionClient::with_fee_schedule(VENUE, SYMBOL, 0.0, FeeSchedule::Free)
        .with_halt_path(path.clone());
    client.submit(&order("flatten-1", true));

    let events = drain(&mut client);
    assert!(
        kinds(&events).contains(&"accepted"),
        "a reduce_only submit must pass under HALT — `market-exit` flattens with the file engaged. \
         Got: {events:?}"
    );
    assert!(
        !kinds(&events).contains(&"rejected"),
        "a reducing submit must not be rejected under HALT: {events:?}"
    );
}

/// Removing the sentinel RESUMES the mount. The check is per-submit and reads the filesystem each
/// time, so `rm` is the documented resume and nothing latches.
#[test]
fn removing_the_sentinel_resumes_an_armed_paper_mount() {
    let (_dir, path) = sentinel_for("resume");
    std::fs::write(&path, b"").expect("engage the sentinel");

    let mut client = PaperExecutionClient::with_fee_schedule(VENUE, SYMBOL, 0.0, FeeSchedule::Free)
        .with_halt_path(path.clone());
    client.submit(&order("halted", false));
    assert!(kinds(&drain(&mut client)).contains(&"rejected"), "should be halted while it exists");

    std::fs::remove_file(&path).expect("disengage");
    client.submit(&order("resumed", false));
    let events = drain(&mut client);
    assert!(
        kinds(&events).contains(&"accepted") && !kinds(&events).contains(&"rejected"),
        "`rm` must resume the mount — the sentinel is re-read per submit, nothing latches. Got: \
         {events:?}"
    );
}

/// The OTHER half, and the one that protects the backtest == paper law: a book that no mount armed
/// ignores a sentinel that exists.
///
/// ⚠ This is not belt-and-braces. `vike-backtest`'s `tests/r7_gate.rs` builds this client directly
/// and compares fills bit-for-bit against the engine; make the sentinel unconditional and a stray
/// `HALT` file anywhere on the resolution path silently changes backtest results.
///
/// ⚠ **Its mutation proof lives NEXT DOOR, and that is the repair of a MEASURED defect in this
/// test.** This one used to name "have `halt_engaged` fall back to the process-wide path when
/// `halt_path` is `None`" as its mutation. That mutation was applied on the CI box (2026-08-08) and this
/// test PASSED — as did its `modify` twin — because the process-wide path resolves to
/// `<project>/settings/state/HALT` or `<exe_dir>/HALT`, and on that runner `VIKE_HALT_FILE` was
/// unset and no such file existed. Only a box that happens to have one reddens, so the named
/// mutation shipped green everywhere the gate actually runs. What this test proves UNCONDITIONALLY
/// is narrower and still worth having: an ARBITRARY existing file the book was never told about
/// changes nothing. `tests/paper_halt_process_wide.rs` proves the fallback half on every box by
/// supplying `VIKE_HALT_FILE` itself, and FAILED under the same mutation in the same run.
#[test]
fn an_unarmed_book_ignores_a_halt_file_that_exists() {
    let (_dir, path) = sentinel_for("unarmed");
    std::fs::write(&path, b"").expect("engage a sentinel this book was never told about");

    // No `with_halt_path` — the shape every backtest, every unit test and the r7 gate construct.
    let mut client = PaperExecutionClient::with_fee_schedule(VENUE, SYMBOL, 0.0, FeeSchedule::Free);
    client.submit(&order("sim-1", false));

    let events = drain(&mut client);
    assert_eq!(
        kinds(&events),
        vec!["submitted", "accepted"],
        "an UNARMED book must be byte-identical to its pre-sentinel behaviour — a backtest's fills \
         may never depend on a file lying around on the box. Got: {events:?}"
    );
}

/// A cancel is never gated. It does not pass through the halt arm at all, and an operator must be
/// able to pull resting orders while halted — the whole point of "halt stops NEW risk".
#[test]
fn a_cancel_passes_through_an_armed_and_engaged_paper_mount() {
    let (_dir, path) = sentinel_for("cancel");

    let mut client = PaperExecutionClient::with_fee_schedule(VENUE, SYMBOL, 0.0, FeeSchedule::Free)
        .with_halt_path(path.clone());
    // Rest an order BEFORE engaging, so there is something to cancel.
    client.submit(&order("resting", false));
    let _ = drain(&mut client);

    std::fs::write(&path, b"").expect("engage the sentinel");
    client.cancel("resting");

    let events = drain(&mut client);
    assert!(
        kinds(&events).contains(&"canceled"),
        "cancel must pass while halted — halt stops NEW risk, it never traps a resting order. Got: \
         {events:?}"
    );
}

/// A MODIFY is blocked while the sentinel is engaged — the arm that was missing when the `submit`
/// arm landed, and the reason paper and live disagreed about what a halt admits.
///
/// A modify can add size or chase price, so it is refused OUTRIGHT: there is no reducing exemption
/// here, unlike `submit`. `crates/vike-bridge-core/src/exec_actor.rs`'s `modify` is the copy this
/// mirrors — down to the SILENCE (non-terminal, nothing synthesized, the resting order keeps its
/// terms), which is a documented gap on all three clients rather than something to fix in this
/// crate alone. `crates/vike-mount/tests/halt_paper_live_agree.rs` is the cross-client half; this is
/// the local one.
///
/// ⚠ Mutation proof: delete the `halt_engaged()` arm from `PaperExecutionClient::modify` and this
/// goes red on the `OrderModified` it then books.
#[test]
fn an_armed_paper_mount_blocks_a_modify_while_the_sentinel_exists() {
    let (_dir, path) = sentinel_for("modify");

    let mut client = PaperExecutionClient::with_fee_schedule(VENUE, SYMBOL, 0.0, FeeSchedule::Free)
        .with_halt_path(path.clone());
    // Rest an order BEFORE engaging, so the modify has a real target — an amend of nothing is a
    // no-op and would look identical to a blocked one.
    client.submit(&order("resting", false));
    let _ = drain(&mut client);

    std::fs::write(&path, b"").expect("engage the sentinel");
    client.modify(&order("resting", false), Some(9.0), Some(101.0));
    assert!(
        drain(&mut client).is_empty(),
        "a modify under HALT must be blocked — it can add size or chase price, and the exit path \
         under a halt is CANCEL, never modify"
    );

    // …and the mutation sentinel in the SAME client, so a build that simply stopped modifying at
    // all would fail here rather than pass the assert above for the wrong reason.
    std::fs::remove_file(&path).expect("disengage");
    client.modify(&order("resting", false), Some(9.0), Some(101.0));
    let events = drain(&mut client);
    assert!(
        matches!(events.as_slice(), [Event::OrderModified(m)] if m.client_order_id == "resting"),
        "with the sentinel gone the SAME amend must go through — otherwise the test above proves \
         nothing about the halt. Got: {events:?}"
    );
}

/// The reducing exemption does NOT extend to a modify, on either client. A `reduce_only` order's
/// amend is still refused: the flag describes the order AS SUBMITTED, and an amend can raise the qty
/// of a reduce_only order past the position it was covering. The exit under a halt is `cancel`.
#[test]
fn a_modify_is_blocked_under_halt_even_for_an_order_that_was_reduce_only() {
    let (_dir, path) = sentinel_for("modify-reduce");

    let mut client = PaperExecutionClient::with_fee_schedule(VENUE, SYMBOL, 0.0, FeeSchedule::Free)
        .with_halt_path(path.clone());
    client.submit(&order("resting-reduce", true));
    let _ = drain(&mut client);

    std::fs::write(&path, b"").expect("engage the sentinel");
    client.modify(&order("resting-reduce", true), Some(99.0), None);
    assert!(
        drain(&mut client).is_empty(),
        "`reduce_only` must NOT buy an amend past the halt: the flag describes the original order, \
         and this amend raises the qty"
    );
}

/// An UNARMED book still modifies under a sentinel that exists — the `modify` twin of
/// [`an_unarmed_book_ignores_a_halt_file_that_exists`], and load-bearing for the same reason: the
/// r7 gate drives this client directly, and a backtest's amends may not depend on a file on disk.
///
/// It shares that test's limit too (a private temp sentinel says nothing about the process-wide
/// one), and the same repair: `tests/paper_halt_process_wide.rs` engages `VIKE_HALT_FILE` itself.
/// Both arms read the ONE `halt_engaged`, so pinning the submit arm there pins this one.
#[test]
fn an_unarmed_book_modifies_under_a_halt_file_that_exists() {
    let (_dir, path) = sentinel_for("unarmed-modify");
    std::fs::write(&path, b"").expect("engage a sentinel this book was never told about");

    let mut client = PaperExecutionClient::with_fee_schedule(VENUE, SYMBOL, 0.0, FeeSchedule::Free);
    client.submit(&order("sim-rest", false));
    let _ = drain(&mut client);
    client.modify(&order("sim-rest", false), Some(3.0), None);

    let events = drain(&mut client);
    assert!(
        matches!(events.as_slice(), [Event::OrderModified(_)]),
        "an UNARMED book must be byte-identical to its pre-sentinel behaviour on modify too. Got: \
         {events:?}"
    );
}

/// EXACTLY ONE terminal event, and no acceptance, for a refused submit. The venue-adapter contract
/// this repo enforces on every real adapter is "no order may silently vanish, and every order
/// reaches exactly one terminal event"; a paper mount that is now a halt-enforcing client owes the
/// same. Asserted over a batch so a partially-gated implementation (one that refused the first order
/// and then fell through) cannot pass.
#[test]
fn every_refused_submit_reaches_exactly_one_terminal_event() {
    let (_dir, path) = sentinel_for("terminal");
    std::fs::write(&path, b"").expect("engage the sentinel");

    let mut client = PaperExecutionClient::with_fee_schedule(VENUE, SYMBOL, 0.0, FeeSchedule::Free)
        .with_halt_path(path.clone());
    for i in 0..3 {
        client.submit(&order(&format!("opening-{i}"), false));
    }

    let events = drain(&mut client);
    let rejected = kinds(&events).iter().filter(|k| **k == "rejected").count();
    assert_eq!(rejected, 3, "each refused submit owes ONE terminal rejection: {events:?}");
    assert!(
        !kinds(&events).contains(&"accepted"),
        "a halted mount must accept none of them: {events:?}"
    );
}
