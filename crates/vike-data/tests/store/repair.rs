//! The repair verb's decisions: the live writer, the rehearsal and the loud verdict.

use std::path::Path;

use vike_data::store::datafusion_hist::RepairPlan;
use vike_data::{CompactionConfig, DataFusionHist, HistStore, SeriesId, TsRange};
use vike_model::TradeTick;

use crate::common::{bar, bars_id, bars_series, four_fragments_for_one_day, only_date_dir};

// ---- the REPAIR verb's two decisions: the live writer, and the loud verdict --------------------

/// Hold the series' OS advisory lock the way a live writer does — **the kernel lock on
/// `_manifest.lock`, not the file's existence**, which is the distinction
/// `crates/vike-data/src/store/datafusion_hist/manifest.rs`'s `SeriesLock` doc spends a paragraph on.
///
/// Taken from a test rather than from a second process on purpose: `try_lock` is per-FILE-HANDLE,
/// so a handle this process opens contends with `SeriesLock::try_acquire` exactly as another
/// process's would, and there is no child to leave behind if an assertion unwinds.
fn hold_the_series_lock(series_dir: &Path) -> std::fs::File {
    let f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(series_dir.join("_manifest.lock"))
        .expect("open the lock file");
    // `try_lock` rather than `lock`: nothing else holds it at this point, so a blocking acquire
    // could only ever hide a bug by waiting for it — and this is the spelling
    // `crates/vike-data/src/store/datafusion_hist/manifest.rs` itself uses.
    f.try_lock().expect("take the advisory lock");
    f
}

/// ⚠ **THE LIVE-WRITER DECISION: a repair REFUSES rather than waiting, and writes nothing.**
///
/// The spinning `rebuild_series_manifest` is right for a writer that must eventually write — a
/// loser waits and the holder pays nothing. It is the wrong shape for a REPAIR, and the reason is
/// the critical SECTION rather than the wait: a rebuild reads every part footer in the series with
/// the lock HELD, so WINNING a contended lock is what costs a live `RecorderSink` its buffer (up to
/// 5,000 rows, `crates/vike-data/src/rec/live_rec.rs`). So the contended case is `Ok(None)` and the
/// store is untouched — asserted here by BYTES, not by an absence of errors.
#[test]
fn a_contended_rebuild_refuses_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    store.set_fold_bytes_for_test(1);
    store.append_bars("binance", "BTCUSDT", "1m", &[bar(1000, 100.0, None)], Some("k1")).unwrap();
    let series = bars_series(dir.path());
    let manifest = series.join("_manifest.json");
    let before = std::fs::read(&manifest).unwrap();

    let held = hold_the_series_lock(&series);
    let answer = store.rebuild_series_manifest_if_uncontended(&bars_id()).unwrap();
    assert!(answer.is_none(), "a held series lock must REFUSE the rebuild, not win it: {answer:?}");
    assert_eq!(
        std::fs::read(&manifest).unwrap(),
        before,
        "a refused rebuild must leave the manifest BYTE-identical"
    );
    drop(held);

    // ...and with the lock released the same call performs it, so the refusal is about contention
    // and not about the verb being broken.
    let report = store
        .rebuild_series_manifest_if_uncontended(&bars_id())
        .unwrap()
        .expect("an uncontended rebuild happens");
    assert_eq!(report.parts_recovered, 1, "{report:?}");
}

/// ⚠ **The REHEARSAL is lock-free, which is what makes it safe to run against a live store.** A
/// plan that had to take the lock would stall the recorder in order to tell an operator what
/// stalling the recorder would cost — the wrong tool for its own question. Proved by planning WHILE
/// the lock is held, and by the manifest being byte-identical afterwards.
#[test]
fn the_rehearsal_takes_no_lock_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    store.set_fold_bytes_for_test(1);
    store.append_bars("binance", "BTCUSDT", "1m", &[bar(1000, 100.0, None)], Some("k1")).unwrap();
    store.append_bars("binance", "BTCUSDT", "1m", &[bar(2000, 101.0, None)], Some("k2")).unwrap();
    let series = bars_series(dir.path());
    let before = std::fs::read(series.join("_manifest.json")).unwrap();

    let held = hold_the_series_lock(&series);
    let plan = store.plan_series_manifest_rebuild(&bars_id()).unwrap();
    drop(held);

    assert!(plan.leaf_present && plan.base_present, "{plan:?}");
    assert_eq!(plan.report.parts_recovered, 2, "the plan runs the SAME pass the write would");
    assert_eq!(plan.current_parts, Some(2), "…and reports the index as it stands today");
    assert!(plan.is_lossless(), "{:?}", plan.losses());
    assert_eq!(plan.parts_seen(), 2, "the critical section the WRITE would hold");
    assert_eq!(
        std::fs::read(series.join("_manifest.json")).unwrap(),
        before,
        "a rehearsal must write nothing at all"
    );
}

