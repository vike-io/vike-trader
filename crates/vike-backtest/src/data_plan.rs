//! **What a run is about to READ, answered before it reads it** — the plan, the coverage gate and
//! the point-in-time universe rule, over one resolution of the slice.
//!
//! # Why one module for three flags
//!
//! `data.explain`, `data.require_coverage` and `data.universe` ask three different questions of the
//! SAME two facts: which series will this run open, and what does the store hold for each of them
//! over the requested window. Splitting them would have meant resolving the slice three times and
//! then keeping three answers in step — and the slice resolution is not trivial (the tick lanes a
//! `SeriesKind` filter admits, the GROUPED directories the tick readers union in). So the slice is
//! resolved ONCE, by [`crate::run_fingerprint::planned_series_ids`], which already had to answer
//! exactly this for the run ADDRESS; this module joins the store's own answers onto it and the
//! three rules read the join.
//!
//! # ⚠ Almost none of this is new machinery, and the part that is, is small
//!
//! The pieces were already here and nothing had put them together:
//!
//! * `crate::run_fingerprint::planned_series_ids` resolves the slice (moved here-ward from
//!   `backtest_cli` so the trait route can reach it — its own doc carries the move).
//! * `vike_data::HistStore::inventory` gives every held series with its cheap coverage (rows,
//!   bytes, parts, dates, first/last ts) as a manifest FOLD — no Parquet scan.
//! * `vike_data::HistStore::series_gaps` gives the holes inside a recorded span.
//! * `vike_data::coverage`'s cross-KIND report already argues the failure this gate exists to
//!   stop, and has since it was written.
//!
//! What genuinely did not exist is `vike_data::window_shortfall`: coverage of a REQUESTED WINDOW.
//! `find_gaps` reports holes STRICTLY INSIDE the recorded span, so a window opening a month before
//! a series' first row has no gap at all by that definition — which is the commonest shape of the
//! failure and the one every existing tool was blind to.
//!
//! # What the gate can and cannot see
//!
//! It knows what the file index knows: which days exist and how many rows they hold. A day that is
//! PRESENT and was recorded through a feed outage passes — `vike_data::quality` is the question
//! over scanned rows, and a coverage gate that implied otherwise would be worse than none.
//!
//! ⚠ And it inherits the GROUPED-SERIES residual from the slice resolver, wearing the opposite
//! sign. A grouped series' coverage is the whole group's, so a window the group covers reads as
//! covered for a symbol whose own rows may be absent from it. For an ADDRESS that direction is
//! conservative (two identical runs can fail to match); for a GATE it is PERMISSIVE — it can pass a
//! run it should have stopped. Neither can be narrowed until the store exposes per-symbol
//! statistics inside a grouped part. Stated here rather than left to be discovered, because a gate
//! whose blind spot is undocumented is worse than no gate.
//!
//! # The posture split: the PLAN is total, the GATE refuses
//!
//! [`plan_data`] never fails on a store that cannot answer — an un-enumerable store yields a plan
//! with no row counts and a NOTE saying so, because `data.explain`'s whole job is to report
//! honestly and "I could not ask" is a report. [`enforce`] takes the opposite posture on the same
//! input: a store that cannot enumerate its inventory cannot PROVE coverage, so an armed gate
//! refuses rather than passing. One resolution, two dispositions, and the disposition belongs to
//! the caller's question rather than to the data.
//!
//! # ⚠ Where the refusals live, and why not in `harness/`
//!
//! The refusals raised here are STORE facts (`HarnessError::Data`), not profile-schema refusals.
//! The `[data]` key SPELLINGS are refused in `crates/vike-backtest/src/harness/profile.rs` as
//! `HarnessError::Validation`, where `crate::profile_surface` harvests them into the published
//! profile surface — which is right, because a reader of that surface is asking what their profile
//! may say. A store's coverage is not something a profile can say, so it is not published there.
//! This module sits outside `src/harness/` for that reason, and `profile_surface`'s
//! `every_harness_module_that_refuses_is_parsed` walk is scoped to that directory.
//!
//! # Nothing here formats a float
//!
//! `vike_data::SeriesCoverage` is integers throughout and so is every span; the durations rendered
//! below are integer division. Same rule `crate::run_fingerprint` states and for the same reason —
//! a float rendering is exactly where two platforms disagree.

use serde::{Deserialize, Serialize};
use vike_data::{
    HistStore, MissingSpan, SeriesCoverage, SeriesId, Shortfall, missing_ms, window_shortfall,
};

use crate::harness::{BacktestProfile, DataKind, HarnessError};
use crate::run_fingerprint::planned_series_ids;

/// The version of the `data.explain` document. Bumped on any shape a consumer could not read.
pub const PLAN_SCHEMA: u32 = 1;

/// What an armed coverage gate DOES about a finding.
///
/// Declared here, beside the code that switches on it, rather than in `harness::profile` — which
/// RESOLVES `data.on_gap` into it. One enumeration, so the accepted set a refusal names and the
/// dispositions this module actually implements cannot come apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnGap {
    /// Nothing runs. The default, because a gate that only warns is invisible on every run whose
    /// output nobody reads — which is every scripted run.
    Refuse,
    /// The findings are logged and the run proceeds, unchanged.
    Warn,
    /// The gate is inert. It exists so a committed profile can keep its statement of what the run
    /// is SUPPOSED to require while an operator overrides today's disposition.
    Run,
}

impl OnGap {
    /// ⚠ **Every spelling this type accepts, in the order a refusal names them — THE roster.**
    ///
    /// It exists because a MUTATION PROOF found the hole it closes. `harness::profile`'s
    /// `DataCfg::on_gap` matched three string arms and refused with a hand-written sentence that
    /// listed the same three, and `crates/vike-cli/src/surface.rs`'s `gap_dispositions` row was a
    /// third copy. A test comparing the CLI's copy to the REFUSAL passed with a fourth arm
    /// planted in the resolver, because the message is a copy too — so what it held was
    /// copy-against-copy, and the arms were free to drift away from both.
    ///
    /// So this array is the only door. [`Self::parse`] is the resolver — a walk over these names,
    /// not a `match` — and [`Self::name`] is an exhaustive `match` on the VARIANT, which makes a
    /// new variant a compile error here and forces its row in. A fourth ACCEPTED spelling now
    /// needs a row, and every reader (the refusal, the CLI roster gate, the docs asset) reads the
    /// row.
    pub const NAMES: [&'static str; 3] = ["refuse", "warn", "run"];

    /// The disposition a NAME means, case- and space-insensitively, or `None` for one this type
    /// does not accept. The ONE acceptance rule: a spelling absent from [`Self::NAMES`] cannot be
    /// accepted by any path.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        let want = raw.trim().to_ascii_lowercase();
        Self::NAMES.iter().position(|n| *n == want).map(|i| match i {
            0 => OnGap::Refuse,
            1 => OnGap::Warn,
            _ => OnGap::Run,
        })
    }

