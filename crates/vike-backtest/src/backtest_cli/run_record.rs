//! What a single run's RECORD is built from: the data fingerprint, the bounded series, the trades.

use vike_data::HistStore;
use vike_model::runs;

use crate::harness::BacktestProfile;
use crate::run_fingerprint;

/// Read what the store holds for every series this profile will load.
///
/// ⚠ **Called BEFORE the handle is coerced to `Arc<dyn HistStore>`, and it has to be.**
/// `list_series` and `series_facts` are INHERENT to `vike_data::DataFusionHist`
/// rather than `HistStore` methods, and [`run`] shadows the concrete handle with the trait object
/// one line later. The alternative was a required trait method, which would redden at least eight
/// `impl HistStore for` sites — `MemHistStore`, `RemoteHistStore` and the doubles in vike-report,
/// vike-datahub, vike-studio, vike-studio-core and this crate — to serve the one caller in the tree
/// that asks the question.
///
/// # What it costs, and who pays it
///
/// ONE `list_series` walk plus **ONE manifest parse per series** (`series_facts`), plus a
/// `fs::metadata` per part for the byte count. It used to be TWO parses per series — the separate
/// `series_coverage` and `series_commits` calls each folded the same file — and that number was the
/// whole argument for keeping this call BELOW the sweep branch, where a search would have paid it
/// before running hundreds of backtests over a grouped store whose manifest is multi-megabyte.
///
/// ⚠ **BOTH paths call it now, and the placement is unchanged.** The single-run call still sits
/// below the sweep branch; the SEARCH calls it at the point inside that branch where it was already
/// paying one parse per series for [`data_witness`]'s input. So the search's cost is the same
/// parse count it always had, and the run ADDRESS falls out of it — which is why this function was
/// not HOISTED to serve both. Hoisting would also have changed what a search prints: this
/// collector's failure reaches stderr as `run NOT addressed` on the single-run path, and the
/// search's call site keeps its own message naming the witness.
///
/// # ⚠ ABSENT and EMPTY are different answers, and the store alone cannot tell them apart
///
/// `series_facts` folds the series' manifest, and
/// `crates/vike-data/src/store/datafusion_hist/manifest.rs`'s `read_manifest` maps `ErrorKind::NotFound`
/// to an EMPTY manifest — so a series the store has never heard of comes back as
/// `Ok(SeriesCoverage::default())`, all zeros, indistinguishable from a series that exists and
/// holds nothing. That is not a new observation: `DataFusionHist::series_dir_of`'s own doc records
/// the five-bug class it caused, found by pointing the Data Manager at a real 148 MB tape whose
/// entire 37.5 M-row Polymarket group rendered as `0 rows · 0 B`.
///
/// So presence is decided by the store's INVENTORY instead — one `list_series` walk, membership
/// checked against it — and `coverage: None` means the store holds no such series, which is the
/// state `run_fingerprint::DataFingerprint::canonical` renders as `coverage missing` and the reason
/// that field is an `Option` at all. Before this, that arm was unreachable from the only producer
/// in the tree.
///
/// # ⚠ A store error makes the address ABSENT, never DIFFERENT
///
/// With `NotFound` handled above, the remaining ways `series_facts` can fail are a manifest that
/// will not PARSE and a `MANIFEST_FORMAT` mismatch. Swallowing either into `coverage: None` would
/// make the address BUILD-DEPENDENT — a format bump would flip every series to `coverage missing`
/// and move every stored address, orphaning every baseline — which is precisely the property
/// `canonical`'s module doc exists to guarantee. So an error propagates: the caller records NO
/// fingerprint and NO address, which says "this run's inputs could not be addressed" instead of
/// addressing them wrongly.
///
/// ⚠ **It still cannot FAIL A RUN.** A fingerprint is metadata about a run that has not happened
/// yet; [`run`] prints the reason on stderr and runs the backtest anyway, which is the same
/// discipline `vike_model::runs` states for persisting.
pub fn collect_data_fingerprint(
    store: Option<&dyn HistStore>,
    profile: &BacktestProfile,
    // ⚠ **What to RECORD as the provenance of this data — a path on a local run, the datahub's
    // ADDRESS on a routed one.** It was `store_root: &Path` until
    // `docs/decisions/0084-only-the-datahub-touches-the-store.md` routed this path, and a `&Path`
    // here is no longer a fact the caller has: on the wire arm the resolved root is THIS box's
    // default, a directory the run never opened, and recording it would put a confident falsehood
    // in the one document a later reader uses to reproduce the run. `HistoryRoute::label` renders
    // it, so the record and the run's own disclosure cannot disagree.
    store_label: &str,
) -> Result<run_fingerprint::DataFingerprint, String> {
    // ⚠ `Option`, and the reason CHANGED on 2026-09-23 while the shape did not. It used to be
    // `Option<&DataFusionHist>` because `series_facts` was INHERENT to that type and an archive run
    // had nothing to ask. That verb is on the `HistStore` trait now
    // (`docs/decisions/0084-only-the-datahub-touches-the-store.md`, the seventh verb), so the
    // parameter is a trait object and a ROUTED store can be fingerprinted — which is the whole
    // point of moving it.
    //
    // ⚠ The `Option` SURVIVES, and collapsing it would be a quiet regression. An archive store
    // inherits the trait's refusing default, so passing it would produce an `Err` too — but only
    // AFTER `list_series` below answered `Ok(empty)`, at which point every planned series is
    // recorded as "the store holds no such series". That is a fingerprint asserting the archive
    // held nothing, which is exactly the fabricated witness the paragraph below refuses. `None`
    // keeps the archive case an honest refusal instead.
    //
    // It is reported as the ordinary `Err` both call sites already handle: they print the reason
    // and record `fingerprint: null`, which is exactly the discipline stated above — a fingerprint
    // is metadata about a run that has not happened yet and may never fail one. Returning `Ok` with
    // an empty fingerprint would be the worse answer: a witness claiming the store held nothing.
    let Some(store) = store else {
        return Err("the run reads archive Parquet in place (--archive), and a data fingerprint \
                    needs the per-series part-file list an archive store does not keep"
            .to_string());
    };
    let range = profile.range().unwrap_or_default();
    let held =
        store.list_series().map_err(|e| format!("{store_label} could not list its series: {e}"))?;

    let mut series = Vec::new();
    for id in run_fingerprint::planned_series_ids(profile, &held) {
        if !held.contains(&id) {
            // The store holds no such series. A REAL and reportable state — a tick profile naming a
            // lane that was never recorded — and a DIFFERENT answer from an empty one.
            series.push(run_fingerprint::SeriesFingerprint {
                id,
                coverage: None,
                commits: Vec::new(),
                commits_len: 0,
            });
            continue;
        }
        // ⚠ **ONE manifest parse, not two.** This was `series_coverage` followed by
        // `series_commits`, and each of those folds the series' manifest INDEPENDENTLY — so the
        // collector parsed a multi-megabyte grouped manifest twice per series. `series_facts`
        // answers both from one parse; the two halves are the same values those accessors return,
        // because they share its fold. That halving is what lets the SEARCH path call this
        // collector at all: a search was already paying one parse per series for its data witness,
        // so it now gets the run ADDRESS for no additional read.
        //
        // The diagnostics merged with the calls. They were two messages naming the same series for
        // two failures of the same parse, and nothing asserted on either.
        let (coverage, commits) =
            store.series_facts(&id).map_err(|e| format!("the store could not read {id:?}: {e}"))?;
        // ⚠ BOUNDED, and `run_fingerprint::MAX_COMMIT_KEYS` carries the arithmetic: a grouped
        // series' log is the whole venue's flush log, which is ~6 MB of JSON for a 30-day window
        // and went into EVERY run directory unbounded. A prefix plus the true count, exactly the
        // shape `vike_model::runs::MAX_TRADES` / `RunTrades::source_len` already uses for the ledger.
        let (commits, commits_len) = run_fingerprint::bound_commits(commits);
        series.push(run_fingerprint::SeriesFingerprint {
            id,
            coverage: Some(coverage),
            commits,
            commits_len,
        });
    }

    Ok(run_fingerprint::DataFingerprint {
        schema: run_fingerprint::DATA_FINGERPRINT_SCHEMA,
        store: store_label.to_string(),
        from_ms: range.start,
        to_ms: range.end,
        series,
    })
}

