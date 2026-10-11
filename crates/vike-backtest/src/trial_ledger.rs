//! What a parameter SEARCH leaves behind: the documents of a search parent run.
//!
//! A search mints ONE run directory under `<project>/user_data/runs/` (the shape
//! `vike_model::runs` owns) and writes three documents of its own inside it — [`SEARCH_FILE`]
//! before the search runs, [`TRIALS_FILE`] one appended line per evaluation, and the resolved
//! [`TrialsDocument`] into `vike_model::runs::REPORT_FILE` afterwards.
//!
//! # Why `<id>#<n>` is an ADDRESS and not a directory
//!
//! A search's trials are addressed `<run_id>#<n>` (the CLI surface design's selector grammar), and
//! there are three places `<n>` could live. Sibling DIRECTORIES named `<id>#<n>` under the runs
//! root are the tempting spelling and the wrong one: `crates/vike-studio-core/src/listing.rs`'s
//! `list_runs` enumerates every folder in that root and knows nothing about parenthood, so a
//! 512-point sweep puts 512 rows in the Studio Research tab. Nested directories under the parent
//! are invisible to that listing (its `read_entries` does one non-recursive `read_dir`), but cost
//! two files per trial and cannot be appended to cheaply. So `<n>` is a LINE INDEX in
//! [`TRIALS_FILE`] — O(1) to append, one row in every listing, and a crashed search leaves a
//! readable prefix rather than a corrupt document.
//!
//! # Why this module sits apart from `harness/`, and why it is in this crate rather than `vike-model`
//!
//! ⚠ Until the 2026-09-27 feature collapse, `crates/vike-backtest/src/lib.rs` declared
//! `pub mod harness;` behind `#[cfg(feature = "hist-replay")]`, and this module's whole point was
//! staying OUTSIDE that gate: these are DOCUMENTS, not search machinery, so a default build compiled
//! and tested them without paying for the harness tree. Both are unconditional now, but the module
//! boundary survives for the same reason it was drawn: the RECORDER, which needs
//! `harness::optimize::{Candidate, Evaluated}`, is the half that depends on `harness/` —
//! `crate::search::trials` — and this module still does not.
//!
//! ⚠ **The obvious home is `vike_model::runs`, beside the manifest, and it is not used — for one
//! measurable reason.** [`TrialRecord::overrides`] is a `Vec<(String, toml::Value)>`, because that
//! is what `harness::optimize::Candidate` IS, and `vike-model` declares no `toml` dependency.
//! `vike-model` is the workspace's bottom crate (layer 10), so adding one puts the whole `toml`
//! parser stack into the build graph of every crate that names the domain vocabulary — including
//! the minimal `vike-bridge-core --no-default-features` configuration the `light-consumers` CI lane
//! exists to keep light. Every document here is plain JSON, so a reader outside this crate reads it
//! with `serde_json::Value` and no edge at all. If a TYPED reader outside this crate is ever
//! wanted, this module moves DOWN whole the way `runs.rs` did — a move, never a copy, and never
//! behind a `pub use` shim — and that `toml` edge is the price to argue then.
//!
//! # Persisting is additive, never a new way to fail
//!
//! Every function here returns its failure as a value, and reuses `vike_model::runs`'s two error
//! types rather than inventing a third. A caller prints its result and persists afterwards; a
//! search whose ledger cannot be written still searches, with the failure named on stderr.

use std::path::Path;

use serde::{Deserialize, Serialize};
use vike_model::runs::{RunPersistError, RunReadError};

/// The trial LEDGER's file name inside a search parent's run directory — **JSON Lines**, one
/// [`TrialRecord`] per line, appended as each evaluation completes.
///
/// ⚠ **Lines are COMPACT JSON, never pretty-printed** — `serde_json::to_string`, not
/// `to_string_pretty`. A pretty record spans lines and there is no longer a format.
pub const TRIALS_FILE: &str = "trials.jsonl";

/// The search HEADER's file name inside a search parent's run directory.
///
/// Written FIRST, before the first evaluation, which is the whole reason it exists:
/// `vike_model::runs::MANIFEST_FILE` is written LAST as the completion marker (that module's doc
/// carries the argument), so an INTERRUPTED search has no manifest — and `--resume` must still be
/// able to prove it is resuming the same search. This file is what it proves it against.
pub const SEARCH_FILE: &str = "search.json";

