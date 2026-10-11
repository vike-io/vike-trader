//! The `--progress` plane: the trade floor, the progress event and sinks, and the per-point tracker.

use std::io::IsTerminal;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use super::Evaluated;

#[cfg(doc)]
use super::{Optimizer, PointEvaluator, SearchOutcome, StoreEvaluator};
#[cfg(doc)]
use crate::harness::sweep::ParamscanReport;

/// The statistical-significance FLOOR: the trade count below which a point is not ranked at all.
///
/// ⚠ **This exists because a lucky pair of trades otherwise wins a 500-point grid**, and nothing in
/// the ranking can tell the difference. Every [`crate::search::objective::Objective`] in this crate
/// is a function of a [`vike_analytics::report::BacktestReport`] alone, and a report over two
/// trades that both went the right way carries a Sharpe, a total return and a profit factor that
/// are arithmetically enormous and statistically empty. The grid then crowns it,
/// `crate::search::trials`'s ledger records it as the winner, and whatever reads the #1 row
/// downstream — a walk-forward window's chosen parameters, an exported params file — inherits a
/// configuration fitted to two bars.
///
/// ⚠ **It is NOT `crate::search::objective::trade_count_penalty`, and the two do different
/// jobs.** That penalty is a SOFT multiplicative term inside
/// `crate::search::objective::multi_metric` only: it is floored at `penalty_floor` (default `0.1`,
/// deliberately, so a thin-but-promising point stays VISIBLE), and it is not applied at all under
/// the four classic `--rank-by` metrics — which includes the default `sharpe` and therefore the
/// invocation nearly every operator actually types. A floor that can be out-earned is not a floor.
/// This one replaces the score outright and applies under every ranking and every method, because
/// it answers a different question: not "how much should thinness cost" but "is this sample
/// admissible evidence at all".
///
/// ⚠ **`0` is DISARMED and is byte-identical to the knob never existing** — that is what makes an
/// explicitly written `--min-trades 0` honest rather than a silent no-op, and it is why
/// [`apply_trade_floor`] returns its argument untouched rather than re-stamping an equal value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TradeFloor(usize);

impl TradeFloor {
    /// No floor. Every point is ranked on its own score, exactly as before this type existed.
    pub const DISARMED: TradeFloor = TradeFloor(0);

    /// A floor of `min_trades`. `0` builds [`TradeFloor::DISARMED`].
    pub fn new(min_trades: usize) -> Self {
        TradeFloor(min_trades)
    }

    /// The count itself — what a progress line or a summary reports.
    pub fn min_trades(self) -> usize {
        self.0
    }

    /// Whether this floor does anything at all.
    pub fn is_armed(self) -> bool {
        self.0 > 0
    }

    /// Whether a run with `n_trades` trades is admissible evidence. A DISARMED floor admits
    /// everything, including a zero-trade run — which is the pre-existing behaviour and must stay
    /// reachable, because a zero-trade point is legitimate information about a parameter region.
    pub fn admits(self, n_trades: usize) -> bool {
        n_trades >= self.0
    }
}

