//! What a run LEAVES BEHIND: one directory under `<project>/user_data/runs/`, holding a manifest
//! every producer writes the same way beside a report only that producer's kind understands.
//!
//! # Why a run has to leave anything behind
//!
//! A backtest that only PRINTS has no history. There is nothing to list, nothing to compare a
//! second run against, and nothing a UI could ever show — the result exists for as long as the
//! terminal scrollback does. `<project>/user_data/runs/` (`crate::paths::state_path::RUNS_SUBDIR`)
//! is where that stops being true.
//!
//! # Why the manifest is the load-bearing half
//!
//! **A listing must be able to render a row without a parser per kind.** A backtest run and a
//! research run differ in what their results CONTAIN, not in what a listing needs to say about
//! them — an id, when it ran, what produced it, which config drove it. So those fields sit at the
//! TOP LEVEL of [`RunManifest`](crate::runs::RunManifest), identical for every producer, and everything a particular kind of
//! run wants to record nests BELOW them in [`RunManifest::detail`](crate::runs::RunManifest::detail). A kind-specific field placed
//! beside them rather than under them is the defect this shape exists to refuse: the moment one
//! kind adds a top-level field of its own, every reader must know which kind it is holding before
//! it can read the first key, and "one common manifest" has become two manifests sharing a
//! filename.
//!
//! # The report is written FIRST and the manifest LAST
//!
//! The report is the irreplaceable half — it is what the run computed — so a disk that fills
//! between the two writes should cost the metadata rather than the result. Ordering them this way
//! also hands a reader a completion marker it never had to be told about: a directory holding a
//! [`MANIFEST_FILE`](crate::runs::MANIFEST_FILE) is a run that finished writing, so a listing can skip a half-written run
//! without a lock file, a temp name or a rename dance.
//!
//! # The runs directory is a PARAMETER, and so is the CLOCK
//!
//! Nothing here resolves a path. The BINARY calls `crate::paths::state_path::user_runs_dir` and hands
//! the answer down, because a library that resolves its own project root reads global state its
//! caller can neither see nor override — the rule `crates/vike-ops/tests/settings/settings_registry.rs`'s
//! `LIBRARY_PIN` ratchets down, and the reason ONE walk decides which project a process is in.
//!
//! Nothing here reads the clock either, for the stronger reason
//! `crates/vike-ops/tests/hygiene/clock_pin.rs`'s `CLOCK_PIN` states: in the crates that carry
//! `backtest == paper == live`, TIME IS AN INPUT, and this crate contains no ambient clock read at
//! all. So [`create_run_dir`](crate::runs::create_run_dir) takes the run's start SECOND and
//! [`utc_rfc3339`](crate::runs::utc_rfc3339) takes any second —
//! which is also what lets a test mint an id against a fixed instant instead of racing one.
//!
//! # One document is written AFTER the completion marker, and only one
//!
//! [`META_FILE`](crate::runs::META_FILE) — the tag sidecar — is written by
//! [`add_tags`](crate::runs::add_tags) whenever somebody labels a run,
//! which is by definition after the run finished. That does not break the rule above, and the
//! reason is the direction of the implication: a directory holding a manifest is a finished run,
//! and the sidecar is OPTIONAL by construction (absent means "no tags", which is every run minted
//! before tagging existed). A document a reader must have would have to go before the marker; one
//! whose absence is an ordinary answer may go after it.
//!
//! The MARK store is the other half of the same feature and is deliberately NOT in a run directory
//! at all: a mark is a NAME that points at a run, so it lives in a file named for the mark under
//! `crate::paths::state_path::MARKS_SUBDIR`, a SIBLING of the runs root. [`Mark`](crate::runs::Mark) and that constant carry
//! the argument.
//!
//! # Persisting is additive, never a new way to fail
//!
//! Every function here returns its failure as a value. A caller prints its result FIRST and
//! persists afterwards, so a run whose directory cannot be created still hands the operator the
//! numbers it computed, with the failure named on stderr rather than swallowed. Saving a run is
//! worth doing; it is not worth losing a run over.
//!
//! # Where this module lives, and why it came down here
//!
//! It was born in `vike-backtest`, beside the first producer, and its own doc named the condition
//! that would move it: a consumer that may not depend on that crate. The CONSUMER arrived rather
//! than the producer — `vike-cli` reads a run directory for `backtest ls`/`show`/`path`, and a
//! normal `vike-backtest` edge would have dragged `vike-exec` (tokio `rt-multi-thread`,
//! `core_affinity`, `ustr`) into a binary whose whole identity is being light and DataFusion-free,
//! which `crates/vike-cli/Cargo.toml`'s `vike-model` rationale refuses BY NAME.
//!
//! So the module came down WHOLE, as that doc required: a move, never a copy, and no `pub use` shim
//! behind it — two definitions of a common manifest is the one outcome this shape cannot survive.
//! The producers call it from here (`crates/vike-backtest/src/backtest_cli.rs`'s `persist_run`,
//! `crates/vike-studio-core/src/study_run.rs`'s `run_study`) and so do both readers
//! (`crates/vike-studio-core/src/listing.rs`'s `list_runs`,
//! `crates/vike-cli/src/cmd/runs/scan.rs`'s `scan_runs`).
//!
//! ⚠ **One thing changed in the move and it is worth knowing:** [`utc_rfc3339`](crate::runs::utc_rfc3339) no longer calls
//! `chrono`. It calls [`crate::time::civil_from_days`], which is exact over the same range, and
//! `the_chrono_free_formatter_is_byte_identical_to_the_one_it_replaced` pins the two equal on
//! instants measured from the implementation this replaced.

