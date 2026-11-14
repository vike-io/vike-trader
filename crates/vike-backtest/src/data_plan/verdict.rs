//! The judgements over one resolved slice: the plan itself, the coverage gate, the universe rule.

use serde::{Deserialize, Serialize};
use vike_data::StoreKindId;
use vike_data::source::FUNDING_INTERVAL;
use vike_data::{
    HistoryStore, SeriesCoverage, SeriesId, SeriesMissingSpan, SeriesShortfall, window_shortfall,
};
use vike_model::run_request::gap::{OnGap, UniverseMode};

use super::{
    CoverageGate, DataPlan, DataPlanConversion, DataPlanInput, DataPlanSeries, DataPlanSeriesRole,
    PLAN_SCHEMA, human_ms, render_window,
};
use crate::harness::profile::resolve_conversions;
use crate::harness::{HarnessError, ProfileBacktest};
use vike_model::run_request::replay::ReplayKind;

/// Which stored series a profile's slice resolves to.
///
/// ⚠ **MOVED down from `crates/vike-backtesthub/src/backtest_cli.rs`, where it was
/// `fingerprint_series_ids`.** It answers ONE question — which series will this run open — and a
/// second consumer arrived that could not reach it there: `crate::data_plan` runs the same
/// resolution for the `data.require_coverage` gate and the `data.explain` plan, and both must work
/// on the REMOTE route, where the store is a trait-only `Arc<dyn HistoryStore>` (this crate)
/// while `vike_backtesthub::backtest_cli` links the concrete store and cannot be reached from here. A copy would have been a second
/// answer to "what does this run read", which is the fact a run's whole ADDRESS rests on. The name
/// widened with the consumers: it is the run's PLANNED slice, and the fingerprint is one of three
/// things now derived from it.
///
/// # Bar mode
///
/// One `kind=bar` series per symbol, sub-partitioned by the bar step — a real path segment in this
/// store, not a column. ⚠ **Never a grouped one**: `crates/vike-data/src/store/store_kind.rs` sets
/// `grouped: false` for `bar` and says why at the row ("no `symbol_col` column exists here, so a
/// bar series can never be grouped"), and `HistoryStoreDataFusion::load_bars` reads the per-symbol
/// directory alone.
///
/// # Tick mode
///
/// One series per LANE the entry's `crate::tick_replay::SeriesKind` admits — its `store_kinds`,
/// which the replay loader's OWN `wants_*` predicates are defined over, because `SeriesKind::Tick`
/// (the default, and what the `venue` x `symbols` expansion produces) means quotes AND trades AND
/// book events: a fingerprint naming only one would claim the run depended on less data than it did.
///
/// ⚠ **...PLUS every GROUPED series of the same `(kind, venue)`, and that half is not optional.**
/// All three tick kinds are `grouped: true`, and `HistoryStoreDataFusion`'s `scan_quotes`/`scan_trades`/
/// `scan_book_updates` each read the per-symbol directory AND union every `group=` directory under
/// `kind=…/venue=…`, filtering by the row-level symbol column. So on a grouped store — which is
/// what a recorded Polymarket tape is — the per-symbol path does not exist and ALL the rows the run
/// replays live in the group. Naming only the per-symbol id there made the data half of the address
/// a CONSTANT: a backfill could add a month, the run could replay half again as many rows and
/// produce a materially different curve, and `vike_backtesthub::run_fingerprint::input_fingerprint` would be unchanged — so a later
/// `diff`/`gate` would attribute a DATA change to an engine edit. Silent in both directions.
///
/// ⚠ **The accepted residual, stated rather than discovered.** A grouped series' coverage is the
/// coverage of the WHOLE GROUP, so rows belonging to a symbol this run never read still move the
/// address. That is a FALSE NEGATIVE — two runs with genuinely identical inputs can address
/// differently, and a comparison then reports "no comparable baseline", which is conservative and
/// visible. The alternative is the FALSE POSITIVE above, where two runs over different data address
/// the same and a comparison silently blames the wrong thing. Narrowing this needs per-symbol
/// statistics inside a grouped part, which the store does not expose.
///
/// ⚠ **The same residual reads DIFFERENTLY for a coverage gate, and that is worth knowing before
/// reusing this list there.** A grouped series' coverage spans the whole group, so a window the
/// group covers reads as covered for a symbol whose own rows may be absent from it — a FALSE
/// NEGATIVE for the gate, i.e. it can pass a run it should have stopped. That is the conservative
/// direction for an ADDRESS and the permissive one for a GATE, and no narrowing exists on either
/// side until the store exposes per-symbol statistics inside a grouped part.
///
/// `held` is the store's own inventory (`HistoryStoreDataFusion::list_series`, or the
/// `vike_data::HistoryStore::inventory` ids on the trait route). It is a PARAMETER rather than a
/// second call so the walk that answers "what does this store hold" happens exactly once.
pub fn planned_series_ids(profile: &ProfileBacktest, held: &[SeriesId]) -> Vec<SeriesId> {
    let mut out = Vec::new();
    for s in profile.data.resolved_series() {
        match profile.data.kind {
            ReplayKind::Bar => {
                let id = SeriesId::per_symbol(
                    StoreKindId::Bar,
                    &s.venue,
                    &s.symbol,
                    Some(profile.data.interval.clone()),
                );
                // A lane's or a file's bars are not the venue's series (decision 0268), so the run
                // addresses the series it reads. Absent for every profile naming neither key.
                out.push(match profile.data.bar_series_source() {
                    Some(source) => id.with_source(source),
                    None => id,
                });
            }
            ReplayKind::Tick => {
                for &kind in s.kind.store_kinds() {
                    out.push(SeriesId::per_symbol(kind, &s.venue, &s.symbol, None));
                    out.extend(
                        held.iter()
                            .filter(|h| h.group.is_some() && h.kind == kind && h.venue == s.venue)
                            .cloned(),
                    );
                }
            }
        }
    }
    // Two symbols of one venue pull the SAME grouped directories, and the reader reads each group
    // once. Sorted so the collector's output order is a function of the inputs rather than of the
    // profile's symbol order — `vike_backtesthub::run_fingerprint::DataFingerprint::canonical` sorts its lines too, so this is for
    // the RECORD's readability rather than for the address.
    out.sort();
    out.dedup();
    out
}