/// ⚠ **THE LOUD PATH, end to end over a REAL store.** A rebuild over keyless parts recovers the
/// index and NOT the idempotency log, which is the shape that must never read as a clean success:
/// `a_keyless_fragment_is_still_double_counted_and_the_report_says_so` is the same store state seen
/// from the other side, where 20 real rows come back as 40.
///
/// So the rehearsal must call it LOSSY, and the loss line must name the consequence (duplicated
/// rows) and a next step — a bare count is not something an operator can act on.
#[test]
fn a_rebuild_that_loses_the_idempotency_log_is_called_lossy_before_it_runs() {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    store.set_fold_bytes_for_test(1);
    for c in 0..3i64 {
        // NO commit key — the append that opted out of idempotency, and the one shape whose part
        // footer carries nothing for a rebuild to re-derive a commit log from.
        store
            .append_bars("binance", "BTCUSDT", "1m", &[bar(c * 1000, 100.0 + c as f64, None)], None)
            .unwrap();
    }
    let plan = store.plan_series_manifest_rebuild(&bars_id()).unwrap();
    assert_eq!(plan.report.parts_recovered, 3, "{plan:?}");
    assert_eq!(plan.report.parts_without_keys, 3, "every part is keyless: {plan:?}");
    assert!(!plan.is_lossless(), "recovering rows but no commit log is LOSSY");

    let losses = plan.losses();
    assert_eq!(losses.len(), 1, "{losses:?}");
    assert!(
        losses[0].contains("DUPLICATE rows"),
        "the consequence, not just the count: {}",
        losses[0]
    );
    assert!(losses[0].contains("Next:"), "...and what to do about it: {}", losses[0]);
    let text = plan.lines().join("\n");
    assert!(text.contains("verdict: LOSSY"), "{text}");

    // ...and the same verdict survives the write, computed from the report the write RETURNED.
    let report = store.rebuild_series_manifest_if_uncontended(&bars_id()).unwrap().unwrap();
    let done = RepairPlan { report, ..plan.clone() };
    assert!(!done.is_lossless(), "a performed lossy rebuild is still lossy: {done:?}");
    assert!(done.outcome_lines().join("\n").contains("verdict: LOSSY"));
}

/// The state `read_manifest` REFUSES — a surviving delta log with no base — rendered by the plan as
/// what it is, with the refusal carried VERBATIM. A repair tool that paraphrased the error it
/// repairs would make the two impossible to match up.
#[test]
fn the_plan_shows_the_base_less_state_and_quotes_the_refusal() {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    store.append_bars("binance", "BTCUSDT", "1m", &[bar(1000, 100.0, None)], Some("k1")).unwrap();
    store.append_bars("binance", "BTCUSDT", "1m", &[bar(2000, 101.0, None)], Some("k2")).unwrap();
    let series = bars_series(dir.path());
    // ⚠ ONLY the base — this is not a smaller disaster than removing both, it is the DIFFERENT one
    // the read path refuses outright.
    std::fs::remove_file(series.join("_manifest.json")).unwrap();

    let plan = store.plan_series_manifest_rebuild(&bars_id()).unwrap();
    assert!(plan.leaf_present, "the parts are all still there");
    assert!(!plan.base_present, "{plan:?}");
    assert!(plan.delta_frames.unwrap_or(0) > 0, "the log survived: {plan:?}");
    let err = plan.current_error.as_deref().expect("the read path refuses this series");
    assert!(err.contains("has a delta log but NO base"), "{err}");
    assert!(
        err.contains("vike-cli data hist repair"),
        "the refusal must name the COMMAND now: {err}"
    );
    let text = plan.lines().join("\n");
    assert!(text.contains("_manifest.json MISSING"), "{text}");

    // ...and the repair genuinely repairs it, which is the claim that refusal makes.
    store.rebuild_series_manifest_if_uncontended(&bars_id()).unwrap().unwrap();
    assert_eq!(
        DataFusionHist::open(dir.path())
            .unwrap()
            .load_bars("binance", "BTCUSDT", "1m", TsRange::all())
            .unwrap()
            .len(),
        2
    );
}