/// Apply a [`TradeFloor`] to one evaluated point — **the ONE place thinness becomes unrankability**,
/// so the four methods, both bar sources and both evaluator constructions cannot answer differently.
///
/// ⚠ **The floored score is `NaN`, and `NaN` is not a shortcut for "very low".** It is this crate's
/// written contract for UNRANKABLE ([`Optimizer`]'s score contract), and choosing it buys four
/// behaviours that already exist rather than four that would have to be written: `super::sweep`'s
/// `cmp_scores_desc` sorts it after every finite score; `crate::search`'s `better` refuses to let it
/// displace a finite incumbent, so euler cannot refine INTO a thin region; `crate::search::tpe`'s
/// observation fold maps it to `f64::NEG_INFINITY` before telling the model, so TPE learns the
/// region is worthless instead of dropping the trial; and `crate::search::genetic` treats an absent
/// score as the worst fitness. `f64::NEG_INFINITY` was the other candidate and is refused by that
/// same contract — it is finite-comparable, so it would sort merely last-among-finite and change
/// tie behaviour for every method at once.
///
/// ⚠ **The stamped `row.score` is updated too, and it has to be**: `sort_scored_rows` reads the ROW,
/// not the steering score, so flooring only the steering value would move the refinement centre
/// while leaving the report's #1 row the thin point. `Option::map` rather than an assignment, so a
/// row that carried no stamped score does not GAIN one — that would add a `"score": null` key to a
/// document that had none.
///
/// ⚠ **A FAILED point is left exactly as it is.** Its report is `None`, so it has no trade count to
/// judge, and it is already unrankable by `score_row`'s own rule. Overwriting it here would be the
/// same value written twice — stated so nobody "fixes" the early return into a stamp.
///
/// The row's `report` is deliberately UNTOUCHED: `n_trades` stays readable in the `--json` document,
/// which is the only way an operator can see WHY a row fell to the bottom. `ParamscanRow`'s
/// report-XOR-error rule forbids writing an explanation into `error`.
pub fn apply_trade_floor(mut ev: Evaluated, floor: TradeFloor) -> Evaluated {
    if !floor.is_armed() {
        return ev;
    }
    let admissible = match ev.row.report.as_ref() {
        Some(report) => floor.admits(report.n_trades),
        // No report: a failed point, already unrankable. Nothing to judge and nothing to change.
        None => return ev,
    };
    if admissible {
        return ev;
    }
    ev.row.score = ev.row.score.map(|_| f64::NAN);
    ev.score = f64::NAN;
    ev
}

/// One progress observation: where a search is, how long it has taken, and what it has found.
///
/// ⚠ **Observational ONLY. Nothing here reaches the report**, and that is the property that makes a
/// clock and a counter admissible inside a seam whose determinism gates compare bytes:
/// `super::sweep`'s `parallel_and_sequential_sweeps_are_byte_identical` and euler's twin compare
/// [`ParamscanReport`]s, which no field of this struct enters. What IS non-deterministic is the
/// EVENT STREAM — under `ParamscanExec::Parallel` events arrive in completion order, so two runs of
/// the same search emit the same events in different orders with different elapsed times. Never
/// assert on it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProgressEvent {
    /// Points evaluated so far, INCLUDING this one. Always `>= 1`.
    pub completed: u64,
    /// [`Optimizer::budget_hint`]'s answer — `None` for a method that cannot promise a total.
    pub total: Option<u64>,
    /// Wall time since the observer was armed, which is the evaluator's construction and therefore
    /// a hair before the first point rather than at it.
    pub elapsed: Duration,
    /// The best score seen so far under `super::sweep::cmp_scores_desc`, AFTER the trade floor —
    /// so a progress line can never advertise a leader the report will bury. `None` before anything
    /// rankable has been seen, and it STAYS `None` through a run where nothing is rankable.
    pub best: Option<f64>,
}

impl ProgressEvent {
    /// Time remaining, extrapolated from the mean cost of the points already done.
    ///
    /// ⚠ `None` without a `total`, and `None` on a run whose elapsed time is still zero — a mean of
    /// zero would render an ETA of zero seconds on a search about to take an hour, which is worse
    /// than printing nothing. Linear extrapolation is the honest model here and nothing more is
    /// claimed for it: every point runs one backtest over the same range, so the per-point cost is
    /// near-constant and the error is the variance of the data loader, not of the algorithm.
    pub fn eta(&self) -> Option<Duration> {
        let total = self.total?;
        let done = self.completed.min(total);
        let remaining = total.checked_sub(done)?;
        if remaining == 0 {
            return Some(Duration::ZERO);
        }
        let per_point = self.elapsed.checked_div(u32::try_from(done).ok()?)?;
        if per_point.is_zero() {
            return None;
        }
        per_point.checked_mul(u32::try_from(remaining).ok()?)
    }

    /// The human line, for STDERR. No trailing newline — the sink owns the line ending, because a
    /// terminal sink may one day want a carriage return instead.
    ///
    /// ⚠ **It renders nothing a machine is meant to parse**, deliberately: the shape is free to
    /// change, and the framed alternative is [`ProgressEvent::render_json`]. A test that greps this
    /// string for a number is pinning the wrong surface.
    pub fn render_human(&self) -> String {
        let of = match self.total {
            Some(total) => format!("{}/{}", self.completed, total),
            None => format!("{}", self.completed),
        };
        let best = match self.best {
            Some(s) => format!("{s:.4}"),
            None => "n/a".to_string(),
        };
        let eta = match self.eta() {
            Some(d) => format!(", eta {}", render_secs(d)),
            None => String::new(),
        };
        format!(
            "backtest: searched {of} points in {}{eta} — best {best}",
            render_secs(self.elapsed)
        )
    }