mod manifest;
mod marks;
mod meta;
mod read;
mod write;

pub use manifest::{
    BuildStamp, DroppedOrder, RunConfig, RunDiagnostics, RunDir, RunExtras, RunManifest, RunSeries,
    RunTrades,
};
pub use marks::{
    MARK_HISTORY_MAX, Mark, MarkError, MarkMove, read_mark, valid_mark_name, write_mark,
};
pub use meta::{META_SCHEMA, RunMeta, RunNote, add_tags, read_meta};
pub use read::{RunReadError, read_config_toml, read_manifest, read_series, read_trades};
pub use write::{
    RunPersistError, create_run_dir, run_id_at, utc_rfc3339, write_json, write_run, write_run_with,
};

/// The manifest's file name inside a run directory — also the completion marker, for the reason
/// this module's doc gives.
pub const MANIFEST_FILE: &str = "manifest.json";

/// The report's file name inside a run directory. Its CONTENT is whatever the producer's own report
/// type serializes to; only the NAME is common.
pub const REPORT_FILE: &str = "report.json";

/// The equity SERIES file inside a run directory — the half [`REPORT_FILE`]'s ten scalars are
/// derived FROM and which, until this document existed, was computed and dropped.
pub const SERIES_FILE: &str = "series.json";

/// The closed-trade LEDGER file inside a run directory.
pub const TRADES_FILE: &str = "trades.json";

/// The RESOLVED CONFIG the run was driven by, verbatim as TEXT rather than as a re-serialized
/// struct. `crates/vike-backtest/src/harness/profile.rs`'s `BacktestProfile` and its nine nested
/// config types derive `Deserialize` ONLY — there is no `Serialize` anywhere in that tree, and
/// `vike_exec::ProfileRisk` is in another crate again — so "the resolved config" as a struct would
/// be eleven new derives across two crates for a document the text already answers exactly. The
/// text is also what already crosses the wire (`crates/vike-backtest/src/compute_server.rs` ships
/// `profile_toml: String`), so persisting it keeps ONE spelling of "what drove this run".
pub const CONFIG_FILE: &str = "config.toml";

/// The per-run TAG SIDECAR's file name — labels and notes a person attached to a run AFTER it
/// finished.
///
/// ⚠ **OPTIONAL by construction, and it must stay that way.** An absent file is a run with no tags,
/// which is every run minted before tagging existed. That is exactly what lets it be written AFTER
/// [`MANIFEST_FILE`] — the completion marker — without breaking the rule that a directory holding a
/// manifest is a finished run.
///
/// ⚠ **It is written by [`add_tags`] rather than by [`write_run_with`]**, and it is the only member
/// of [`RESERVED_FILES`] that is. A run directory is a namespace several producers write into, so
/// the roster is "every name THIS MODULE writes" and not "every name the run WRITER writes"; a
/// study artifact called `meta.json` would otherwise silently overwrite a user's tags.
pub const META_FILE: &str = "meta.json";