/// The series a run opens that [`planned_series_ids`] does not name — the intrabar detail tape, the
/// market funding-rate history and the point-in-time instrument grid.
///
/// ⚠ **Kept OUT of `planned_series_ids` on purpose.** That function feeds the run ADDRESS, and
/// widening it would move every stored address and orphan every baseline — the one property
/// `vike_backtesthub::run_fingerprint`'s module doc exists to guarantee. So the hole in the address stays open
/// and declared ([`DataPlanSeriesRole`]'s doc names it), while the PLAN is complete: a plan that omitted the
/// detail tape would report a clean slice for a run that is about to refuse because the finer series
/// is absent (`crate::harness::run`'s `load_profile_detail_bars` does exactly that).
///
/// A search's `--resume` witness reads this too, and may: it names a series only for a key the
/// profile set, so a witness over a profile that set none stays what it was
/// (`crates/vike-backtesthub/src/backtest_cli/persist.rs`'s `search_witness`).
pub fn auxiliary_series(profile: &ProfileBacktest) -> Vec<(SeriesId, DataPlanSeriesRole)> {
    let mut out = Vec::new();
    for s in profile.data.resolved_series() {
        // Bar mode only: `detail_interval` is refused in tick mode by `ProfileData::detail_interval_ms`,
        // so naming it here for a tick profile would describe a read that cannot happen.
        if profile.data.kind == ReplayKind::Bar
            && let Some(detail) = profile.data.detail_interval.as_deref()
        {
            let id = SeriesId::per_symbol(
                StoreKindId::Bar,
                &s.venue,
                &s.symbol,
                Some(detail.to_string()),
            );
            // Read from the coarse bars' lane (`crate::harness::run`'s `load_profile_detail_bars`).
            let id = match profile.data.bar_series_source() {
                Some(source) => id.with_source(source),
                None => id,
            };
            out.push((id, DataPlanSeriesRole::Detail));
        }
        // Both lanes read the funding series (the tick lane charges the same schedule), a
        // perpetual's by default (`ProfileEngine::attaches_funding`) — so an armed
        // `data.require_coverage` refuses a perpetual run with no funding history unless the
        // profile says `attach_funding = false`.
        if profile.engine.attaches_funding(&s.venue, &s.symbol) {
            // `interval=funding` under `kind=bar` — see [`DataPlanSeriesRole::Funding`] for the pinned
            // name collision with the `kind=exec_funding` series.
            out.push((
                SeriesId::per_symbol(
                    StoreKindId::Bar,
                    &s.venue,
                    &s.symbol,
                    Some(FUNDING_INTERVAL.to_string()),
                ),
                DataPlanSeriesRole::Funding,
            ));
        }
        if profile.engine.snap_to_properties {
            out.push((
                SeriesId::per_symbol(StoreKindId::Properties, &s.venue, &s.symbol, None),
                DataPlanSeriesRole::Properties,
            ));
        }
    }
    out
}