/// `vike_model::runs::RunManifest::kind` for a parameter search. A plain string like every other
/// kind, so a reader that has never heard of a search still renders every common field.
pub const SEARCH_RUN_KIND: &str = "search";

/// The schema version carried BY every trial-ledger document, shipped with the first one rather
/// than retrofitted. A schema tag added to documents already in people's scripts is the one item
/// that gets strictly more expensive every week — `vike_model::runs::SERIES_SCHEMA` argues the same
/// point at length for the documents beside these.
pub const TRIAL_LEDGER_SCHEMA: u32 = 1;

/// What [`SearchIdentity::profile_fnv1a64`] holds when the profile could not be re-read. It is a
/// SENTINEL rather than an `Option` so that [`SearchIdentity::differences`] can refuse it on both
/// sides — see that method.
pub const PROFILE_UNREADABLE: &str = "unreadable";

/// What [`SearchIdentity::store_data`] holds when the store could not be inventoried. A SENTINEL on
/// the same terms as [`PROFILE_UNREADABLE`] and refused on both sides for the same reason: a search
/// whose data cannot be witnessed cannot be proved to be over the same data.
pub const DATA_UNREADABLE: &str = "unreadable";

/// FNV-1a, 64-bit, as lowercase hex — the profile fingerprint `--resume` compares.
///
/// Hand-written rather than pulled from a crate: this is a CHANGE DETECTOR, not a cryptographic
/// commitment, and `deny.toml` polices every new dependency in a workspace that links into one
/// order-signing binary. Twelve lines is cheaper than an audit-surface entry.
///
/// ⚠ It is deliberately NOT the run ADDRESS. `crate::run_fingerprint::input_fingerprint` is SHA-256
/// over the config text AND the data slice the store held, and it is what
/// `vike_model::runs::RunManifest::fingerprint` carries. This one answers a narrower question —
/// "are these the same profile BYTES" — which is the half a resume can check without reading the
/// store at all.
pub fn fnv1a64_hex(bytes: &[u8]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

/// Everything a resumed search must match before it may reuse a single trial.
///
/// ⚠ Every field is a genuine INPUT to a trial's score. The profile decides the strategy, the data
/// slice and the engine costs; the store decides which bytes those names resolve to; the method,
/// seed and budget decide which candidates get proposed and in what order; the BINARY decides how a
/// fill, a fee and a size are computed. Change any one and a cached score is an answer to a
/// different question.
///
/// ⚠ **The store PATH is not a substitute for the store CONTENTS, and both are here.** An earlier
/// draft of this type recorded [`Self::store`] alone, and the hole it left is the worst failure this
/// whole feature can produce — not a crash, a silently WRONG ANSWER. Kill a 200-trial search at 60,
/// backfill a gap or re-fetch a corrected day inside the profile's own `[data] from/to`, resume:
/// [`Self::differences`] is empty, 60 cached scores computed on dataset A are ranked against 140
/// computed on dataset B, and the leaderboard, `report.json`, the trials table and the
/// `--export-params` winner are all incoherent with no error and no field a reader could detect it
/// from afterwards. A search exists to CHOOSE by comparison; a comparison across two datasets
/// chooses nothing. [`Self::store_data`] is the witness that closes it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchIdentity {
    /// [`fnv1a64_hex`] of the profile FILE's bytes, or [`PROFILE_UNREADABLE`].
    pub profile_fnv1a64: String,
    /// The profile's own `name`, for a human reading the refusal. Not compared — it is derived from
    /// the same bytes, so it can only differ when the hash already does.
    pub profile_name: Option<String>,
    /// The hist-store root this search read, CANONICALIZED.
    ///
    /// ⚠ Canonical rather than as-spelled, unlike `vike_model::runs::RunConfig::path` (which keeps
    /// the operator's own spelling precisely because a paste-back must reproduce the run). The
    /// question here is the opposite one — "is this the same STORE" — and `--store store` resolves
    /// to two different directories from two different projects. With a shared runs root
    /// (`VIKE_USER_DATA_DIR`) the as-spelled form made those two identities compare EQUAL.
    pub store: String,
    /// The witness for what the store HELD, one line per series this profile resolves to. See
    /// [`DATA_UNREADABLE`] and this type's doc;
    /// `crates/vike-backtest/src/backtest_cli/persist.rs`'s `data_witness` is what produces it and
    /// argues what it catches and what it misses.
    ///
    /// ⚠ It is a RENDERING of `crate::run_fingerprint::DataFingerprint` rather than a store walk of
    /// its own — the walk it used to perform was merged into the collector the search now also uses
    /// for its run ADDRESS. The rendered text is byte-identical to what the separate walk produced,
    /// deliberately: this field is compared as a STRING by [`SearchIdentity::differences`], so a
    /// search started by an older binary must still be resumable by a newer one.
    pub store_data: String,
    /// The commit the searching BINARY was built from, or `None` when it cannot name one.
    ///
    /// ⚠ A resume after a pull that changed fill logic, sizing or fees would otherwise reuse scores
    /// the current engine would not produce. ⚠ **Declared residual:** the standalone `backtest`
    /// binary passes no `vike_model::runs::BuildStamp` at all, so this is `None` on BOTH sides
    /// there and contributes no witness — the same absence `RunManifest::git_sha` records, for the
    /// same dependency reason. It is a real witness only under the `vike-backend` multicall. `None` and
    /// `Some` still differ, so resuming a multicall search with the standalone engine is refused.
    #[serde(default)]
    pub build: Option<String>,
    /// `grid` | `euler` | `tpe` | `genetic` — `harness::Optimizer::name`'s own spelling.
    pub method: String,
    /// The rank label: a `harness::RankMetric`'s CLI short name, or `multi`.
    pub rank_by: String,
    /// The reproducibility seed, for the two stochastic methods. `None` for grid and euler.
    pub seed: Option<u64>,
    /// The method's budget scalar — euler's `max_depth`, tpe's `n_trials`, genetic's
    /// `max_evaluations`. `None` for the grid (whose budget is the expansion) and for a genetic run
    /// whose budget derives from the grid size.
    pub budget: Option<u64>,
}

