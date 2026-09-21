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
//! # Why this module is UNGATED, and why it is in this crate rather than `vike-model`
//!
//! `crates/vike-backtest/src/lib.rs` declares `pub mod harness;` behind `#[cfg(feature =
//! "hist-replay")]`. These are DOCUMENTS, not search machinery, so they sit outside that gate: a
//! default build compiles and tests them, and a reader that wants the shapes never pays for the
//! harness tree. The RECORDER, which needs `harness::optimize::{Candidate, Evaluated}`, is the
//! half that lives under the gate — `crate::harness::trials`.
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
    /// [`DATA_UNREADABLE`] and this type's doc; `crates/vike-backtest/src/backtest_cli.rs`'s
    /// `data_witness` is what produces it and argues what it catches and what it misses.
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
    /// same dependency reason. It is a real witness only under the `vike` multicall. `None` and
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
    /// `crates/vike-backtest/src/backtest_cli.rs`'s `parse_keep_trials` carries what `returns`
    /// keeps instead and why.
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
/// ⚠ `metrics` is a [`serde_json::Value`] and NOT a `harness::BacktestReport`, deliberately — and
/// the reason CHANGED while this was being written, so it is stated as it now stands rather than as
/// the plan that produced it assumed. `vike_analytics::report::BacktestReport` gained `Deserialize`
/// with the run record, so a typed field would now COMPILE; it would still be the wrong shape.
/// `profit_factor` carries two different house sentinels (`f64::INFINITY` when there are no losing
/// trades but some profit, `0.0` when there is neither), both serialize to `null`, and
/// `de_f64_null_as_infinity` resolves that `null` to `INFINITY` — so a trial whose profit factor
/// was the `0.0` sentinel reads back as `inf`. A ledger must return what was written. A `Value`
/// writes the report exactly and reads anything back, which is all a ledger needs, and it also
/// keeps this module free of the analytics types. `crates/vike-cli/src/cmd/backtest.rs`'s `execute`
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
    /// The trial's `harness::BacktestReport`, serialized. `None` for a failed trial.
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
    write_json_doc(&dir.join(SEARCH_FILE), SEARCH_FILE, header)
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