/// Resolve this profile's slice and join the store's answers onto it — the shared core of
/// `data.explain`, `data.require_coverage` and `data.universe`.
///
/// # TOTAL: a store that cannot answer produces a plan, never an error
///
/// The only `Err` this returns is `ProfileBacktest::range`'s, because a profile whose window does
/// not parse has no plan to make. Every store failure becomes a NOTE and an absent coverage — see
/// the module doc's posture split, and [`enforce`] for the caller that must refuse on the same
/// input.
///
/// `store_label` and `store_rung` are PARAMETERS rather than something resolved here: this module
/// cannot see how the caller found its store, and a rung it guessed at would be the most
/// confidently wrong field in the document. The backtesthub passes `None` for the rung because
/// it did not resolve one.
///
/// # Cost
///
/// One `inventory()` — a manifest fold per series, no Parquet scan — plus one `series_gaps()` per
/// PLANNED series that the store holds. The same cost class `vike_backtesthub::backtest_cli`'s
/// `collect_data_fingerprint` already pays on every single run, which is why an armed gate is
/// affordable on a run that was going to pay it anyway; [`enforce`] still refuses to pay any of it
/// for a profile that armed nothing.
///
/// # The inputs
///
/// `inputs` are the series the strategy reads by name (decision 0271): what
/// [`super::declared_inputs`] answers for a door that plans without running, nothing for one whose
/// strategy reads none. Each is a [`DataPlanSeriesRole::Input`] row after the others, its coverage asked of
/// the store as theirs is, and judged up to its last bar's CLOSE (`last_timestamp` plus one interval), since
/// that bar is what a decision reads until the next one closes.
///
/// # The conversion pairs
///
/// Each pair `engine.conversion` names (decision 0243) is a [`DataPlanSeriesRole::Conversion`] row after
/// the inputs, judged as an input row is, from one of its bars before the window: that bar's close is
/// the rate of the window's first instant.
pub fn plan_data(
    profile: &ProfileBacktest,
    inputs: &[DataPlanInput],
    store: &(dyn HistoryStore + Send + Sync),
    store_label: &str,
    store_rung: Option<(&str, &str)>,
) -> Result<DataPlan, HarnessError> {
    plan(profile, inputs, &resolve_conversions(profile)?, store, store_label, store_rung)
}

/// [`plan_data`] over the conversion pairs it is handed: the armed gate ([`enforce`]) hands none, as it
/// hands no input, because the run reads and judges both on their bars.
fn plan(
    profile: &ProfileBacktest,
    inputs: &[DataPlanInput],
    conversions: &[DataPlanConversion],
    store: &(dyn HistoryStore + Send + Sync),
    store_label: &str,
    store_rung: Option<(&str, &str)>,
) -> Result<DataPlan, HarnessError> {
    let range = profile.range()?;
    let mut notes: Vec<String> = Vec::new();

    let (inventory, enumerable) = match store.inventory() {
        Ok(listed_inventory) => (listed_inventory, true),
        Err(e) => {
            notes.push(format!(
                "this store cannot enumerate its inventory ({e}), so NO row count below is a \
                 measurement — every series reads as not held because nobody could ask. An armed \
                 data.require_coverage refuses on exactly this, rather than passing a run it \
                 cannot prove"
            ));
            (Vec::new(), false)
        }
    };
    let held: Vec<SeriesId> = inventory.iter().map(|(id, _)| id.clone()).collect();

    // ONE resolution, shared with the run ADDRESS. The auxiliary series are appended rather than
    // folded into it — see `auxiliary_series`.
    let mut planned: Vec<(SeriesId, DataPlanSeriesRole)> = planned_series_ids(profile, &held)
        .into_iter()
        .map(|id| (id, DataPlanSeriesRole::Price))
        .collect();
    planned.extend(auxiliary_series(profile));
    planned.sort();
    planned.dedup_by(|a, b| a.0 == b.0);

    let mut gaps_unknown = false;
    let mut series = Vec::with_capacity(planned.len() + inputs.len() + conversions.len());
    for (id, role) in planned {
        let (coverage, gaps) = held_facts(store, &inventory, &id, &mut notes, &mut gaps_unknown);
        let recorded =
            coverage.as_ref().filter(|c| c.rows > 0).map(|c| (c.first_timestamp, c.last_timestamp));
        let missing = window_shortfall(range.start, range.end, recorded, &gaps);
        series.push(DataPlanSeries { id, role, coverage, missing, input: None, conversion: None });
    }
    // A series read on its bars (an input, a conversion pair) covers up to its last bar's CLOSE.
    let mut by_bars = |id: &SeriesId, interval_ms: i64, start: Option<i64>| {
        let (coverage, gaps) = held_facts(store, &inventory, id, &mut notes, &mut gaps_unknown);
        let last_close = |c: &SeriesCoverage| c.last_timestamp.saturating_add(interval_ms - 1);
        let recorded =
            coverage.as_ref().filter(|c| c.rows > 0).map(|c| (c.first_timestamp, last_close(c)));
        let missing = window_shortfall(start, range.end, recorded, &gaps);
        (coverage, missing)
    };
    for input in inputs {
        let (coverage, missing) = by_bars(&input.id, input.interval_ms, range.start);
        series.push(DataPlanSeries {
            id: input.id.clone(),
            role: DataPlanSeriesRole::Input,
            coverage,
            missing,
            input: Some(input.name.clone()),
            conversion: None,
        });
    }
    for c in conversions {
        let (coverage, missing) = by_bars(&c.id, c.interval_ms, c.judged_from(range.start));
        series.push(DataPlanSeries {
            id: c.id.clone(),
            role: DataPlanSeriesRole::Conversion,
            coverage,
            missing,
            input: None,
            conversion: Some(c.quote_currency.clone()),
        });
    }

    if profile.walkforward.is_some() {
        notes.push(
            "[walkforward] slices this window into out-of-sample windows; the coverage below is \
             the WHOLE range every window is cut from, so a shortfall at either end lands in the \
             first or last window rather than being spread across all of them"
                .to_string(),
        );
    }
    if profile.is_paramscan() {
        notes.push(
            "[paramscan] runs this same slice once per grid point — the data facts below are the \
             same for every trial, so a coverage problem here is a problem with the whole search"
                .to_string(),
        );
    }

    Ok(DataPlan {
        schema: PLAN_SCHEMA,
        store: store_label.to_string(),
        store_rung: store_rung.map(|(r, _)| r.to_string()),
        store_rung_why: store_rung.map(|(_, w)| w.to_string()),
        from_ms: range.start,
        to_ms: range.end,
        kind: match profile.data.kind {
            ReplayKind::Bar => "bar".to_string(),
            ReplayKind::Tick => "tick".to_string(),
        },
        interval: profile.data.interval.clone(),
        enumerable,
        gaps_unknown,
        series,
        notes,
    })
}