impl SearchIdentity {
    /// Every field that differs, named with BOTH values — the refusal message's whole content.
    ///
    /// ⚠ [`PROFILE_UNREADABLE`] on EITHER side is always a difference, including when both sides
    /// carry it, and [`DATA_UNREADABLE`] behaves identically. Two runs whose profile could not be
    /// re-read — or whose store could not be inventoried — would otherwise compare equal, and a
    /// resume would reuse trials from a search it cannot prove was the same one.
    ///
    /// ⚠ The DATA difference renders as a count rather than as the two witnesses: the witness is one
    /// line per series and printing both sides of it whole would bury the other fields in a refusal
    /// an operator has to read.
    pub fn differences(&self, other: &Self) -> Vec<String> {
        // ⚠ A FREE function taking `&mut Vec`, not a capturing closure. A `let mut cmp = |…|` that
        // closes over `out` holds a mutable borrow for its whole scope, so the unreadable-profile
        // arm below — which pushes directly — is a second mutable borrow and does not compile.
        fn cmp(out: &mut Vec<String>, field: &str, a: &str, b: &str) {
            if a != b {
                out.push(format!("{field}: {a} -> {b}"));
            }
        }
        let mut out = Vec::new();
        if self.profile_fnv1a64 == PROFILE_UNREADABLE || other.profile_fnv1a64 == PROFILE_UNREADABLE
        {
            out.push(format!(
                "profile: {} -> {} (a profile that could not be re-read can never be proved \
                 unchanged, so a resume against it is refused)",
                self.profile_fnv1a64, other.profile_fnv1a64
            ));
        } else {
            cmp(&mut out, "profile", &self.profile_fnv1a64, &other.profile_fnv1a64);
        }
        cmp(&mut out, "store", &self.store, &other.store);
        // ⚠ THE DATA. Not a path — what the store HELD. See this type's doc for the wrong answer
        // this exists to refuse.
        if self.store_data == DATA_UNREADABLE || other.store_data == DATA_UNREADABLE {
            out.push(format!(
                "data: {} -> {} (a store that could not be inventoried can never be proved \
                 unchanged, so a resume against it is refused)",
                short_witness(&self.store_data),
                short_witness(&other.store_data)
            ));
        } else if self.store_data != other.store_data {
            out.push(format!(
                "data: the store's contents MOVED since that search ran ({} -> {}) — a backfill, a \
                 re-fetch or a delete inside this profile's own window. Cached scores were computed \
                 on the earlier data and cannot be ranked against fresh ones, so this resume is \
                 refused rather than resolved. Run the search again without --resume",
                short_witness(&self.store_data),
                short_witness(&other.store_data)
            ));
        }
        cmp(
            &mut out,
            "build",
            &fmt_opt_str(self.build.as_deref()),
            &fmt_opt_str(other.build.as_deref()),
        );
        cmp(&mut out, "optimizer", &self.method, &other.method);
        cmp(&mut out, "rank-by", &self.rank_by, &other.rank_by);
        cmp(&mut out, "seed", &fmt_opt(self.seed), &fmt_opt(other.seed));
        cmp(&mut out, "budget", &fmt_opt(self.budget), &fmt_opt(other.budget));
        out
    }
}

