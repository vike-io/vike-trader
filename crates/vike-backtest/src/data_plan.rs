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
//! * `vike_data::store::coverage`'s cross-KIND report already argues the failure this gate exists to
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
//! PRESENT and was recorded through a feed outage passes — `vike_data::store::quality` is the question
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
#[cfg(test)]
use vike_data::window_shortfall;
use vike_data::{MissingSpan, SeriesCoverage, SeriesId, Shortfall, missing_ms};

mod verdict;

#[cfg(test)]
use self::verdict::fetch_hint;
pub use self::verdict::{
    CoverageFinding, CoverageVerdict, UniverseVerdict, coverage_verdict, enforce, explain_document,
    explain_lines, plan_data, universe_verdict,
};

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
    /// never-recorded lane as a 1970 tape (`crates/vike-data/src/store/datafusion_hist.rs`'s
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
    /// `interval=funding` — ⚠ NOT the `kind=exec_funding` series, which is an ACCOUNT's realized
    /// payments. `crates/vike-data/src/store/store_kind.rs`'s `STORE_KINDS` pins that collision at the
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
    /// WHICH RUNG chose that root (`vike_model::paths::store_path::StoreRootRung::as_str`), when the
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

#[path = "data_plan_tests.rs"]
#[cfg(test)]
mod data_plan_tests;