/// What the store holds of one planned series and the holes it reports inside it, for
/// [`plan_data`]: a failed read is a NOTE (the plan is total) and an unreported hole sets
/// `gaps_unknown`, which [`enforce`] refuses on.
fn held_facts(
    store: &(dyn HistoryStore + Send + Sync),
    inventory: &[(SeriesId, SeriesCoverage)],
    id: &SeriesId,
    notes: &mut Vec<String>,
    gaps_unknown: &mut bool,
) -> (Option<SeriesCoverage>, Vec<(i64, i64)>) {
    let coverage = match inventory.iter().find(|(h, _)| h == id) {
        Some((_, c)) => Some(c.clone()),
        // `inventory` answers for the sourceless leaves alone (decision 0255), so a lane's
        // coverage (`data.source`) is asked of its series; an empty one is the absent series.
        None if id.source.is_some() => match store.series_facts(id) {
            Ok((c, _)) => (c.rows > 0).then_some(c),
            Err(e) => {
                notes.push(format!(
                    "the store could not report the lane source={} of {} {} ({e}), so it \
                     reads as not held",
                    id.source.as_deref().unwrap_or_default(),
                    id.venue,
                    id.label()
                ));
                None
            }
        },
        None => None,
    };
    // Only for a series that is actually there: `series_gaps` on an absent one is a manifest
    // read that answers "no holes" about a span that does not exist, and the ABSENCE is already
    // the finding.
    let gaps = if coverage.is_some() {
        match store.series_gaps(id) {
            Ok(g) => g,
            Err(e) => {
                *gaps_unknown = true;
                notes.push(format!(
                    "the store could not report gaps for {} {} {} ({e}), so only the leading \
                     and trailing ends of this series were checked — an interior hole would \
                     not appear below",
                    id.kind,
                    id.venue,
                    id.label()
                ));
                Vec::new()
            }
        }
    } else {
        Vec::new()
    };
    (coverage, gaps)
}

/// One coverage finding: a series, and the spans of the window it cannot answer for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoverageFinding {
    pub id: SeriesId,
    /// The spans that exceeded the tolerance, or every span when there is none.
    pub spans: Vec<SeriesMissingSpan>,
    /// `true` when the store holds no such series at all — the case `data.max_gap` can never
    /// tolerate.
    pub absent: bool,
}

/// What an armed gate found, and what it means to do about it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoverageVerdict {
    pub findings: Vec<CoverageFinding>,
    /// `true` when the store could not be enumerated, or would not report the holes in a series it
    /// holds, so coverage could not be PROVEN either way.
    /// A distinct field rather than a synthetic finding: "I found holes" and "I could not look" are
    /// different sentences and an operator must not have to tell them apart from a span list.
    pub unprovable: bool,
}

impl CoverageVerdict {
    /// `true` when nothing was found and coverage was provable.
    pub fn is_clean(&self) -> bool {
        self.findings.is_empty() && !self.unprovable
    }