fn fmt_opt(v: Option<u64>) -> String {
    v.map(|n| n.to_string()).unwrap_or_else(|| "unset".to_string())
}

fn fmt_opt_str(v: Option<&str>) -> String {
    v.map(str::to_string).unwrap_or_else(|| "unset".to_string())
}

/// A data witness rendered for a REFUSAL MESSAGE: its sentinel, or its [`fnv1a64_hex`] plus how many
/// series lines it holds. The witness itself is one line per series and is the document's business,
/// not the message's.
fn short_witness(w: &str) -> String {
    if w == DATA_UNREADABLE {
        return DATA_UNREADABLE.to_string();
    }
    format!(
        "{} over {} series",
        fnv1a64_hex(w.as_bytes()),
        w.lines().filter(|l| !l.is_empty()).count()
    )
}

/// [`SEARCH_FILE`]'s content: what this search IS, written before it runs anything.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchHeader {
    /// [`TRIAL_LEDGER_SCHEMA`] at write time.
    pub schema: u32,
    /// The parent run's id — duplicated into the file for the same reason
    /// `vike_model::runs::RunManifest::run_id` is.
    pub run_id: String,
    /// `none` | `scalars` | `returns`. `returns` is `scalars` PLUS an in-memory bucketed return
    /// vector per trial, which is what produces [`TrialsDocument::overfit`]; the ledger it writes
    /// is byte-identical to `scalars`', so `--resume` treats the two the same. The design's fourth
    /// spelling, `series` (a whole equity CURVE per trial), is still refused at the flag —
    /// `crates/vike-backtest/src/backtest_cli/search_flags.rs`'s `parse_keep_trials` carries what
    /// `returns` keeps instead and why.
    pub keep_trials: String,
    /// [`TRIALS_FILE`], written into the document so a reader never has to know the constant.
    pub trials_file: String,
    /// What `--resume` proves it is resuming. See [`SearchIdentity`].
    pub identity: SearchIdentity,
    /// The run id this search was resumed FROM, when it was. `None` for a fresh search. A resume
    /// continues the SAME directory, so this is normally that directory's own id — it is recorded
    /// so a reader can tell a one-shot search from one that was picked up.
    #[serde(default)]
    pub resumed_from: Option<String>,
}