/// The run's equity SERIES and its diagnostic counters, bounded — [`runs::MAX_EQUITY_SAMPLES`]
/// carries the argument for the bound and the residual it accepts.
///
/// ⚠ **ONE stride for every curve.** `equity_ts` and each per-symbol curve are thinned with the
/// SAME stride as `equity_curve`, because their meaning is positional: a per-symbol curve thinned
/// independently would still have a plausible length and would no longer line up by index with the
/// equity it is supposed to explain.
///
/// ⚠ **Per-bar returns are deliberately NOT persisted.** `vike_analytics::metrics::returns` SKIPS
/// zero-denominator steps, so a returns vector is not index-alignable with either vector here —
/// persisting one beside them would be a misaligned artifact by construction. A reader derives
/// returns from `equity` with that same function, which is also the only way its numbers and the
/// report's Sharpe are guaranteed to agree. [`runs::RunSeries`] states this at the type.
pub fn run_series_from(result: &vike_analytics::BacktestResult) -> runs::RunSeries {
    let (equity, stride) = runs::decimate(&result.equity_curve, runs::MAX_EQUITY_SAMPLES);

    runs::RunSeries {
        schema: runs::SERIES_SCHEMA,
        equity,
        // A real state, not a gap: `BacktestResult::equity_ts` is documented as empty when the
        // producer did not track timestamps (the vector kernels). `RunSeries`'s invariant admits
        // exactly this and nothing between it and "same length".
        equity_ts: keep_at_stride(&result.equity_ts, stride),
        per_symbol_equity: result
            .per_symbol_curves
            .iter()
            .map(|(sym, curve)| (sym.clone(), keep_at_stride(curve, stride)))
            .collect(),
        stride,
        source_len: result.equity_curve.len(),
        diagnostics: runs::RunDiagnostics {
            warmup: result.warmup,
            intrabar_both_hit: result.intrabar_both_hit,
            stale_deferrals: result.stale_deferrals,
            impact_unpriced: result.impact_unpriced,
            session_deferrals: result.session_deferrals,
            below_min_reversals: result.below_min_reversals,
            // The REALISED maker/taker mix and the commission total. Carried rather than declared
            // dropped (`crates/vike-backtest/tests/run_record_completeness.rs`'s first preference)
            // because neither is recoverable from any other document a run writes: `report.json`
            // carries no fee figure at all, and the per-trade `fees` are empty whenever a kernel
            // ran with `build_trades = false`. A run record that could not say what it was charged
            // is the same defect one layer down from the one
            // `docs/decisions/0063-the-studio-optimizer-derives-its-cost-model-and-declares-what-it-cannot.md`
            // ends on the wire.
            maker_fills: result.maker_fills,
            taker_fills: result.taker_fills,
            fees_paid: result.fees_paid,
            dropped: result
                .dropped
                .iter()
                .map(|(symbol, reason, size, weight)| runs::DroppedOrder {
                    symbol: symbol.clone(),
                    reason: reason.clone(),
                    size: *size,
                    weight: *weight,
                })
                .collect(),
        },
    }
}