    /// The findings as operator-facing lines, each ending in the command that would fill the hole.
    ///
    /// ⚠ The fetch command is per SERIES and names the window that is missing, not the run's whole
    /// window: re-fetching a year to fill four days is how an operator learns to ignore the
    /// suggestion. `vike-cli data hist fetch` takes `VENUE:SYMBOL:INTERVAL`, so the line is only
    /// offered for a shape that spelling can express — a GROUPED series or a tick lane has no such
    /// argument,
    /// and a command that would not run is worse than none.
    pub fn lines(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.unprovable {
            out.push(
                "coverage is UNPROVABLE: this store would not list what it holds, or the holes in a \
                 series it holds, so the window was not fully checked"
                    .to_string(),
            );
        }
        for f in &self.findings {
            let what = if f.absent { "holds no" } else { "does not cover the window for" };
            out.push(format!(
                "the store {what} kind={} venue={} {}{}",
                f.id.kind,
                f.id.venue,
                f.id.label(),
                f.id.interval.as_deref().map(|i| format!(" interval={i}")).unwrap_or_default()
            ));
            for m in &f.spans {
                out.push(format!(
                    "    {} {} ({})",
                    m.kind.as_str(),
                    render_window(Some(m.start_ms), Some(m.end_ms)),
                    human_ms(m.len_ms())
                ));
            }
            if let Some(cmd) = fetch_hint(&f.id, f.spans.first()) {
                out.push(format!("    fill it with: {cmd}"));
            }
        }
        out
    }
}

/// The `vike-cli data hist fetch` line that would fill one missing span, or `None` for a series
/// that spelling cannot name.
///
/// ⚠ **It printed `vike-cli data fetch …` until 2026-09-26** — the flat, pre-group spelling, which
/// `vike-cli` has REFUSED (exit 2, naming `data hist fetch`) since the data verbs moved into groups
/// (#2072). So a plan that found a hole handed the operator a command that did not run, which is
/// the one thing this function's doc below says is worse than offering none. The spelling lives in
/// another crate this one cannot link, so nothing here can drive its parser; the test pins the
/// prefix, and the next move of that grammar has to find this line by grep.
///
/// `kind=bar` per-symbol series only, and that is not a shortcut: `data hist fetch`'s spec is
/// `VENUE:SYMBOL:INTERVAL` and it always writes `kind=bar` per symbol. A suggested command for a
/// grouped or tick series would name a series the fetch cannot write, which is worse than offering
/// nothing.
///
/// ⚠ That sentence used to cite "its own module doc" — `crate::fetch`'s, which is DELETED along
/// with this crate's venue call. The claim survives its source because the SPEC is what carries it:
/// `VENUE:SYMBOL:INTERVAL` has no field for a kind, a group or a tick, so there is no other shape
/// to express. `crates/vike-cli/src/cmd/data/hist/parse.rs`'s `parse` is where that grammar lives now.
///
/// ⚠ **The hint is no longer runnable on a box with no server.** `vike-cli data hist fetch` asks
/// a DATAHUB now rather than calling a venue, so the line this emits needs one reachable (default
/// `127.0.0.1:7878`, or `--addr`). It is still the right command to offer — it is the only fetch
/// there is — but a reader who runs it against nothing gets a connect error, not a fill.
pub(super) fn fetch_hint(id: &SeriesId, span: Option<&SeriesMissingSpan>) -> Option<String> {
    let span = span?;
    if id.kind != StoreKindId::Bar || id.group.is_some() || id.symbol.is_empty() {
        return None;
    }
    let interval = id.interval.as_deref()?;
    if span.start_ms == i64::MIN || span.end_ms == i64::MAX {
        // An unbounded window has no `--from`/`--to` to offer, and `data fetch` requires one form
        // or the other.
        return None;
    }
    Some(format!(
        "vike-cli data hist fetch {}:{}:{} --from {} --to {}",
        id.venue,
        id.symbol,
        interval,
        vike_model::time::epoch_ms_to_utc_date(span.start_ms),
        vike_model::time::epoch_ms_to_utc_date(span.end_ms)
    ))
}

/// Judge a plan against an armed gate — PURE, so both dispositions are testable without a store.
///
/// # ⚠ `SeriesShortfall::Everything` ignores `max_gap` under every value
///
/// A series the store does not hold is not a gap of some length; it is a lane that was never
/// recorded. A `max_gap` generous enough to swallow the window would otherwise disarm the gate for
/// the one case it most exists to catch — the window with a complete trade tape and no book at all,
/// which is the failure `crates/vike-data/src/store/coverage.rs`'s module doc measured.
pub fn coverage_verdict(plan: &DataPlan, gate: &CoverageGate) -> CoverageVerdict {
    if !gate.armed {
        return CoverageVerdict { findings: Vec::new(), unprovable: false };
    }
    let tolerance = gate.max_gap_ms.unwrap_or(0);
    let mut findings = Vec::new();
    for s in &plan.series {
        let spans: Vec<SeriesMissingSpan> = s
            .missing
            .iter()
            .copied()
            .filter(|m| m.kind == SeriesShortfall::Everything || m.len_ms() > tolerance)
            .collect();
        if !spans.is_empty() {
            let absent = !s.held();
            findings.push(CoverageFinding { id: s.id.clone(), spans, absent });
        }
    }
    CoverageVerdict { findings, unprovable: !plan.enumerable || plan.gaps_unknown }
}

