//! Loss reporting: dropped and discarded rows are announced in a `DropReport`, not just counted.

use super::*;

// ---- loss reporting: `dropped`/`discarded` are ANNOUNCED, not merely counted ---------------
//
// The bug these cover: `RecorderHandle::dropped()` had exactly two call sites in the whole
// workspace, both `#[cfg(test)]` in this file. Nothing in `vike-app`/`vike-tradehub` read it, so
// a live recorder could gap its tape forever in silence — the failure mode `vike-alerting` was
// split out for, with its own metric unread.

/// Captures every [`DropReport`] the writer thread emits, so a test can assert on what an
/// operator (or a pager) would actually have seen.
#[derive(Clone, Default)]
struct ReportLog(Arc<std::sync::Mutex<Vec<DropReport>>>);

impl ReportLog {
    fn observer(&self) -> DropObserver {
        let inner = self.0.clone();
        // `clone`, not `*`: a report now carries its per-series breakdown, so it is not `Copy`.
        Arc::new(move |r: &DropReport| inner.lock().unwrap().push(r.clone()))
    }
    fn reports(&self) -> Vec<DropReport> {
        self.0.lock().unwrap().clone()
    }
    /// Named `count` rather than `len` so no reader mistakes this for a collection.
    fn count(&self) -> usize {
        self.0.lock().unwrap().len()
    }
}

/// A short-cadence config: the writer wakes every 20ms (`max_age`) and considers reporting every
/// 50ms, so a test observes many report windows in well under a second.
fn chatty(channel_cap: usize) -> RecorderConfig {
    RecorderConfig {
        channel_cap,
        max_age: Duration::from_millis(20),
        drop_report_every: Duration::from_millis(50),
        ..RecorderConfig::default()
    }
}

/// The series key a `sink.quote(venue, symbol, _)` row is charged to when it is dropped — the
/// one constructor, so no test can pin a `kind` spelling the sink does not produce.
fn quote_key(venue: &str, symbol: &str) -> SeriesId {
    SeriesId::per_symbol("quote", venue, symbol, None)
}

/// Whether a report NAMES this series among the rows it says were dropped.
fn names_series(report: &DropReport, series: &SeriesId) -> bool {
    report.dropped_series.iter().any(|s| &s.series == series)
}

/// The single series every [`drive_meter`] burst is charged to — the arithmetic tests care about
/// the counting, not about who lost the rows, but the counting is now keyed so SOME key must be
/// named.
fn drive_series() -> SeriesId {
    quote_key("polymarket", "TOK")
}

/// A [`DropTally`] with `n` rows already lost on [`drive_series`] — the keyed replacement for
/// what used to be `Arc::new(AtomicU64::new(n))`.
fn tally_of(n: u64) -> Arc<DropTally> {
    let tally = Arc::new(DropTally::new());
    for _ in 0..n {
        tally.record(Some(drive_series()));
    }
    tally
}

/// Drive a real [`LossMeter`] with `every = ZERO` (so every `report_if_due` is due) through
/// `bursts`, calling `report_if_due` `silent_windows` extra times after each burst — the
/// windows in which nothing new was lost but the CUMULATIVE total is still non-zero. Ends with
/// the closing `report_final`. No store, no writer thread, no wall clock: the whole delta
/// discipline is decided by arithmetic here, which is why this is where it is gated.
fn drive_meter(bursts: &[u64], silent_windows: usize) -> Vec<DropReport> {
    let dropped = Arc::new(DropTally::new());
    let discarded = Arc::new(AtomicU64::new(0));
    let log = ReportLog::default();
    let mut meter = LossMeter::new(
        dropped.clone(),
        discarded,
        Some(log.observer()),
        Duration::ZERO, // every check is due — the cadence is not what is under test
    );
    for burst in bursts {
        // Row by row through the real `record`, on ONE series: the delta discipline is the
        // subject, and driving the real ledger is what puts the per-series half under the same
        // checker as the scalar half.
        for _ in 0..*burst {
            dropped.record(Some(drive_series()));
        }
        meter.report_if_due();
        for _ in 0..silent_windows {
            meter.report_if_due();
        }
    }
    meter.report_final();
    log.reports()
}