/// ONE trial: what was tried, what it scored, and what it measured.
///
/// ⚠ `metrics` is a [`serde_json::Value`] and NOT a `vike_analytics::report::BacktestReport`, deliberately — and
/// the reason CHANGED while this was being written, so it is stated as it now stands rather than as
/// the plan that produced it assumed. `vike_analytics::report::BacktestReport` gained `Deserialize`
/// with the run record, so a typed field would now COMPILE; it would still be the wrong shape.
/// `profit_factor` carries two different house sentinels (`f64::INFINITY` when there are no losing
/// trades but some profit, `0.0` when there is neither), and only `INFINITY` serializes to `null`
/// (`0.0` is finite, so it is written as `0.0`); `de_f64_null_as_infinity` resolves that `null`
/// back to `INFINITY`, so the typed field round-trips today only because `INFINITY` is the sole
/// non-finite sentinel (`crates/vike-analytics/src/report.rs` argues it). A ledger must return
/// what was written without depending on that. A `Value`
/// writes the report exactly and reads anything back, which is all a ledger needs, and it also
/// keeps this module free of the analytics types. `crates/vike-cli/src/cmd/backtest/execute.rs`'s `execute`
/// reaches the same conclusion from the other side.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TrialRecord {
    /// The 0-based EVALUATION index — the line index in [`TRIALS_FILE`], and the `<n>` of the
    /// `<run_id>#<n>` address. Evaluation order rather than RANK, because rank is not stable across
    /// a tie-break change and a resume cannot reproduce it, while evaluation order is what every
    /// seed-deterministic searcher replays exactly.
    pub n: usize,
    /// The candidate: `harness::optimize::Candidate`, which is the same value as
    /// `harness::sweep::ParamscanRow`'s `overrides` field.
    pub overrides: Vec<(String, toml::Value)>,
    /// The searcher's steering score. Higher is better; **`null` means UNRANKABLE** and nothing
    /// else. A plain `f64` rather than an `Option<f64>` precisely so there is one meaning: every
    /// evaluation HAS a steering score (`harness::optimize::Evaluated::score` is a bare `f64`), and
    /// `ParamscanRow`'s `null` — which means either a non-finite score or a skipped `None` — is the
    /// ambiguity this document refuses to inherit.
    #[serde(serialize_with = "ser_score", deserialize_with = "de_score")]
    pub score: f64,
    /// The trial's `vike_analytics::report::BacktestReport`, serialized. `None` for a failed trial.
    pub metrics: Option<serde_json::Value>,
    /// The stringified `harness::HarnessError` for a failed trial — never both this and `metrics`.
    pub error: Option<String>,
}

fn ser_score<S: serde::Serializer>(v: &f64, s: S) -> Result<S::Ok, S::Error> {
    if v.is_finite() { s.serialize_f64(*v) } else { s.serialize_none() }
}

fn de_score<'de, D: serde::Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
    Ok(Option::<f64>::deserialize(d)?.unwrap_or(f64::NAN))
}

/// What [`read_trials`] answered: the records it could parse, and the lines it could not.
#[derive(Clone, Debug, Default)]
pub struct TrialsRead {
    /// The records, in the file's own order.
    pub trials: Vec<TrialRecord>,
    /// `(0-based line index, serde_json's own words)` for every line that did not parse.
    pub unreadable: Vec<(usize, String)>,
}

/// `vike_model::runs::REPORT_FILE`'s content for a run of kind [`SEARCH_RUN_KIND`] — the ledger,
/// resolved.
///
/// ⚠ A search's `report.json` is THIS and not a `harness::ParamscanReport`, on the fresh path as well
/// as the resumed one. A resumed run's rows include reused trials, and `ParamscanReport`'s `Display`
/// branches on `report: None` to print `FAILED: <error>` — so a reused row would render as a
/// failure. Persisting the `ParamscanReport` would also give searches two different `report.json`
/// shapes depending on how they finished, which is a schema a reader cannot rely on. This document
/// has one shape either way, and unlike a `ParamscanReport` it derives `Deserialize`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TrialsDocument {
    /// [`TRIAL_LEDGER_SCHEMA`] at write time.
    pub schema: u32,
    /// The parent run's id.
    pub run_id: String,
    /// `none` | `scalars` | `returns`, as [`SearchHeader::keep_trials`].
    pub keep_trials: String,
    /// What this search was. See [`SearchIdentity`].
    pub identity: SearchIdentity,
    /// Evaluations this search performed, reused ones INCLUDED — the searcher's own budget spend.
    pub evaluated: usize,
    /// How many of those came from the warm cache rather than from a backtest.
    pub reused: usize,
    /// How many trials carry an `error`.
    pub failed: usize,
    /// Ledger lines that did not parse. Non-zero is a finding, never a reason to refuse the rest.
    pub unreadable: usize,
    /// Lines whose `n` a later line replaced — see [`latest_by_n`].
    pub superseded: usize,
    /// The trials. In `n` order as PERSISTED; the `trials` verb may reorder its own copy for
    /// display, and the array ORDER is then the rank.
    pub trials: Vec<TrialRecord>,
    /// The anti-overfitting statistics of the whole search — [`OverfitStats`], `None` unless the
    /// search opted into `--keep-trials returns` AND produced a measurable matrix.
    ///
    /// ⚠ **`skip_serializing_if` is load-bearing here for the same reason it is on
    /// `harness::ParamscanReport::summary`**: every search run before this field existed, and every
    /// search that does not opt in, writes a document byte-identical to the one this module emitted
    /// without it — so the `report.json` shape the `trials` verb, `runs diff` and
    /// `crates/vike-backtest/tests/search_persist_cli.rs` all read is unchanged. `serde(default)`
    /// is the other half: a document written by an older binary still deserializes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overfit: Option<OverfitStats>,
}