/// What the universe rule found: which declared members the tape spans, and which it does not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoverageUniverseVerdict {
    /// Declared members whose every PRICE series spans the window.
    pub present: Vec<String>,
    /// `(symbol, why)` for each member that does not — the survivorship signature.
    pub absent: Vec<(String, String)>,
}

impl CoverageUniverseVerdict {
    /// `true` when every declared member was there for the whole window.
    pub fn is_clean(&self) -> bool {
        self.absent.is_empty()
    }

    /// The verdict as operator-facing lines.
    pub fn lines(&self) -> Vec<String> {
        let mut out = Vec::new();
        for (symbol, why) in &self.absent {
            out.push(format!("{symbol}: {why}"));
        }
        if !out.is_empty() {
            out.push(format!(
                "{} of {} declared members were not there for the whole window — a symbol list \
                 chosen today is a list of SURVIVORS, and running it over a past window assumes \
                 every member existed then",
                self.absent.len(),
                self.absent.len() + self.present.len()
            ));
        }
        out
    }
}

/// Judge the declared universe against the plan — PURE, testable without a store.
///
/// # Why it reads the EXISTENCE shortfalls only
///
/// A leading shortfall is a member that was not there when the window opened; a trailing one is a
/// member that stopped. An INTERIOR hole is a recorder outage in the middle of an instrument's life
/// and says nothing about membership — including it would make every store with one bad afternoon
/// report a survivorship problem, which is how a warning stops being read.
/// [`CoverageGate`] is the rule that judges interior holes, and the two are deliberately separate.
///
/// PRICE series only ([`DataPlanSeriesRole::Price`]). An absent properties grid or funding history is a
/// data gap, not a statement about whether the instrument existed.
pub fn universe_verdict(plan: &DataPlan, mode: UniverseMode) -> CoverageUniverseVerdict {
    let mut present = Vec::new();
    let mut absent = Vec::new();
    if !mode.consults_the_store() {
        return CoverageUniverseVerdict { present, absent };
    }
    // Grouped series carry an EMPTY symbol and name a group, so they cannot answer a question about
    // one member; they are skipped rather than blamed. That is the grouped residual again, and it
    // means a purely grouped tick slice has no universe verdict to give — which the caller sees as
    // an empty `present` rather than as a clean bill of health.
    for s in
        plan.series.iter().filter(|s| s.role == DataPlanSeriesRole::Price && s.id.group.is_none())
    {
        let shortfalls = s.existence_shortfalls();
        if shortfalls.is_empty() {
            present.push(s.id.symbol.clone());
            continue;
        }
        let why = shortfalls
            .iter()
            .map(|m| match m.kind {
                SeriesShortfall::Everything => format!(
                    "the store holds no {} series for it at all over {}",
                    s.id.kind,
                    render_window(Some(m.start_ms), Some(m.end_ms))
                ),
                SeriesShortfall::Leading => format!(
                    "its {} tape does not reach back to the window's start — {} is missing ({})",
                    s.id.kind,
                    render_window(Some(m.start_ms), Some(m.end_ms)),
                    human_ms(m.len_ms())
                ),
                SeriesShortfall::Trailing => format!(
                    "its {} tape stops before the window's end — {} is missing ({})",
                    s.id.kind,
                    render_window(Some(m.start_ms), Some(m.end_ms)),
                    human_ms(m.len_ms())
                ),
                SeriesShortfall::Interior => {
                    unreachable!("existence_shortfalls filters interior out")
                }
            })
            .collect::<Vec<_>>()
            .join("; ");
        absent.push((s.id.symbol.clone(), why));
    }
    present.sort();
    present.dedup();
    absent.sort();
    absent.dedup();
    CoverageUniverseVerdict { present, absent }
}