/// Pretty JSON plus a trailing newline — the same shape `vike_model::runs`'s own `write_json`
/// writes, spelled here because that one is private to its module. ⚠ Not for [`TRIALS_FILE`], which
/// is COMPACT one-line JSON: see that constant.
fn write_json_doc<T>(path: &Path, file: &'static str, value: &T) -> Result<(), RunPersistError>
where
    T: Serialize + ?Sized,
{
    let mut json = serde_json::to_string_pretty(value)
        .map_err(|e| RunPersistError::Serialize { file, why: e.to_string() })?;
    json.push('\n');
    std::fs::write(path, json)
        .map_err(|e| RunPersistError::Write { path: path.to_path_buf(), why: e.to_string() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn an_identity() -> SearchIdentity {
        SearchIdentity {
            profile_fnv1a64: "0123456789abcdef".to_string(),
            profile_name: Some("sweep-demo".to_string()),
            store: "/proj/market_data/hist".to_string(),
            store_data: "series bar demo BTCUSDT - 1h commits 2 c-one c-two\n".to_string(),
            build: Some("abc1234".to_string()),
            method: "tpe".to_string(),
            rank_by: "sharpe".to_string(),
            seed: Some(7),
            budget: Some(64),
        }
    }

    fn a_record(n: usize, score: f64) -> TrialRecord {
        TrialRecord {
            n,
            overrides: vec![
                ("fast".to_string(), toml::Value::Integer(8)),
                ("slow".to_string(), toml::Value::Float(34.5)),
            ],
            score,
            metrics: Some(json!({ "sharpe": 1.25, "n_trades": 42 })),
            error: None,
        }
    }

    fn a_document(overfit: Option<OverfitStats>) -> TrialsDocument {
        TrialsDocument {
            schema: TRIAL_LEDGER_SCHEMA,
            run_id: "1756000000-1-0".to_string(),
            keep_trials: "scalars".to_string(),
            identity: an_identity(),
            evaluated: 2,
            reused: 0,
            failed: 0,
            unreadable: 0,
            superseded: 0,
            trials: vec![a_record(0, 1.5), a_record(1, 0.5)],
            overfit,
        }
    }

    /// ⚠ **A search that did not opt in writes the document it always wrote.** The statistics are
    /// ADDITIVE, so a reader of `report.json` — the `trials` verb, `runs diff`,
    /// `crates/vike-backtest/tests/search_persist_cli.rs` — sees no new key at all unless the
    /// matrix was actually measured. This is the `skip_serializing_if` half of the field's doc,
    /// held as a test rather than as a claim.
    #[test]
    fn a_search_without_statistics_grows_no_key() {
        let json = serde_json::to_string(&a_document(None)).unwrap();
        assert!(!json.contains("overfit"), "no key when there is nothing to report: {json}");
    }

    fn some_stats() -> OverfitStats {
        OverfitStats {
            buckets: 512,
            requested_buckets: 512,
            trials: 64,
            excluded: 2,
            splits: 16,
            pbo: Some(0.34),
            effective_n: Some(11.5),
            deflated_sharpe: Some(0.72),
            observed_sharpe: Some(0.081),
            observations: 512,
            verdict: "medium".to_string(),
        }
    }

    /// …and the `serde(default)` half: a document written before the field existed still reads.
    ///
    /// ⚠ The key is REMOVED from a document that HAD one, and the removal is asserted as a
    /// precondition. Starting from `a_document(None)` would have proved nothing — that document
    /// never carries the key, so the removal would be a no-op and the test would pass against a
    /// REQUIRED field just as happily.
    #[test]
    fn a_document_written_before_the_field_existed_still_deserializes() {
        let mut value = serde_json::to_value(a_document(Some(some_stats()))).unwrap();
        assert!(
            value.as_object_mut().expect("a JSON object").remove("overfit").is_some(),
            "precondition: there was a key to remove"
        );
        let back: TrialsDocument = serde_json::from_value(value).expect("an older document reads");
        assert!(back.overfit.is_none(), "and reads as ABSENT rather than as a zeroed block");
    }

    /// ⚠ **The gateable spellings, pinned.** `vike-cli backtest gate --fail-if` resolves a
    /// criterion as a DOTTED LEAF PATH into this document, so these four key names ARE the
    /// operator-facing surface — renaming one silently breaks every `--fail-if` naming it, with no
    /// compile error anywhere, because that verb never mentions this type.
    #[test]
    fn the_statistics_are_reachable_at_the_paths_a_gate_names() {
        let stats = some_stats();
        let doc = serde_json::to_value(a_document(Some(stats.clone()))).unwrap();
        for (path, want) in [
            ("pbo", 0.34),
            ("effective_n", 11.5),
            ("deflated_sharpe", 0.72),
            ("observed_sharpe", 0.081),
        ] {
            assert_eq!(
                doc["overfit"][path].as_f64(),
                Some(want),
                "`overfit.{path}` is what a --fail-if criterion names"
            );
        }
        assert_eq!(doc["overfit"]["verdict"], "medium");
        let back: TrialsDocument = serde_json::from_value(doc).unwrap();
        assert_eq!(back.overfit.as_ref(), Some(&stats), "and the block round-trips whole");
    }

    /// An UNCOMPUTABLE statistic is `null`, which `crates/vike-cli/src/cmd/runs/jsondoc.rs`'s
    /// `number_at` reports as "carries no finite number" — never `0.0`, which under
    /// `vike_analytics::overfit::overfit_verdict`'s thresholds would read as "no overfit".
    #[test]
    fn an_uncomputable_statistic_is_null_rather_than_zero() {
        let stats = OverfitStats {
            buckets: 16,
            requested_buckets: 512,
            trials: 1,
            excluded: 40,
            splits: 16,
            pbo: None,
            effective_n: Some(1.0),
            deflated_sharpe: None,
            observed_sharpe: None,
            observations: 16,
            verdict: "low".to_string(),
        };
        let doc = serde_json::to_value(a_document(Some(stats))).unwrap();
        assert!(doc["overfit"]["pbo"].is_null(), "null, not 0.0");
        assert!(doc["overfit"]["deflated_sharpe"].is_null());
        assert!(
            doc["overfit"].as_object().unwrap().contains_key("pbo"),
            "the KEY is still present, so a gate names a null leaf rather than a missing one"
        );
    }

    /// The ledger is APPEND-ONLY and one line per trial — the whole reason a 512-trial TPE search
    /// does not rewrite a growing document 512 times.
    #[test]
    fn trials_append_one_line_each_and_read_back_in_order() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path();

        append_trial(dir, &a_record(0, 1.5)).unwrap();
        append_trial(dir, &a_record(1, 0.5)).unwrap();
        append_trial(dir, &a_record(2, 2.5)).unwrap();

        let text = std::fs::read_to_string(dir.join(TRIALS_FILE)).unwrap();
        assert_eq!(text.lines().count(), 3, "one LINE per trial, not one document: {text:?}");
        assert!(
            !text.contains("\n  "),
            "compact JSON per line — a pretty-printed record would break the format: {text:?}"
        );

        let back = read_trials(dir).unwrap();
        assert!(back.unreadable.is_empty(), "nothing unreadable: {:?}", back.unreadable);
        assert_eq!(
            back.trials.iter().map(|t| t.n).collect::<Vec<_>>(),
            vec![0, 1, 2],
            "EVALUATION order is the file's order and is preserved on the way back"
        );
        assert_eq!(back.trials[0].overrides, a_record(0, 1.5).overrides, "toml values round-trip");
        assert_eq!(back.trials[2].score, 2.5);
    }

    /// ⚠ `null` means UNRANKABLE and nothing else. `ParamscanRow`'s `ser_opt_score` writes `null` for
    /// BOTH a non-finite score and a skipped `None`, which is the ambiguity this document must not
    /// inherit — so the field is a plain `f64` whose `null` deserializes back to `NaN`.
    #[test]
    fn an_unrankable_score_round_trips_as_null_and_comes_back_nan() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path();
        append_trial(dir, &a_record(0, f64::NAN)).unwrap();

        let text = std::fs::read_to_string(dir.join(TRIALS_FILE)).unwrap();
        assert!(text.contains("\"score\":null"), "a NaN score writes null: {text:?}");

        let back = read_trials(dir).unwrap();
        assert!(back.trials[0].score.is_nan(), "…and reads back as NaN, not as a missing field");
    }

    /// A crashed writer leaves a truncated final line. Losing the whole ledger over it is exactly
    /// the failure JSON Lines exists to avoid, so the line is REPORTED and the rest survives.
    #[test]
    fn a_truncated_final_line_costs_that_line_and_nothing_else() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path();
        append_trial(dir, &a_record(0, 1.0)).unwrap();
        append_trial(dir, &a_record(1, 2.0)).unwrap();

        let path = dir.join(TRIALS_FILE);
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str("{\"n\":2,\"overri");
        std::fs::write(&path, text).unwrap();

        let back = read_trials(dir).unwrap();
        assert_eq!(back.trials.len(), 2, "the two whole lines survive");
        assert_eq!(back.unreadable.len(), 1, "the torn one is reported, not swallowed");
        assert_eq!(back.unreadable[0].0, 2, "…by its 0-based LINE index, so it can be found");
    }

    /// A missing ledger is `Missing`, not an empty success — the same distinction `read_manifest`
    /// makes, for the same reason: "no trials yet" and "this run kept none" have different fixes.
    #[test]
    fn an_absent_ledger_is_missing_rather_than_empty() {
        let root = tempfile::tempdir().unwrap();
        match read_trials(root.path()) {
            Err(RunReadError::Missing { path }) => {
                assert!(path.ends_with(TRIALS_FILE), "names the file it looked for: {path:?}");
            }
            other => panic!("expected Missing, got {other:?}"),
        }
    }

    /// A re-evaluated trial appends a SECOND line carrying the same `n` (a resume re-runs anything
    /// its warm cache missed). Append order decides: the LAST line for an `n` is the answer.
    #[test]
    fn a_repeated_n_is_superseded_by_the_later_line() {
        let first = a_record(1, 1.0);
        let mut second = a_record(1, 9.0);
        second.metrics = Some(json!({ "sharpe": 9.0 }));
        let (kept, superseded) = latest_by_n(vec![a_record(0, 0.5), first, second]);

        assert_eq!(superseded, 1, "one line was superseded and the count says so");
        assert_eq!(kept.len(), 2);
        assert_eq!(kept.iter().map(|t| t.n).collect::<Vec<_>>(), vec![0, 1], "sorted by n");
        assert_eq!(kept[1].score, 9.0, "the LATER line wins");
    }

    /// The header is written BEFORE the search runs, which is what an interrupted run has and a
    /// manifest (written last) does not.
    #[test]
    fn a_search_header_round_trips() {
        let root = tempfile::tempdir().unwrap();
        let header = SearchHeader {
            schema: TRIAL_LEDGER_SCHEMA,
            run_id: "1756000000-1-0".to_string(),
            keep_trials: "scalars".to_string(),
            trials_file: TRIALS_FILE.to_string(),
            identity: an_identity(),
            resumed_from: None,
        };
        write_search_header(root.path(), &header).unwrap();
        let back = read_search_header(root.path()).unwrap();
        assert_eq!(back, header);
    }

    /// Every field of the identity is compared, and a difference NAMES itself — a resume that
    /// refused with a bare "the search changed" would leave an operator guessing which knob.
    #[test]
    fn an_identity_difference_names_the_field() {
        let a = an_identity();
        let mut b = an_identity();
        b.seed = Some(8);
        let diffs = a.differences(&b);
        assert_eq!(diffs.len(), 1, "one field differs: {diffs:?}");
        assert!(diffs[0].contains("seed") && diffs[0].contains('7') && diffs[0].contains('8'));
        assert!(
            a.differences(&an_identity()).is_empty(),
            "and an equal identity differs in nothing"
        );
    }

    /// ⚠ An UNREADABLE profile can never match anything, including another unreadable one. Without
    /// this, two runs whose profile could not be re-read would compare EQUAL and a resume would
    /// reuse trials from a search it cannot prove was the same.
    #[test]
    fn an_unreadable_profile_hash_never_matches() {
        let mut a = an_identity();
        let mut b = an_identity();
        a.profile_fnv1a64 = PROFILE_UNREADABLE.to_string();
        b.profile_fnv1a64 = PROFILE_UNREADABLE.to_string();
        assert!(!a.differences(&b).is_empty(), "unreadable never equals unreadable");
    }

    /// ⚠ **THE REVERT-PROOF for the data witness.** Drop `store_data` from [`SearchIdentity`] or
    /// from [`SearchIdentity::differences`] and this test goes red: two searches over the SAME
    /// profile, store PATH, method, seed and budget, differing only in what the store held, would
    /// compare EQUAL — which is the silent wrong answer this type's doc describes. The refusal must
    /// also SAY it was the data, because "the search changed" sends an operator to the wrong knob.
    #[test]
    fn a_store_whose_contents_moved_is_a_named_difference() {
        let before = an_identity();
        let mut after = an_identity();
        after.store_data.push_str("series bar demo ETHUSDT - 1h commits 1 c-three\n");

        assert_eq!(before.store, after.store, "the store PATH is identical — only the data moved");
        let diffs = before.differences(&after);
        assert_eq!(diffs.len(), 1, "exactly one field differs: {diffs:?}");
        assert!(diffs[0].starts_with("data:"), "…and it is NAMED as the data: {diffs:?}");
        assert!(
            diffs[0].contains("backfill") || diffs[0].contains("re-fetch"),
            "the message says what MOVES a store, so an operator knows what to look for: {diffs:?}"
        );
        assert!(
            before.differences(&an_identity()).is_empty(),
            "and an unchanged store is not a difference — otherwise no resume could ever happen"
        );
    }

    /// ⚠ An UNREADABLE data witness can never match anything, including another unreadable one —
    /// the same rule as [`PROFILE_UNREADABLE`] and for the same reason. A store that could not be
    /// inventoried is evidence of nothing, and two of them are not evidence of sameness.
    #[test]
    fn an_unreadable_data_witness_never_matches() {
        let mut a = an_identity();
        let mut b = an_identity();
        a.store_data = DATA_UNREADABLE.to_string();
        b.store_data = DATA_UNREADABLE.to_string();
        assert!(!a.differences(&b).is_empty(), "unreadable never equals unreadable");
        assert!(
            a.differences(&b)[0].starts_with("data:"),
            "and it is named: {:?}",
            a.differences(&b)
        );
    }

    /// The BINARY is an input to every score — a pull that changes fill logic, sizing or fees makes
    /// a cached score an answer the current engine would not give. `unset` on both sides is the
    /// standalone engine's ordinary state and must NOT be a difference; `unset` against a real
    /// commit must be.
    #[test]
    fn a_different_build_is_a_difference_and_two_unknown_builds_are_not() {
        let mut newer = an_identity();
        newer.build = Some("def5678".to_string());
        let diffs = an_identity().differences(&newer);
        assert_eq!(diffs.len(), 1, "{diffs:?}");
        assert!(diffs[0].contains("build") && diffs[0].contains("abc1234"), "{diffs:?}");

        let mut a = an_identity();
        let mut b = an_identity();
        a.build = None;
        b.build = None;
        assert!(
            a.differences(&b).is_empty(),
            "a binary that cannot name its build contributes NO witness — making that a refusal \
             would make the standalone engine unable to resume at all"
        );
        assert!(!a.differences(&an_identity()).is_empty(), "…but unset against a commit differs");
    }

    /// The hash is a value, so it is pinned as one. FNV-1a 64 over the empty input is its offset
    /// basis, and one byte of change moves it.
    #[test]
    fn the_profile_hash_is_stable_and_sensitive() {
        assert_eq!(fnv1a64_hex(b""), "cbf29ce484222325", "the FNV-1a 64 offset basis");
        assert_ne!(fnv1a64_hex(b"cash = 10000.0"), fnv1a64_hex(b"cash = 10001.0"));
        assert_eq!(fnv1a64_hex(b"abc"), fnv1a64_hex(b"abc"), "and it is a function of the bytes");
    }

    /// ⚠ The two file names this module adds to a run directory must not collide with the ones
    /// `vike_model::runs` writes — that module publishes `RESERVED_FILES` so a producer with
    /// artifacts of its own can check against ONE roster rather than a list it maintains itself,
    /// and a search parent is exactly such a producer.
    #[test]
    fn the_search_documents_do_not_collide_with_the_run_records_own_files() {
        for name in [TRIALS_FILE, SEARCH_FILE] {
            assert!(
                !vike_model::runs::RESERVED_FILES.contains(&name),
                "{name} collides with a file the run record itself writes"
            );
        }
    }
}