/// What a TOTAL-based meter would have handed the observer for the same input — the
/// obvious-but-wrong implementation this whole mechanism exists to not be. It re-states the
/// running total every window it is asked, so it never falls silent once anything is lost.
///
/// This is the NEGATIVE CONTROL's input, not production code: the checker below must reject it.
fn total_based_reports(bursts: &[u64], silent_windows: usize) -> Vec<DropReport> {
    let mut out = Vec::new();
    let mut total = 0u64;
    let mut push = |total: u64| {
        out.push(DropReport {
            dropped_delta: total, // ← THE mutation: the total, restated as if it were new
            dropped_total: total,
            // ...and the second half of the same mutation: a scalar report names nobody, which
            // is the state this whole change exists to leave behind.
            dropped_series: Vec::new(),
            discarded_delta: 0,
            discarded_total: 0,
            window: Duration::ZERO,
            final_report: false,
        });
    };
    for burst in bursts {
        total += burst;
        push(total);
        for _ in 0..silent_windows {
            push(total); // still non-zero ⇒ still "reportable" ⇒ one line per window, forever
        }
    }
    push(total); // the closing report re-states it one last time
    out
}

/// The delta discipline, as a CHECKER rather than a pile of inline asserts, so the exact same
/// judgement can be pointed at the real meter and at the broken one and be seen to disagree.
///
/// `Err` describes the violation; `Ok` means: one report per burst, in order, each carrying
/// that burst's own delta and the running total at the time — and nothing else at all.
fn check_delta_discipline(reports: &[DropReport], bursts: &[u64]) -> Result<(), String> {
    if let Some(empty) = reports.iter().find(|r| r.lost_delta() == 0) {
        return Err(format!("an empty report was emitted: {empty:?}"));
    }
    if reports.len() != bursts.len() {
        return Err(format!(
            "expected exactly one report per burst ({}), got {} — a report was re-emitted for \
                 a window in which nothing new was lost",
            bursts.len(),
            reports.len()
        ));
    }
    let mut running = 0u64;
    for (i, (report, burst)) in reports.iter().zip(bursts).enumerate() {
        running += burst;
        if report.dropped_delta != *burst {
            return Err(format!(
                "report {i}: dropped_delta {} is not the {burst} rows lost in THAT window \
                     (a total-based report would say {running})",
                report.dropped_delta
            ));
        }
        if report.dropped_total != running {
            return Err(format!(
                "report {i}: dropped_total {} is not the running total {running}",
                report.dropped_total
            ));
        }
        // The named series must ACCOUNT for the same delta. `drive_meter` charges every burst
        // to one series, so this is exact — and it is what fails on a report that counts rows
        // it cannot attribute (the bare-scalar shape), not merely on a wrong number.
        let named: u64 = report.dropped_series.iter().map(|s| s.delta).sum();
        if named != *burst {
            return Err(format!(
                "report {i}: the named series account for {named} of the {burst} rows lost in \
                     that window — a drop nothing can name is a gap nobody can find: {:?}",
                report.dropped_series
            ));
        }
    }
    Ok(())
}

/// THE property, gated where it actually lives: [`LossMeter`] reports each non-zero DELTA
/// exactly once and then stays silent, however many windows pass, however non-zero the
/// CUMULATIVE total remains.
///
/// Deterministic by construction — no store, no writer thread, no sleeping — because none of
/// those participate in the decision. The end-to-end test below proves the WIRING (a real burst
/// through a real writer thread reaches a real observer); this proves the RULE.
#[test]
fn the_meter_reports_each_delta_once_and_then_falls_silent() {
    let bursts = [7_u64, 5, 1];
    // 20 windows of nothing-new between bursts: a total-based meter would emit 20 lines each.
    let reports = drive_meter(&bursts, 20);
    check_delta_discipline(&reports, &bursts).expect("the real meter must satisfy this");

    // The deltas partition the total exactly: nothing double-counted, nothing missed.
    let summed: u64 = reports.iter().map(|r| r.dropped_delta).sum();
    assert_eq!(summed, bursts.iter().sum::<u64>(), "the deltas must sum to the total");
    // And the discriminator, stated outright: after the first burst a delta is NOT the total.
    assert_ne!(
        reports[1].dropped_delta, reports[1].dropped_total,
        "a delta that equals the running total is the total-based bug wearing a delta's name"
    );
    // Nothing was lost to a failed flush here, so that axis stays silent throughout.
    assert!(reports.iter().all(|r| r.discarded_delta == 0 && r.discarded_total == 0));
}

