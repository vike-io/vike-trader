//! The operator HALT sentinel on cTrader — the shared `reduce_only` rule PLUS a position-verified
//! admit, driven end to end against the stateful fake server so the verdict comes from the REAL
//! routing decision and not from a mock.
//!
//! # Why this venue could not just copy the check every other venue uses…
//!
//! cTrader was the last LIVE-MOUNTABLE venue `touch $VIKE_HALT_FILE` did not reach. The obvious fix
//! — paste hyperliquid's three-line arm — would have introduced a WORSE bug than the gap it closed.
//! Every other client gates on `vike_exec::halt::halt_admits_submit`, whose whole rule is
//! `request.reduce_only`, because an adapter with no position book has no other evidence. This
//! client HAS one: it routes reduces by inspecting the actor-maintained position map
//! (`crates/bridges/ctrader/src/positions.rs`'s `plan_reduce`), precisely because on a HEDGING
//! account a plain opposite-side order — no flag — is the ordinary way to flatten. A flag-only gate
//! would therefore have refused the very orders that close a position, on the one venue where those
//! orders usually carry no flag: an operator halted WITH the position open, which is exactly the
//! failure `docs/ops/kill-switches.md`'s "halting cannot trap you" exists to prevent.
//!
//! [`a_plain_opposite_order_that_closes_is_admitted_under_halt`] is the test that a copy-paste arm
//! would fail. It carries NO `reduce_only` flag, and it must still close.
//!
//! # …and why it could not REPLACE it either
//!
//! The first shipped version of this arm admitted ONLY what the book proved, and that trapped the
//! operator in the most ordinary situation there is: a restart. The book holds nothing until the
//! venue answers a reconcile for it, and that answer is best-effort — so a fresh process can easily
//! have heard nothing about a position opened before it started; a verified-only rule read that
//! silence as "this order opens" and refused the exit. **Absence from the book is not evidence.**
//! The rule is therefore a UNION — the shared flag predicate, PLUS what the book can prove — which
//! makes this client a strict SUPERSET of every other one at the same mode: it can let more out
//! under a halt, never less.
//! [`a_reduce_only_exit_is_admitted_after_a_restart_when_the_book_never_learned_the_position`] is
//! that test, and [`an_unflagged_order_on_a_book_that_knows_nothing_is_still_refused`] is its pair.
//!
//! # The halt-admit POLICY, and the ONE thing it may refuse
//!
//! `policy.toml`'s `halt_admit` (`vike_model::HaltAdmit`) selects how much evidence the flag half
//! demands. `admit` — the default every test here runs at unless it says otherwise — trusts the
//! flag, byte-identically. `verify` refuses ONE thing and nothing else: a `reduce_only` order in a
//! symbol the venue **POSITIVELY reported a position in**, whose opposing side that same report says
//! is empty. [`verify_refuses_a_reduce_only_order_the_reported_book_proves_adds_to_a_position`] is
//! that refusal and [`admit_still_admits_the_very_order_verify_refuses`] is the A/B proving the knob
//! caused it.
//!
//! ⚠ **Everything else ADMITS, and the list is longer than "never fetched".** A book never fetched,
//! invalidated by a reconnect, rebuilt from an answer carrying a row this build could not classify
//! ([`verify_admits_when_the_reconcile_answer_could_not_be_read_in_full`]), rebuilt from an answer
//! for ANOTHER account ([`a_reconcile_answer_for_another_account_is_not_this_accounts_evidence`]),
//! or simply holding NO row for the symbol in question
//! ([`verify_admits_the_exit_from_a_position_the_reconcile_answer_omitted`],
//! [`verify_admits_a_mistagged_entry_on_a_symbol_the_venue_reported_nothing_about`]) — every one of
//! them is an ABSENCE, and an absence is not a fact. The last pair is the important one: those two
//! orders are the same bytes against the same (empty) book, one a genuine exit from a position the
//! venue omitted and one a mis-tagged entry on a genuinely flat account, and NOTHING can tell them
//! apart. Only one of them can be refused, and refusing the exit traps an operator under a halt.
//!
//! # Both directions, and the consequences
//!
//! Admitted: a plain closing order, a `reduce_only` closing order (tracked or not), a `close_all`, a
//! cancel. Refused: an unflagged opening order, a genuine flip, an amend.
//!
//! Each test injects its own sentinel through `CtraderExec::with_halt_path` (a temp path it OWNS,
//! [`unique_sentinel`]), so nothing here touches the memoized process-wide `halt_path_from_env` or
//! races a sibling test — the idiom `crates/vike-bridge-core/tests/exec_actor_halt.rs` established.
//! ⚠ The `TempDir` guard [`rig`] hands back is what DELETES that directory: bind it for the whole
//! test, never `_`.

mod common;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use vike_bridge_core::halt::HALT_REJECT_REASON;
use vike_ctrader::conn::{connect_and_auth_exec, ConnConfig};
use vike_ctrader::exec::CtraderExec;
use vike_exec::lanes::Ingest;
use vike_exec::{event_channel, ExecutionClient};
use vike_model::events::Event;
use vike_model::{HaltAdmit, OrderRequest};

use common::{FakeCtrader, NoopSink, CLOSE_POSITION_ID_BASE};

/// A EURUSD market order in `qty` UNITS.
fn market(coid: &str, side: i32, qty: f64) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.into(),
        venue: "ctrader".into(),
        symbol: "EURUSD".into(),
        side,
        qty,
        order_type: "market".into(),
        ..Default::default()
    }
}