/// **What a whole search's trials say about whether its winner is overfit** — PBO via CSCV, the
/// correlation-corrected effective trial count, and the deflated Sharpe benchmarked against it.
/// Computed by `harness::optimize::overfit_stats`, which is the authority for what each number is
/// computed OVER.
///
/// # ⚠ These are REPORT FIELDS, not printed output, and that was a ruling
///
/// A search does not print them and `render_trials` does not render them. They exist so that
/// `vike-cli backtest gate <run> --against <mark> --fail-if EXPR` can NAME them: that verb resolves
/// a criterion as a DOTTED LEAF PATH into this document (`crates/vike-cli/src/cmd/runs/jsondoc.rs`'s
/// `number_at`), so `overfit.pbo`, `overfit.deflated_sharpe`, `overfit.effective_n` and
/// `overfit.observed_sharpe` are gateable the moment they are written here and need no edit in that
/// verb at all. A printed line would have been one more thing to read and nothing a CI step could
/// act on.
///
/// # ⚠ The per-trial VECTORS are not persisted, and what that costs
///
/// This block is the RESIDUE of a matrix that existed only in memory. `harness::sweep`'s
/// `ReturnBuckets` retains 4 KB per trial during the search and the vectors are dropped with the
/// evaluator, so nothing can recompute a PBO at a different split count, or over a subset of the
/// trials, without re-running the search. The alternative — a `returns` array on every
/// [`TrialRecord`] — would put ~5 MB of JSON into a 500-trial `trials.jsonl` and change the LEDGER
/// schema that `--resume`'s warm cache reads, for a recomputation nobody has asked for. The
/// statistics are the answer; the matrix was the working.
///
/// # ⚠ Every statistic is `Option<f64>` and `None` means UNCOMPUTABLE
///
/// `vike_analytics::overfit::pbo_cscv` answers `NaN` for a degenerate or non-finite matrix, and
/// `overfit_verdict` reads that as "not assessed" — deliberately, because a `0.0` would read as
/// "no overfit", the exact opposite. A `null` leaf makes `backtest gate` report "carries no finite
/// number under `overfit.pbo`" rather than silently judging a zero.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OverfitStats {
    /// `T` — observations per trial in the matrix, i.e. the LENGTH of each retained column. Equal
    /// to [`Self::requested_buckets`] except on a range holding fewer per-bar returns than
    /// buckets, where `harness::sweep`'s `ReturnBuckets::capture` shortens the column rather than
    /// padding it with fabricated zeros.
    pub buckets: usize,
    /// The bucket count that was ASKED for. Carried beside [`Self::buckets`] so a reader can tell
    /// "this range was short" from "this search used a smaller setting".
    pub requested_buckets: usize,
    /// `N` — columns in the matrix, which is how many trials retained an admissible vector rather
    /// than how many the search evaluated ([`TrialsDocument::evaluated`] is that).
    pub trials: usize,
    /// Ranked rows that contributed NO column. The reachable causes are a failed point, a resume's
    /// reused trial (whose ledger answer is a score, not a report) and a curve that touched
    /// exactly zero. Non-zero beside a small [`Self::trials`] is what makes a thin matrix visible.
    pub excluded: usize,
    /// The CSCV split count — `harness::optimize::DEFAULT_CSCV_SPLITS` in practice, recorded
    /// because a PBO is only comparable to another PBO computed over the same split count.
    pub splits: usize,
    /// Probability of Backtest Overfitting, `0..1`. Above ~0.5 the winner is more likely than not
    /// curve-fit.
    pub pbo: Option<f64>,
    /// The correlation-corrected trial count `N / (1 + (N-1)·mean_corr)`, clamped to `[1, N]` —
    /// what the deflated Sharpe is benchmarked against instead of the raw `N`. Near `1` means the
    /// trials were all the same strategy wearing different numbers.
    pub effective_n: Option<f64>,
    /// Deflated Sharpe of the WINNER, `0..1`: the probability its edge is real once the number of
    /// (effective) trials is accounted for. Below ~0.5 the edge is not significant.
    pub deflated_sharpe: Option<f64>,
    /// The winner's PER-OBSERVATION (per-bucket) Sharpe — the number [`Self::deflated_sharpe`]
    /// deflates. ⚠ NOT the annualized `sharpe` on a [`TrialRecord`]'s `metrics`, and the two are
    /// not comparable: `vike_analytics::overfit::sharpe_moments` carries the argument for why an
    /// annualized value saturates the significance test to ~1.0 and makes it vacuous.
    pub observed_sharpe: Option<f64>,
    /// The `n` behind [`Self::observed_sharpe`] — the observation count the significance test used.
    pub observations: usize,
    /// `low` | `medium` | `high` — `vike_analytics::overfit::overfit_verdict`'s level over
    /// [`Self::pbo`] and [`Self::deflated_sharpe`]. A word rather than the verdict's `reasons`
    /// list, because the reasons are derivable from the two numbers already here and an array of
    /// sentences in a gateable document is leaf noise.
    pub verdict: String,
}