    /// The name a variant carries. ⚠ An exhaustive `match` deliberately, NOT an index: this is
    /// what makes a new VARIANT fail to compile until [`Self::NAMES`] and [`Self::parse`] have
    /// been taught about it.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            OnGap::Refuse => "refuse",
            OnGap::Warn => "warn",
            OnGap::Run => "run",
        }
    }

    /// The accepted set as a refusal renders it — so no message spells the roster by hand.
    #[must_use]
    pub fn roster() -> String {
        Self::NAMES.join(" | ")
    }
}
/// The three `[data]` coverage keys folded into the one value the gate consults.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoverageGate {
    /// `data.require_coverage`. `false` (the default) makes the whole gate a no-op that reads
    /// nothing from the store — see [`enforce`].
    pub armed: bool,
    /// `data.max_gap` resolved to ms, or `None` for ZERO tolerance (any missing span is a finding).
    pub max_gap_ms: Option<i64>,
    pub on_gap: OnGap,
}

/// The point-in-time universe rule — `data.universe`.
///
/// ⚠ **None of the three ever DROPS a member.** `harness::profile::DataCfg::universe` carries the
/// whole argument; the short form is that two sites resolve the slice and the bar engine asserts
/// one row per symbol per step, so a filter applied in one of them desyncs the symbol slots — and a
/// universe a run narrowed silently would not be the one the committed profile names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UniverseMode {
    /// The symbol list verbatim. The default, and byte-identical to before the key existed.
    Declared,
    /// A member whose tape does not span the window is NAMED; the run proceeds.
    Covered,
    /// That run is REFUSED.
    Strict,
}

impl UniverseMode {
    /// Every spelling this type accepts, in refusal order — THE roster, on [`OnGap::NAMES`]'
    /// terms exactly, including the mutation proof that motivated it.
    pub const NAMES: [&'static str; 3] = ["declared", "covered", "strict"];

    /// The mode a NAME means, or `None` for one this type does not accept.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        let want = raw.trim().to_ascii_lowercase();
        Self::NAMES.iter().position(|n| *n == want).map(|i| match i {
            0 => UniverseMode::Declared,
            1 => UniverseMode::Covered,
            _ => UniverseMode::Strict,
        })
    }

    /// The name a variant carries — an exhaustive `match`, for [`OnGap::name`]'s reason.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            UniverseMode::Declared => "declared",
            UniverseMode::Covered => "covered",
            UniverseMode::Strict => "strict",
        }
    }

    /// The accepted set as a refusal renders it.
    #[must_use]
    pub fn roster() -> String {
        Self::NAMES.join(" | ")
    }

    /// `true` when this mode reads the store at all. [`UniverseMode::Declared`] does not, which is
    /// what makes an ordinary profile pay nothing.
    pub fn consults_the_store(self) -> bool {
        !matches!(self, UniverseMode::Declared)
    }
}

/// One series the run will open, joined to what the store holds for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlannedSeries {
    /// `(kind, venue, symbol | group, interval)`.
    pub id: SeriesId,
    /// Why this run opens it — the coarse price lane, the intrabar detail tape, the market
    /// funding-rate history or the point-in-time instrument grid. Recorded because a plan naming
    /// six series and no reason is a plan nobody can check against their profile.
    pub role: SeriesRole,
    /// What the store's manifest says it holds. `None` when the store holds no such series —
    /// decided from the INVENTORY, never from a manifest fold, because an absent manifest folds to
    /// an all-zero [`SeriesCoverage`] and a caller that trusted it would report every
    /// never-recorded lane as a 1970 tape (`crates/vike-data/src/datafusion_hist.rs`'s
    /// `series_dir_of` records the five-bug class that caused).
    pub coverage: Option<SeriesCoverage>,
    /// What the requested window asked for and this series does not hold, in time order.
    pub missing: Vec<MissingSpan>,
}

impl PlannedSeries {
    /// The recorded span to judge the window against, or `None` when there is nothing to judge.
    ///
    /// ⚠ **An INVENTORIED series holding zero rows answers `None`, not `Some((0, 0))`.** That state
    /// is real — a series directory whose manifest lists no parts — and a `(0, 0)` span would make
    /// every such lane report a 1970 tape plus a trailing shortfall instead of the honest
    /// [`Shortfall::Everything`]. The same distinction [`Self::coverage`] draws one level up.
    pub fn recorded_span(&self) -> Option<(i64, i64)> {
        self.coverage.as_ref().filter(|c| c.rows > 0).map(|c| (c.first_ts, c.last_ts))
    }

    /// Rows the store holds for this series — `0` for one it does not hold.
    pub fn rows(&self) -> u64 {
        self.coverage.as_ref().map_or(0, |c| c.rows)
    }

    /// `true` when the store holds this series with at least one row.
    pub fn held(&self) -> bool {
        self.recorded_span().is_some()
    }

    /// The EXISTENCE shortfalls alone — the leading and trailing ends, plus the wholly-absent case.
    ///
    /// This, not [`Self::missing`], is what the universe rule reads: an interior hole is a recorder
    /// outage in the middle of an instrument's life and says nothing about whether it was listed.
    pub fn existence_shortfalls(&self) -> Vec<&MissingSpan> {
        self.missing
            .iter()
            .filter(|m| {
                matches!(m.kind, Shortfall::Leading | Shortfall::Trailing | Shortfall::Everything)
            })
            .collect()
    }
}

/// Why a run opens a series.
///
/// ⚠ **Two of these are NOT addressed by the run fingerprint**, and the plan is where that shows.
/// `crate::run_fingerprint::planned_series_ids` names the price lanes only, so a run whose
/// `data.detail_interval` tape or whose `engine.attach_funding` history changed underneath it
/// addresses IDENTICALLY — a real hole in the address, found by writing this and deliberately not
/// closed here: widening the fingerprint would move every stored address and orphan every baseline,
/// which is the one thing `crate::run_fingerprint`'s module doc exists to prevent. The plan names
/// them because a plan's job is completeness, not stability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SeriesRole {
    /// The lane the strategy decides on — `kind=bar` at `data.interval`, or a tick lane.
    Price,
    /// `data.detail_interval`: the finer tape every order is resolved against.
    Detail,
    /// `engine.attach_funding`: the MARKET funding-RATE history, which is `kind=bar` at
    /// `interval=funding` — ⚠ NOT the `kind=funding` series, which is an ACCOUNT's realized
    /// payments. `crates/vike-data/src/store_kind.rs`'s `STORE_KINDS` pins that collision at the
    /// `funding` row, in those words, because neither name says so.
    Funding,
    /// `engine.snap_to_properties`: the point-in-time instrument grid fills are snapped to.
    Properties,
}