    /// The MACHINE line: one self-contained JSON object, no trailing newline, for a caller that
    /// writes newline-delimited JSON to STDERR.
    ///
    /// ⚠ **Hand-assembled rather than serde-derived, and that is a dependency decision.** This
    /// crate's harness tree carries no `serde_json` edge — [`ParamscanReport`] derives `Serialize`
    /// and lets the BINARY choose a serializer — and adding one so a progress line can be printed
    /// would put a serializer in the search's build graph for five scalar fields. None of them can
    /// contain a character needing an escape, so there is nothing for a serializer to get right that
    /// this does not.
    ///
    /// ⚠ **A non-finite `best` renders `null`**, matching `super::sweep`'s `ser_opt_score` rule
    /// exactly — `null` means unrankable on both surfaces. A bare `NaN` token would make the line
    /// unparseable, which on the one output shape whose entire purpose is being parsed is the worst
    /// available failure.
    pub fn render_json(&self) -> String {
        let total = match self.total {
            Some(t) => t.to_string(),
            None => "null".to_string(),
        };
        let best = match self.best {
            Some(s) if s.is_finite() => format!("{s}"),
            _ => "null".to_string(),
        };
        let eta = match self.eta() {
            Some(d) => format!("{:.3}", d.as_secs_f64()),
            None => "null".to_string(),
        };
        format!(
            "{{\"event\":\"search_progress\",\"completed\":{},\"total\":{total},\"elapsed_s\":{:.3},\"eta_s\":{eta},\"best\":{best}}}",
            self.completed,
            self.elapsed.as_secs_f64()
        )
    }
}

/// `1h02m03s` / `2m03s` / `3.4s` — a duration a human reads at a glance, with no dependency behind
/// it. Seconds get a decimal only below a minute, where the difference between 3s and 3.4s is the
/// difference between "fast" and "this is going to take a while".
fn render_secs(d: Duration) -> String {
    let secs = d.as_secs();
    if secs >= 3600 {
        format!("{}h{:02}m{:02}s", secs / 3600, (secs % 3600) / 60, secs % 60)
    } else if secs >= 60 {
        format!("{}m{:02}s", secs / 60, secs % 60)
    } else {
        format!("{:.1}s", d.as_secs_f64())
    }
}

/// Where a [`ProgressEvent`] goes.
///
/// ⚠ **A TRAIT rather than a hard-wired `eprintln!`, because a harness module writing to a stream is
/// the thing this tree has deliberately never done.** Every other cost line a method produces
/// ([`SearchOutcome`]'s `summary`) is returned to the caller ALREADY RENDERED and printed by the
/// binary, precisely so a library cannot decide what an operator's terminal shows. Progress cannot
/// use that shape — it is per-point and the run has not returned yet — so the caller supplies the
/// destination instead, and [`StderrProgress`] is the one this crate ships for it.
///
/// `Send + Sync` is not decorative: [`StoreEvaluator`]'s `evaluate` fans a batch out with
/// `into_par_iter()` over `&self`, so every rayon worker calls [`ProgressSink::point`] concurrently.
/// An implementation that buffers must synchronise; `eprintln!` is line-atomic under the `Stderr`
/// lock, which is why the shipped sink needs nothing.
pub trait ProgressSink: Send + Sync {
    /// One point finished. MUST NOT write to stdout — see [`StderrProgress`]'s own argument.
    fn point(&self, ev: &ProgressEvent);
}

