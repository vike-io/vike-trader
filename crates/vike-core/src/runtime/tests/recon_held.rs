//! Runtime-wiring tests for the held-alert STORE bound — the on-`CoreThread` half of
//! [`super::recon_held`] (whose pure identity/announcement logic is unit-tested in that module).
//!
//! # The defect these gate
//!
//! `CoreThread::reconcile_reports` refreshed a held row in place only when the alert carried a
//! `vike_exec::recon::ReconAlert::dedup_key`. Everything else APPENDED a row per pass with a fresh
//! monotonic id, and nothing in this tree caps, prunes or retains `recon_alerts` —
//! `IndexMap::shift_remove` fires only on an operator confirm. So under
//! `VIKE_RECONCILE_POLICY=quarantine`, a divergence nothing heals grew the store forever. Measured
//! on the live the CI box box 2026-08-25: alert id **337 at 00:00 → 815 at 06:59** (daemon up since
//! 08-24 18:24), ~65 rows/hour ≈ **1,560/day**, all describing the same TWO divergences
//! (`PositionOnlyExternal` + `MissingFill`) — each row holding its own `proposed_events` and each
//! projected into every published `CoreSnapshot` by `CoreThread::recon_block`.
//!
//! These drive a synchronous `CoreThread` (no OS thread) through `reconcile_reports` directly — the
//! same call `Command::ReconcileReports` makes — with the exact the CI box shape: an un-keyed
//! `PositionOnlyExternal` whose synthesized legs re-mint their `ts`/`trade_id`/`client_order_id`
//! every pass. Zero wall-clock sleeps; the pass clock is an injected `ts`. White-box for the
//! reasons `crates/vike-core/src/runtime/tests/route_key.rs` states — `reconcile_reports`,
//! `confirm_recon` and the `recon_alerts` store are private to `super` and are precisely what is
//! under test.

use super::*;
use tracing_test::traced_test;
use vike_exec::testing::RecordingClient;
use vike_exec::{Account, RiskGate};
use vike_model::RiskLimits;

// The runtime's shared white-box assembly; this file's own bybit/`1_000` engine is built below.
use crate::runtime::test_support::core_of;

const VENUE: &str = "bybit";
const SYMBOL: &str = "BTCUSDT";
/// One reconcile pass's worth of clock (the `VIKE_RECONCILE_INTERVAL_MS` default).
const PASS_MS: i64 = 60_000;