impl SeriesRole {
    /// The word a plan line uses.
    pub fn as_str(self) -> &'static str {
        match self {
            SeriesRole::Price => "price",
            SeriesRole::Detail => "detail",
            SeriesRole::Funding => "funding",
            SeriesRole::Properties => "properties",
        }
    }
}

/// The data slice a run is about to read, joined to what the store holds for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataPlan {
    /// [`PLAN_SCHEMA`] at render time.
    pub schema: u32,
    /// The store root, as the side that opened it spells it.
    pub store: String,
    /// WHICH RUNG chose that root (`vike_model::store_path::StoreRootRung::as_str`), when the
    /// caller knows. `None` on a route that was handed an already-open store and cannot say — the
    /// compute daemon's, where the honest answer is that this process did not resolve it. A
    /// fabricated rung would be the most confidently wrong field in the document.
    pub store_rung: Option<String>,
    /// The one operator-facing sentence for that rung, when known.
    pub store_rung_why: Option<String>,
    /// The resolved window, inclusive. `None` is unbounded, which is a different answer from zero —
    /// and the difference is load-bearing for [`vike_data::window_shortfall`], which cannot report
    /// an unbounded side as short.
    pub from_ms: Option<i64>,
    pub to_ms: Option<i64>,
    /// `"bar"` or `"tick"`.
    pub kind: String,
    /// `data.interval`, meaningful in bar mode.
    pub interval: String,
    /// `true` when the store answered an inventory. `false` means every `coverage` below is `None`
    /// because nobody could ask — NOT because the store holds nothing. [`enforce`] refuses on this;
    /// [`plan_data`] reports it.
    pub enumerable: bool,
    /// One entry per series, sorted by id.
    pub series: Vec<PlannedSeries>,
    /// Facts about the plan that are not per-series: an un-enumerable store, a `[walkforward]`
    /// table that will slice this window, a gap query the store refused.
    pub notes: Vec<String>,
}

impl DataPlan {
    /// Rows the store holds across every planned series.
    pub fn rows(&self) -> u64 {
        self.series.iter().fold(0u64, |a, s| a.saturating_add(s.rows()))
    }

    /// How many planned series the store actually holds.
    pub fn held(&self) -> usize {
        self.series.iter().filter(|s| s.held()).count()
    }

    /// Total missing milliseconds across every series, saturating.
    ///
    /// ⚠ A SUM ACROSS SERIES, so a two-series run each missing a day reports two days. That is the
    /// right shape for "how much work is there to do" and the wrong shape for "how much of my
    /// window is uncovered"; the per-series spans are what answer the second, and they are all
    /// rendered.
    pub fn missing_ms(&self) -> i64 {
        self.series.iter().fold(0i64, |a, s| a.saturating_add(missing_ms(&s.missing)))
    }

    /// The plan as operator-facing lines: the store, the window, then one line per series with its
    /// shortfalls indented under it.
    ///
    /// The store LEADS, exactly as `crate::backtest_cli`'s `run_rm_series` plan does and for the
    /// same reason: "which store" is the question to answer before "which series", and a resolution
    /// that moved is invisible until it has produced the wrong answer.
    pub fn lines(&self) -> Vec<String> {
        let mut out = Vec::with_capacity(self.series.len() * 2 + 4);
        out.push(match (&self.store_rung_why, &self.store_rung) {
            (Some(why), _) => format!("store: {} ({why})", self.store),
            (None, Some(rung)) => format!("store: {} ({rung})", self.store),
            (None, None) => format!("store: {}", self.store),
        });
        out.push(format!("window: {}", render_window(self.from_ms, self.to_ms)));
        out.push(format!(
            "slice: kind={} interval={} · {} series, {} held · {} rows",
            self.kind,
            self.interval,
            self.series.len(),
            self.held(),
            self.rows()
        ));
        for note in &self.notes {
            out.push(format!("note: {note}"));
        }
        for s in &self.series {
            out.push(render_series_line(s));
            for m in &s.missing {
                out.push(format!(
                    "    MISSING {} {} ({})",
                    m.kind.as_str(),
                    render_window(Some(m.start_ms), Some(m.end_ms)),
                    human_ms(m.len_ms())
                ));
            }
        }
        let missing = self.missing_ms();
        if missing > 0 {
            out.push(format!(
                "total: {} missing across {} series (summed, so one day short on two series reads \
                 as two days)",
                human_ms(missing),
                self.series.iter().filter(|s| !s.missing.is_empty()).count()
            ));
        } else if self.enumerable {
            out.push("total: the store covers every planned series over this window".to_string());
        }
        out
    }

    /// The plan as the `--json` / wire document.
    ///
    /// ⚠ **It carries the rendered LINES as well as the fields**, which is not duplication for its
    /// own sake. On the remote route this document IS the run's report: `crate::compute_server`
    /// returns it and the client pretty-prints whatever JSON came back, having no renderer for a
    /// shape it has never seen. Without the sentences inside it, a remote `--explain-data` would
    /// hand an operator a field dump and the one thing they asked for — what is missing — would be
    /// the thing they had to reconstruct.
    pub fn to_json(&self) -> serde_json::Value {
        let mut doc = serde_json::to_value(self)
            .expect("a tree of plain data and integers; serialization is total");
        if let Some(map) = doc.as_object_mut() {
            map.insert("rows".into(), serde_json::json!(self.rows()));
            map.insert("held".into(), serde_json::json!(self.held()));
            map.insert("planned".into(), serde_json::json!(self.series.len()));
            map.insert("missing_ms".into(), serde_json::json!(self.missing_ms()));
            map.insert("explain".into(), serde_json::json!(self.lines()));
        }
        doc
    }
}