/// What `engine.snap_to_properties` has to say about the store, as lines for the run's report.
///
/// "No record" is a legal answer to a fill's lookup (`vike_simulator`'s `OrderBrokerSimulator::grid_for`): the fill then
/// skips its rounding and its minimums, and the run reads as clean. The answer at the window's END says
/// whether the store holds a grid at all, and the one at its START whether every fill had one: a tape
/// begins the day its recorder was switched on, so a window older than that holds fills with none.
///
/// `properties_as_of` because it is the read a fill makes and the one a remote store serves. A store that
/// will not answer refuses the run: nothing can be said about what its fills skip.
fn missing_grid_lines(
    profile: &ProfileBacktest,
    store: &(dyn HistoryStore + Send + Sync),
) -> Result<Vec<String>, HarnessError> {
    if !profile.engine.snap_to_properties {
        return Ok(Vec::new());
    }
    let range = profile.range()?;
    let mut lines = Vec::new();
    for s in profile.data.resolved_series() {
        let symbol = &s.symbol;
        let has_grid_at = |ts| {
            store.properties_as_of(&s.venue, symbol, ts).map(|grid| grid.is_some()).map_err(|e| {
                HarnessError::Data(format!(
                    "engine.snap_to_properties is on and the instrument properties of {} {symbol} \
                     could not be read ({e}). Nothing ran.",
                    s.venue
                ))
            })
        };
        if !has_grid_at(range.end.unwrap_or(i64::MAX))? {
            lines.push(format!(
                "{symbol}: engine.snap_to_properties is on and the store holds no instrument \
                 properties for it, so this report's fills skipped its tick and lot rounding, its \
                 min_qty and min_notional, and any venue hold"
            ));
        } else if !has_grid_at(range.start.unwrap_or(i64::MIN))? {
            lines.push(format!(
                "{symbol}: engine.snap_to_properties is on and the store's instrument properties for \
                 it begin after this window opens, so this report's fills before the first record \
                 skipped its tick and lot rounding, its min_qty and min_notional, and the run has no \
                 venue hold, which it reads at its start"
            ));
        }
    }
    Ok(lines)
}

/// **The pre-flight a run calls**: resolve the gate and the universe rule from the profile, and
/// either refuse the run or hand back the lines it should warn with, plus what
/// [`missing_grid_lines`] finds under `engine.snap_to_properties`.
///
/// # ⚠ A profile that armed nothing reads NOTHING from the store
///
/// The early return below is what makes every profile written before these keys existed
/// byte-identical AND free: no `inventory()`, no `series_gaps()`, no allocation. That is not an
/// optimisation — a pre-flight that cost a manifest fold per series on every run would have to be
/// justified against the latency of a sweep's ten-thousandth trial, and it cannot be.
/// `engine.snap_to_properties` is an arming of its own, and it pays one or two `properties_as_of` per
/// symbol, which is what its first fill pays.
///
/// # ⚠ ...and an ARMED profile pays it PER TRIAL, which is the cost to own
///
/// Every caller of `super::run::run_backtest` reaches this — a single run and every `[paramscan]`
/// trial, while a walk-forward calls it once, in `crate::walkforward::runner`'s pre-flight — so an
/// armed gate over a grid pays one `inventory()` plus one
/// `series_gaps()` per series per TRIAL. That is a manifest fold, not a Parquet scan, and it is the
/// same read `vike_backtesthub::backtest_cli`'s `collect_data_fingerprint` already performs once per run; but
/// on a grouped store whose manifest is multi-megabyte, times a thousand trials, it is real. The
/// arming is opt-in and per profile, so a search can simply not arm it — and the honest place to
/// arm it for a search is `--explain-data` on the same profile FIRST, which answers the same
/// question once. Hoisting a per-search pre-flight above the trial loop is the obvious cure and is
/// deliberately NOT done here: it would put the check in the two search entry points rather than in
/// the one function every route funnels through, and a gate that some routes skip is the class of
/// hole this file exists to close.
///
/// # What each disposition returns
///
/// * [`OnGap::Refuse`] (the default) — `Err(HarnessError::Data(…))` naming every missing span and,
///   where the spelling can express it, the `vike-cli data hist fetch` line that would fill the
///   first.
/// * [`OnGap::Warn`] — `Ok(lines)`; the caller logs them and runs unchanged.
/// * [`OnGap::Run`] — `Ok(vec![])`; the gate is inert.
///
/// Every `Ok` leads with the run's kept-row warnings (`ProfileBacktest::kept`: a series no kept instrument row
/// covers), empty for a run no door handed the lists.
///
/// [`UniverseMode::Strict`] refuses on its own terms, independently of the coverage gate — the two
/// judge different things (see [`universe_verdict`]) and a profile may arm either alone.
///
/// # ⚠ It refuses when coverage is UNPROVABLE, not only when it is bad
///
/// A store that cannot enumerate its inventory, or will not report the holes in a series it holds,
/// cannot prove the window is covered. An armed gate
/// that passed there would be the "green means nothing ran" failure with a gate's authority behind
/// it: the operator asked for a proof and got silence rendered as success. `OnGap::Warn` still only
/// warns — they asked for that too. The plan's notes go with the verdict there, because they are
/// what names the series and carries the store's own error.
pub fn enforce(
    profile: &ProfileBacktest,
    store: &(dyn HistoryStore + Send + Sync),
    store_label: &str,
) -> Result<Vec<String>, HarnessError> {
    let gate = profile.data.coverage_gate()?;
    let mode = profile.data.universe_mode()?;
    // What the run's kept instrument rows do not cover (`ProfileBacktest::kept`), whatever the gate: empty for a
    // run no door handed the lists.
    let mut warnings = profile.kept.warnings.clone();
    warnings.extend(missing_grid_lines(profile, store)?);
    if !gate.armed && !mode.consults_the_store() {
        return Ok(warnings);
    }
    // No input or conversion rows: the run reads and judges both on their bars (`super::load_inputs`,
    // `super::load_conversions`).
    let plan = plan(profile, &[], &[], store, store_label, None)?;
    let coverage = coverage_verdict(&plan, &gate);
    let universe = universe_verdict(&plan, mode);

    if !coverage.is_clean() {
        let mut lines = coverage.lines();
        if coverage.unprovable {
            lines.extend(plan.notes.iter().cloned());
        }
        match gate.on_gap {
            OnGap::Refuse => {
                return Err(HarnessError::Data(format!(
                    "data.require_coverage is armed and the store does not cover this run's \
                     window. Nothing ran.\n{}\n{}",
                    lines.join("\n"),
                    "A window with a complete trade tape and no book runs to completion and \
                     REPORTS FILLS — which is why this refuses rather than warns. Fill the spans \
                     above, widen data.max_gap if the gap is genuinely acceptable, or set \
                     data.on_gap = \"warn\"."
                )));
            }
            OnGap::Warn => warnings.extend(lines),
            OnGap::Run => {}
        }
    }

    if !universe.is_clean() {
        match mode {
            UniverseMode::Strict if !plan.enumerable => {
                return Err(HarnessError::Data(format!(
                    "the store would not say what it holds, so data.universe = \"strict\" could not \
                     check this run's declared universe. Nothing ran.\n{}",
                    universe_lines(&plan, &universe).join("\n")
                )));
            }
            UniverseMode::Strict => {
                return Err(HarnessError::Data(format!(
                    "data.universe = \"strict\" and this run's declared universe was not fully \
                     listed over the window. Nothing ran.\n{}\n{}",
                    universe.lines().join("\n"),
                    "Move data.from to a date every member covers, drop the members that were not \
                     there, or set data.universe = \"covered\" to run anyway with the finding on \
                     the record. Nothing here narrows the slice for you — a universe a run edited \
                     silently would not be the one this profile names."
                )));
            }
            UniverseMode::Covered => {
                // An unprovable gate above may already have said these same notes.
                for line in universe_lines(&plan, &universe) {
                    if !warnings.contains(&line) {
                        warnings.push(line);
                    }
                }
            }
            UniverseMode::Declared => {}
        }
    }

    Ok(warnings)
}