/// Thin `v` at a stride somebody else DERIVED, keeping the last element the same way
/// [`runs::decimate`] does.
///
/// ⚠ The stride is an ARGUMENT rather than recomputed, and that is the whole point: `equity_ts` and
/// every per-symbol curve are positional companions of `equity_curve`, so a second `decimate` call
/// deriving its own stride would produce vectors of plausible length that no longer line up by
/// index with the equity they are supposed to explain — and [`runs::RunSeries::is_aligned`] would
/// then be checking an invariant this function had already broken.
///
/// ⚠ **Stride `0` means KEEP NOTHING and must be handled before the `stride == 1` fast path.**
/// [`runs::decimate`]'s own doc makes `0` the "nothing was kept" answer — the shape a
/// `--keep-series none` flag spells — and this function once spelled its fast path `stride <= 1`,
/// which swallowed `0` and returned the WHOLE vector. That disagreement is silent and it is
/// exactly the failure [`runs::RunSeries::is_aligned`] exists to make visible: `decimate` would
/// hand back an EMPTY `equity` while this kept every timestamp, so the document this build wrote
/// would fail its own invariant — and the flag meant to SUPPRESS the series would have written a
/// LARGER file than keeping it.
pub(super) fn keep_at_stride<T: Copy>(v: &[T], stride: usize) -> Vec<T> {
    if v.is_empty() || stride == 1 {
        return v.to_vec();
    }
    if stride == 0 {
        return Vec::new();
    }
    let mut out: Vec<T> = v.iter().step_by(stride).copied().collect();
    let last = v.len() - 1;
    if !last.is_multiple_of(stride) {
        out.push(v[last]);
    }
    out
}

/// The run's closed-trade LEDGER, bounded as a CHRONOLOGICAL PREFIX —
/// [`runs::MAX_TRADES`] carries the argument for why a ledger may not be sampled.
pub fn run_trades_from(result: &vike_analytics::BacktestResult) -> runs::RunTrades {
    runs::RunTrades {
        schema: runs::TRADES_SCHEMA,
        trades: result.trades.iter().take(runs::MAX_TRADES).cloned().collect(),
        source_len: result.trades.len(),
    }
}