fn reduce(coid: &str, side: i32, qty: f64) -> OrderRequest {
    OrderRequest { reduce_only: true, ..market(coid, side, qty) }
}

macro_rules! drain {
    ($rx:expr) => {{
        let mut out: Vec<Event> = Vec::new();
        while let Ok(ing) = $rx.try_recv() {
            if let Ingest::Event(e) = ing {
                out.push(e);
            }
        }
        out
    }};
}

/// Poll until `want` events have accumulated (or a 5s deadline), returning them all.
fn collect(rx: &mut tokio::sync::mpsc::Receiver<Ingest>, want: usize) -> Vec<Event> {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut got: Vec<Event> = Vec::new();
    while got.len() < want && Instant::now() < deadline {
        got.extend(drain!(rx));
        if got.len() < want {
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    got
}

/// Drain and DISCARD until the OPEN's terminal `OrderFilled` lands, so the position is TRACKED
/// before the sentinel is engaged. Every "closing order" test depends on this: `plan_reduce` reads
/// the position map, and an order submitted before the open filled would find it empty and route to
/// OPEN — passing or failing for the wrong reason.
fn wait_open_filled(rx: &mut tokio::sync::mpsc::Receiver<Ingest>, coid: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        for e in drain!(rx) {
            if let Event::OrderFilled(f) = e {
                if f.client_order_id == coid {
                    return;
                }
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("open {coid} never filled");
}

/// A sentinel path inside a temp directory the CALLER owns: `(guard, <dir>/HALT)`.
///
/// ⚠ **The guard must stay bound for the whole test** — dropping it removes the directory, so
/// `unique_sentinel(..).0` or a `_` binding deletes it out from under the client. Same contract as
/// `crates/vike-tradehub/src/config.rs`'s `own_script` and `crates/vike-core/src/scratch.rs`.
///
/// # Why this is not `env::temp_dir().join(format!("…-{pid}-{n}-{tag}"))` any more
///
/// That spelling — plus a `create_dir_all` — carried TWO defects, both measured on the shared the CI box
/// box on 2026-08-25. **Nothing ever removed the directory**: one per test per run, forever. This
/// family alone had left **10,509** directories under `/tmp` there and was still growing by ~19 a
/// day, out of 44,840 leaked by this one idiom across the workspace. And **a PID is REUSED**, while
/// the CI box runs these tests as TWO users (`the CI user` for CI, `the operator` for the verification lanes):
/// when a pid collided with a directory the other user had left behind, `create_dir_all` SUCCEEDED
/// (it already exists) and the `write` that engages the sentinel then failed `PermissionDenied` — a
/// live intermittent flake, not a tidiness problem.
///
/// `tempfile` closes both: the random suffix makes the path unique by construction (so no
/// stale-directory pre-clean is needed either — one cannot be picked twice), and `Drop` removes the
/// tree on the unwinding path as well as the passing one. The `{tag}` is kept in the PREFIX so a
/// directory seen mid-run still names the test that owns it.
fn unique_sentinel(tag: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::Builder::new()
        .prefix(&format!("vike-ctrader-halt-{tag}-"))
        .tempdir()
        .expect("temp sentinel dir");
    let path = dir.path().join("HALT");
    (dir, path)
}

fn kinds(events: &[Event]) -> Vec<&'static str> {
    events
        .iter()
        .map(|e| match e {
            Event::OrderSubmitted(_) => "Submitted",
            Event::OrderAccepted(_) => "Accepted",
            Event::Fill(_) => "Fill",
            Event::OrderPartiallyFilled(_) => "PartiallyFilled",
            Event::OrderFilled(_) => "Filled",
            Event::OrderRejected(_) => "Rejected",
            Event::OrderModifyRejected(_) => "ModifyRejected",
            Event::OrderCancelRejected(_) => "CancelRejected",
            _ => "other",
        })
        .collect()
}

/// What a halt test drives: the fake server, the client, its ingest receiver, the sentinel PATH —
/// and last, the guard that OWNS the directory that path lives in.
///
/// ⚠ The guard is a return value rather than an implementation detail because it must be BOUND:
/// every call site names it (`_halt_dir`), and dropping it early deletes the sentinel's directory.
type Rig =
    (FakeCtrader, CtraderExec, tokio::sync::mpsc::Receiver<Ingest>, PathBuf, tempfile::TempDir);

/// The exec client + its ingest receiver + the fake server, with `sentinel` injected but NOT yet
/// created — a test engages it by `write`ing the file when it wants to. Default halt-admit mode
/// (`Admit`), i.e. the behaviour every deployment with no `policy.toml` gets.
fn rig(tag: &str) -> Rig {
    rig_on(tag, FakeCtrader::start_close_scripted(), HaltAdmit::Admit)
}

/// [`rig`] over an explicit server and halt-admit mode — the seam the `verify` tests need, because
/// the interesting variable is what the venue's RECONCILE answer looks like at the handshake.
fn rig_on(tag: &str, server: FakeCtrader, mode: HaltAdmit) -> Rig {
    let (halt_dir, sentinel) = unique_sentinel(tag);
    let cfg = ConnConfig::for_test(server.addr(), "cid", "secret", "token");
    let (events, rx) = event_channel(64);
    let handle = connect_and_auth_exec(cfg, Arc::new(NoopSink), events.clone()).expect("auth ok");
    let exec =
        CtraderExec::new(handle, events).with_halt_path(sentinel.clone()).with_halt_admit(mode);
    (server, exec, rx, sentinel, halt_dir)
}

// ── ADMITTED: what the book says CLOSES ──────────────────────────────────────────────────────

/// **THE test a copy-pasted flag-only halt arm would fail.** A PLAIN opposite-side order — no
/// `reduce_only` anywhere on it — closes a tracked long, and must still be admitted with the
/// sentinel engaged. On a hedging account this is the ordinary way to flatten, so refusing it is
/// refusing the exit.
///
/// ⚠ **That is a MEASUREMENT, not a claim.** The mutation was applied and run (the CI box, 2026-08-08):
/// hyperliquid's arm pasted verbatim at the top of `submit`, i.e. BEFORE the position routing, which
/// is where every other client puts it —
///
/// ```ignore
/// let halted = self.halt_engaged();
/// if halted && !vike_bridge_core::halt::halt_admits_submit(request) {
///     self.reject(&request.client_order_id, vike_bridge_core::halt::HALT_REJECT_REASON);
///     return;
/// }
/// ```
///
/// — and the result was `17 tests run: 15 passed, 2 failed`: THIS test and
/// [`verify_still_admits_what_the_position_book_proves_closes`], its `verify`-mode twin. The
/// operator would have been halted with the position still open.
///
/// ⚠ The PLACEMENT is the load-bearing half. The same flag-only predicate placed AFTER the routing
/// is not a bug at all — it is what this client now does, because every submit the book proves is a
/// close has already returned down the close path by then, so what remains is exactly the set the
/// shared predicate should judge. A halt gate on this venue is wrong iff it runs BEFORE the routing,
/// which is where a reviewer diffing against hyperliquid would expect to find it.
#[test]
fn a_plain_opposite_order_that_closes_is_admitted_under_halt() {
    let (server, mut exec, mut rx, sentinel, _halt_dir) = rig("plain-close");

    exec.submit(&market("open-1", 1, 1000.0)); // BUY 1000 → opens a long
    wait_open_filled(&mut rx, "open-1");

    std::fs::write(&sentinel, b"").expect("engage the sentinel");
    exec.submit(&market("flat-1", -1, 1000.0)); // plain SELL 1000 — NO reduce_only flag

    let got = collect(&mut rx, 4);
    assert_eq!(
        kinds(&got),
        vec!["Submitted", "Accepted", "Fill", "Filled"],
        "a plain opposite order that this account's own book says CLOSES must pass the halt — the \
         flag-only predicate every other venue uses would have refused it, trapping the operator in \
         the position. events: {got:?}"
    );
    let closes = server.wait_for_closes(1, Duration::from_secs(5));
    assert_eq!(
        closes,
        vec![(CLOSE_POSITION_ID_BASE, 100_000)],
        "it must have gone out as a CLOSE_POSITION_REQ for the tracked position"
    );
    assert_eq!(
        server.count_seen("NEW_ORDER_REQ"),
        1,
        "only the OPEN used a NewOrder — the halted close must not have reached the new-order path"
    );

    drop(exec);
}

/// The `reduce_only` closing order — the shape `OrderIntent::Flatten` mints — is admitted too, and
/// partially closes. Same admit for both spellings of "close this", which is the property that makes
/// `market-exit` work identically on this venue and every other one.
#[test]
fn a_reduce_only_closing_order_is_admitted_under_halt() {
    let (server, mut exec, mut rx, sentinel, _halt_dir) = rig("reduce-close");

    exec.submit(&market("open-2", 1, 3000.0));
    wait_open_filled(&mut rx, "open-2");

    std::fs::write(&sentinel, b"").expect("engage the sentinel");
    exec.submit(&reduce("part-1", -1, 1000.0));

    let got = collect(&mut rx, 4);
    assert!(kinds(&got).contains(&"Filled"), "a reduce_only close must pass the halt: {got:?}");
    assert_eq!(server.wait_for_closes(1, Duration::from_secs(5)).len(), 1);

    drop(exec);
}

/// `close_all` is NOT gated, and could not be: it takes `(position_id, volume)` legs and can express
/// nothing but a close. Gating it could only ever remove an exit — and it is the first-class flatten
/// for positions a fresh connect never tracked, i.e. the tool an operator reaches for during exactly
/// the incident that made them `touch` the file.
#[test]
fn close_all_flattens_under_an_engaged_halt() {
    let (server, mut exec, mut rx, sentinel, _halt_dir) = rig("close-all");

    exec.submit(&market("open-3", 1, 1000.0));
    wait_open_filled(&mut rx, "open-3");

    std::fs::write(&sentinel, b"").expect("engage the sentinel");
    let coids = exec.close_all(&[(CLOSE_POSITION_ID_BASE, 100_000)]);
    assert_eq!(coids.len(), 1, "one close order issued");

    assert_eq!(
        server.wait_for_closes(1, Duration::from_secs(5)),
        vec![(CLOSE_POSITION_ID_BASE, 100_000)],
        "close_all must reach the venue with the sentinel engaged — it is an exit, and a halt never \
         removes an exit"
    );

    drop(exec);
}

// ── REFUSED: what the book says OPENS ────────────────────────────────────────────────────────

/// An OPENING order is refused with the SHARED halt reason, and never reaches the venue.
///
/// ⚠ Mutation proof: delete the `halt_admits_this_submit` arm before the `order_to_new_order` match
/// in `crates/bridges/ctrader/src/exec.rs`'s `submit` and this goes red — the order is accepted and
/// `NEW_ORDER_REQ` climbs to 1.
#[test]
fn an_opening_order_is_refused_under_halt_and_never_reaches_the_venue() {
    let (server, mut exec, mut rx, sentinel, _halt_dir) = rig("open-refused");

    std::fs::write(&sentinel, b"").expect("engage the sentinel");
    exec.submit(&market("open-me", 1, 1000.0));

    let got = collect(&mut rx, 2);
    assert_eq!(
        kinds(&got),
        vec!["Submitted", "Rejected"],
        "an opening submit under HALT must be SUBMITTED then terminally REJECTED — never accepted, \
         never dropped. events: {got:?}"
    );
    match got.iter().find(|e| matches!(e, Event::OrderRejected(_))) {
        Some(Event::OrderRejected(r)) => assert_eq!(
            r.reason.as_str(),
            HALT_REJECT_REASON,
            "refused BY THE HALT, with the wording every venue and the GUI recognize"
        ),
        _ => panic!("no rejection: {got:?}"),
    }
    assert_eq!(server.count_seen("NEW_ORDER_REQ"), 0, "the order must NOT have reached the venue");

    drop(exec);
}

/// A genuine FLIP — a plain order LARGER than the total opposing exposure — is refused. It routes to
/// the new-order path (preserving netting-account flip behaviour) and so opens risk past flat, which
/// is what a halt is for. It does not trap: the same position still closes under an exactly-sized or
/// `reduce_only` order, which the two admit tests above prove.
#[test]
fn a_flip_past_flat_is_refused_under_halt() {
    let (server, mut exec, mut rx, sentinel, _halt_dir) = rig("flip");

    exec.submit(&market("open-4", 1, 1000.0)); // long 1000
    wait_open_filled(&mut rx, "open-4");

    std::fs::write(&sentinel, b"").expect("engage the sentinel");
    exec.submit(&market("flip-me", -1, 3000.0)); // SELL 3000 > 1000 opposing → flip → OPEN path

    let got = collect(&mut rx, 2);
    assert_eq!(kinds(&got), vec!["Submitted", "Rejected"], "a flip opens risk: {got:?}");
    assert_eq!(
        server.count_seen("NEW_ORDER_REQ"),
        1,
        "only the OPEN used a NewOrder — the flip must not have reached the venue"
    );
    assert_eq!(server.close_requests().len(), 0, "and it must not have half-closed either");

    drop(exec);
}

/// ⚠ **THE RESTART CASE — the finding this file was rewritten for.** A fresh `CtraderExec` holds no
/// position this process did not see, and `plan_reduce` therefore returns `Open` for the exit: this
/// rig IS the daemon that just restarted.
///
/// ⚠ **The MECHANISM behind the empty book moved, and the case did not.** `conn.rs`'s
/// `seed_positions_at_connect` now asks the venue at the handshake, so a mount usually knows the
/// account's book — but that ask is best-effort (a write failure, an `ERROR_RES`, the bounded
/// `RECONCILE_TIMEOUT`), it runs on exec mounts only, a socket death re-clears the evidence, and an
/// answer this build cannot read in full leaves the book UNVERIFIED. Whichever way the book ends up
/// empty, the rule below is the one that keeps the operator's exit working, and this test still runs
/// against a book holding nothing for the position it is closing.
///
/// An operator who then halts and sends the exit for a position opened before the restart must get
/// it out. The first shipped version of this arm refused exactly that — it admitted only what
/// `plan_reduce` returned as [`ClosePlan::Close`], and an empty book returns `Open` for everything —
/// which is the trap `docs/ops/kill-switches.md`'s first promise forbids, on the ONE venue that was
/// supposed to be better at this than the others.
///
/// The fix is not "fetch first": that reconcile is best-effort and is allowed to fail. It is that
/// the book is evidence FOR a close and never against one, so a submit it cannot confirm falls back
/// to the shared `reduce_only` predicate — the answer every other venue gives.
///
/// ⚠ **MEASURED, not claimed** (the CI box, 2026-08-08, lane a). Drop the shared-predicate half of
/// `crates/bridges/ctrader/src/exec.rs`'s `halt_admits_this_submit`, leaving the verified-only rule
/// that shipped, and the run is `17 tests run: 11 passed, 6 failed` with THIS among them — the
/// operator trapped, along with every other exit this file proves must get out. Environment-
/// independent: the rig's position map is empty by construction, not because of anything on the box.
#[test]
fn a_reduce_only_exit_is_admitted_after_a_restart_when_the_book_never_learned_the_position() {
    let (server, mut exec, mut rx, sentinel, _halt_dir) = rig("restart-exit");

    // NOTHING is opened through this client first: an empty map is what a fresh connect has.
    std::fs::write(&sentinel, b"").expect("engage the sentinel");
    exec.submit(&reduce("exit-after-restart", -1, 1000.0));

    let got = collect(&mut rx, 2);
    assert!(
        !kinds(&got).contains(&"Rejected"),
        "a reduce_only EXIT must be admitted when this process's position book is empty — that is \
         every restart, and refusing it traps the operator in a position that plainly exists. \
         events: {got:?}"
    );
    server.wait_until_count_at_least("NEW_ORDER_REQ", 1, Duration::from_secs(5));
    assert_eq!(
        server.count_seen("NEW_ORDER_REQ"),
        1,
        "and it must actually have reached the venue"
    );

    drop(exec);
}

/// The SAME empty book, and the half that keeps the halt a halt: a PLAIN opening order — no flag,
/// nothing for the book to confirm — is still refused. This is the pair that shows the fallback is a
/// deferral to the shared predicate and not "admit everything the book cannot see".
///
/// The residual it inherits is the documented one: this admits a `reduce_only` order that closes
/// nothing (a strategy bug's mis-tagged entry), exactly as
/// `crates/vike-bridge-core/tests/exec_actor_halt.rs`'s
/// `this_boundary_trusts_the_flag_because_it_cannot_see_a_position` describes. Refusing it here
/// would mean reading an empty map as "flat", and an empty map is equally "not learned yet" — the
/// restart case above. One of the two has to be admitted, and trapping an operator is the worse
/// failure.
#[test]
fn an_unflagged_order_on_a_book_that_knows_nothing_is_still_refused() {
    let (server, mut exec, mut rx, sentinel, _halt_dir) = rig("unflagged-open");

    std::fs::write(&sentinel, b"").expect("engage the sentinel");
    exec.submit(&market("plain-open", -1, 1000.0)); // no flag, nothing tracked to close

    let got = collect(&mut rx, 2);
    assert_eq!(
        kinds(&got),
        vec!["Submitted", "Rejected"],
        "an order carrying neither a proof nor a claim that it closes must be refused: {got:?}"
    );
    assert_eq!(server.count_seen("NEW_ORDER_REQ"), 0);

    drop(exec);
}

/// A MODIFY is refused under halt — no position-verified exemption, because there is nothing for the
/// book to verify: an amend changes the terms of a RESTING order and `new_qty` can raise them. Same
/// VERDICT as every other client; unlike them it says so, with a NON-terminal `OrderModifyRejected`
/// (the resting order keeps its terms), because this function already owes an advisory on every
/// other refusal it makes.
#[test]
fn a_modify_is_refused_under_halt_and_says_so() {
    let (_server, mut exec, mut rx, sentinel, _halt_dir) = rig("modify");

    std::fs::write(&sentinel, b"").expect("engage the sentinel");
    exec.modify(&market("some-order", 1, 1000.0), Some(2000.0), Some(1.2));

    let got = collect(&mut rx, 1);
    assert_eq!(kinds(&got), vec!["ModifyRejected"], "events: {got:?}");
    match &got[0] {
        Event::OrderModifyRejected(m) => assert_eq!(
            m.reason.as_str(),
            HALT_REJECT_REASON,
            "the advisory must name the HALT, not the coid-unknown path it would otherwise take"
        ),
        other => panic!("expected OrderModifyRejected, got {other:?}"),
    }

    drop(exec);
}

/// Removing the sentinel RESUMES the client. The check is per-submit and re-reads the filesystem
/// each time, so `rm` is the documented resume and nothing latches.
#[test]
fn removing_the_sentinel_resumes_ctrader() {
    let (server, mut exec, mut rx, sentinel, _halt_dir) = rig("resume");

    std::fs::write(&sentinel, b"").expect("engage the sentinel");
    exec.submit(&market("halted", 1, 1000.0));
    assert_eq!(kinds(&collect(&mut rx, 2)), vec!["Submitted", "Rejected"]);
    assert_eq!(server.count_seen("NEW_ORDER_REQ"), 0);

    std::fs::remove_file(&sentinel).expect("disengage");
    exec.submit(&market("resumed", 1, 1000.0));
    let got = collect(&mut rx, 2);
    assert!(
        !kinds(&got).contains(&"Rejected"),
        "`rm` must resume trading — the sentinel is re-read per submit, nothing latches. events: \
         {got:?}"
    );
    server.wait_until_count_at_least("NEW_ORDER_REQ", 1, Duration::from_secs(5));

    drop(exec);
}

// ── the halt-admit POLICY (`policy.toml`'s `halt_admit`) ─────────────────────────────────────────
//
// Every test above runs at the DEFAULT mode, `admit`. The three below are the whole of what the knob
// changes on this venue, and they come in an A/B pair plus the state that must never refuse.

/// **THE `verify` refusal**, and the whole of what it refuses: the venue ANSWERED the reconcile,
/// the answer was read in full, and it POSITIVELY reports a LONG in this symbol — so a `reduce_only`
/// BUY closes nothing here, it ADDS to that long. Refused, and it never reaches the venue.
///
/// ⚠ **The refusal rests on a POSITIVE report, never on an absence, and that is the safety
/// argument.** The venue said "you hold 1000 long EURUSD"; the order claims to reduce a SHORT the
/// same answer says does not exist. Contrast
/// [`verify_admits_the_exit_from_a_position_the_reconcile_answer_omitted`], where the answer says
/// nothing about the symbol at all: THAT absence must admit, because a position the venue simply
/// did not mention is indistinguishable from a flat account and refusing it traps the operator.
///
/// ⚠ **MEASURED** (the CI box, 2026-08-08, lane a). Make `vike_exec::halt::halt_admits_submit_under`'s
/// `Verify` arm return `true` — the knob carried but never consulted, the shape a "wired" setting
/// rots into — and this binary runs `17 tests run: 16 passed, 1 failed` with THIS the failure,
/// alongside `8 tests run: 7 passed, 1 failed` in vike-exec's own `halt` unit suite
/// (`verify_refuses_a_reduce_only_order_against_a_fetched_flat_book`).
#[test]
fn verify_refuses_a_reduce_only_order_the_reported_book_proves_adds_to_a_position() {
    let (server, mut exec, mut rx, sentinel, _halt_dir) = rig_on(
        "verify-adds",
        // The venue holds a 1000-unit LONG and says so, faithfully.
        FakeCtrader::start_close_preseeded(&[(1, 100_000)]),
        HaltAdmit::Verify,
    );

    std::fs::write(&sentinel, b"").expect("engage the sentinel");
    // A BUY tagged reduce_only: it opposes SHORTS, and the venue's own answer reports none.
    exec.submit(&reduce("mistagged-entry", 1, 1000.0));

    let got = collect(&mut rx, 2);
    assert_eq!(
        kinds(&got),
        vec!["Submitted", "Rejected"],
        "halt_admit=verify must refuse a reduce_only order the venue POSITIVELY reported has \
         nothing to reduce — that order adds to a live long through an engaged kill switch. \
         events: {got:?}"
    );
    match got.last() {
        Some(Event::OrderRejected(r)) => assert_eq!(r.reason, HALT_REJECT_REASON),
        other => panic!("expected a halt rejection, got {other:?}"),
    }
    assert_eq!(server.count_seen("NEW_ORDER_REQ"), 0, "and it must not have reached the venue");

    drop(exec);
}

/// …and the A/B half that proves the KNOB did it. Byte-for-byte the same rig and the same order at
/// the DEFAULT mode: ADMITTED, and on the wire. Without this, the test above is satisfied by any
/// bug that refuses `reduce_only` orders on this venue.
#[test]
fn admit_still_admits_the_very_order_verify_refuses() {
    let (server, mut exec, mut rx, sentinel, _halt_dir) =
        rig_on("admit-adds", FakeCtrader::start_close_preseeded(&[(1, 100_000)]), HaltAdmit::Admit);

    std::fs::write(&sentinel, b"").expect("engage the sentinel");
    exec.submit(&reduce("mistagged-entry", 1, 1000.0));

    let got = collect(&mut rx, 2);
    assert!(
        !kinds(&got).contains(&"Rejected"),
        "the DEFAULT mode must be byte-identical to the flag-trusting rule — this is the same \
         order the verify test refuses, and admit has to let it out. events: {got:?}"
    );
    server.wait_until_count_at_least("NEW_ORDER_REQ", 1, Duration::from_secs(5));
    assert_eq!(server.count_seen("NEW_ORDER_REQ"), 1, "and it must actually reach the venue");

    drop(exec);
}

/// ⚠ **THE COST OF THE CURE, asserted rather than left implicit.** On a GENUINELY FLAT account the
/// venue legitimately reports no rows — which is byte-identical to an answer that omitted a
/// position it holds — so `verify` ADMITS the mis-tagged entry there, exactly as every other venue
/// does. `verify` catches the SECOND such order in a symbol, not the first.
///
/// This test used to be the refusal (`…when_the_seeded_book_proves_the_account_is_flat`). It was
/// INVERTED deliberately: the refusal it asserted was reachable only through an inference — "the
/// answer mentioned nothing, therefore there is nothing" — that
/// [`verify_admits_the_exit_from_a_position_the_reconcile_answer_omitted`] measured refusing a live
/// operator's exit. Keeping both is not available: the two orders are the same bytes on the wire
/// against the same book, and only one of them can be refused.
#[test]
fn verify_admits_a_mistagged_entry_on_a_symbol_the_venue_reported_nothing_about() {
    let (server, mut exec, mut rx, sentinel, _halt_dir) =
        rig_on("verify-flat", FakeCtrader::start_close_scripted(), HaltAdmit::Verify);

    std::fs::write(&sentinel, b"").expect("engage the sentinel");
    exec.submit(&reduce("mistagged-entry-on-a-flat-account", -1, 1000.0));

    let got = collect(&mut rx, 2);
    assert!(
        !kinds(&got).contains(&"Rejected"),
        "an EMPTY reconcile answer cannot tell a flat account from one whose positions it omitted, \
         so it may not refuse anything — this admit is the documented cost of never trapping an \
         operator. events: {got:?}"
    );
    server.wait_until_count_at_least("NEW_ORDER_REQ", 1, Duration::from_secs(5));
    assert_eq!(server.count_seen("NEW_ORDER_REQ"), 1);

    drop(exec);
}

/// ⚠ **THE STATE A FAKE SERVER THAT ONLY SPEAKS `PositionStatusOpen` CANNOT EXPRESS.** The venue
/// answered, and the answer carried a row this build cannot classify (an ERROR-status position). The
/// tracked set is therefore INCOMPLETE, so an empty slice for the order's symbol is a HOLE and not a
/// flat account — and `verify` must ADMIT, exactly as it does for a book that was never fetched.
///
/// This is the finding that made the knob's refusal path untrustworthy: it had only ever been driven
/// against a server whose every reconcile row was OPEN, i.e. against a rig in which the failure it
/// must not commit was unrepresentable.
/// `crates/bridges/ctrader/tests/common/mod.rs`'s `ReconcileFidelity` is what makes it
/// representable.
///
/// ⚠ **The rig also PRE-SEEDS a faithfully-reported long, so this test isolates the arm it names.**
/// Without it the book would lack COVERAGE of the symbol and would admit for that reason instead,
/// leaving the unreadable count unexercised end to end — a test that passes for a reason other than
/// the one in its name. Here coverage is present and the order is a reduce_only BUY whose opposing
/// side is empty, so an authoritative book would REFUSE: only the unreadable row can save it.
///
/// ⚠ **MEASURED, not claimed** (the CI box, 2026-08-08, lane a). Have
/// `crates/bridges/ctrader/src/conn.rs`'s `rebuild_position_book` call `replace_all`
/// unconditionally — i.e. go back to `filter_map(tracked_position)` and mark the book fetched
/// regardless — and this binary runs `17 tests run: 16 passed, 1 failed`, THIS the failure: the
/// dropped ERROR row reads as "flat" and the order is refused under an engaged halt. Mutating the
/// COUNTING layer instead (`positions::reconcile_rows` stops separating a known-empty row from an
/// unreadable one) gives the same single failure here, plus `24 tests run: 23 passed, 1 failed` in
/// `positions`'s own unit suite.
#[test]
fn verify_admits_when_the_reconcile_answer_could_not_be_read_in_full() {
    let (server, mut exec, mut rx, sentinel, _halt_dir) = rig_on(
        "verify-unreadable",
        FakeCtrader::start_close_preseeded_with_unreadable_positions(&[(1, 100_000)], 1),
        HaltAdmit::Verify,
    );

    std::fs::write(&sentinel, b"").expect("engage the sentinel");
    exec.submit(&reduce("exit-through-a-hole", 1, 1000.0));

    let got = collect(&mut rx, 2);
    assert!(
        !kinds(&got).contains(&"Rejected"),
        "a reconcile answer with a row this build could not read must leave the book UNVERIFIED, \
         and an unverified book ADMITS — reading the hole as `flat` would refuse an operator's \
         exit on a decode failure. events: {got:?}"
    );
    server.wait_until_count_at_least("NEW_ORDER_REQ", 1, Duration::from_secs(5));
    assert_eq!(server.count_seen("NEW_ORDER_REQ"), 1, "and it must actually reach the venue");

    drop(exec);
}

// ── the TRAP PROBES: an absence in a FETCHED book is still an absence ────────────────────────────
//
// Both servers below hold a REAL 1000-unit long and answer the connect-time reconcile with something
// that does not mention it. Neither answer is DECODABLY wrong — one is empty, one is another
// account's — so `reconcile_rows`' `unreadable` count is `0` for both, which is exactly why a
// decode-completeness check could not see either.

/// ⚠ **THE TRAP.** The venue HOLDS a 1000-unit long. Its `RECONCILE_RES` omits it: a well-formed,
/// fully-classifiable, EMPTY position list for the right account. The operator halts and sends the
/// `reduce_only` exit — and it must GO OUT.
///
/// ⚠ **MEASURED as a REFUSAL before the fix** (the CI box, 2026-08-08, lane a): with `is_fetched()` as
/// the whole evidence test, this run produced
/// `kinds = ["Submitted", "Rejected"]`, `NEW_ORDER_REQ seen = 0` — the exit refused on a position
/// the venue was holding, under a halt, which is the one thing `docs/ops/kill-switches.md`'s first
/// promise forbids. The cause was not a decode failure and no counting could have caught it: a row
/// the venue never SENT is uncountable, so `unreadable == 0`, `replace_all` marked the book
/// AUTHORITATIVE, `opposing_available` returned `0`, and `PositionEvidence::fetched(0.0)` turned
/// that absence into `Flat`.
///
/// The cure is that evidence is now COVERAGE, not decodability: a book may only speak for symbols
/// the venue positively reported a position in (`crates/bridges/ctrader/src/positions.rs`'s
/// `PositionBook::unauthoritative_for`). ⚠ **MEASURED both ways** (the CI box, 2026-08-08, lane a):
/// disable that method's COVERAGE arm, so provenance alone decides again, and this binary is back to
/// `17 tests run: 15 passed, 2 failed` — THIS test and
/// [`verify_admits_a_mistagged_entry_on_a_symbol_the_venue_reported_nothing_about`], its
/// indistinguishable twin — plus `24 tests run: 22 passed, 2 failed` in `positions`'s own suite.
#[test]
fn verify_admits_the_exit_from_a_position_the_reconcile_answer_omitted() {
    let (server, mut exec, mut rx, sentinel, _halt_dir) = rig_on(
        "verify-omitted",
        FakeCtrader::start_close_preseeded_omitted_from_reconcile(&[(1, 100_000)]),
        HaltAdmit::Verify,
    );

    std::fs::write(&sentinel, b"").expect("engage the sentinel");
    exec.submit(&reduce("exit-an-omitted-position", -1, 1000.0));

    let got = collect(&mut rx, 2);
    // Printed so the probe REPORTS rather than only asserts: under `--nocapture` this line IS the
    // reproduction, and it is what a reviewer compares against the refusal that used to be here
    // (`kinds = ["Submitted", "Rejected"]`, `NEW_ORDER_REQ = 0`).
    eprintln!(
        "TRAP PROBE (position omitted from RECONCILE_RES): kinds = {:?}, NEW_ORDER_REQ = {}",
        kinds(&got),
        server.count_seen("NEW_ORDER_REQ")
    );
    assert!(
        !kinds(&got).contains(&"Rejected"),
        "the venue is HOLDING a 1000-unit long and simply did not mention it in its reconcile \
         answer. Refusing this exit traps the operator in a live position, under a halt — the one \
         thing halting must never do. An absence in a fetched book is still an absence. events: \
         {got:?}"
    );
    server.wait_until_count_at_least("NEW_ORDER_REQ", 1, Duration::from_secs(5));
    assert_eq!(
        server.count_seen("NEW_ORDER_REQ"),
        1,
        "and it must actually have reached the venue"
    );

    drop(exec);
}

/// The same trap reached through a DIFFERENT door, and the one that made it more than theoretical:
/// `RECONCILE_RES` carries a `ctidTraderAccountId`, and nothing checked it. A reconcile answer for
/// ANOTHER (flat) account was folded in as authoritative for ours — wiping the routing entries and
/// manufacturing the same `Flat`.
///
/// ⚠ **MEASURED as a REFUSAL before the fix** (the CI box, 2026-08-08, lane a):
/// `kinds = ["Submitted", "Rejected"]`, `NEW_ORDER_REQ seen = 0`.
///
/// ⚠ **This test is now defended IN DEPTH, and saying which layer catches it matters.** Deleting the
/// `res.ctid_trader_account_id != ctid` guard in `crates/bridges/ctrader/src/conn.rs`'s
/// `rebuild_position_book` leaves this binary GREEN (`17 tests run: 17 passed`, measured) — because
/// the COVERAGE rule catches the same frame a second time: the stranger's answer is empty, so the
/// book ends up holding no row for this symbol and refuses to speak for it regardless. The guard's
/// own proof is therefore the unit test that isolates it,
/// `conn::tests::a_reconcile_answer_for_another_ctid_is_discarded_and_clears_the_evidence`
/// (`10 tests run: 9 passed, 1 failed` under that mutation). This test's job is the END-TO-END
/// property — the exit gets out — not the attribution, and it is kept because a future change that
/// weakened EITHER layer alone would leave the other holding, while weakening both would land here.
#[test]
fn a_reconcile_answer_for_another_account_is_not_this_accounts_evidence() {
    let (server, mut exec, mut rx, sentinel, _halt_dir) = rig_on(
        "verify-wrong-ctid",
        FakeCtrader::start_close_preseeded_reconcile_for_another_account(&[(1, 100_000)]),
        HaltAdmit::Verify,
    );

    std::fs::write(&sentinel, b"").expect("engage the sentinel");
    exec.submit(&reduce("exit-under-a-strangers-book", -1, 1000.0));

    let got = collect(&mut rx, 2);
    eprintln!(
        "TRAP PROBE (RECONCILE_RES for another ctid): kinds = {:?}, NEW_ORDER_REQ = {}",
        kinds(&got),
        server.count_seen("NEW_ORDER_REQ")
    );
    assert!(
        !kinds(&got).contains(&"Rejected"),
        "a RECONCILE_RES stamped with somebody else's ctidTraderAccountId says NOTHING about this \
         account — folding it in and calling the result authoritative refuses this account's exit \
         on a stranger's emptiness. events: {got:?}"
    );
    server.wait_until_count_at_least("NEW_ORDER_REQ", 1, Duration::from_secs(5));
    assert_eq!(server.count_seen("NEW_ORDER_REQ"), 1);

    drop(exec);
}

/// `verify` cannot touch the half of the rule the BOOK proves. A plain opposite-side order that
/// routes to `ClosePlan::Close` is admitted under both modes — `closes` short-circuits, because a
/// routing decision that produced close legs is the strongest evidence in existence that the order
/// reduces. This is what keeps `verify` a SUBSET of `admit` end to end rather than only at the
/// predicate.
#[test]
fn verify_still_admits_what_the_position_book_proves_closes() {
    let (server, mut exec, mut rx, sentinel, _halt_dir) =
        rig_on("verify-close", FakeCtrader::start_close_scripted(), HaltAdmit::Verify);

    exec.submit(&market("open-v", 1, 1000.0));
    wait_open_filled(&mut rx, "open-v");

    std::fs::write(&sentinel, b"").expect("engage the sentinel");
    exec.submit(&market("flat-v", -1, 1000.0)); // plain SELL — no flag, but it CLOSES

    let got = collect(&mut rx, 4);
    assert_eq!(
        kinds(&got),
        vec!["Submitted", "Accepted", "Fill", "Filled"],
        "the tightened mode must not reach the close route — halting still cannot trap you. \
         events: {got:?}"
    );
    assert_eq!(
        server.wait_for_closes(1, Duration::from_secs(5)),
        vec![(CLOSE_POSITION_ID_BASE, 100_000)],
        "it went out as a CLOSE_POSITION_REQ for the tracked position"
    );

    drop(exec);
}

/// An UNENGAGED sentinel changes nothing: the byte-identical control, so every refusal above is
/// attributable to the FILE and not to the arm having broken the ordinary path.
#[test]
fn an_unengaged_sentinel_leaves_the_client_byte_identical() {
    let (server, mut exec, mut rx, sentinel, _halt_dir) = rig("disengaged");
    assert!(!sentinel.exists(), "the sentinel must NOT exist for this control");

    exec.submit(&market("open-5", 1, 1000.0));
    wait_open_filled(&mut rx, "open-5");
    exec.submit(&market("flat-5", -1, 1000.0));

    let got = collect(&mut rx, 4);
    assert_eq!(kinds(&got), vec!["Submitted", "Accepted", "Fill", "Filled"], "events: {got:?}");
    assert_eq!(server.count_seen("NEW_ORDER_REQ"), 1);
    assert_eq!(server.wait_for_closes(1, Duration::from_secs(5)).len(), 1);

    drop(exec);
}
