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
//! reasons `route_key_tests.rs` states — `reconcile_reports`, `confirm_recon` and the
//! `recon_alerts` store are private to `super` and are precisely what is under test.

use super::*;
use tracing_test::traced_test;
use vike_exec::testing::RecordingClient;
use vike_exec::{Account, RiskGate, RiskLimits};

const VENUE: &str = "bybit";
const SYMBOL: &str = "BTCUSDT";
/// One reconcile pass's worth of clock (the `VIKE_RECONCILE_INTERVAL_MS` default).
const PASS_MS: i64 = 60_000;

/// A single-venue synchronous core. The `Account` is labelled with the canonical venue because
/// `Account::apply_fill` asserts `fill.venue == self.venue`, and a reconcile pass's synthesized
/// legs carry the venue off the venue's own position report.
fn core() -> CoreThread<RecordingClient> {
    let engine = ExecutionEngine::new(
        Account::new(1_000.0, VENUE, None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        VENUE,
        SYMBOL,
    );
    let market = Arc::new(Conflated {
        state: Mutex::new(ConflatedState::default()),
        drops: AtomicU64::new(0),
    });
    let snapshot =
        Arc::new(ArcSwap::from_pointee(CoreSnapshot::empty(&engine.venue, &engine.symbol)));
    assemble_core(
        engine,
        Vec::new(),
        CoreConfig::default(),
        market,
        snapshot,
        Arc::new(AtomicU64::new(0)),
    )
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
        policy: vike_exec::ReconPolicy {
            default: vike_exec::ReconMode::Quarantine,
            ..Default::default()
        },
        balance: None,
        generate_missing_orders: false,
        reconcile_balance: false,
        balance_tol: vike_exec::recon::BalanceTol::default(),
        route_key: None,
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
    let (&id, _) = c.recon_alerts.first().expect("one row");
    assert_eq!(id, 1, "…and it kept the id the FIRST pass minted");
    assert_eq!(c.recon_next_alert_id, 2, "exactly one id was ever consumed");
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
    let ids: Vec<u64> = c.recon_alerts.keys().copied().collect();
    assert_eq!(ids, vec![1, 2], "…and its own confirm id");
}

/// A changed position SIZE is a different divergence, so it lands as its own row rather than
/// silently overwriting the one an operator may already be looking at.
#[test]
fn a_changed_size_is_a_new_row_not_an_overwrite() {
    let mut c = core();
    c.reconcile_reports(pass(vec![position(SYMBOL, 0.5, 1_000)]));
    c.reconcile_reports(pass(vec![position(SYMBOL, 0.9, 1_000 + PASS_MS)]));
    assert_eq!(c.recon_alerts.len(), 2, "0.5 and 0.9 are different divergences");
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
/// The divergence used is `OrphanLocalPosition`, which folds NOTHING under any policy (there is no
/// price to close at — `vike_exec::recon::resolve`'s module doc is the authority), so confirming it
/// is guaranteed not to resolve it. That is the premise, and it is asserted rather than assumed.
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
    assert!(row.proposed_events.is_empty(), "PREMISE: a confirm of this cannot fold anything");

    c.confirm_recon(orphan);
    assert_eq!(held_qty(&c, SYMBOL), 0.5, "…and indeed it folded nothing — still orphaned");
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
// running `VIKE_LOG_FILE_LEVEL=warn` it never reaches the log FILE and floods the journal instead:
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
        policy: vike_exec::ReconPolicy {
            default: vike_exec::ReconMode::Synthesize,
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