/// What `--progress` selected.
///
/// ⚠ `Auto` is not "on": it is "on IF a human is watching". A non-terminal stderr means a log file,
/// a CI transcript or a captured pipe, and appending one line per backtest to a captured log that
/// nobody will read is how a log directory reached 341 GB elsewhere in this workspace.
/// [`StderrProgress::for_mode`] performs the decision once, at construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProgressMode {
    /// Human-readable, throttled, ONLY when stderr is a terminal. The default.
    #[default]
    Auto,
    /// Silent. Nothing is constructed and nothing is counted. ⚠ Spelled `none` on the command line
    /// and `Off` here, deliberately: a variant literally named `None` sits beside `Option::None` in
    /// every match in this file, and a reader cannot tell which one an arm means at a glance.
    Off,
    /// One newline-delimited JSON event per point on stderr, terminal or not, UNTHROTTLED — a
    /// machine consuming a stream wants every event, and dropping some to save a line is a
    /// disservice to the only consumer that can use them all.
    Json,
}

impl ProgressMode {
    /// The spellings, in the order a usage line should print them. The ONE roster — a refusal
    /// RENDERS it rather than restating it, so a fourth mode cannot be accepted by the parser and
    /// missing from the message.
    pub const NAMES: [&'static str; 3] = ["auto", "none", "json"];

    /// Case-insensitive parse, `None` for anything else — the same shape
    /// `super::sweep::RankMetric::from_str_ci` has, so the caller owns the refusal SENTENCE and this
    /// owns only the grammar.
    pub fn from_str_ci(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "auto" => Some(ProgressMode::Auto),
            "none" => Some(ProgressMode::Off),
            "json" => Some(ProgressMode::Json),
            _ => None,
        }
    }
}

/// The shipped [`ProgressSink`]: **STDERR, and structurally never stdout.**
///
/// ⚠ **The stdout rule is the load-bearing one on this plane, and here it is the type's shape rather
/// than a convention.** `vike-cli backtest path` exists to print one line and nothing else, and a
/// `--json` report must stay parseable — so a single progress line on stdout corrupts a document a
/// script is piping. This type writes through `eprintln!` at both of its two call sites and holds no
/// handle a caller could redirect, so there is no configuration under which it can reach the other
/// stream.
pub struct StderrProgress {
    json: bool,
    /// `0` = emit every event. Milliseconds of elapsed time between human lines.
    min_interval_ms: u64,
    /// Elapsed-millis of the last emitted line, or `u64::MAX` for "nothing emitted yet". An atomic
    /// rather than an `Instant` because [`ProgressEvent`] already carries the elapsed time, so the
    /// throttle needs no clock of its own and no lock to be poisoned in a rayon worker.
    last_ms: AtomicU64,
}

impl StderrProgress {
    /// Twice a second. Fast enough that a search feels live, slow enough that a 4000-point grid
    /// costs a terminal a few hundred lines instead of four thousand.
    pub const HUMAN_MIN_INTERVAL_MS: u64 = 500;

    /// The sink `mode` implies, or `None` for "emit nothing" — which is both [`ProgressMode::Off`]
    /// and [`ProgressMode::Auto`] with a non-terminal stderr.
    ///
    /// ⚠ The terminal probe happens HERE, once, rather than per event: `IsTerminal` is a syscall,
    /// and asking it once per backtest would be a syscall per point for an answer that cannot change
    /// mid-process.
    pub fn for_mode(mode: ProgressMode) -> Option<Self> {
        match mode {
            ProgressMode::Off => None,
            ProgressMode::Json => Some(Self::json()),
            ProgressMode::Auto => std::io::stderr().is_terminal().then(Self::human),
        }
    }

    /// Newline-delimited JSON, every event.
    pub fn json() -> Self {
        StderrProgress { json: true, min_interval_ms: 0, last_ms: AtomicU64::new(u64::MAX) }
    }

    /// The throttled human line.
    pub fn human() -> Self {
        StderrProgress {
            json: false,
            min_interval_ms: Self::HUMAN_MIN_INTERVAL_MS,
            last_ms: AtomicU64::new(u64::MAX),
        }
    }