/// NEGATIVE CONTROL for the test above. The checks it uses are only worth running if they can
/// FAIL, so point the very same checker at what a total-based meter would have produced for the
/// very same input and require the opposite verdict.
///
/// Without this, softening `check_delta_discipline` (or the meter) leaves a test that passes
/// whether or not the delta discipline still holds — which is the failure mode of every
/// threshold quietly raised past anything a run can reach.
#[test]
fn the_delta_checks_reject_a_total_based_meter() {
    let bursts = [7_u64, 5, 1];
    let broken = total_based_reports(&bursts, 20);
    let verdict = check_delta_discipline(&broken, &bursts);
    assert!(
        verdict.is_err(),
        "the checker ACCEPTED a total-based meter — it proves nothing: {broken:?}"
    );
    // ...and it accepts the real one, from the identical input. One checker, two verdicts.
    assert!(check_delta_discipline(&drive_meter(&bursts, 20), &bursts).is_ok());
}

/// The closing report is emitted OFF cadence (that is its whole point — a burst in the final
/// window must not be swallowed by the shutdown), but it is still a delta: with everything
/// already reported it says nothing at all.
#[test]
fn the_closing_report_carries_only_what_was_not_yet_reported() {
    // Nothing reported yet ⇒ the closing report carries the whole burst.
    let only_final = {
        let log = ReportLog::default();
        let mut meter = LossMeter::new(
            tally_of(9),
            Arc::new(AtomicU64::new(0)),
            Some(log.observer()),
            Duration::from_secs(3_600), // never due on cadence — only `report_final` fires
        );
        meter.report_if_due();
        meter.report_final();
        log.reports()
    };
    assert_eq!(only_final.len(), 1, "the final window's loss is never swallowed");
    assert!(only_final[0].final_report);
    assert_eq!(only_final[0].dropped_delta, 9);

    // Already reported ⇒ the closing report is silent. `drive_meter` ends with `report_final`,
    // so the exact-count check above already proves this; stated here as its own case because
    // it is the load-immune half of the end-to-end test below.
    let bursts = [4_u64];
    assert_eq!(drive_meter(&bursts, 0).len(), 1, "a closing report with a zero delta is silent");
}