/// A single-venue synchronous core. The `Account` is labelled with the canonical venue because
/// `Account::apply_fill` refuses a fill whose `venue` is not `self.venue`, and a reconcile pass's synthesized
/// legs carry the venue off the venue's own position report.
fn core() -> CoreThread<RecordingClient> {
    let engine = ExecutionEngine::new(
        Account::new(1_000.0, VENUE, None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        VENUE,
        SYMBOL,
    );
    core_of(engine, Vec::new(), CoreConfig::default())
}

/// A venue position row with NO local counterpart ⇒ `diff` emits `PositionOnlyExternal`, which
/// `resolve` holds UN-KEYED (`dedup_key: None`) with synthesized
/// position legs. The the CI box divergence, exactly.
fn position(symbol: &str, qty: f64, ts: i64) -> vike_model::PositionStatusReport {
    vike_model::PositionStatusReport {
        venue: VENUE.into(),
        symbol: symbol.into(),
        position_side: vike_model::events::PositionSide::Both,
        qty,
        avg_px: 100.0,
        ts,
        margin_mode: Default::default(),
        isolated_margin: None,
        delta: None,
    }
}

/// One pass's reports under the `quarantine` policy the CI box runs (`default: Quarantine`, empty
/// `per_kind` — nothing folds, everything is held for an operator).
fn pass(positions: Vec<vike_model::PositionStatusReport>) -> ReconcileReports {
    ReconcileReports {
        venue: VENUE.into(),
        since: 0,
        orders: Vec::new(),
        fills: Vec::new(),
        positions,
        policy: vike_exec::recon::ReconPolicy {
            default: vike_exec::recon::ReconMode::Quarantine,
            ..Default::default()
        },
        balance: None,
        generate_missing_orders: false,
        reconcile_balance: false,
        balance_tol: vike_exec::recon::BalanceTol::default(),
        route_key: None,
        admitted_symbols: None,
    }
}

/// Signed `BOTH` size the engine holds for `symbol`.
fn held_qty(core: &CoreThread<RecordingClient>, symbol: &str) -> f64 {
    core.eng(0)
        .account
        .positions
        .get(&(
            ustr::Ustr::from(VENUE),
            ustr::Ustr::from(symbol),
            vike_model::events::PositionSide::Both,
        ))
        .map(|p| p.size)
        .unwrap_or(0.0)
}

/// Overwrite the engine's signed `BOTH` size for `symbol` — a stand-in for "a fill landed since".
fn set_local_qty(core: &mut CoreThread<RecordingClient>, symbol: &str, qty: f64) {
    core.eng_mut(0)
        .account
        .positions
        .get_mut(&(
            ustr::Ustr::from(VENUE),
            ustr::Ustr::from(symbol),
            vike_model::events::PositionSide::Both,
        ))
        .expect("the engine holds the position")
        .size = qty;
}

/// How many recent-events ring lines mention the held-alert store.
fn recon_notes(core: &CoreThread<RecordingClient>) -> usize {
    core.recent.iter().filter(|l| l.contains("RECON alert")).count()
}

// -------------------------------------------------------------------------------------------
// THE PREMISE — without this, every assertion below could pass vacuously.
// -------------------------------------------------------------------------------------------

/// The divergence under test really is the UN-KEYED shape whose rows used to accumulate, and its
/// proposed events really do churn every pass. If `vike_exec` ever gives `PositionOnlyExternal` a
/// `dedup_key`, or stops baking the pass clock into its legs, this test says so instead of letting
/// the bound tests below start proving nothing.
#[test]
fn the_divergence_under_test_is_unkeyed_and_its_payload_churns_every_pass() {
    let mut c = core();
    c.reconcile_reports(pass(vec![position(SYMBOL, 0.5, 1_000)]));
    assert_eq!(c.recon_alerts.len(), 1, "held, not folded — the precondition for everything below");
    let (_, first) = c.recon_alerts.first().expect("one row");
    assert_eq!(first.kind, DivergenceKind::PositionOnlyExternal);
    // UN-KEYED, asserted on what the STORED row can actually show. The dedup key does not travel
    // onto `HeldReconAlert`, so the original line established this through the detail STRING — true
    // then, because the alert path fell into a `format!("{:?}", d.kind())` fallback. That fallback
    // is gone (`Divergence::describe`), and the observable that replaces it is the identity's own
    // `instance`: for a KEYED alert it IS the key; for an un-keyed one it is derived from the
    // churn-free `identity_detail` plus legs.
    assert!(
        first.identity.instance.contains(SYMBOL),
        "un-keyed: the instance is derived, not a bare key: {}",
        first.identity.instance
    );
    // ⚠ AND THE IDENTITY CARRIES NO QUANTITY. This is the line that would have caught the hazard
    // `Divergence::describe` introduced: a detail carrying `qty=`/`avg_px=` becomes the identity of
    // an un-keyed alert, so a drifting position turns into a NEW row every pass — the 60-rows-an-hour,
    // ~1,560-a-day bug this whole file exists to prevent. The fixture holds qty constant, so nothing
    // else here would have noticed.
    assert!(
        !first.identity.instance.contains("qty="),
        "the identity must carry no quantity: {}",
        first.identity.instance
    );
    assert!(
        first.detail.contains(SYMBOL),
        "...and the detail names the instrument, so a held row is actionable: {}",
        first.detail
    );
    let before = first.proposed_events.clone();
    assert!(!before.is_empty(), "the alert proposes events an operator can confirm");

    c.reconcile_reports(pass(vec![position(SYMBOL, 0.5, 1_000 + PASS_MS)]));
    let (_, after) = c.recon_alerts.first().expect("still one row");
    assert_ne!(
        before, after.proposed_events,
        "the payload MUST churn pass to pass — that churn is why the identity excludes ts/trade_id"
    );
}

// -------------------------------------------------------------------------------------------
// THE BOUND — the money test.
// -------------------------------------------------------------------------------------------

/// 60 passes (an hour at the default interval) of ONE unchanged external position leave ONE row,
/// under ONE confirm id. Before this change that was 60 rows and 60 ids; on the CI box it was ~1,560 a
/// day.
#[test]
fn an_unchanged_unkeyed_divergence_holds_exactly_one_row_across_many_passes() {
    let mut c = core();
    for p in 0..60 {
        c.reconcile_reports(pass(vec![position(SYMBOL, 0.5, 1_000 + p * PASS_MS)]));
        assert_eq!(c.recon_alerts.len(), 1, "pass {p}: the store must not grow");
    }
    let (&id, row) = c.recon_alerts.first().expect("one row");
    assert_eq!(
        id,
        row.identity.confirm_id(),
        "…and its id is its identity's digest, pass after pass"
    );
}

/// The ring is not where the repetition moved to. A refresh whose only difference is the churned
/// payload writes NO note — otherwise the per-minute noise this batch removes from the log would
/// reappear in the bounded recent-events ring and flush real events out of it.
#[test]
fn a_recurring_pass_adds_no_ring_notes() {
    let mut c = core();
    c.reconcile_reports(pass(vec![position(SYMBOL, 0.5, 1_000)]));
    let after_first = recon_notes(&c);
    assert_eq!(after_first, 1, "the RAISE is worth exactly one note");
    for p in 1..30 {
        c.reconcile_reports(pass(vec![position(SYMBOL, 0.5, 1_000 + p * PASS_MS)]));
    }
    assert_eq!(recon_notes(&c), after_first, "29 more passes, no more notes");
}

/// The refreshed row carries the LATEST view of the divergence, not the one from the pass that
/// raised it — a confirm minutes later must fold fresh events, which is the whole reason the
/// payload is refreshed unconditionally while the NOTE is not.
#[test]
fn the_surviving_row_is_refreshed_with_the_latest_payload() {
    let mut c = core();
    c.reconcile_reports(pass(vec![position(SYMBOL, 0.5, 1_000)]));
    c.reconcile_reports(pass(vec![position(SYMBOL, 0.5, 1_000 + 40 * PASS_MS)]));
    let (_, row) = c.recon_alerts.first().expect("one row");
    let latest_ts = row
        .proposed_events
        .iter()
        .filter_map(|e| match e {
            Event::Fill(f) => Some(f.ts),
            _ => None,
        })
        .max()
        .expect("a synthesized leg");
    assert_eq!(latest_ts, 1_000 + 40 * PASS_MS, "the row holds the LATEST pass's events");
}

// -------------------------------------------------------------------------------------------
// …AND A GENUINELY NEW DIVERGENCE STILL GETS ITS OWN ROW.
// -------------------------------------------------------------------------------------------

/// The shape the collapse must never swallow: two same-kind, same-venue divergences differing only
/// in symbol. Both `detail`s are the bare kind name, so only the instrument legs tell them apart.
#[test]
fn a_second_symbol_gets_its_own_row_and_its_own_confirm_id() {
    let mut c = core();
    for p in 0..5 {
        c.reconcile_reports(pass(vec![position(SYMBOL, 0.5, 1_000 + p * PASS_MS)]));
    }
    assert_eq!(c.recon_alerts.len(), 1);

    for p in 5..10 {
        let ts = 1_000 + p * PASS_MS;
        c.reconcile_reports(pass(vec![position(SYMBOL, 0.5, ts), position("ETHUSDT", 3.0, ts)]));
    }
    assert_eq!(
        c.recon_alerts.len(),
        2,
        "the newcomer gets its OWN row, not a refresh of the first"
    );
    let ids = alert_ids(&c);
    assert_ne!(ids[0], ids[1], "…and its own confirm id");
}

/// A changed position SIZE is a different divergence, so it lands as its own row (its own id)
/// rather than silently overwriting the one an operator may already be looking at - and the 0.5
/// row, which the position-carrying pass covered and no longer raises, is cleared.
#[test]
fn a_changed_size_is_a_new_row_and_the_old_one_is_cleared() {
    let mut c = core();
    c.reconcile_reports(pass(vec![position(SYMBOL, 0.5, 1_000)]));
    let old = alert_ids(&c)[0];
    c.reconcile_reports(pass(vec![position(SYMBOL, 0.9, 1_000 + PASS_MS)]));
    let now = alert_ids(&c);
    assert_eq!(now.len(), 1, "0.9 is raised; the stale 0.5 row is gone");
    assert_ne!(now[0], old, "0.5 and 0.9 are different divergences with different ids");
}

// -------------------------------------------------------------------------------------------
// CONFIRM — the surface the collapse changes.
// -------------------------------------------------------------------------------------------

/// ⚠ THE SEMANTIC CHANGE, pinned. After 20 passes there is ONE row, and ONE `ConfirmRecon`
/// resolves the divergence completely — where before it took 20 confirms, 19 of which would have
/// re-folded stale snapshots of the SAME external position (each synthesizing its own leg, so the
/// book would have booked the position 20 times over). The single confirm folds the position once.
#[test]
fn one_confirm_resolves_a_divergence_that_previously_needed_one_per_pass() {
    let mut c = core();
    for p in 0..20 {
        c.reconcile_reports(pass(vec![position(SYMBOL, 0.5, 1_000 + p * PASS_MS)]));
    }
    assert_eq!(c.recon_alerts.len(), 1, "20 passes, one row");
    assert_eq!(held_qty(&c, SYMBOL), 0.0, "nothing folds before the confirm");

    let (&id, _) = c.recon_alerts.first().expect("one row");
    c.confirm_recon(id);

    assert_eq!(held_qty(&c, SYMBOL), 0.5, "the confirm folds the external position ONCE");
    assert!(c.recon_alerts.is_empty(), "…and the store is empty, with nothing left to re-confirm");
}

/// A confirm that does NOT resolve the divergence re-raises a row AND re-announces —
/// `confirm_recon` forgets the same identity the row was stored under, so the two can never fall
/// out of step and an operator cannot act, see nothing, and be left believing it took.
///
/// The divergence used is `OrphanLocalPosition`, whose confirm DROPS the local row
/// (`CoreThread::confirm_recon`) only while the engine still holds the qty the pass saw. Here a
/// "strategy fill" moves it between the pass and the confirm, so the confirm is guaranteed not to
/// resolve the divergence. That is the premise, and it is asserted rather than assumed.
#[test]
fn a_confirm_that_does_not_resolve_it_re_raises_a_fresh_row() {
    let mut c = core();
    // Give the local book a position, by confirming an external one the venue really reported.
    c.reconcile_reports(pass(vec![position(SYMBOL, 0.5, 1_000)]));
    let (&id, _) = c.recon_alerts.first().expect("one row");
    c.confirm_recon(id);
    assert_eq!(held_qty(&c, SYMBOL), 0.5, "local now holds it");
    assert!(c.recon_alerts.is_empty());

    // Now the venue reports a DIFFERENT symbol only, so local's BTC has no venue row to compare
    // against: `OrphanLocalPosition`, held and event-free.
    let ts = 1_000 + PASS_MS;
    c.reconcile_reports(pass(vec![position("ETHUSDT", 3.0, ts)]));
    let (&orphan, row) = c
        .recon_alerts
        .iter()
        .find(|(_, h)| h.kind == DivergenceKind::OrphanLocalPosition)
        .expect("the local BTC position is now orphaned");
    assert!(row.proposed_events.is_empty(), "PREMISE: it carries no events to fold");

    // The book moves after the pass (a strategy fill): the row now describes a book that is gone.
    set_local_qty(&mut c, SYMBOL, 0.7);
    c.confirm_recon(orphan);
    assert_eq!(held_qty(&c, SYMBOL), 0.7, "…and indeed it dropped nothing — still orphaned");
    assert_eq!(
        c.recon_announce.held(VENUE),
        c.recon_alerts.len(),
        "the identity was forgotten in step with its row"
    );

    // The next pass still finds it, so it must be re-raised AND re-announced.
    let ts = 1_000 + 2 * PASS_MS;
    c.reconcile_reports(pass(vec![position("ETHUSDT", 3.0, ts)]));
    assert!(
        c.recon_alerts.values().any(|h| h.kind == DivergenceKind::OrphanLocalPosition),
        "re-raised, not swallowed"
    );
    assert_eq!(c.recon_announce.held(VENUE), c.recon_alerts.len(), "…and re-announced");
}

// -------------------------------------------------------------------------------------------
// THE INVARIANT — the store and the summary count the same thing.
// -------------------------------------------------------------------------------------------

/// One identity, one row: the periodic summary's backlog count and the store size AGREE for a set
/// the venue keeps reporting. Before the collapse the summary said 2 while the store held hundreds,
/// which is precisely how the leak stayed invisible once the per-pass WARN was silenced.
#[test]
fn the_store_size_and_the_announcer_count_agree() {
    let mut c = core();
    for p in 0..40 {
        let ts = 1_000 + p * PASS_MS;
        c.reconcile_reports(pass(vec![position(SYMBOL, 0.5, ts), position("ETHUSDT", 3.0, ts)]));
        assert_eq!(
            c.recon_alerts.len(),
            c.recon_announce.held(VENUE),
            "pass {p}: the store and the backlog count must not diverge"
        );
    }
    assert_eq!(c.recon_alerts.len(), 2, "two divergences, two rows, forty passes");
}

/// ⚠ The ONE place they deliberately disagree, pinned so it is a decision rather than a surprise:
/// a divergence the venue STOPS reporting is dropped by the announcer (it is no longer held) but
/// its ROW SURVIVES for the operator.
///
/// Evicting it would bound the store harder and was rejected: `MissingFill`'s proposed events stay
/// valid after the fill ages out of the reconcile LOOKBACK WINDOW, and the venue silently ceasing
/// to report it is exactly what that ageing looks like from here — so eviction-on-clear would
/// destroy the operator's only route to booking a fill that really happened. The store is bounded
/// by distinct divergence IDENTITIES; that is the bound this change buys.
#[test]
fn a_divergence_the_venue_stops_reporting_keeps_its_row_for_the_operator() {
    let mut c = core();
    c.reconcile_reports(pass(vec![position(SYMBOL, 0.5, 1_000)]));
    assert_eq!(c.recon_alerts.len(), 1);

    c.reconcile_reports(pass(Vec::new()));
    assert_eq!(c.recon_announce.held(VENUE), 0, "the announcer no longer holds it");
    assert_eq!(c.recon_alerts.len(), 1, "…but the operator's row, and its confirm id, survive");

    // …and it does not re-append on every later empty pass either.
    for _ in 0..10 {
        c.reconcile_reports(pass(Vec::new()));
    }
    assert_eq!(c.recon_alerts.len(), 1, "an empty pass raises nothing at all");
}

// -------------------------------------------------------------------------------------------
// THE PER-PASS LINE — the last per-pass repetition, and the one that floods the JOURNAL.
// -------------------------------------------------------------------------------------------
//
// `CoreThread::reconcile_reports` closes with an INFO `reconcile pass folded venue=… events=…
// alerts=…`, emitted on every pass carrying ANY alert. Under `quarantine` that is every pass
// forever: measured on the live the CI box box 2026-08-25, `events=0 alerts=1` at 11:41:43 and again at
// 11:42:43, identical — one such line a minute per venue, saying only "the same divergence is
// still held", which the announcer above already reports properly. It is INFO, so on a daemon
// running a `warn` file level it never reaches the log FILE and floods the journal instead:
// lower stakes than the WARN flood, which is why it shipped one release later, and the same shape.
//
// These tests assert HOW MANY lines a run emits, never their fields or phrasing. `PASS_LINE` is
// the single text anchor; re-wording the payload cannot redden them, and the mutation that must
// redden them is restoring the unconditional emission.

/// The one text anchor. `logs_assert` (from `#[traced_test]`) hands us this test's own log lines —
/// its span name scopes them, so a sibling test running in parallel cannot contribute.
const PASS_LINE: &str = "reconcile pass folded";

/// The announcer's periodic backlog line, counted alongside the pass line to prove the two ride
/// ONE clock rather than two rate limiters that happen to agree today.
const SUMMARY_LINE: &str = "reconcile divergences still HELD";

fn count(lines: &[&str], needle: &str) -> usize {
    lines.iter().filter(|l| l.contains(needle)).count()
}

/// A pass under `synthesize`, which folds every divergence immediately and holds NONE — the way a
/// pass that genuinely DID something is staged below. Everything else is [`pass`]'s shape.
fn folding_pass(positions: Vec<vike_model::PositionStatusReport>) -> ReconcileReports {
    ReconcileReports {
        policy: vike_exec::recon::ReconPolicy {
            default: vike_exec::recon::ReconMode::Synthesize,
            ..Default::default()
        },
        ..pass(positions)
    }
}

/// Advance the core clock and run one pass, the way a live interval pass arrives: the announcer
/// reads `engine.now_ms`, so a test that left it at zero could never reach the periodic cadence.
fn tick(c: &mut CoreThread<RecordingClient>, ts: i64, reports: ReconcileReports) {
    c.engine.now_ms = ts;
    c.reconcile_reports(reports);
}

/// A pass that FOLDED something is news EVERY time and must never be suppressed — those events
/// changed the book, and three of them in a row are three separate things that happened.
///
/// This is also the mutation guard in the other direction: a "fix" that gated the line on the held
/// set alone would silence a folding pass entirely, and this test is what says so.
#[traced_test]
#[test]
fn every_pass_that_folds_events_logs() {
    let mut c = core();
    for p in 0..3 {
        let ts = 1_000 + p * PASS_MS;
        // A GROWING venue position: pass 0 is `PositionOnlyExternal`, 1 and 2 are `PositionDrift`,
        // and under `synthesize` all three fold.
        tick(&mut c, ts, folding_pass(vec![position(SYMBOL, 0.5 * (p + 1) as f64, ts)]));
    }
    assert_eq!(held_qty(&c, SYMBOL), 1.5, "PREMISE: every pass really folded — not a quiet run");
    assert!(c.recon_alerts.is_empty(), "PREMISE: synthesize holds nothing at all");
    logs_assert(|lines: &[&str]| match count(lines, PASS_LINE) {
        3 => Ok(()),
        n => Err(format!("a pass that folded events must always log: wanted 3, got {n}")),
    });
}

/// ⚠ THE MEASURED DEFECT, as an assertion. An hour of the the CI box shape — ONE unchanged bybit
/// `PositionOnlyExternal`, held under `quarantine`, re-diffed every 60 s — logged a pass line on
/// every single pass. It must log exactly once: the raise. The 59 repetitions after it carry no
/// information the first one did not.
#[traced_test]
#[test]
fn an_unchanged_held_divergence_logs_one_pass_line_not_one_per_pass() {
    let mut c = core();
    let passes: i64 = 60;
    for p in 0..passes {
        let ts = 1_000 + p * PASS_MS;
        tick(&mut c, ts, pass(vec![position(SYMBOL, 0.5, ts)]));
    }
    assert_eq!(c.recon_alerts.len(), 1, "PREMISE: one divergence, held on every pass");
    // The SPAN this run covers is one pass shorter than the pass count — it must stay strictly
    // inside the summary cadence, or the ONE expected line would be the raise plus a summary and
    // this test would be measuring the periodic line instead of the suppression.
    assert!(
        ((passes - 1) * PASS_MS) < super::recon_held::HELD_SUMMARY_INTERVAL_MS,
        "PREMISE: this run stays INSIDE the summary cadence, so the ONE line is the raise"
    );
    logs_assert(move |lines: &[&str]| match count(lines, PASS_LINE) {
        1 => Ok(()),
        n => Err(format!("{passes} identical passes must log once, got {n}")),
    });
}

/// A pass that CHANGED the held set logs even though it folded nothing — an arriving divergence
/// and a departing one are both real state changes, and suppressing them is the failure mode the
/// flood itself caused (the new divergence indistinguishable among its identical neighbours).
#[traced_test]
#[test]
fn a_pass_that_changes_the_held_set_logs_even_though_nothing_folded() {
    let mut c = core();
    // The raise, then nine repetitions of it.
    for p in 0..10 {
        let ts = 1_000 + p * PASS_MS;
        tick(&mut c, ts, pass(vec![position(SYMBOL, 0.5, ts)]));
    }
    // A SECOND symbol appears — quarantine folds nothing, but the set changed.
    let ts = 1_000 + 10 * PASS_MS;
    tick(&mut c, ts, pass(vec![position(SYMBOL, 0.5, ts), position("ETHUSDT", 3.0, ts)]));
    assert_eq!(c.recon_alerts.len(), 2, "PREMISE: the newcomer really is a second divergence");
    // ...and stops being reported. A clear is a change too.
    let ts = 1_000 + 11 * PASS_MS;
    tick(&mut c, ts, pass(vec![position(SYMBOL, 0.5, ts)]));
    // ...followed by more repetitions, which say nothing.
    for p in 12..20 {
        let ts = 1_000 + p * PASS_MS;
        tick(&mut c, ts, pass(vec![position(SYMBOL, 0.5, ts)]));
    }
    logs_assert(|lines: &[&str]| match count(lines, PASS_LINE) {
        3 => Ok(()),
        n => Err(format!("raise + arrival + clear = 3 lines over 20 passes, got {n}")),
    });
}

/// The backlog an operator still has to see: a DAY of the the CI box shape logs on the announcer's own
/// cadence, not the pass's — and every one of those lines sits beside the summary that armed it,
/// which is the assertion that there is ONE clock here rather than a second rate limiter.
#[traced_test]
#[test]
fn a_day_of_unchanged_passes_logs_on_the_summary_cadence() {
    let mut c = core();
    let passes: i64 = 24 * 60; // one day at the 60 s default interval
    for p in 0..passes {
        let ts = 1_000 + p * PASS_MS;
        tick(&mut c, ts, pass(vec![position(SYMBOL, 0.5, ts)]));
    }
    // The raise re-arms the summary clock, so a day yields one line per interval: the raise, then
    // a summary every interval after it.
    let expected = ((passes * PASS_MS) / super::recon_held::HELD_SUMMARY_INTERVAL_MS) as usize;
    logs_assert(move |lines: &[&str]| {
        let emitted = count(lines, PASS_LINE);
        let summaries = count(lines, SUMMARY_LINE);
        if emitted != expected {
            return Err(format!("a day of held passes: wanted {expected} lines, got {emitted}"));
        }
        if summaries + 1 != emitted {
            return Err(format!(
                "the periodic pass line must ride the announcer's summary clock: {emitted} pass \
                 lines vs {summaries} summaries"
            ));
        }
        if emitted * 50 >= passes as usize {
            return Err(format!("{emitted} lines is not an order of magnitude below {passes}"));
        }
        Ok(())
    });
}

// -------------------------------------------------------------------------------------------
// THE REFRESH PATH'S OWN ANNOUNCE, AND THE TWO LOG-ONLY LINES OF A PASS.
// -------------------------------------------------------------------------------------------
//
// Three lines `CoreThread::reconcile_reports` emits that nothing above counted:
//
// - the refresh path of `hold_recon_alerts` announces when the ANNOUNCER has forgotten an identity
//   whose ROW survived — a divergence the venue stopped reporting (cleared, row kept for the
//   operator) and then reports again. It must WARN once, against the row's EXISTING confirm id;
// - the Class E refusal's `tracing::error!` (the ring note beside it is pinned by
//   `crates/vike-tradehub-core/tests/recon/recon_per_account.rs`);
// - the folded balance crossing's `tracing::warn!` (the ring note beside it is pinned by
//   `crates/vike-core/tests/recon/recon_balance_crossing.rs`).
//
// Log lines only reach `logs_assert` from the test's own thread, which is why these are white-box:
// the integration twins in `crates/vike-core/tests/recon/recon_pass_branches.rs` drive a spawned
// core and can read only the snapshot. Counts and levels are asserted, never phrasing beyond the
// one text anchor per line.

/// `crate::runtime::recon_held`'s `warn_newly_held` message — the anchor for the WARN count.
const HELD_LINE: &str = "reconcile divergence HELD";
/// The refusal's `tracing::error!` message prefix.
const REFUSED_LINE: &str = "reconcile pass REFUSED";
/// The folded crossing's `tracing::warn!` message prefix.
const ABSORBED_LINE: &str = "cash reconcile ABSORBED";

/// Lines carrying `needle` that were emitted at `level` (the fmt layer prints `WARN`/`ERROR`).
fn count_at(lines: &[&str], level: &str, needle: &str) -> usize {
    lines.iter().filter(|l| l.contains(needle) && l.contains(level)).count()
}

/// The held-alert store's confirm ids, in insertion order.
fn alert_ids(c: &CoreThread<RecordingClient>) -> Vec<u64> {
    c.recon_alerts.keys().copied().collect()
}

/// A pass reporting only `balance`, with cash reconcile ON, under the policy an operator NAMES.
fn balance_pass(policy: &str, balance: f64) -> ReconcileReports {
    ReconcileReports {
        policy: vike_exec::recon::ReconPolicy::from_policy_name(policy)
            .expect("an operator-nameable policy"),
        balance: Some(balance),
        reconcile_balance: true,
        ..pass(Vec::new())
    }
}

/// A held divergence that CLEARS and RETURNS is announced again — once, against its surviving row's
/// confirm id — while the plain repetition before it says nothing. A new identity beside it gets
/// its own id and its own WARN.
///
/// Three WARNs in all: the raise (the first row's id), the return (the same id again, through the
/// refresh path), and the newcomer (the second row's id). Dropping the refresh path's announce leaves two; announcing
/// every refresh makes five.
#[traced_test]
#[test]
fn a_divergence_that_returns_after_clearing_is_announced_again_against_its_surviving_row() {
    let mut c = core();
    let at = |p: i64| 1_000 + p * PASS_MS;
    tick(&mut c, at(0), pass(vec![position(SYMBOL, 0.5, at(0))])); // the raise
    tick(&mut c, at(1), pass(vec![position(SYMBOL, 0.5, at(1))])); // a repetition
    tick(&mut c, at(2), pass(Vec::new())); // the venue stops reporting it
    assert_eq!(c.recon_announce.held(VENUE), 0, "PREMISE: the announcer forgot the identity...");
    let first = alert_ids(&c);
    assert_eq!(first.len(), 1, "PREMISE: ...while its row survived for the operator");

    tick(&mut c, at(3), pass(vec![position(SYMBOL, 0.5, at(3))])); // it returns
    assert_eq!(alert_ids(&c), first, "the return refreshes the surviving row in place, same id");
    assert_eq!(recon_notes(&c), 1, "...and an unchanged detail writes no ring note");

    let both = vec![position(SYMBOL, 0.5, at(4)), position("ETHUSDT", 3.0, at(4))];
    tick(&mut c, at(4), pass(both)); // a new identity beside it
    let ids = alert_ids(&c);
    assert_eq!((ids.len(), ids[0]), (2, first[0]), "the newcomer gets its own row and id");
    assert_eq!(recon_notes(&c), 2, "...and its own raise note");

    // The ids are digests now, so the two rows are told apart by the id each was held under.
    let (id1, id2) = (format!("alert={} ", ids[0]), format!("alert={} ", ids[1]));
    logs_assert(|lines: &[&str]| {
        let held: Vec<&str> = lines.iter().copied().filter(|l| l.contains(HELD_LINE)).collect();
        let warned = count_at(&held, "WARN", HELD_LINE);
        let row1 = held.iter().filter(|l| l.contains(id1.as_str())).count();
        let row2 = held.iter().filter(|l| l.contains(id2.as_str())).count();
        match (held.len(), warned, row1, row2) {
            (3, 3, 2, 1) => Ok(()),
            got => Err(format!(
                "wanted 3 HELD WARNs (the first row twice, the second once), got (lines, warn, \
                 #1, #2) = {got:?}: {held:#?}"
            )),
        }
    });
}

/// The Class E refusal logs ONE `error!` — a pass that names no account on a venue this process
/// runs two engines of folds nothing, and says so at ERROR, where an operator's alerting looks.
#[traced_test]
#[test]
fn a_pass_naming_no_account_on_a_two_account_venue_logs_one_refusal_error() {
    let engine = |route_key: &str| {
        let mut e = ExecutionEngine::new(
            Account::new(1_000.0, VENUE, None, BalanceMode::Delta),
            RiskGate::new(RiskLimits::new()),
            RecordingClient::default(),
            VENUE,
            SYMBOL,
        );
        e.route_key = route_key.to_string();
        e
    };
    let mut c = core_of(engine(VENUE), vec![(0.0, engine("bybit-sub2"))], CoreConfig::default());
    let reports = pass(vec![position(SYMBOL, 0.5, 1_000)]);
    assert!(reports.route_key.is_none(), "PREMISE: the pass names no account");

    c.reconcile_reports(reports);
    assert!(
        c.recon_alerts.is_empty(),
        "REFUSED: quarantine would otherwise have held the position"
    );
    assert_eq!(held_qty(&c, SYMBOL), 0.0, "...and nothing folded");
    assert_eq!(c.recent.iter().filter(|l| l.starts_with("RECON refused")).count(), 1);

    logs_assert(|lines: &[&str]| {
        match (count(lines, REFUSED_LINE), count_at(lines, "ERROR", REFUSED_LINE)) {
            (1, 1) => Ok(()),
            got => Err(format!("wanted one refusal line, at ERROR; got (lines, errors) = {got:?}")),
        }
    });
}

/// A balance crossing under a FOLDING policy WARNs once, beside its ring note, and an agreeing pass
/// after it adds no second line: the folded `AccountState` re-anchors, so the crossing is one-shot.
#[traced_test]
#[test]
fn a_folded_balance_crossing_warns_once() {
    let mut c = core();
    tick(&mut c, 1_000, balance_pass("synthesize", 9_999.0)); // the first sync adopts
    tick(&mut c, 1_000 + PASS_MS, balance_pass("synthesize", 10_250.0)); // crosses, folds
    tick(&mut c, 1_000 + 2 * PASS_MS, balance_pass("synthesize", 10_250.0)); // agrees
    assert!(c.recon_alerts.is_empty(), "PREMISE: synthesize holds nothing");
    assert_eq!(
        c.recent.iter().filter(|l| l.contains("absorbed")).count(),
        1,
        "PREMISE: the ring note fired, once"
    );

    logs_assert(|lines: &[&str]| {
        match (count(lines, ABSORBED_LINE), count_at(lines, "WARN", ABSORBED_LINE)) {
            (1, 1) => Ok(()),
            got => Err(format!("wanted one ABSORBED line, at WARN; got (lines, warns) = {got:?}")),
        }
    });
}

/// The same crossing under a policy that HOLDS a `BalanceDrift`: the log says HELD (once, through
/// the announcer) and never ABSORBED — the absorption line is read off the resolved alerts, so a
/// held drift suppresses it.
#[traced_test]
#[test]
fn a_held_balance_crossing_logs_the_hold_and_no_absorption() {
    let mut c = core();
    tick(&mut c, 1_000, balance_pass("hybrid", 9_999.0)); // the first sync adopts
    tick(&mut c, 1_000 + PASS_MS, balance_pass("hybrid", 10_250.0)); // crosses, held
    let kinds: Vec<DivergenceKind> = c.recon_alerts.values().map(|h| h.kind).collect();
    assert_eq!(kinds, vec![DivergenceKind::BalanceDrift], "PREMISE: hybrid holds the drift");

    logs_assert(|lines: &[&str]| {
        let held_drift = lines
            .iter()
            .filter(|l| l.contains(HELD_LINE) && l.contains("kind=BalanceDrift"))
            .count();
        match (count(lines, ABSORBED_LINE), held_drift) {
            (0, 1) => Ok(()),
            got => Err(format!("wanted no ABSORBED and one HELD BalanceDrift; got {got:?}")),
        }
    });
}

// -------------------------------------------------------------------------------------------
// THE OPERATOR VERB'S FOLD HALF — what `trade recon confirm <id>` makes each held kind do.
// -------------------------------------------------------------------------------------------
//
// `PositionOnlyExternal` and `PositionDrift` are pinned by
// `one_confirm_resolves_a_divergence_that_previously_needed_one_per_pass` above and by
// `tests/recon/recon_quarantine.rs`; the tests below pin the two kinds the verb had no way to
// resolve before it existed: the PHANTOM local position (`OrphanLocalPosition`) and the held cash
// drift, plus the evidence the pre-trade gate reads.

/// The the CI box shape (bybit#DEMO, 2026-10-17): the book holds a position the venue closed
/// externally. Confirming the `OrphanLocalPosition` row drops it, books a closing fill at the
/// position's OWN average price, and so moves no PnL, no cash and no fee; the divergence is gone
/// for good (the next pass finds nothing local to sweep).
#[test]
fn confirming_an_orphan_local_position_drops_it_and_books_nothing() {
    let mut c = core();
    // A local 0.5 BTC @ 100: the confirm of an external position the venue really reported.
    c.reconcile_reports(pass(vec![position(SYMBOL, 0.5, 1_000)]));
    let (&id, _) = c.recon_alerts.first().expect("one row");
    c.confirm_recon(id);
    assert_eq!(held_qty(&c, SYMBOL), 0.5, "PREMISE: local holds it");
    let acct = &c.eng(0).account;
    let (balance, realized, fees, trades) =
        (acct.balance, acct.realized_pnl, acct.fees_paid, acct.closed_pnls.len());

    // The venue closed it: its report names a DIFFERENT symbol only.
    c.reconcile_reports(pass(vec![position("ETHUSDT", 3.0, 1_000 + PASS_MS)]));
    let (&orphan, row) = c
        .recon_alerts
        .iter()
        .find(|(_, h)| h.kind == DivergenceKind::OrphanLocalPosition)
        .expect("PREMISE: the local BTC position is orphaned");
    assert!(row.orphan_position.is_some(), "the row names the local position a confirm drops");

    c.confirm_recon(orphan);

    assert_eq!(held_qty(&c, SYMBOL), 0.0, "the phantom is gone");
    let acct = &c.eng(0).account;
    assert_eq!(
        (acct.balance, acct.realized_pnl, acct.fees_paid),
        (balance, realized, fees),
        "closing at the position's own average price moves no cash, no PnL, no fee"
    );
    assert_eq!(acct.closed_pnls.len(), trades + 1, "the one residue: a flat 0.0 closed trade");
    assert_eq!(acct.closed_pnls.last().copied(), Some(0.0));
    assert!(
        c.recent.iter().any(|l| l.contains("dropped the local BTCUSDT")),
        "the confirm note says what was booked: {:?}",
        c.recent
    );

    // …and it does not come back: nothing local is left for the sweep to find.
    c.reconcile_reports(pass(vec![position("ETHUSDT", 3.0, 1_000 + 2 * PASS_MS)]));
    assert!(
        !c.recon_alerts.values().any(|h| h.kind == DivergenceKind::OrphanLocalPosition),
        "the dropped position must not be re-raised"
    );
}

/// The drop refuses to sweep away a book it has not seen: if the qty moved after the pass (a
/// strategy fill) the confirm drops NOTHING and says so, and the next pass re-raises the
/// divergence against the real number.
#[test]
fn a_confirm_after_the_book_moved_drops_nothing_and_says_so() {
    let mut c = core();
    c.reconcile_reports(pass(vec![position(SYMBOL, 0.5, 1_000)]));
    let (&id, _) = c.recon_alerts.first().expect("one row");
    c.confirm_recon(id);
    c.reconcile_reports(pass(vec![position("ETHUSDT", 3.0, 1_000 + PASS_MS)]));
    let (&orphan, _) = c
        .recon_alerts
        .iter()
        .find(|(_, h)| h.kind == DivergenceKind::OrphanLocalPosition)
        .expect("PREMISE: orphaned");

    set_local_qty(&mut c, SYMBOL, 0.7);
    c.confirm_recon(orphan);

    assert_eq!(held_qty(&c, SYMBOL), 0.7, "untouched");
    assert!(
        c.recent.iter().any(|l| l.contains("NOT dropped")),
        "the refusal is visible to the operator: {:?}",
        c.recent
    );
}

/// Hold 0.5 BTC @ 100 in the book (confirming the external-position row), and return the figures a
/// no-PnL close must leave alone: `(balance, realized, fees, closed trades)`.
fn seed_half_btc(c: &mut CoreThread<RecordingClient>) -> (f64, f64, f64, usize) {
    c.reconcile_reports(pass(vec![position(SYMBOL, 0.5, 1_000)]));
    let (&id, _) = c.recon_alerts.first().expect("one row");
    c.confirm_recon(id);
    assert_eq!(held_qty(c, SYMBOL), 0.5, "PREMISE: local holds it");
    let a = &c.eng(0).account;
    (a.balance, a.realized_pnl, a.fees_paid, a.closed_pnls.len())
}

/// The the CI box shape (bybit#DEMO ETHUSDT): the venue reports the symbol FLAT with no average price,
/// so the drift's own legs would close at `0.0`. The confirm closes at the LOCAL average price
/// instead, like the orphan drop: no cash, PnL or fee moves beyond the one flat closed trade.
#[test]
fn confirming_a_drift_on_a_flat_unpriced_venue_row_closes_at_the_local_average() {
    let mut c = core();
    let before = seed_half_btc(&mut c);

    let flat = vike_model::PositionStatusReport { avg_px: 0.0, ..position(SYMBOL, 0.0, 61_000) };
    c.reconcile_reports(pass(vec![flat]));
    let (&id, row) = c.recon_alerts.first().expect("PREMISE: the drift is held");
    assert_eq!(row.kind, DivergenceKind::PositionDrift);
    assert!(row.proposed_events.is_empty(), "PREMISE: no fill at the venue's 0.0");
    assert!(row.orphan_position.is_some(), "PREMISE: the row names the local drop");

    c.confirm_recon(id);

    assert_eq!(held_qty(&c, SYMBOL), 0.0, "the local remainder is closed");
    let a = &c.eng(0).account;
    assert_eq!(
        (a.balance, a.realized_pnl, a.fees_paid, a.closed_pnls.len()),
        (before.0, before.1, before.2, before.3 + 1),
        "closing at the position's own average price moves no cash, no PnL, no fee"
    );
    assert_eq!(a.closed_pnls.last().copied(), Some(0.0));
    assert!(c.recent.iter().any(|l| l.contains("dropped the local BTCUSDT")), "{:?}", c.recent);
}

/// A flat venue row that DOES carry a real price keeps the old behaviour: the close books at the
/// venue's average price, realizing a loss against the 100 basis.
#[test]
fn confirming_a_drift_on_a_priced_venue_row_still_books_at_the_venue_price() {
    let mut c = core();
    let before = seed_half_btc(&mut c);

    let flat = vike_model::PositionStatusReport { avg_px: 90.0, ..position(SYMBOL, 0.0, 61_000) };
    c.reconcile_reports(pass(vec![flat]));
    let (&id, row) = c.recon_alerts.first().expect("PREMISE: the drift is held");
    assert_eq!(row.kind, DivergenceKind::PositionDrift);
    assert_eq!(row.proposed_events.len(), 1);
    assert!(row.orphan_position.is_none());

    c.confirm_recon(id);

    assert_eq!(held_qty(&c, SYMBOL), 0.0);
    // Not `-5.0`: the test engine's contract multiplier scales it. A loss at all is the point.
    assert!(
        c.eng(0).account.realized_pnl < before.1,
        "closed at the VENUE's 90, below the 100 basis"
    );
}

/// The flat-unpriced drop refuses a book it has not seen, exactly as the orphan drop does.
#[test]
fn a_flat_unpriced_drift_confirm_after_the_book_moved_drops_nothing() {
    let mut c = core();
    seed_half_btc(&mut c);
    let flat = vike_model::PositionStatusReport { avg_px: 0.0, ..position(SYMBOL, 0.0, 61_000) };
    c.reconcile_reports(pass(vec![flat]));
    let (&id, _) = c.recon_alerts.first().expect("PREMISE: the drift is held");

    set_local_qty(&mut c, SYMBOL, 0.7);
    c.confirm_recon(id);

    assert_eq!(held_qty(&c, SYMBOL), 0.7, "untouched");
    assert!(c.recent.iter().any(|l| l.contains("NOT dropped")), "{:?}", c.recent);
}

/// ⚠ The flat-unpriced drift is HELD under the policies that fold a drift with no operator in front
/// of it (`hybrid`, `synthesize`): the pass folds nothing at the venue's `0.0`, raises the same row
/// `quarantine` raises, and its confirm closes at the LOCAL average price. A priced flat row keeps
/// folding under both (`confirming_a_drift_on_a_priced_venue_row_still_books_at_the_venue_price` is
/// its held counterpart; `vike-exec`'s `a_priced_drift_still_folds_under_the_folding_policies`
/// the policy half).
#[test]
fn a_flat_unpriced_drift_is_held_under_hybrid_and_synthesize() {
    for policy in
        [vike_exec::recon::ReconPolicy::hybrid(), vike_exec::recon::ReconPolicy::default()]
    {
        let mut c = core();
        let before = seed_half_btc(&mut c);

        let flat =
            vike_model::PositionStatusReport { avg_px: 0.0, ..position(SYMBOL, 0.0, 61_000) };
        c.reconcile_reports(ReconcileReports { policy, ..pass(vec![flat]) });

        assert_eq!(held_qty(&c, SYMBOL), 0.5, "nothing folded at the venue's 0.0");
        let a = &c.eng(0).account;
        assert_eq!(
            (a.balance, a.realized_pnl, a.fees_paid, a.closed_pnls.len()),
            before,
            "no cash, PnL or fee moved"
        );
        let (&id, row) = c.recon_alerts.first().expect("the drift is held, not folded");
        assert_eq!(row.kind, DivergenceKind::PositionDrift);
        assert!(row.proposed_events.is_empty() && row.orphan_position.is_some());

        c.confirm_recon(id);
        assert_eq!(held_qty(&c, SYMBOL), 0.0, "the confirm closes the local remainder");
        let a = &c.eng(0).account;
        assert_eq!(
            (a.realized_pnl, a.fees_paid),
            (before.1, before.2),
            "at the LOCAL average price"
        );
        assert_eq!(a.closed_pnls.last().copied(), Some(0.0));
    }
}

/// A held `BalanceDrift` is adopted by the confirm: the venue's figure becomes the book's cash.
/// (The deposit/withdrawal/external-close cases of the CI box all end here.)
#[test]
fn confirming_a_held_balance_drift_adopts_the_venues_figure() {
    let mut c = core();
    tick(&mut c, 1_000, balance_pass("quarantine", 9_999.0)); // the first sync adopts
    tick(&mut c, 1_000 + PASS_MS, balance_pass("quarantine", 10_250.0)); // beyond tolerance: held
    let (&id, row) = c.recon_alerts.first().expect("PREMISE: the drift is held");
    assert_eq!(row.kind, DivergenceKind::BalanceDrift);
    assert_eq!(c.eng(0).account.balance, 9_999.0, "PREMISE: held, not adopted");

    c.confirm_recon(id);

    assert_eq!(c.eng(0).account.balance, 10_250.0, "the confirm adopts the venue's figure");
    assert!(c.recon_alerts.is_empty());
}

/// Every pass REPLACES the venue-position evidence the pre-trade gate reads
/// (`ExecutionEngine::cover_position_size`): a symbol the venue stops reporting drops out, so no
/// stale evidence outlives the pass that contradicted it.
#[test]
fn every_pass_replaces_the_venue_position_evidence_the_gate_reads() {
    let mut c = core();
    c.reconcile_reports(pass(vec![position(SYMBOL, 0.5, 1_000)]));
    assert_eq!(c.eng(0).venue_reported_positions.get(SYMBOL), Some(&0.5));

    c.reconcile_reports(pass(vec![position("ETHUSDT", 3.0, 1_000 + PASS_MS)]));
    assert_eq!(c.eng(0).venue_reported_positions.get(SYMBOL), None, "BTC dropped out");
    assert_eq!(c.eng(0).venue_reported_positions.get("ETHUSDT"), Some(&3.0));
}

// -------------------------------------------------------------------------------------------
// FRESHNESS - a row a later pass contradicts is not confirmable (`recon_pass`).
// -------------------------------------------------------------------------------------------

/// The id of the held row of `kind`.
fn held_id_of(c: &CoreThread<RecordingClient>, kind: DivergenceKind) -> Option<u64> {
    c.recon_alerts.iter().find(|(_, h)| h.kind == kind).map(|(&id, _)| id)
}

/// Stage a held `OrphanLocalPosition` on BTC: the book holds 0.5 (an external position confirmed),
/// then a pass reports ETH only. Returns the orphan's id.
fn stage_orphan(c: &mut CoreThread<RecordingClient>) -> u64 {
    c.reconcile_reports(pass(vec![position(SYMBOL, 0.5, 1_000)]));
    let (&id, _) = c.recon_alerts.first().expect("one row");
    c.confirm_recon(id);
    c.reconcile_reports(pass(vec![position("ETHUSDT", 3.0, 1_000 + PASS_MS)]));
    held_id_of(c, DivergenceKind::OrphanLocalPosition).expect("PREMISE: BTC is orphaned")
}

/// THE DEFECT: pass N omitted BTC (an orphan was held), pass N+1 reports it correctly. The orphan is
/// gone from the store, and a confirm of its id closes NOTHING while the venue still holds it.
#[test]
fn a_completed_pass_that_no_longer_raises_an_orphan_clears_its_row() {
    let mut c = core();
    let orphan = stage_orphan(&mut c);

    let ts = 1_000 + 2 * PASS_MS;
    c.reconcile_reports(pass(vec![position(SYMBOL, 0.5, ts), position("ETHUSDT", 3.0, ts)]));
    assert!(!c.recon_alerts.contains_key(&orphan), "the pass that saw BTC again cleared the row");
    assert!(c.recent.iter().any(|l| l.contains("RECON cleared")), "{:?}", c.recent);

    c.confirm_recon(orphan);
    assert_eq!(held_qty(&c, SYMBOL), 0.5, "the stale confirm closed nothing");
    assert!(c.recent.iter().any(|l| l.contains("unknown alert id")), "{:?}", c.recent);
}

/// A pass that cannot judge a kind clears nothing of it: an EMPTY position report is silence (a
/// venue with no position concept, a symbol-scoped fetch that was not asked), not "all flat".
/// (A FAILED pass never reaches the fold at all - `ReconManager` sends nothing - so there is no
/// call to make for it.)
#[test]
fn a_pass_that_cannot_judge_positions_clears_no_position_row() {
    let mut c = core();
    let orphan = stage_orphan(&mut c);

    c.reconcile_reports(pass(Vec::new()));
    assert!(c.recon_alerts.contains_key(&orphan), "an empty position report is not an all-clear");
    assert!(held_id_of(&c, DivergenceKind::PositionOnlyExternal).is_some(), "nor the ETH row");
}

/// Cash is judged only by a pass that reported a figure under `reconcile_balance`: a pass with no
/// figure keeps a held `BalanceDrift`, a pass that reports an agreeing one clears it.
#[test]
fn a_balance_drift_clears_only_on_a_pass_that_reported_an_agreeing_figure() {
    let mut c = core();
    tick(&mut c, 1_000, balance_pass("quarantine", 9_999.0)); // the first sync adopts
    tick(&mut c, 1_000 + PASS_MS, balance_pass("quarantine", 10_250.0)); // beyond tolerance: held
    let drift = held_id_of(&c, DivergenceKind::BalanceDrift).expect("PREMISE: held");

    let none = ReconcileReports { balance: None, ..balance_pass("quarantine", 0.0) };
    tick(&mut c, 1_000 + 2 * PASS_MS, none);
    assert!(c.recon_alerts.contains_key(&drift), "a pass that reported no cash says nothing");

    tick(&mut c, 1_000 + 3 * PASS_MS, balance_pass("quarantine", 9_999.0));
    assert!(!c.recon_alerts.contains_key(&drift), "the venue agrees again: the row is cleared");
}

/// A missed fill is a historical venue fact: it ages out of the lookback while it stays true, so the
/// venue ceasing to report it must NOT clear its row (the fill ledger makes the confirm idempotent).
#[test]
fn a_missed_fill_the_venue_stops_reporting_keeps_its_row() {
    let mut c = core();
    let fill = vike_model::FillReport {
        venue: VENUE.into(),
        symbol: SYMBOL.into(),
        trade_id: "t1".into(),
        venue_order_id: "v9".into(),
        client_order_id: None,
        side: 1,
        last_qty: 1.0,
        last_px: 100.0,
        commission: 0.0,
        commission_asset: "USDT".into(),
        liquidity_side: vike_model::events::LiquiditySide::Taker,
        ts: 5,
    };
    let eth = vec![position("ETHUSDT", 3.0, 1_000)];
    c.reconcile_reports(ReconcileReports { fills: vec![fill], ..pass(eth.clone()) });
    let missed = held_id_of(&c, DivergenceKind::MissingFill).expect("PREMISE: the fill is held");

    c.reconcile_reports(pass(eth)); // position rows present, the fill aged out of the window
    assert!(c.recon_alerts.contains_key(&missed), "the operator's route to book it survives");
}

/// The confirm door re-validates for itself: a row whose account has moved on to a pass that covered
/// its kind and did not raise it is refused, and folds nothing.
#[test]
fn a_confirm_of_a_row_the_latest_pass_contradicts_is_refused() {
    let mut c = core();
    c.reconcile_reports(pass(vec![position(SYMBOL, 0.5, 1_000)]));
    let (&id, _) = c.recon_alerts.first().expect("one row");
    // The contradiction the prune would have removed already: the account's latest pass is newer
    // than the one that raised this row, and covers its kind.
    c.recon_pass.get_mut(VENUE).expect("a pass was delivered").seq += 1;

    c.confirm_recon(id);

    assert_eq!(held_qty(&c, SYMBOL), 0.0, "nothing folded");
    assert!(c.recon_alerts.is_empty(), "the stale row is gone");
    assert!(c.recent.iter().any(|l| l.contains("REFUSED")), "{:?}", c.recent);
}

/// THE RESTART: ids used to be a counter that began at 1 with the process, so a retried
/// `confirm <id>` hit whichever divergence the new run held under that number. The id is the
/// identity's digest now: the same divergence has the same id in a fresh core, and an id the fresh
/// core does not hold names nothing.
#[test]
fn a_confirm_id_names_the_same_divergence_after_a_restart() {
    let row_of = |c: &CoreThread<RecordingClient>, symbol: &str| {
        c.recon_alerts.iter().find(|(_, h)| h.detail.contains(symbol)).map(|(&id, _)| id)
    };
    let (btc, eth) = (position(SYMBOL, 0.5, 1_000), position("ETHUSDT", 3.0, 1_000));

    let mut before = core();
    before.reconcile_reports(pass(vec![btc.clone(), eth.clone()]));
    let seen = row_of(&before, SYMBOL).expect("BTC held");

    // The restarted run meets ETH first: under the old counter ETH took the number BTC had.
    let mut after = core();
    after.reconcile_reports(pass(vec![eth.clone()]));
    assert_eq!(row_of(&after, SYMBOL), None, "PREMISE: BTC is not held yet");
    after.confirm_recon(seen);
    assert_eq!(held_qty(&after, "ETHUSDT"), 0.0, "a retried id the new run lacks folds nothing");

    after.reconcile_reports(pass(vec![eth, btc]));
    assert_eq!(row_of(&after, SYMBOL), Some(seen), "…and it names BTC once BTC is held again");
    after.confirm_recon(seen);
    assert_eq!((held_qty(&after, SYMBOL), held_qty(&after, "ETHUSDT")), (0.5, 0.0));
}

/// ⚠ A venue row that is NOT flat and carries no usable price (local `0.5`, venue `0.3` at `0.0`):
/// every policy holds it with no event and no drop, and the CONFIRM books nothing, says it refused,
/// and the next pass raises the row again. The reduce leg would have sold 0.2 at `0.0`.
#[test]
fn a_non_flat_unpriced_drift_is_held_under_every_policy_and_its_confirm_books_nothing() {
    use vike_exec::recon::ReconPolicy;
    for policy in [
        ReconPolicy::hybrid(),
        ReconPolicy::default(),
        ReconPolicy { default: vike_exec::recon::ReconMode::Quarantine, ..Default::default() },
    ] {
        let mut c = core();
        let before = seed_half_btc(&mut c);

        let row = vike_model::PositionStatusReport { avg_px: 0.0, ..position(SYMBOL, 0.3, 61_000) };
        c.reconcile_reports(ReconcileReports { policy: policy.clone(), ..pass(vec![row.clone()]) });
        assert_eq!(held_qty(&c, SYMBOL), 0.5, "nothing folded at the venue's 0.0: {policy:?}");
        let (&id, held) = c.recon_alerts.first().expect("the drift is held, not folded");
        assert_eq!(held.kind, DivergenceKind::PositionDrift);
        assert!(held.proposed_events.is_empty() && held.orphan_position.is_none());
        assert!(held.confirm_refused, "PREMISE: the row is marked unconfirmable");

        c.confirm_recon(id);
        assert_eq!(held_qty(&c, SYMBOL), 0.5, "the confirm booked nothing: {policy:?}");
        let a = &c.eng(0).account;
        assert_eq!(
            (a.balance, a.realized_pnl, a.fees_paid, a.closed_pnls.len()),
            before,
            "no cash, PnL, fee or closed trade moved"
        );
        assert!(c.recon_alerts.is_empty(), "the refused row is gone");
        assert!(c.recent.iter().any(|l| l.contains("REFUSED")), "{:?}", c.recent);

        // ...and while the venue keeps the row unpriced the next pass raises it again.
        c.reconcile_reports(ReconcileReports { policy, ..pass(vec![row]) });
        assert_eq!(c.recon_alerts.len(), 1, "re-raised");
        assert_eq!(held_qty(&c, SYMBOL), 0.5);
    }
}

/// The external twin: a position the venue reports with no price is never adopted at cost basis `0`.
#[test]
fn an_unpriced_external_position_is_held_and_its_confirm_adopts_nothing() {
    for policy in
        [vike_exec::recon::ReconPolicy::default(), vike_exec::recon::ReconPolicy::hybrid()]
    {
        let mut c = core();
        let row = vike_model::PositionStatusReport { avg_px: 0.0, ..position(SYMBOL, 0.4, 1_000) };
        c.reconcile_reports(ReconcileReports { policy, ..pass(vec![row]) });
        assert_eq!(held_qty(&c, SYMBOL), 0.0, "not adopted at cost basis 0");
        let (&id, held) = c.recon_alerts.first().expect("held");
        assert_eq!(held.kind, DivergenceKind::PositionOnlyExternal);
        assert!(held.confirm_refused && held.proposed_events.is_empty());

        c.confirm_recon(id);
        assert_eq!(held_qty(&c, SYMBOL), 0.0);
        assert!(c.recent.iter().any(|l| l.contains("REFUSED")), "{:?}", c.recent);
    }
}

// ---- the operator confirm's VERDICT is findable in the log file -------------------------------
//
// The hub's audit line (`vike_tradehub::audit::record_recon_confirm`) says only that the confirm
// reached the core's queue. What the core DID with it (applied, refused, with the reason) is a
// `reconcile confirm …` line, and its `applied` half is INFO, which a `warn` file level drops. The
// lines now ride `CONFIRM_VERDICT_TARGET`, a target `vike-tradehub` pins onto the file at `info`
// (`vike_tradehub::audit::CONFIRM_VERDICT_PIN`); these hold the core's half of that pairing.

/// Whether some captured line carries `needle` AND was emitted on the pinned target.
fn verdict_logged(lines: &[&str], needle: &str) -> bool {
    lines.iter().any(|l| l.contains(crate::CONFIRM_VERDICT_TARGET) && l.contains(needle))
}

/// An applied confirm and a repeat of the same id (now unknown) are both logged on the pinned target.
#[traced_test]
#[test]
fn an_applied_confirm_and_an_unknown_id_are_logged_on_the_pinned_target() {
    let mut c = core();
    c.reconcile_reports(pass(vec![position(SYMBOL, 0.5, 1_000)]));
    let (&id, _) = c.recon_alerts.first().expect("one row");

    c.confirm_recon(id);
    c.confirm_recon(id);

    logs_assert(|lines: &[&str]| {
        if !verdict_logged(lines, "reconcile confirm applied") {
            return Err("the applied verdict is not on the pinned target".to_string());
        }
        if !verdict_logged(lines, "reconcile confirm REFUSED: unknown id") {
            return Err("the unknown-id refusal is not on the pinned target".to_string());
        }
        Ok(())
    });
}

/// A confirm `resolve` marked unconfirmable (a non-flat venue row with no price) books nothing; it
/// used to leave only a ring note, which rolls out. Its refusal is a log line on the pinned target.
#[traced_test]
#[test]
fn a_refused_unpriced_confirm_leaves_a_log_line_not_only_a_ring_note() {
    let mut c = core();
    seed_half_btc(&mut c);
    let row = vike_model::PositionStatusReport { avg_px: 0.0, ..position(SYMBOL, 0.3, 61_000) };
    c.reconcile_reports(pass(vec![row]));
    let (&id, held) = c.recon_alerts.first().expect("the drift is held");
    assert!(held.confirm_refused, "PREMISE: the row is marked unconfirmable");

    c.confirm_recon(id);

    logs_assert(|lines: &[&str]| {
        if verdict_logged(lines, "reconcile confirm REFUSED") {
            Ok(())
        } else {
            Err("a refused unpriced confirm left no verdict line".to_string())
        }
    });
}