    /// Whether this event is due. **The FINAL event of a run with a known total always passes**, so
    /// the last line a human sees reports the finished count rather than whatever the throttle let
    /// through last — a progress line frozen at `61/64` above a printed report reads as a search
    /// that stopped early.
    pub(super) fn due(&self, ev: &ProgressEvent) -> bool {
        if self.min_interval_ms == 0 {
            return true;
        }
        if ev.total.is_some_and(|t| ev.completed >= t) {
            return true;
        }
        let now = u64::try_from(ev.elapsed.as_millis()).unwrap_or(u64::MAX);
        let last = self.last_ms.load(Ordering::Relaxed);
        if last != u64::MAX && now.saturating_sub(last) < self.min_interval_ms {
            return false;
        }
        // ⚠ A plain store, not a compare-exchange: two workers finishing inside the same window can
        // both pass. That is a DUPLICATE LINE, which is the right failure for a progress display —
        // the alternative is a CAS loop in the path of every backtest to protect a cosmetic
        // property. Under `ParamscanExec::Sequential` (the determinism lever) it cannot happen at
        // all.
        self.last_ms.store(now, Ordering::Relaxed);
        true
    }
}

impl ProgressSink for StderrProgress {
    fn point(&self, ev: &ProgressEvent) {
        if !self.due(ev) {
            return;
        }
        if self.json {
            eprintln!("{}", ev.render_json());
        } else {
            eprintln!("{}", ev.render_human());
        }
    }
}

/// The counter, the clock and the running best behind one [`ProgressSink`] — armed on an evaluator,
/// never on an [`Optimizer`].
///
/// ⚠ **The evaluator is the only place this works for all four methods at once.** A hook on
/// `Optimizer::search` would have to be implemented four times, in four loops with four different
/// shapes, and a fifth method would ship with no progress until somebody remembered.
/// [`PointEvaluator::evaluate`] is the one funnel every candidate of every method passes through, in
/// both bar sources — so one arming site covers grid, euler, tpe, genetic and whatever comes fifth,
/// by construction.
pub(super) struct ProgressTracker {
    sink: Box<dyn ProgressSink>,
    total: Option<u64>,
    started: Instant,
    completed: AtomicU64,
    /// The running best as raw bits, seeded with `NaN` = nothing rankable yet. Bits rather than a
    /// `Mutex<f64>` so the fold is lock-free in a rayon worker and there is no poisoning to handle
    /// when a backtest panics.
    best_bits: AtomicU64,
}

impl ProgressTracker {
    pub(super) fn new(sink: Box<dyn ProgressSink>, total: Option<u64>) -> Self {
        ProgressTracker {
            sink,
            total,
            started: Instant::now(),
            completed: AtomicU64::new(0),
            best_bits: AtomicU64::new(f64::NAN.to_bits()),
        }
    }

    /// Fold one finished point in and emit.
    ///
    /// ⚠ `score` must ALREADY have been through [`apply_trade_floor`] — see [`observed`], the only
    /// caller, which enforces the order. A progress line built from the raw score would advertise a
    /// running best the floor has already disqualified, i.e. name a leader the printed report
    /// buries.
    pub(super) fn observe(&self, score: f64) {
        let completed = self.completed.fetch_add(1, Ordering::Relaxed) + 1;
        // ⚠ The ONE ordering rule (`super::sweep::cmp_scores_desc`), never `f64::max`: `max`
        // returns the non-NaN operand, so it would let an unrankable score be treated as an
        // improvement on nothing and, worse, quietly install it as the incumbent best.
        let _ = self.best_bits.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |bits| {
            match super::sweep::cmp_scores_desc(score, f64::from_bits(bits)) {
                std::cmp::Ordering::Less => Some(score.to_bits()),
                _ => None,
            }
        });
        let best = f64::from_bits(self.best_bits.load(Ordering::Relaxed));
        self.sink.point(&ProgressEvent {
            completed,
            total: self.total,
            elapsed: self.started.elapsed(),
            best: if best.is_nan() { None } else { Some(best) },
        });
    }
}

/// Floor, THEN observe — **the ONE post-evaluation site both shipped evaluators fold through**, so
/// the two cannot drift about the order.
///
/// ⚠ The order is the whole reason this is a function rather than two lines written twice. Observing
/// first would publish a running best the floor then disqualifies, and an operator watching a search
/// would see a leader that is absent from the report they are handed ninety seconds later.
pub(crate) fn observed(
    ev: Evaluated,
    floor: TradeFloor,
    progress: Option<&ProgressTracker>,
) -> Evaluated {
    let ev = apply_trade_floor(ev, floor);
    if let Some(p) = progress {
        p.observe(ev.score);
    }
    ev
}