/// One series' rendered plan line.
fn render_series_line(s: &PlannedSeries) -> String {
    let id = &s.id;
    let scope = match &id.group {
        Some(g) => format!("group={g}"),
        None => format!("symbol={}", id.symbol),
    };
    let interval = id.interval.as_deref().unwrap_or("-");
    let head = format!(
        "series kind={} venue={} {scope} interval={interval} [{}]",
        id.kind,
        id.venue,
        s.role.as_str()
    );
    match s.coverage.as_ref().filter(|c| c.rows > 0) {
        Some(c) => format!(
            "{head}  {} rows · {} days · {} parts · {} B · {}",
            c.rows,
            c.dates,
            c.parts,
            c.bytes,
            render_window(Some(c.first_ts), Some(c.last_ts))
        ),
        None if s.coverage.is_some() => {
            format!("{head}  PRESENT AND EMPTY — the series exists and its manifest lists no rows")
        }
        None => format!("{head}  NOT HELD — the store holds no such series"),
    }
}

/// `start .. end` as UTC timestamps, with an unbounded side spelled outright.
///
/// ⚠ An unbounded bound is NOT formatted as an instant. `i64::MIN`/`i64::MAX` are the sentinels a
/// `Shortfall::Everything` span over an unbounded window carries, and handing either to a civil
/// calendar conversion asks it to render a year no operator can use. `-inf`/`+inf` says the true
/// thing in three characters.
fn render_window(from_ms: Option<i64>, to_ms: Option<i64>) -> String {
    let side = |v: Option<i64>, unbounded: &str| match v {
        None | Some(i64::MIN) | Some(i64::MAX) => unbounded.to_string(),
        Some(ms) => vike_model::time::epoch_ms_to_utc_timestamp(ms),
    };
    format!("{} .. {}", side(from_ms, "-inf"), side(to_ms, "+inf"))
}

/// A duration as the largest whole unit that fits, integer arithmetic only.
///
/// Deliberately coarse and deliberately not a float: `"3d"` is what an operator needs in order to
/// decide whether to backfill, and `"3.27d"` would invite a comparison the value cannot support
/// (the underlying spans are day-aligned wherever the store's own gap index produced them).
fn human_ms(ms: i64) -> String {
    const S: i64 = 1_000;
    const M: i64 = 60 * S;
    const H: i64 = 60 * M;
    const D: i64 = 24 * H;
    match ms {
        _ if ms >= D => format!("{}d", ms / D),
        _ if ms >= H => format!("{}h", ms / H),
        _ if ms >= M => format!("{}m", ms / M),
        _ if ms >= S => format!("{}s", ms / S),
        _ => format!("{ms}ms"),
    }
}