/// Append one trial to [`TRIALS_FILE`], creating it when absent.
///
/// ONE line, ONE `write_all`, no read of what is already there — which is what makes a 512-trial
/// search cost 512 appends rather than 512 rewrites of a growing document.
pub fn append_trial(dir: &Path, record: &TrialRecord) -> Result<(), RunPersistError> {
    use std::io::Write;
    let path = dir.join(TRIALS_FILE);
    let mut line = serde_json::to_string(record)
        .map_err(|e| RunPersistError::Serialize { file: TRIALS_FILE, why: e.to_string() })?;
    line.push('\n');
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut f| f.write_all(line.as_bytes()))
        .map_err(|e| RunPersistError::Write { path, why: e.to_string() })
}

/// Read every line of [`TRIALS_FILE`].
///
/// A line that does not parse is REPORTED and skipped, never fatal: the writer appends as the
/// search runs, so a process killed mid-write leaves a torn final line, and losing a 500-trial
/// ledger over the 501st is precisely the failure this format exists to avoid. An absent file is
/// [`RunReadError::Missing`] rather than an empty success, for the same reason
/// `vike_model::runs::read_manifest` distinguishes the two — "not written yet" and "kept none" have
/// different fixes.
pub fn read_trials(dir: &Path) -> Result<TrialsRead, RunReadError> {
    let path = dir.join(TRIALS_FILE);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(RunReadError::Missing { path });
        }
        Err(e) => return Err(RunReadError::Read { path, why: e.to_string() }),
    };
    let mut out = TrialsRead::default();
    for (i, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<TrialRecord>(line) {
            Ok(r) => out.trials.push(r),
            Err(e) => out.unreadable.push((i, e.to_string())),
        }
    }
    Ok(out)
}

/// Collapse repeated `n`s keeping the LAST line, and sort by `n`. Returns the kept records and how
/// many were superseded.
///
/// A repeat happens when a resume's warm cache MISSES a candidate the ledger already holds — the
/// trial is re-evaluated and appended again at the same evaluation index. Append order decides,
/// because the later line is the one this process actually computed.
pub fn latest_by_n(trials: Vec<TrialRecord>) -> (Vec<TrialRecord>, usize) {
    let mut by_n: std::collections::BTreeMap<usize, TrialRecord> =
        std::collections::BTreeMap::new();
    let mut superseded = 0usize;
    for t in trials {
        if by_n.insert(t.n, t).is_some() {
            superseded += 1;
        }
    }
    (by_n.into_values().collect(), superseded)
}

/// Write [`SEARCH_FILE`]. Called BEFORE the first evaluation — see that constant.
pub fn write_search_header(dir: &Path, header: &SearchHeader) -> Result<(), RunPersistError> {
    vike_model::runs::write_json(&dir.join(SEARCH_FILE), SEARCH_FILE, header)
}

/// Read [`SEARCH_FILE`] — what `--resume` proves its identity against.
pub fn read_search_header(dir: &Path) -> Result<SearchHeader, RunReadError> {
    let path = dir.join(SEARCH_FILE);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(RunReadError::Missing { path });
        }
        Err(e) => return Err(RunReadError::Read { path, why: e.to_string() }),
    };
    serde_json::from_str(&text).map_err(|e| RunReadError::Parse { path, why: e.to_string() })
}

#[path = "trial_ledger_tests.rs"]
#[cfg(test)]
mod trial_ledger_tests;