/// Every file name this module writes, so a producer that also writes artifacts of its own can
/// refuse a collision against ONE roster rather than against a list it maintains itself —
/// `crates/vike-studio-core/src/study_run.rs`'s `persist` is that producer, and it checked two
/// names when there were two.
///
/// ⚠ **"Writes" means this MODULE, not [`write_run_with`] alone.** [`META_FILE`] is written by
/// [`add_tags`], long after the run finished, and it is reserved for exactly the same reason the
/// other five are.
pub const RESERVED_FILES: &[&str] =
    &[MANIFEST_FILE, REPORT_FILE, SERIES_FILE, TRADES_FILE, CONFIG_FILE, META_FILE];

/// The version of [`SERIES_FILE`]'s shape.
///
/// ⚠ **A version here is not the contradiction it looks like.** `crates/vike-backtest/src/binutil.rs`'s
/// `stats_provenance` argues at length AGAINST a schema version, and the argument is correct for
/// the documents it is about: "this tree does version a payload where a DECODER has to know how to
/// read bytes it did not write… Nothing decodes THESE documents", whose readers are a human and an
/// ad-hoc `jq` filter. A run directory is the OTHER case by that test's own terms —
/// [`read_manifest`], [`read_series`] and [`read_trades`] are real decoders,
/// `crates/vike-studio-core/src/listing.rs`'s `list_runs` is a real reader of bytes it did not
/// write, and a document missing a common field already fails to parse. So that argument does not
/// apply, and the version ships WITH the document rather than after it, because a schema tag
/// retrofitted onto documents already in people's scripts gets strictly more expensive every week.
pub const SERIES_SCHEMA: u32 = 1;

/// The version of [`TRADES_FILE`]'s shape. See [`SERIES_SCHEMA`] for why these exist at all.
pub const TRADES_SCHEMA: u32 = 1;

/// The version of [`MANIFEST_FILE`]'s shape — [`SERIES_SCHEMA`] carries the argument for versioning
/// these documents at all, and for why `crates/vike-backtest/src/binutil.rs`'s opposite conclusion
/// about the stats documents does not reach here.
///
/// `0` is not a version this build ever writes: it is what [`RunManifest::schema`] reads as in a
/// document written BEFORE the field existed, which is a real state and a reportable one.
pub const MANIFEST_SCHEMA: u32 = 1;

/// The [`RunManifest::kind`] a backtest run carries.
///
/// Exported because it was already being spelled twice: `crates/vike-studio/src/panes/research.rs`
/// carried its own copy with a comment saying this is what should replace it, and
/// `crates/vike-studio-core/src/study_run.rs` already exports the study twin as `STUDY_RUN_KIND`.
/// That twin keeps its spelling: the CLI's top-level `study` verb moves under a `research` plane,
/// and the BACKEND's names — this constant's neighbour included — are not renamed with it.
///
/// ⚠ **`kind` is the WHAT axis, and a WALK-FORWARD is not a third value of it.** A walk-forward and
/// a study both split into train/test segments and fit on the train half; what differs is WHAT is
/// fitted — strategy PARAMETERS, measured as out-of-sample PnL, versus a MODEL, measured as signal
/// quality. So the taxonomy is two axes, WHAT (`backtest` | `study`) x HOW VALIDATED (one slice |
/// walked forward), and HOW VALIDATED rides in the run's own config where a reader can see the
/// windows, never in this field. A producer that wrote `"walkforward"` here would be readable
/// nowhere: `crates/vike-studio/src/panes/research.rs`'s `kind_status` draws an unrecognised kind in the
/// warning status under its own name, deliberately, and a later comparison verb would treat a
/// walked-forward backtest as a different species from the single-slice backtest it exists to be
/// compared against.
pub const BACKTEST_RUN_KIND: &str = "backtest";

/// How many ids [`create_run_dir`] tries before giving up. A bound rather than an unbounded loop,
/// because a directory tree that reports `AlreadyExists` without ever yielding a free name would
/// otherwise spin forever; a thousand runs started by ONE process inside ONE second is far past
/// anything a backtest binary can do.
const MINT_ATTEMPTS: u32 = 1_000;