/// THE regression test for the reported shape, end to end: a burst of drops made by a real
/// `RecorderSink` is announced through a real writer thread, and then — while the CUMULATIVE
/// total stays non-zero forever — reporting falls SILENT again.
///
/// ⚠ **This test waits on CONDITIONS, never on a fixed sleep, and that is the fix for a real
/// flake** (it failed on the CI box in a full-roster run, then went 10/10 green in isolation). It
/// used to `thread::sleep(300ms)` and then assert the burst had been reported. But a report's
/// latency is not set by `drop_report_every` (50ms here) — it is set by when the writer thread
/// next gets between store commits, and a commit costs ~30-39ms idle and far more under load.
/// MEASURED on the CI box: the report lands ~80ms after the burst when idle, but p90 176ms / max
/// 350ms under a 4-way parallel test load and max 1,305ms under an 8-way one, so the 300ms
/// budget was ~1.7x the p90 and NEGATIVE at the tail. Under a deliberate 4-way load the old
/// shape failed **12 of 60 runs**, always at "the drop burst was never reported".
///
/// It is a fidelity bug, not a lost report: across 260 instrumented runs the report arrived
/// late 6 times and was lost **zero** times (the delta accumulates and the next report carries
/// it; teardown always emits a closing one). That is what makes a bounded wait the right tool —
/// see [`REPORT_DEADLINE`] for why the opposite finding would forbid one.
///
/// **The silence half is gated causally, not by a clock.** A quiet sleep can only produce a
/// false PASS (a stalled writer emits nothing, which is what the assertion wants to see), so it
/// cannot be the proof. `shutdown()` can: `report_final` emits UNCONDITIONALLY, so a total-based
/// implementation is guaranteed to hand the observer a fresh non-zero `dropped_delta` there,
/// and the sum below catches it with no timing involved. `the_meter_reports_each_delta_once_and_then_falls_silent`
/// gates the periodic windows the same way — by arithmetic.
#[test]
fn a_drop_burst_is_reported_once_per_delta_then_falls_silent() {
    let (_dir, store) = open_store();
    let log = ReportLog::default();
    let (sink, handle) =
        RecorderSink::spawn_with_observer(store.clone(), chatty(1), Some(log.observer())).unwrap();

    // ONE burst: flood a capacity-1 channel so a pile of `try_send`s fail.
    for i in 0..20_000i64 {
        sink.quote("polymarket", "TOK", quote(i, i as f64));
    }
    // Every `try_send` above ran on THIS thread, so the counter is final at this line: no other
    // producer exists and the writer never bumps it. That is what lets the wait below be a
    // CONDITION ("has all of it been reported yet?") rather than a guess about elapsed time.
    let dropped = handle.dropped();
    assert!(dropped > 0, "flooding a capacity-1 channel must have dropped rows");

    // Wait for the burst to be FULLY reported. Bounded, polled — never a fixed sleep: see this
    // test's doc for the measurements that killed the 300ms one.
    //
    // Reaching the `break` IS the "it was reported at all" assertion, so there is deliberately
    // no `reports.len() >= 1` line after it: with `dropped > 0` the sum cannot match on an
    // empty log, and an assertion that cannot fail is exactly what this PR is about deleting.
    let started = Instant::now();
    loop {
        let reports = log.reports();
        let summed: u64 = reports.iter().map(|r| r.dropped_delta).sum();
        if summed == dropped {
            break;
        }
        assert!(
            started.elapsed() < REPORT_DEADLINE,
            "the drop burst was never fully reported to the observer: {summed} of {dropped} \
                 rows across {} report(s) after {:?}",
            reports.len(),
            started.elapsed()
        );
        thread::sleep(Duration::from_millis(2));
    }

    // Every emitted report is a real loss, and this burst's losses are all `dropped` ones.
    let reports = log.reports();
    assert!(reports.iter().all(|r| r.lost_delta() > 0), "an empty report must never be emitted");
    assert!(
        reports.iter().all(|r| r.discarded_delta == 0),
        "channel overflow is a `dropped`, never a `discarded`"
    );
    assert_eq!(
        reports.last().expect("non-empty").dropped_total,
        dropped,
        "each report also carries the running total for context"
    );

    // The burst is over. No new rows are sent, so every subsequent window has a ZERO delta —
    // even though `dropped_total` remains non-zero for the rest of the recorder's life.
    //
    // ⚠ This sleep is CORROBORATION, not proof: a writer stalled through the whole window emits
    // nothing either, so it can only ever produce a false pass. The proof is the shutdown below
    // (causal) and `the_meter_reports_each_delta_once_and_then_falls_silent` (arithmetic).
    thread::sleep(chatty(1).drop_report_every * 4);
    let quiet: u64 = log.reports().iter().map(|r| r.dropped_delta).sum();
    assert_eq!(
        quiet, dropped,
        "a drop must be reported once per DELTA, not re-reported every window while the \
             cumulative total stays non-zero"
    );

    // THE mutation gate, and the one step here with no timing in it at all: `shutdown` joins
    // the writer, and `report_final` emits UNCONDITIONALLY on the way out. A total-based
    // implementation therefore MUST hand the observer another `dropped_delta` of `dropped`
    // right here, doubling this sum; the delta-based one contributes exactly nothing.
    //
    // Summing the deltas (rather than counting reports) is deliberate: a shutdown flush that
    // genuinely failed would emit a real closing report on the `discarded` axis, which is not
    // this test's subject and must not read as a failure of it.
    handle.shutdown();
    let after: u64 = log.reports().iter().map(|r| r.dropped_delta).sum();
    assert_eq!(
        after, dropped,
        "the closing report re-stated a dropped delta that had already been reported — the \
             deltas must partition the total exactly, teardown included"
    );
}

