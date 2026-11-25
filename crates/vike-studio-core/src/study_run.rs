//! **What a study run IS, as every reader shows it**: the manifest's `kind` ([`STUDY_RUN_KIND`]), the
//! study's typed `detail` subtree ([`StudyDetail`]), one metric's on-disk shape and printed form
//! ([`StudyMetric`]) and the note every study run carries ([`STUDY_METRICS_NOTE`]).
//!
//! ⚠ **Only the VOCABULARY is here since 2026-11-19.** The runner that writes it (`run_study_with` and
//! its request and errors) is `crates/vike-study-runner/src/study_run.rs`'s, on the server side: studies
//! run only on the backtest server (the owner's ruling of that day), and this crate is what the desktop
//! links, so the desktop shows a run with these types and links no runner.
//!
//! # How a study run is told apart from a backtest run — TWO ways, and the second is structural
//!
//! `docs/superpowers/specs/2026-08-24-research-engine-user-split-design.md` spends a ⚠ on this: a
//! study's Sharpe is vectorized (hysteresis, min-hold, slippage — no order book, no queue position,
//! no fee schedule) and a backtest's is event-driven with real fills. *"Showing both in one `runs/`
//! list is what makes the gap between them visible rather than assumed"* — visible, which means the
//! surface must not let them merge.
//!
//! * **[`STUDY_RUN_KIND`] as the manifest's `kind`.** The one top-level field `vike_runs` says a
//!   reader may branch on. It is what puts a study row and a backtest row in one list at all.
//! * **The metrics NEST under `detail.metrics` and are never hoisted beside the common fields.**
//!   This is the half that does the work. A top-level `sharpe` would have been renderable in a
//!   shared column by a listing that never asked which scorer produced it — the two numbers would
//!   have become one number, in a column, permanently. Nested, a reader must branch on `kind` and
//!   reach into a kind-specific subtree to get the number at all, which is exactly the moment it
//!   has to know which claim it is holding.
//! * …and [`STUDY_METRICS_NOTE`] rides in the same subtree, because a manifest is also read ALONE —
//!   copied out of its directory, opened by hand — where no listing is present to carry the
//!   distinction for it.
//!
//! ⚠ The note states only what this writer can HONESTLY assert, and it used to overclaim: it said
//! a study "cannot have reached the event-driven simulator", arguing from `vike-user-research`
//! declaring no `vike-backtest`/`vike-simulator` dependency. That premise never covered the actual path —
//! `crates/vike-study-runner/src/study_run.rs`'s `run_study_with` hands a study the backtest seam whenever
//! its HOST supplies one (its `StudyRunRequest::sim`; the backtest server and `vike-study` hand
//! `vike_backtesthub::harness_sim::HarnessSim`, and until 2026-11-10 every host did, unconditionally),
//! which drives the full event-driven harness through an erased `StudySim` trait `vike-user-research`
//! DOES declare. A study that calls
//! `ctx.sim()` reaches the real order book, queue position and fee schedule with no dependency
//! edge to show for it, and 0076's `poly_mm` study is exactly that case. So the note asserts only
//! what is actually knowable from here: the numbers were produced by a study rather than by a
//! `backtest` run, and whether the seam was called is not something this manifest records.
//!
//! # Non-finite metrics are RECORDED, and JSON is where that nearly went wrong
//!
//! `crates/vike-user-research/src/contract/outcome.rs`'s `StudyOutcome::metric` accepts a non-finite value
//! deliberately: a `NaN` Sharpe over a fold that never traded is an observation, and rounding it to
//! zero is the `unwrap_or(0.0)` that turns a gap into a number.
//!
//! ⚠ **`serde_json` writes a non-finite `f64` as `null`, silently** — `serde_json::Number::from_f64`
//! refuses one and the serializer maps the refusal to `Null`. That is the same defect arriving at
//! the last possible moment: three distinct observations (`NaN`, `inf`, `-inf`) and an absent value
//! all reaching disk as one token. So every metric is written as an OBJECT carrying both halves —
//! `value` (a number, or `null` when there is no number to write) and `nonfinite` (which one it
//! was, or `null`) — and [`StudyMetric::of`] is the one place that mapping is spelled.
//!
//! # …and why metrics are an ARRAY rather than an object
//!
//! `StudyOutcome` keeps metrics in EMISSION order on purpose — its own doc: *"a study puts its
//! headline first, and a `BTreeMap` would file `sharpe` after `n_trades` for no reason anyone
//! chose"*. This workspace pins `serde_json` without `preserve_order` (root `Cargo.toml`), so a
//! `serde_json::Map` IS a `BTreeMap` and an object would sort that order away at the last step. An
//! array preserves it, and it is the only shape that can.

use vike_data::TimestampRange;
use vike_runs::RunManifest;