/// How many characters of an input address ride in the run id. Sixteen hex characters is 64 bits —
/// enough that two DIFFERENT input sets sharing a prefix is not something anyone will meet, and
/// short enough that the whole id still fits a terminal column. The authority for "same inputs" is
/// [`RunManifest::fingerprint`], which carries the full digest; this is the legible prefix of it.
pub const ID_FINGERPRINT_LEN: usize = 16;

/// The most equity samples a run record keeps.
///
/// ⚠ **A cap is not a preference here, it is the only available shape.**
/// `crates/vike-sim/src/engine/sim_broker.rs`'s `EquitySampling` carries the MEASURED
/// in-memory figure — the curve grows by 16 bytes per priced tick, so a 100M-tick tape is 1.6 GB
/// resident per run — and `crates/vike-backtest/src/harness/sweep.rs`'s `DEFAULT_SWEEP_THREADS` is
/// small BECAUSE that figure multiplies. Written as JSON the same curve is roughly 3 GB, on a box
/// that also hosts the live daemon. That knob is TICK-LANE ONLY
/// (`crates/vike-backtest/src/harness/profile.rs`'s `BacktestProfile::validate` refuses it on a bar
/// profile), so it bounds neither a bar run's memory nor anybody's disk.
///
/// 20,000 samples is roughly 600 KB of JSON, so a thousand stored runs cost about 600 MB — a bound
/// an operator can reason about — and it is more points than any screen has pixels, so a thinned
/// curve renders identically to the full one at every practical zoom.
///
/// ⚠ **The DECLARED RESIDUAL:** a max-drawdown recomputed from a thinned curve can be SHALLOWER
/// than the one in [`REPORT_FILE`], because the trough may fall between two kept samples.
/// `report.json` is the authority for every scalar; the series is for SHAPE and for re-rendering.
/// [`decimate`] keeps the LAST sample unconditionally so the two can never disagree about where the
/// run ENDED, which is the comparison a reader actually makes.
pub const MAX_EQUITY_SAMPLES: usize = 20_000;

/// The most closed trades a run record keeps, as a CHRONOLOGICAL PREFIX rather than a sample.
///
/// A ledger cannot be thinned: a sampled trade list is not a smaller ledger, it is a wrong one —
/// win rate, profit factor and every per-trade statistic computed from it would be fiction. So the
/// bound is a prefix and [`RunTrades::source_len`] records what the run actually closed, which is
/// how a reader knows it is holding part of a ledger rather than all of a short one.
pub const MAX_TRADES: usize = 50_000;

/// Keep at most `cap` of `samples`, evenly strided, ALWAYS keeping the first and the LAST.
///
/// Returns the kept samples and the STRIDE used. `1` means nothing was dropped — a reader keys on
/// that to know whether a statistic recomputed from the result is exact. `0` means nothing was
/// kept (`cap == 0`), which is a different answer from an empty run and must not be confused with
/// one.
///
/// ⚠ The cap is SOFT BY EXACTLY ONE: the final sample is appended when the stride does not land on
/// it, because it is the run's outcome ([`MAX_EQUITY_SAMPLES`] carries the argument).
pub fn decimate<T: Copy>(samples: &[T], cap: usize) -> (Vec<T>, usize) {
    if cap == 0 {
        return (Vec::new(), 0);
    }
    if samples.len() <= cap {
        return (samples.to_vec(), 1);
    }
    // The smallest stride that brings the count to `cap` or under, computed in integers so two
    // boxes cannot answer differently: ceil(len / cap).
    let stride = samples.len().div_ceil(cap);
    let mut kept: Vec<T> = samples.iter().step_by(stride).copied().collect();
    let last = samples.len() - 1;
    // ⚠ `is_multiple_of` rather than `%`: clippy 1.97's `manual_is_multiple_of` refuses the modulo
    // spelling at `-D warnings`, which is the merge gate.
    if !last.is_multiple_of(stride) {
        kept.push(samples[last]);
    }
    (kept, stride)
}

#[cfg(test)]
mod tests;