/// ⚠ **A MISTYPED selector must not MINT a series**, and `leaf_present` is the flag that stops it.
/// `rebuild_manifest` reads an absent directory as an empty series (right for a series that legally
/// holds no parts) and `SeriesLock::acquire` would create the leaf — so a rebuild driven straight
/// off a typo would publish an empty manifest at a path nothing ever wrote, and `list_series`, which
/// finds leaves by that very file, would enumerate the phantom forever.
///
/// The plan therefore reports the absence and CREATES NOTHING, which is asserted on the filesystem
/// rather than inferred from the flag.
#[test]
fn a_plan_for_a_series_that_is_not_there_creates_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    store.append_bars("binance", "BTCUSDT", "1m", &[bar(1000, 100.0, None)], Some("k1")).unwrap();

    let typo = SeriesId::per_symbol("bar", "binanc", "BTCUSDT", Some("1m".to_string()));
    let plan = store.plan_series_manifest_rebuild(&typo).unwrap();
    assert!(!plan.leaf_present, "{plan:?}");
    assert_eq!(plan.report.parts_recovered, 0);
    assert!(
        !dir.path().join("kind=bar").join("venue=binanc").exists(),
        "a rehearsal must not create the series it was pointed at by mistake"
    );
    assert_eq!(store.list_series().unwrap().len(), 1, "no phantom joined the enumeration");
}

/// A crashed compaction's fragments are NOT a loss — they are double-counting PREVENTED — but they
/// are still evidence about the store, so the plan reports them as a NOTE while the verdict stays
/// LOSSLESS. The distinction is what stops "the mechanism worked" reading as "something went
/// wrong", and vice versa.
#[test]
fn superseded_fragments_are_a_note_and_not_a_loss() {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    store.set_fold_bytes_for_test(1);
    let series = four_fragments_for_one_day(dir.path(), &store);
    let date_dir = only_date_dir(&series);
    let stash = tempfile::tempdir().unwrap();
    let mut inputs = Vec::new();
    for e in std::fs::read_dir(&date_dir).unwrap() {
        let p = e.unwrap().path();
        let name = p.file_name().unwrap().to_str().unwrap().to_string();
        std::fs::copy(&p, stash.path().join(&name)).unwrap();
        inputs.push(name);
    }
    let cfg = CompactionConfig { target_bytes: 1 << 20, min_parts: 3, ..Default::default() };
    store.compact_series("bar", "binance", "BTCUSDT", Some("1m"), &cfg).unwrap();
    // The crash state: the sealed part plus the dead inputs the unlink loop never reached.
    for name in &inputs {
        std::fs::copy(stash.path().join(name), date_dir.join(name)).unwrap();
    }

    let plan = store.plan_series_manifest_rebuild(&bars_id()).unwrap();
    assert_eq!(plan.report.parts_superseded, 4, "{plan:?}");
    assert_eq!(plan.report.parts_recovered, 1, "{plan:?}");
    assert!(plan.is_lossless(), "the containment rule working is not a LOSS: {:?}", plan.losses());
    let notes = plan.notes().join("\n");
    assert!(notes.contains("crashed before unlinking"), "…but it IS reported: {notes}");
    assert!(plan.lines().join("\n").contains("verdict: LOSSLESS"));
}

/// A GROUP is a directory component too, and the grouped write verbs refuse a hostile one.
///
/// ⚠ The companion to `a_path_hostile_symbol_is_refused_by_every_write_verb`. That refusal landed
/// on the per-symbol verbs and DECLARED the grouped ones uncovered, because they take a `group`
/// rather than a `symbol`. This closes it — and the message says GROUP, because an operator who
/// typed `--group` and is told about a symbol goes looking for one they never supplied.
#[test]
fn a_path_hostile_group_is_refused_by_the_grouped_verbs() {
    let dir = tempfile::tempdir().unwrap();
    let df = DataFusionHist::open(dir.path()).unwrap();
    let trades = vec![TradeTick {
        ts: 0,
        local_ts: 0,
        price: 1.0,
        size: 1.0,
        is_buyer_maker: false,
        symbol: "AAA".to_string(),
    }];

    let err =
        df.append_trades_grouped("binance", "*USDT.P", &trades, None).unwrap_err().to_string();
    assert!(
        err.contains("group") && err.contains('*'),
        "the refusal must say GROUP and name the character — got {err:?}"
    );
    assert!(
        !err.contains("symbol \""),
        "the shared message's noun leaked through: an operator who typed --group is sent looking \
         for a symbol they never supplied — got {err:?}"
    );

    // ...and the migrate verb, which is the ONE path where a human types a group name directly.
    let moved = df.migrate_series_to_group("trade", "binance", "BTCUSDT", "*USDT.P");
    assert!(moved.is_err(), "migrate_series_to_group accepted a group that cannot be a directory");

    // A SAFE group still works, so the refusal is not simply rejecting everything.
    df.append_trades_grouped("binance", "USDT.P", &trades, None)
        .expect("a path-safe group must still be accepted");
}