/// The `kind` every study run declares in its manifest — the one top-level field
/// `crates/vike-runs/src/lib.rs`'s `RunManifest` says a reader may branch on.
///
/// A bare `"study"` rather than a tier-qualified name (`"study-rust"`): the tier is a property of
/// where the SOURCE lives (`crates/vike-study-runner/src/listing.rs`'s `StudyTier`), and the spec's
/// own words are that the tiers *"differ only in REACH"*. A run produced by an interpreted study and
/// one produced by a compiled study are the same kind of result and belong in the same column.
pub const STUDY_RUN_KIND: &str = "study";

/// The sentence a study run's `detail` carries about its own numbers.
///
/// Only the structurally-true negative — see this module's doc for why it does not name a scorer.
pub const STUDY_METRICS_NOTE: &str = "Produced by a STUDY, not by a `backtest` run: the study \
     computed these numbers its own way, and may have called the event-driven simulator through \
     the host's backtest seam, which this manifest does not record. Not comparable with a \
     `backtest` run's metrics.";

/// A study run's manifest `detail` subtree, TYPED — the writer (the study runner's `persist`, on the
/// server) and every reader (the Studio's Research pane) share this one shape, so a key renamed on one side is a compile error
/// rather than a pane gone blank. Field order and names ARE the on-disk document.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct StudyDetail {
    /// The study's name, its folder's.
    pub study: String,
    /// The window the run was ASKED about.
    pub window: StudyWindow,
    /// Whether a learner was supplied — what decides whether a FITTING study could run at all.
    pub learner: bool,
    /// Every metric, in EMISSION order (see this module's doc for why an array).
    pub metrics: Vec<StudyMetric>,
    /// The artifact file names the study wrote beside the manifest.
    pub artifacts: Vec<String>,
    /// [`STUDY_METRICS_NOTE`].
    pub metrics_note: String,
}

impl StudyDetail {
    /// A study manifest's `detail`, or `None` when the manifest is not a study run's.
    pub fn of(manifest: &RunManifest) -> Option<Self> {
        serde_json::from_value(manifest.detail.clone()).ok()
    }
}

/// The window, as the two nullable bounds `vike_data::TimestampRange` actually is. `null` is "unbounded",
/// which is a different answer from "zero" and must not be written as one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StudyWindow {
    /// Epoch ms, or `None` for unbounded.
    pub start: Option<i64>,
    /// Epoch ms, or `None` for unbounded.
    pub end: Option<i64>,
}

impl From<TimestampRange> for StudyWindow {
    fn from(w: TimestampRange) -> Self {
        Self { start: w.start, end: w.end }
    }
}

/// One metric as it lands on disk: `{ "name": …, "value": <number|null>, "nonfinite": <tag|null> }`.
/// The ONE place the non-finite mapping is spelled; see this module's doc for why `serde_json`'s own
/// treatment of a non-finite `f64` is the thing this shape exists to refuse.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct StudyMetric {
    /// The metric's name, as the study emitted it.
    pub name: String,
    /// The value, or `None` when it is non-finite (then [`Self::nonfinite`] says which).
    pub value: Option<f64>,
    /// [`nonfinite_tag`]'s token for a non-finite value, else `None`.
    pub nonfinite: Option<String>,
}

impl StudyMetric {
    /// `value` under `name`, with a non-finite value RECORDED as its tag rather than flattened.
    pub fn of(name: &str, value: f64) -> Self {
        let nonfinite = nonfinite_tag(value);
        Self {
            name: name.to_string(),
            value: nonfinite.is_none().then_some(value),
            nonfinite: nonfinite.map(str::to_string),
        }
    }

    /// The value as every surface PRINTS it — the Studio's Research pane, the `vike-study` runner
    /// and (through the wire answer's `text`) `vike-cli study run`: six decimals, or the
    /// non-finite tag, never a number a non-finite value was rounded into.
    pub fn text(&self) -> String {
        match (&self.nonfinite, self.value) {
            (Some(tag), _) => tag.clone(),
            (None, Some(v)) => format!("{v:.6}"),
            (None, None) => "null".to_string(),
        }
    }
}

/// Which non-finite value this is, or `None` for an ordinary number.
///
/// The three tags are IEEE-754's own names, so a reader that has to reconstruct the value has an
/// unambiguous token to match on rather than a rendering that varies by language.
pub fn nonfinite_tag(value: f64) -> Option<&'static str> {
    if value.is_nan() {
        Some("NaN")
    } else if value == f64::INFINITY {
        Some("inf")
    } else if value == f64::NEG_INFINITY {
        Some("-inf")
    } else {
        None
    }
}

#[path = "study_run_tests.rs"]
#[cfg(test)]
mod study_run_tests;