/// THE regression test for "a gapped tape can never be NAMED": TWO producers share one bounded
/// queue, and the report must say WHICH series lost rows, not merely how many rows were lost.
///
/// ⚠ **Two producers is the whole point, and no single-producer test can stand in.** The three
/// tests that already saturate the bound — `overflow_drops_and_counts`,
/// `equity_overflow_drops_and_counts` and
/// `a_drop_burst_is_reported_once_per_delta_then_falls_silent` — flood ONE series from ONE
/// thread and assert `dropped() > 0`, which a counter that names nobody passes exactly as well
/// as one that names everybody. `age_flush_fires_under_busy_other_series` IS two-producer, but
/// runs at the 65,536 default and never reaches the bound at all.
///
/// ⚠ **This is not starvation, and the test does not measure fairness.** While the queue is full
/// EVERY producer's `try_send` fails; the quiet one simply gets far fewer attempts at a freed
/// slot, so it loses a much larger SHARE of its own rows — which is the the CI box shape, a Polymarket
/// L2 stream sharing this queue with a Binance tape. What makes that invisible is that the quiet
/// series is still receiving SOME rows: `flush_buf_with` names a series only on a PERMANENT
/// FLUSH FAILURE (none here — asserted), and `liveness`/`SilenceWatch` name one only when it
/// stops ENTIRELY (it does not). PARTIAL loss is the residual between them, and it is what this
/// pins.
///
/// Every wait is a CONDITION, never a fixed sleep (this file's established idiom): the loop
/// sends quiet rows until the observer has NAMED the quiet series, so it exits the instant the
/// property holds and the deadline can only ever turn a hang into a named failure.
#[test]
fn two_producers_overflow_and_the_quiet_series_is_named() {
    let (_dir, store) = open_store();
    let log = ReportLog::default();
    // `channel_cap: 1` + the chatty report cadence: the queue is full essentially always, which
    // is the condition BOTH producers are dropping under.
    let (sink, handle) =
        RecorderSink::spawn_with_observer(store.clone(), chatty(1), Some(log.observer())).unwrap();

    // BUSY: floods `trade` rows continuously, as a live venue tape would. A different KIND and
    // a different venue, so the quiet series can only be found by its own key.
    let busy_sink = sink.clone();
    let busy_stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let busy_stop_thread = busy_stop.clone();
    let busy = thread::spawn(move || {
        let mut i = 0i64;
        while !busy_stop_thread.load(Ordering::Relaxed) {
            // Bursts rather than an unbroken spin: 32 rows into a ONE-slot queue already keeps
            // it occupied through the writer's ~30ms commits, and pausing between them keeps a
            // test that shares a CI box with seven others from pinning a core.
            for _ in 0..32 {
                busy_sink.trade("binance", "BTCUSDT", trade(i, i as f64));
                i += 1;
            }
            thread::sleep(Duration::from_millis(1));
        }
    });

    // QUIET: a low-rate producer on its own series, sent from THIS thread — so once the loop
    // stops, its per-series count is FINAL (nothing else in this test sends a quote), which is
    // what lets the persisted-row equation below be an equality rather than a bound.
    let quiet = quote_key("polymarket", "QUIET");
    let mut sent_quiet = 0i64;
    let started = Instant::now();
    loop {
        for _ in 0..32 {
            sink.quote("polymarket", "QUIET", quote(sent_quiet, sent_quiet as f64));
            sent_quiet += 1;
        }
        let reports = log.reports();
        // ⚠ BOTH series, and that is a 2026-09-20 fix for a flake this test carried for a
        // month. The exit used to be "the quiet series is named", after which the busy producer
        // was STOPPED and the assertion below demanded it be named too — an ASSUMPTION that it
        // had already contended. On a loaded box it had not: measured on the CI box at a load average
        // three times the core count, `dropped_by_series` held `{QUIET: 60}` and nothing else,
        // because the busy THREAD had not been scheduled yet. Nothing was wrong with the
        // recorder; the test stopped a producer that had never run and then required evidence
        // only running produces.
        //
        // Waiting for both is a CONDITION, which is this file's own stated idiom and what the
        // rest of this test already does. It does not weaken what is pinned — the assertions
        // below are unchanged, and the one that matters ("the busy series must be named as
        // well") is now GUARANTEED by the exit rather than hoped for. A genuine regression, a
        // counter that names nobody or only the series asked about, still fails: it fails on
        // the deadline, which now says WHICH half is missing.
        let by_series = handle.dropped_by_series();
        let quiet_named = reports.iter().any(|r| names_series(r, &quiet));
        let busy_named = by_series.keys().any(|k| k.kind == "trade" && k.symbol == "BTCUSDT");
        if quiet_named && busy_named {
            break;
        }
        assert!(
            started.elapsed() < REPORT_DEADLINE,
            "after {:?} the drop report still does not name BOTH series — quiet named: \
                 {quiet_named}, busy named: {busy_named} ({sent_quiet} quiet rows sent; {} rows \
                 dropped in total, attributed to [{}]). A `busy named: false` with drops recorded \
                 for the quiet series means the busy producer never contended — widen the deadline \
                 or quiesce the box. Both false means nothing overflowed at all. Quiet false with \
                 busy true is the real regression this test exists to catch: per-series attribution \
                 that misses the series losing the LARGEST SHARE of its own rows",
            started.elapsed(),
            handle.dropped(),
            handle.dropped_summary()
        );
        thread::sleep(Duration::from_millis(2));
    }
    busy_stop.store(true, Ordering::Relaxed);
    busy.join().unwrap();

    // Both producers have stopped, so every count below is final.
    let by_series = handle.dropped_by_series();
    let dropped_quiet = by_series.get(&quiet).copied().unwrap_or(0);
    assert!(dropped_quiet > 0, "the loop exits only once the quiet series was named");
    // ⚠ Now GUARANTEED by the loop exit above, exactly like `dropped_quiet > 0` on the line
    // before it — both are re-asserted because a future edit to the exit condition must redden
    // something, and because the message is what a reader lands on. Until 2026-09-20 this was
    // the only place the property was checked, and it was checked against a producer the test
    // had just stopped without ever confirming it ran.
    assert!(
        by_series.keys().any(|k| k.kind == "trade" && k.symbol == "BTCUSDT"),
        "the busy series shares the queue and loses rows too, so it must be named as well — \
             not only whichever series the test thought to ask about: {by_series:?}"
    );
    let summary = handle.dropped_summary();
    assert!(
        summary.contains("quote/polymarket/QUIET="),
        "the rendered tally must spell the series the way `liveness` keys it: {summary}"
    );

    // WHAT THE QUIET SERIES PERSISTS. Every row that reached the writer flushes on the age
    // cadence (20ms here), so the store converges to exactly what was not dropped. A CONDITION
    // wait again — rows may still be in flight at this line — and coarse, because each poll is
    // a full DataFusion read competing with the writer it is waiting for.
    let settle = Instant::now();
    let want = sent_quiet - dropped_quiet as i64;
    let persisted = loop {
        let got = store.scan_quotes("polymarket", "QUIET", TsRange::all()).unwrap();
        if got.len() as i64 == want {
            break got.len() as i64;
        }
        assert!(
            settle.elapsed() < REPORT_DEADLINE,
            "the quiet series never settled at sent({sent_quiet}) - dropped({dropped_quiet}) \
                 = {want}: {} rows persisted after {:?}, discarded={}",
            got.len(),
            settle.elapsed(),
            handle.discarded()
        );
        thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(handle.discarded(), 0, "no flush failed — every loss here was a drop");
    assert!(
        persisted < sent_quiet,
        "the quiet tape MUST have a hole: {dropped_quiet} of {sent_quiet} rows never made it \
             onto the queue, and a series still persisting {persisted} looks alive from every \
             other vantage point"
    );

    // ...and what the REPORT said about it. `shutdown` joins the writer after emitting the
    // closing report, so by the line after it everything lost has been reported exactly once.
    handle.shutdown();
    let mut reported_quiet = 0u64;
    for report in log.reports() {
        for row in &report.dropped_series {
            if row.series == quiet {
                reported_quiet += row.delta;
            }
        }
        // The named rows never exceed the scalar: a drop nothing could attribute is still
        // counted, and never counted twice (see `DropReport::dropped_series`).
        let named: u64 = report.dropped_series.iter().map(|s| s.delta).sum();
        assert!(
            named <= report.dropped_delta,
            "a report named {named} rows but claims {} were dropped: {report:?}",
            report.dropped_delta
        );
    }
    assert_eq!(
        reported_quiet, dropped_quiet,
        "the per-series deltas must partition that series' own total exactly, teardown \
             included — the same discipline the scalar deltas are held to"
    );
}

/// The other half of the contract: a recorder that loses nothing must be COMPLETELY silent.
/// This is what makes the first warn above meaningful rather than background noise.
#[test]
fn a_healthy_recorder_reports_nothing() {
    let (_dir, store) = open_store();
    let log = ReportLog::default();
    let cfg = RecorderConfig { max_rows: 10, ..chatty(65_536) };
    let (sink, handle) =
        RecorderSink::spawn_with_observer(store.clone(), cfg, Some(log.observer())).unwrap();

    for i in 0..100i64 {
        sink.quote("polymarket", "TOK", quote(i, i as f64));
    }
    thread::sleep(Duration::from_millis(300)); // many report windows pass

    assert_eq!(handle.dropped(), 0, "a roomy channel drops nothing");
    assert_eq!(handle.discarded(), 0, "a healthy store discards nothing");
    assert_eq!(handle.lost(), 0);
    // ...and the keyed half is silent too: no series is named, not even with a zero beside it.
    assert!(handle.dropped_by_series().is_empty(), "nothing lost ⇒ nothing named");
    assert_eq!(handle.dropped_summary(), "none");
    assert!(log.reports().is_empty(), "nothing lost ⇒ no report at all");

    handle.shutdown(); // the CLOSING report must also stay silent
    assert!(log.reports().is_empty(), "a clean teardown emits no closing report either");
}

/// A flush that fails permanently DISCARDS its buffer — that is unavoidable, there is nowhere
/// else to put the rows — but the loss must be counted and reported, not just logged once.
///
/// The wedge: plant a plain FILE where the series directory belongs, so `SeriesLock::acquire`'s
/// opening `create_dir_all` fails immediately and every `append_quotes` for this series fails
/// fast and permanently. (Failing FAST also exercises the retry — a slow failure is skipped, see
/// `append_retry_skips_a_slow_failure`.)
#[test]
fn a_permanently_failed_flush_discards_and_counts_the_rows() {
    let (dir, store) = open_store();
    let series = quote_series_dir(dir.path(), "polymarket", "TOK");
    std::fs::create_dir_all(series.parent().expect("series dir has a parent")).unwrap();
    std::fs::write(&series, b"not a directory").unwrap();

    let log = ReportLog::default();
    // max_rows == the row count, so ingesting the last row triggers the doomed flush directly —
    // no shutdown needed, which lets the handle still be read afterwards.
    let cfg = RecorderConfig { max_rows: 3, ..chatty(65_536) };
    let (sink, handle) =
        RecorderSink::spawn_with_observer(store.clone(), cfg, Some(log.observer())).unwrap();

    for i in 0..3i64 {
        sink.quote("polymarket", "TOK", quote(i, i as f64));
    }

    let start = Instant::now();
    while log.count() == 0 {
        assert!(
            start.elapsed() < REPORT_DEADLINE,
            "a discarded flush was never reported (discarded={})",
            handle.discarded()
        );
        thread::sleep(Duration::from_millis(20));
    }

    assert_eq!(handle.discarded(), 3, "every row of the failed buffer is counted lost");
    assert_eq!(handle.dropped(), 0, "these rows reached the writer — they were not dropped");
    assert_eq!(handle.lost(), 3, "`lost` is the one number that answers 'any holes?'");

    let first = log.reports()[0].clone();
    assert_eq!(first.discarded_delta, 3);
    assert_eq!(first.discarded_total, 3);
    assert_eq!(first.dropped_delta, 0);
    // Nothing was lost at the CHANNEL, so nothing is named there — a discarded batch is named
    // by `flush_buf_with`'s own `venue`/`symbol` warn instead. The summary must say so in
    // words rather than render a blank field.
    assert!(first.dropped_series.is_empty(), "no row was dropped at the channel here");
    assert_eq!(first.dropped_series_summary(), "none");
    assert_eq!(first.lost_delta(), 3);
    assert_eq!(first.lost_total(), 3);
    assert!(!first.final_report, "this one came from the periodic cadence");

    handle.shutdown();
}