/// What the universe rule reports. A store that would not list itself reads every member as absent
/// because nobody could ask, so the store's own error (the plan's notes) is the report: "holds no series
/// for it" and the survivorship line would be a failed read in the words of a true answer.
fn universe_lines(plan: &DataPlan, universe: &CoverageUniverseVerdict) -> Vec<String> {
    if plan.enumerable { universe.lines() } else { plan.notes.clone() }
}

/// The `data.explain` answer: the plan, plus what an armed gate and universe rule would say about
/// it, as ONE document.
///
/// Both explain doors call this — `vike_backtesthub::backtest_cli`'s local profile run and
/// `vike_backtesthub::backtesthub_server`'s three profile arms — so a plan seen over the wire and a plan printed
/// on the box cannot differ. The gate and universe sections are present only when the profile armed
/// them, because a report full of "nothing to say" trains a reader to skim.
pub fn explain_document(
    profile: &ProfileBacktest,
    plan: &DataPlan,
) -> Result<serde_json::Value, HarnessError> {
    let gate = profile.data.coverage_gate()?;
    let mode = profile.data.universe_mode()?;
    let coverage = coverage_verdict(plan, &gate);
    let universe = universe_verdict(plan, mode);
    let mut doc = plan.to_json();
    if let Some(map) = doc.as_object_mut() {
        map.insert("explained".into(), serde_json::json!(true));
        if gate.armed {
            map.insert("coverage".into(), serde_json::to_value(&coverage).unwrap_or_default());
        }
        if mode.consults_the_store() {
            map.insert("universe".into(), serde_json::to_value(&universe).unwrap_or_default());
        }
        // The rendered sentences, extended with whatever the two rules added — see
        // `DataPlan::to_json` for why the lines ride inside the document at all.
        let mut lines = plan.lines();
        if gate.armed && !coverage.is_clean() {
            lines.push("-- data.require_coverage would report --".to_string());
            lines.extend(coverage.lines());
        }
        if mode.consults_the_store() && !universe.is_clean() {
            lines.push("-- data.universe would report --".to_string());
            lines.extend(universe_lines(plan, &universe));
        }
        map.insert("explain".into(), serde_json::json!(lines));
    }
    Ok(doc)
}

/// The `data.explain` answer as operator-facing lines — the human half of [`explain_document`],
/// read out of the same document so the two cannot disagree about what was found.
pub fn explain_lines(
    profile: &ProfileBacktest,
    plan: &DataPlan,
) -> Result<Vec<String>, HarnessError> {
    let doc = explain_document(profile, plan)?;
    Ok(doc
        .get("explain")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_default())
}