/// The series a run opens that [`planned_series_ids`] does not name — the intrabar detail tape, the
/// market funding-rate history and the point-in-time instrument grid.
///
/// ⚠ **Kept OUT of `planned_series_ids` on purpose.** That function feeds the run ADDRESS, and
/// widening it would move every stored address and orphan every baseline — the one property
/// `crate::run_fingerprint`'s module doc exists to guarantee. So the hole in the address stays open
/// and declared ([`SeriesRole`]'s doc names it), while the PLAN is complete: a plan that omitted the
/// detail tape would report a clean slice for a run that is about to refuse because the finer series
/// is absent (`crate::harness::run`'s `load_profile_detail_bars` does exactly that).
fn auxiliary_series(profile: &BacktestProfile) -> Vec<(SeriesId, SeriesRole)> {
    let mut out = Vec::new();
    for s in profile.data.resolved_series() {
        // Bar mode only, both of them: `detail_interval` is refused in tick mode by
        // `DataCfg::detail_interval_ms`, and `attach_funding` in tick mode by
        // `BacktestProfile::refusals` — so naming either here for a tick profile would describe a
        // read that cannot happen.
        if profile.data.kind == DataKind::Bar {
            if let Some(detail) = profile.data.detail_interval.as_deref() {
                out.push((
                    SeriesId::per_symbol("bar", &s.venue, &s.symbol, Some(detail.to_string())),
                    SeriesRole::Detail,
                ));
            }
            if profile.engine.attach_funding {
                // `interval=funding` under `kind=bar` — see [`SeriesRole::Funding`] for the pinned
                // name collision with the `kind=funding` series.
                out.push((
                    SeriesId::per_symbol("bar", &s.venue, &s.symbol, Some("funding".to_string())),
                    SeriesRole::Funding,
                ));
            }
        }
        if profile.engine.snap_to_properties {
            out.push((
                SeriesId::per_symbol("properties", &s.venue, &s.symbol, None),
                SeriesRole::Properties,
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
/// The only `Err` this returns is `BacktestProfile::range`'s, because a profile whose window does
/// not parse has no plan to make. Every store failure becomes a NOTE and an absent coverage — see
/// the module doc's posture split, and [`enforce`] for the caller that must refuse on the same
/// input.
///
/// `store_label` and `store_rung` are PARAMETERS rather than something resolved here: this module
/// cannot see how the caller found its store, and a rung it guessed at would be the most
/// confidently wrong field in the document. The compute daemon passes `None` for the rung because
/// it did not resolve one.
///
/// # Cost
///
/// One `inventory()` — a manifest fold per series, no Parquet scan — plus one `series_gaps()` per
/// PLANNED series that the store holds. The same cost class `crate::backtest_cli`'s
/// `collect_data_fingerprint` already pays on every single run, which is why an armed gate is
/// affordable on a run that was going to pay it anyway; [`enforce`] still refuses to pay any of it
/// for a profile that armed nothing.
pub fn plan_data(
    profile: &BacktestProfile,
    store: &(dyn HistStore + Send + Sync),
    store_label: &str,
    store_rung: Option<(&str, &str)>,
) -> Result<DataPlan, HarnessError> {
    let range = profile.range()?;
    let mut notes: Vec<String> = Vec::new();

    let (inventory, enumerable) = match store.inventory() {
        Ok(inv) => (inv, true),
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
    let mut planned: Vec<(SeriesId, SeriesRole)> =
        planned_series_ids(profile, &held).into_iter().map(|id| (id, SeriesRole::Price)).collect();
    planned.extend(auxiliary_series(profile));
    planned.sort();
    planned.dedup_by(|a, b| a.0 == b.0);

    let mut series = Vec::with_capacity(planned.len());
    for (id, role) in planned {
        let coverage = inventory.iter().find(|(h, _)| *h == id).map(|(_, c)| c.clone());
        // Only for a series that is actually there: `series_gaps` on an absent one is a manifest
        // read that answers "no holes" about a span that does not exist, and the ABSENCE is already
        // the finding.
        let gaps = if coverage.is_some() {
            match store.series_gaps(&id) {
                Ok(g) => g,
                Err(e) => {
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
        let recorded = coverage.as_ref().filter(|c| c.rows > 0).map(|c| (c.first_ts, c.last_ts));
        let missing = window_shortfall(range.start, range.end, recorded, &gaps);
        series.push(PlannedSeries { id, role, coverage, missing });
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
            DataKind::Bar => "bar".to_string(),
            DataKind::Tick => "tick".to_string(),
        },
        interval: profile.data.interval.clone(),
        enumerable,
        series,
        notes,
    })
}

/// One coverage finding: a series, and the spans of the window it cannot answer for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoverageFinding {
    pub id: SeriesId,
    /// The spans that exceeded the tolerance, or every span when there is none.
    pub spans: Vec<MissingSpan>,
    /// `true` when the store holds no such series at all — the case `data.max_gap` can never
    /// tolerate.
    pub absent: bool,
}

/// What an armed gate found, and what it means to do about it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoverageVerdict {
    pub findings: Vec<CoverageFinding>,
    /// `true` when the store could not be enumerated, so coverage could not be PROVEN either way.
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
    /// suggestion. `vike-cli data fetch` takes `VENUE:SYMBOL:INTERVAL`, so the line is only offered
    /// for a shape that spelling can express — a GROUPED series or a tick lane has no such argument,
    /// and a command that would not run is worse than none.
    pub fn lines(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.unprovable {
            out.push(
                "coverage is UNPROVABLE: this store cannot enumerate its inventory, so the window \
                 was never checked against anything"
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

/// The `vike-cli data fetch` line that would fill one missing span, or `None` for a series that
/// spelling cannot name.
///
/// `kind=bar` per-symbol series only, and that is not a shortcut: `data fetch`'s spec is
/// `VENUE:SYMBOL:INTERVAL` and it always writes `kind=bar` per symbol. A suggested command for a
/// grouped or tick series would name a series the fetch cannot write, which is worse than offering
/// nothing.
///
/// ⚠ That sentence used to cite "its own module doc" — `crate::fetch`'s, which is DELETED along
/// with this crate's venue call. The claim survives its source because the SPEC is what carries it:
/// `VENUE:SYMBOL:INTERVAL` has no field for a kind, a group or a tick, so there is no other shape
/// to express. `crates/vike-cli/src/cmd/data.rs`'s `parse` is where that grammar lives now.
///
/// ⚠ **The hint is no longer runnable on a box with no server.** `vike-cli data fetch` asks a
/// DATAHUB now rather than calling a venue, so the line this emits needs one reachable (default
/// `127.0.0.1:7878`, or `--addr`). It is still the right command to offer — it is the only fetch
/// there is — but a reader who runs it against nothing gets a connect error, not a fill.
fn fetch_hint(id: &SeriesId, span: Option<&MissingSpan>) -> Option<String> {
    let span = span?;
    if id.kind != "bar" || id.group.is_some() || id.symbol.is_empty() {
        return None;
    }
    let interval = id.interval.as_deref()?;
    if span.start_ms == i64::MIN || span.end_ms == i64::MAX {
        // An unbounded window has no `--from`/`--to` to offer, and `data fetch` requires one form
        // or the other.
        return None;
    }
    Some(format!(
        "vike-cli data fetch {}:{}:{} --from {} --to {}",
        id.venue,
        id.symbol,
        interval,
        vike_model::epoch_ms_to_utc_date(span.start_ms),
        vike_model::epoch_ms_to_utc_date(span.end_ms)
    ))
}

/// Judge a plan against an armed gate — PURE, so both dispositions are testable without a store.
///
/// # ⚠ `Shortfall::Everything` ignores `max_gap` under every value
///
/// A series the store does not hold is not a gap of some length; it is a lane that was never
/// recorded. A `max_gap` generous enough to swallow the window would otherwise disarm the gate for
/// the one case it most exists to catch — the window with a complete trade tape and no book at all,
/// which is the failure `crates/vike-data/src/coverage.rs`'s module doc measured.
pub fn coverage_verdict(plan: &DataPlan, gate: &CoverageGate) -> CoverageVerdict {
    if !gate.armed {
        return CoverageVerdict { findings: Vec::new(), unprovable: false };
    }
    let tolerance = gate.max_gap_ms.unwrap_or(0);
    let mut findings = Vec::new();
    for s in &plan.series {
        let spans: Vec<MissingSpan> = s
            .missing
            .iter()
            .copied()
            .filter(|m| m.kind == Shortfall::Everything || m.len_ms() > tolerance)
            .collect();
        if !spans.is_empty() {
            let absent = !s.held();
            findings.push(CoverageFinding { id: s.id.clone(), spans, absent });
        }
    }
    CoverageVerdict { findings, unprovable: !plan.enumerable }
}

/// What the universe rule found: which declared members the tape spans, and which it does not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UniverseVerdict {
    /// Declared members whose every PRICE series spans the window.
    pub present: Vec<String>,
    /// `(symbol, why)` for each member that does not — the survivorship signature.
    pub absent: Vec<(String, String)>,
}

impl UniverseVerdict {
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
/// PRICE series only ([`SeriesRole::Price`]). An absent properties grid or funding history is a
/// data gap, not a statement about whether the instrument existed.
pub fn universe_verdict(plan: &DataPlan, mode: UniverseMode) -> UniverseVerdict {
    let mut present = Vec::new();
    let mut absent = Vec::new();
    if !mode.consults_the_store() {
        return UniverseVerdict { present, absent };
    }
    // Grouped series carry an EMPTY symbol and name a group, so they cannot answer a question about
    // one member; they are skipped rather than blamed. That is the grouped residual again, and it
    // means a purely grouped tick slice has no universe verdict to give — which the caller sees as
    // an empty `present` rather than as a clean bill of health.
    for s in plan.series.iter().filter(|s| s.role == SeriesRole::Price && s.id.group.is_none()) {
        let shortfalls = s.existence_shortfalls();
        if shortfalls.is_empty() {
            present.push(s.id.symbol.clone());
            continue;
        }
        let why = shortfalls
            .iter()
            .map(|m| match m.kind {
                Shortfall::Everything => format!(
                    "the store holds no {} series for it at all over {}",
                    s.id.kind,
                    render_window(Some(m.start_ms), Some(m.end_ms))
                ),
                Shortfall::Leading => format!(
                    "its {} tape does not reach back to the window's start — {} is missing ({})",
                    s.id.kind,
                    render_window(Some(m.start_ms), Some(m.end_ms)),
                    human_ms(m.len_ms())
                ),
                Shortfall::Trailing => format!(
                    "its {} tape stops before the window's end — {} is missing ({})",
                    s.id.kind,
                    render_window(Some(m.start_ms), Some(m.end_ms)),
                    human_ms(m.len_ms())
                ),
                Shortfall::Interior => unreachable!("existence_shortfalls filters interior out"),
            })
            .collect::<Vec<_>>()
            .join("; ");
        absent.push((s.id.symbol.clone(), why));
    }
    present.sort();
    present.dedup();
    absent.sort();
    absent.dedup();
    UniverseVerdict { present, absent }
}

/// **The pre-flight a run calls**: resolve the gate and the universe rule from the profile, and
/// either refuse the run or hand back the lines it should warn with.
///
/// # ⚠ A profile that armed nothing reads NOTHING from the store
///
/// The early return below is what makes every profile written before these keys existed
/// byte-identical AND free: no `inventory()`, no `series_gaps()`, no allocation. That is not an
/// optimisation — a pre-flight that cost a manifest fold per series on every run would have to be
/// justified against the latency of a sweep's ten-thousandth trial, and it cannot be.
///
/// # ⚠ ...and an ARMED profile pays it PER TRIAL, which is the cost to own
///
/// Every caller of `super::run::run_backtest` reaches this — a single run, each walk-forward window,
/// and every `[paramscan]` trial — so an armed gate over a grid pays one `inventory()` plus one
/// `series_gaps()` per series per TRIAL. That is a manifest fold, not a Parquet scan, and it is the
/// same read `crate::backtest_cli`'s `collect_data_fingerprint` already performs once per run; but
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
///   where the spelling can express it, the `vike-cli data fetch` line that would fill the first.
/// * [`OnGap::Warn`] — `Ok(lines)`; the caller logs them and runs unchanged.
/// * [`OnGap::Run`] — `Ok(vec![])`; the gate is inert.
///
/// [`UniverseMode::Strict`] refuses on its own terms, independently of the coverage gate — the two
/// judge different things (see [`universe_verdict`]) and a profile may arm either alone.
///
/// # ⚠ It refuses when coverage is UNPROVABLE, not only when it is bad
///
/// A store that cannot enumerate its inventory cannot prove the window is covered. An armed gate
/// that passed there would be the "green means nothing ran" failure with a gate's authority behind
/// it: the operator asked for a proof and got silence rendered as success. `OnGap::Warn` still only
/// warns — they asked for that too.
pub fn enforce(
    profile: &BacktestProfile,
    store: &(dyn HistStore + Send + Sync),
    store_label: &str,
) -> Result<Vec<String>, HarnessError> {
    let gate = profile.data.coverage_gate()?;
    let mode = profile.data.universe_mode()?;
    if !gate.armed && !mode.consults_the_store() {
        return Ok(Vec::new());
    }
    let plan = plan_data(profile, store, store_label, None)?;
    let coverage = coverage_verdict(&plan, &gate);
    let universe = universe_verdict(&plan, mode);

    let mut warnings = Vec::new();

    if !coverage.is_clean() {
        match gate.on_gap {
            OnGap::Refuse => {
                return Err(HarnessError::Data(format!(
                    "data.require_coverage is armed and the store does not cover this run's \
                     window. Nothing ran.\n{}\n{}",
                    coverage.lines().join("\n"),
                    "A window with a complete trade tape and no book runs to completion and \
                     REPORTS FILLS — which is why this refuses rather than warns. Fill the spans \
                     above, widen data.max_gap if the gap is genuinely acceptable, or set \
                     data.on_gap = \"warn\"."
                )));
            }
            OnGap::Warn => warnings.extend(coverage.lines()),
            OnGap::Run => {}
        }
    }

    if !universe.is_clean() {
        match mode {
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
            UniverseMode::Covered => warnings.extend(universe.lines()),
            UniverseMode::Declared => {}
        }
    }

    Ok(warnings)
}

/// The `data.explain` answer: the plan, plus what an armed gate and universe rule would say about
/// it, as ONE document.
///
/// Both explain doors call this — `crate::backtest_cli`'s local profile run and
/// `crate::compute_server`'s three profile arms — so a plan seen over the wire and a plan printed
/// on the box cannot differ. The gate and universe sections are present only when the profile armed
/// them, because a report full of "nothing to say" trains a reader to skim.
pub fn explain_document(
    profile: &BacktestProfile,
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
            lines.extend(universe.lines());
        }
        map.insert("explain".into(), serde_json::json!(lines));
    }
    Ok(doc)
}

/// The `data.explain` answer as operator-facing lines — the human half of [`explain_document`],
/// read out of the same document so the two cannot disagree about what was found.
pub fn explain_lines(
    profile: &BacktestProfile,
    plan: &DataPlan,
) -> Result<Vec<String>, HarnessError> {
    let doc = explain_document(profile, plan)?;
    Ok(doc
        .get("explain")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cov(first_ts: i64, last_ts: i64, rows: u64) -> SeriesCoverage {
        SeriesCoverage { first_ts, last_ts, rows, bytes: 1, parts: 1, dates: 1 }
    }

    fn bar(symbol: &str) -> SeriesId {
        SeriesId::per_symbol("bar", "binance", symbol, Some("1h".to_string()))
    }

    fn planned(
        id: SeriesId,
        coverage: Option<SeriesCoverage>,
        from: Option<i64>,
        to: Option<i64>,
        gaps: &[(i64, i64)],
    ) -> PlannedSeries {
        let recorded = coverage.as_ref().filter(|c| c.rows > 0).map(|c| (c.first_ts, c.last_ts));
        PlannedSeries {
            missing: window_shortfall(from, to, recorded, gaps),
            id,
            role: SeriesRole::Price,
            coverage,
        }
    }

    fn plan(series: Vec<PlannedSeries>, enumerable: bool) -> DataPlan {
        DataPlan {
            schema: PLAN_SCHEMA,
            store: "/tmp/hist".to_string(),
            store_rung: None,
            store_rung_why: None,
            from_ms: Some(0),
            to_ms: Some(1_000),
            kind: "bar".to_string(),
            interval: "1h".to_string(),
            enumerable,
            series,
            notes: Vec::new(),
        }
    }

    const ARMED: CoverageGate =
        CoverageGate { armed: true, max_gap_ms: None, on_gap: OnGap::Refuse };

    #[test]
    fn an_unarmed_gate_finds_nothing_even_over_a_wholly_absent_slice() {
        let p = plan(vec![planned(bar("BTCUSDT"), None, Some(0), Some(1_000), &[])], true);
        let gate = CoverageGate { armed: false, max_gap_ms: None, on_gap: OnGap::Refuse };
        assert!(coverage_verdict(&p, &gate).is_clean());
    }

    #[test]
    fn a_covered_window_is_clean() {
        let p = plan(
            vec![planned(bar("BTCUSDT"), Some(cov(0, 1_000, 10)), Some(0), Some(1_000), &[])],
            true,
        );
        assert!(coverage_verdict(&p, &ARMED).is_clean());
    }

    /// The case `find_gaps` is blind to: a contiguous tape that simply starts late.
    #[test]
    fn a_late_starting_tape_is_a_finding_with_no_interior_hole_anywhere() {
        let p = plan(
            vec![planned(bar("BTCUSDT"), Some(cov(400, 1_000, 10)), Some(0), Some(1_000), &[])],
            true,
        );
        let v = coverage_verdict(&p, &ARMED);
        assert_eq!(v.findings.len(), 1);
        assert_eq!(v.findings[0].spans.len(), 1);
        assert_eq!(v.findings[0].spans[0].kind, Shortfall::Leading);
        assert!(!v.findings[0].absent);
    }

    #[test]
    fn max_gap_tolerates_a_small_span_and_not_a_large_one() {
        let small = plan(
            vec![planned(bar("BTCUSDT"), Some(cov(100, 1_000, 10)), Some(0), Some(1_000), &[])],
            true,
        );
        let tolerant = CoverageGate { armed: true, max_gap_ms: Some(500), on_gap: OnGap::Refuse };
        assert!(
            coverage_verdict(&small, &tolerant).is_clean(),
            "a 100ms span under a 500ms tolerance"
        );
        let strict = CoverageGate { armed: true, max_gap_ms: Some(50), on_gap: OnGap::Refuse };
        assert_eq!(coverage_verdict(&small, &strict).findings.len(), 1);
    }

    /// The rule that keeps `max_gap` from disarming the gate for the case it most exists to catch.
    #[test]
    fn an_absent_series_is_never_tolerated_however_generous_max_gap_is() {
        let p = plan(vec![planned(bar("BTCUSDT"), None, Some(0), Some(1_000), &[])], true);
        let generous =
            CoverageGate { armed: true, max_gap_ms: Some(i64::MAX), on_gap: OnGap::Refuse };
        let v = coverage_verdict(&p, &generous);
        assert_eq!(v.findings.len(), 1);
        assert!(v.findings[0].absent);
        assert_eq!(v.findings[0].spans[0].kind, Shortfall::Everything);
    }

    /// An INVENTORIED series with zero rows is not a 1970 tape.
    #[test]
    fn a_present_but_empty_series_reads_as_wholly_missing() {
        let p = plan(
            vec![planned(bar("BTCUSDT"), Some(cov(0, 0, 0)), Some(0), Some(1_000), &[])],
            true,
        );
        let v = coverage_verdict(&p, &ARMED);
        assert_eq!(v.findings[0].spans[0].kind, Shortfall::Everything);
        assert!(v.findings[0].absent, "zero rows is not held, whatever the inventory listed");
    }

    /// "I found holes" and "I could not look" must not be the same answer.
    #[test]
    fn an_unenumerable_store_is_unprovable_rather_than_clean() {
        let p = plan(vec![planned(bar("BTCUSDT"), None, Some(0), Some(1_000), &[])], false);
        let v = coverage_verdict(&p, &ARMED);
        assert!(v.unprovable);
        assert!(!v.is_clean());
        assert!(v.lines().iter().any(|l| l.contains("UNPROVABLE")));
    }

    #[test]
    fn the_refusal_offers_a_fetch_command_for_a_bar_series_and_none_for_a_group() {
        let id = bar("BTCUSDT");
        let span = MissingSpan { start_ms: 0, end_ms: 86_400_000, kind: Shortfall::Leading };
        let hint = fetch_hint(&id, Some(&span)).expect("a per-symbol bar series can be fetched");
        assert!(hint.starts_with("vike-cli data fetch binance:BTCUSDT:1h --from "));
        let grouped = SeriesId::grouped("trade", "polymarket", "g1");
        assert!(
            fetch_hint(&grouped, Some(&span)).is_none(),
            "`data fetch` cannot write a grouped series, so suggesting it would be worse than \
             saying nothing"
        );
    }

    #[test]
    fn no_fetch_command_is_offered_for_an_unbounded_span() {
        let span =
            MissingSpan { start_ms: i64::MIN, end_ms: i64::MAX, kind: Shortfall::Everything };
        assert!(fetch_hint(&bar("BTCUSDT"), Some(&span)).is_none());
    }

    // ---- the universe rule -----------------------------------------------------------------------

    #[test]
    fn declared_mode_consults_nothing_and_names_nobody() {
        let p = plan(vec![planned(bar("BTCUSDT"), None, Some(0), Some(1_000), &[])], true);
        let v = universe_verdict(&p, UniverseMode::Declared);
        assert!(v.is_clean());
        assert!(v.present.is_empty(), "declared mode does not even classify");
    }

    /// The survivorship signature: a member whose tape begins after the window does.
    #[test]
    fn a_member_listed_after_the_window_opened_is_named() {
        let p = plan(
            vec![
                planned(bar("BTCUSDT"), Some(cov(0, 1_000, 10)), Some(0), Some(1_000), &[]),
                planned(bar("NEWCOIN"), Some(cov(600, 1_000, 5)), Some(0), Some(1_000), &[]),
            ],
            true,
        );
        let v = universe_verdict(&p, UniverseMode::Covered);
        assert_eq!(v.present, vec!["BTCUSDT".to_string()]);
        assert_eq!(v.absent.len(), 1);
        assert_eq!(v.absent[0].0, "NEWCOIN");
        assert!(v.absent[0].1.contains("does not reach back"));
    }

    /// An INTERIOR hole is a recorder outage, not a listing date — the split that keeps this
    /// warning worth reading.
    #[test]
    fn an_interior_hole_is_not_a_universe_finding() {
        let p = plan(
            vec![planned(
                bar("BTCUSDT"),
                Some(cov(0, 1_000, 10)),
                Some(0),
                Some(1_000),
                &[(400, 500)],
            )],
            true,
        );
        assert!(universe_verdict(&p, UniverseMode::Strict).is_clean());
        // ...while the COVERAGE gate does find it, which is the whole reason they are two rules.
        assert_eq!(coverage_verdict(&p, &ARMED).findings.len(), 1);
    }

    #[test]
    fn a_grouped_series_answers_no_question_about_one_member() {
        // A PRICE series (the role `planned` gives) that is nonetheless grouped: its coverage is
        // the whole group's, so it cannot answer for one member and must not be blamed for one.
        let s = planned(
            SeriesId::grouped("trade", "polymarket", "g1"),
            None,
            Some(0),
            Some(1_000),
            &[],
        );
        let p = plan(vec![s], true);
        let v = universe_verdict(&p, UniverseMode::Strict);
        assert!(v.is_clean());
        assert!(v.present.is_empty(), "an empty `present` is not a clean bill of health");
    }

    #[test]
    fn a_non_price_series_is_not_a_universe_finding() {
        let mut s = planned(
            SeriesId::per_symbol("properties", "binance", "BTCUSDT", None),
            None,
            Some(0),
            Some(1_000),
            &[],
        );
        s.role = SeriesRole::Properties;
        let p = plan(vec![s], true);
        assert!(universe_verdict(&p, UniverseMode::Strict).is_clean());
    }

    // ---- rendering -------------------------------------------------------------------------------

    #[test]
    fn an_unbounded_bound_renders_as_infinity_rather_than_a_calendar_year() {
        assert_eq!(render_window(None, None), "-inf .. +inf");
        assert_eq!(render_window(Some(i64::MIN), Some(i64::MAX)), "-inf .. +inf");
    }

    #[test]
    fn durations_render_as_the_largest_whole_unit() {
        assert_eq!(human_ms(0), "0ms");
        assert_eq!(human_ms(999), "999ms");
        assert_eq!(human_ms(1_000), "1s");
        assert_eq!(human_ms(60_000), "1m");
        assert_eq!(human_ms(3_600_000), "1h");
        assert_eq!(human_ms(86_400_000), "1d");
        assert_eq!(human_ms(4 * 86_400_000), "4d");
    }

    #[test]
    fn the_plan_leads_with_the_store_and_names_every_series() {
        let p = plan(
            vec![
                planned(bar("BTCUSDT"), Some(cov(400, 1_000, 10)), Some(0), Some(1_000), &[]),
                planned(bar("ETHUSDT"), None, Some(0), Some(1_000), &[]),
            ],
            true,
        );
        let lines = p.lines();
        assert!(lines[0].starts_with("store: /tmp/hist"), "the store LEADS: {:?}", lines[0]);
        assert!(lines.iter().any(|l| l.contains("symbol=BTCUSDT")));
        assert!(lines.iter().any(|l| l.contains("symbol=ETHUSDT") && l.contains("NOT HELD")));
        assert!(lines.iter().any(|l| l.contains("MISSING leading")));
        assert!(lines.iter().any(|l| l.contains("MISSING everything")));
        assert_eq!(p.rows(), 10);
        assert_eq!(p.held(), 1);
    }

    /// The document must carry its own sentences: on the remote route nothing else can render it.
    #[test]
    fn the_json_document_carries_the_rendered_lines_and_the_folded_totals() {
        let p = plan(
            vec![planned(bar("BTCUSDT"), Some(cov(400, 1_000, 10)), Some(0), Some(1_000), &[])],
            true,
        );
        let doc = p.to_json();
        assert_eq!(doc["schema"], serde_json::json!(PLAN_SCHEMA));
        assert_eq!(doc["rows"], serde_json::json!(10u64));
        assert_eq!(doc["held"], serde_json::json!(1usize));
        assert_eq!(doc["planned"], serde_json::json!(1usize));
        let explain = doc["explain"].as_array().expect("the lines ride inside the document");
        assert!(explain.iter().any(|l| l.as_str().is_some_and(|s| s.starts_with("store: "))));
    }

    #[test]
    fn an_empty_and_a_missing_series_render_as_different_sentences() {
        let empty = planned(bar("A"), Some(cov(0, 0, 0)), Some(0), Some(1_000), &[]);
        let absent = planned(bar("B"), None, Some(0), Some(1_000), &[]);
        assert!(render_series_line(&empty).contains("PRESENT AND EMPTY"));
        assert!(render_series_line(&absent).contains("NOT HELD"));
    }

    /// ⚠ **The roster, the resolver and the refusal are ONE thing — proven, because a mutation
    /// showed they were three.**
    ///
    /// The hole this closes, in the order it was found: `harness::profile`'s `DataCfg::on_gap`
    /// matched three string arms; its refusal spelled the same three by hand; and
    /// `crates/vike-cli/src/surface.rs`'s `gap_dispositions` row spelled them a third time. A
    /// CLI-side test compared the roster it PARSED OUT OF THE REFUSAL to the surface row — and a
    /// fourth arm planted in the resolver left that test GREEN, because the message it read is a
    /// copy too. Copy-against-copy is not a gate.
    ///
    /// Now [`OnGap::parse`] walks [`OnGap::NAMES`] instead of matching, the refusal renders
    /// [`OnGap::roster`], and this test holds the last edge: every name resolves, and every
    /// resolved value names itself back. Combined with [`OnGap::name`] being an exhaustive `match`
    /// on the VARIANT — a new variant fails to compile there — a spelling cannot be accepted
    /// without a row, and a row cannot exist without a variant that answers for it.
    #[test]
    fn the_coverage_rosters_are_the_one_door_their_resolvers_walk() {
        for name in OnGap::NAMES {
            let v = OnGap::parse(name)
                .unwrap_or_else(|| panic!("`{name}` is in NAMES and parse refuses it"));
            assert_eq!(
                v.name(),
                name,
                "`{name}` resolves to a value that names itself differently"
            );
        }
        for name in UniverseMode::NAMES {
            let v = UniverseMode::parse(name)
                .unwrap_or_else(|| panic!("`{name}` is in NAMES and parse refuses it"));
            assert_eq!(
                v.name(),
                name,
                "`{name}` resolves to a value that names itself differently"
            );
        }
        // The acceptance is case- and space-insensitive, which is what the CLI's own
        // `flag_vocab` ascii_ci rule relies on — and it is the roster that decides, not a
        // second lowercase list.
        assert_eq!(OnGap::parse("  REFUSE "), Some(OnGap::Refuse));
        assert_eq!(UniverseMode::parse("Strict"), Some(UniverseMode::Strict));
        // ...and nothing outside the roster is accepted by any path.
        assert_eq!(OnGap::parse("fill"), None);
        assert_eq!(UniverseMode::parse("all"), None);
        // The rendered set is the roster verbatim, so no refusal can spell it by hand again.
        assert_eq!(OnGap::roster(), "refuse | warn | run");
        assert_eq!(UniverseMode::roster(), "declared | covered | strict");
    }
}
