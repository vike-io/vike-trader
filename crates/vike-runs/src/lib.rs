//! What a run LEAVES BEHIND: one directory under `<project>/user_data/runs/`, holding a manifest
//! every producer writes the same way beside a report only that producer's kind understands. The
//! crate mints the run id, writes and reads the documents, keeps the tag sidecar and the mark
//! store, and owns the one retention rule that removes saved runs ([`RunRetention`], opt-in).
//!
//! # Why a run has to leave anything behind
//!
//! A backtest that only PRINTS has no history. There is nothing to list, nothing to compare a
//! second run against, and nothing a UI could ever show — the result exists for as long as the
//! terminal scrollback does. `<project>/user_data/runs/` (`vike_paths::state_path::RUNS_SUBDIR`)
//! is where that stops being true.
//!
//! # Why the manifest is the load-bearing half
//!
//! **A listing must be able to render a row without a parser per kind.** A backtest run and a
//! research run differ in what their results CONTAIN, not in what a listing needs to say about
//! them — an id, when it ran, what produced it, which config drove it. So those fields sit at the
//! TOP LEVEL of [`RunManifest`], identical for every producer, and everything a particular kind of
//! run wants to record nests BELOW them in [`RunManifest::detail`]. A kind-specific field placed
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
//! [`MANIFEST_FILE`] is a run that finished writing, so a listing can skip a half-written run
//! without a lock file, a temp name or a rename dance.
//!
//! # The runs directory is a PARAMETER, and so is the CLOCK
//!
//! Nothing here resolves a path. The BINARY calls `vike_paths::state_path::user_runs_dir` and hands
//! the answer down, because a library that resolves its own project root reads global state its
//! caller can neither see nor override — the rule `crates/vike-ops/tests/settings_secrets/settings_registry.rs`'s
//! `LIBRARY_PIN` ratchets down, and the reason ONE walk decides which project a process is in.
//!
//! Nothing here reads the clock either, for the stronger reason
//! `crates/vike-ops/tests/architecture/clock_pin.rs`'s `CLOCK_PIN` states: in the crates that carry
//! `backtest == paper == live`, TIME IS AN INPUT, and this crate contains no ambient clock read at
//! all. So [`create_run_dir`] takes the run's start SECOND and
//! [`utc_rfc3339`](vike_model::time::utc_rfc3339) takes any second —
//! which is also what lets a test mint an id against a fixed instant instead of racing one.
//!
//! # Two documents are written AFTER the completion marker, and only two
//!
//! [`META_FILE`] — the tag sidecar — is written by
//! [`add_tags`] whenever somebody labels a run,
//! which is by definition after the run finished. That does not break the rule above, and the
//! reason is the direction of the implication: a directory holding a manifest is a finished run,
//! and the sidecar is OPTIONAL by construction (absent means "no tags", which is every run minted
//! before tagging existed). A document a reader must have would have to go before the marker; one
//! whose absence is an ordinary answer may go after it.
//!
//! [`FORWARD_END_FILE`] is the second, by the same argument: a FORWARD run ([`FORWARD_RUN_KIND`]) is
//! written when its mount opens, and [`end_forward_run`] adds the end when the mount closes; its
//! absence means the forward test is still running (or its node stopped without an unmount).
//!
//! The MARK store is the other half of the same feature and is deliberately NOT in a run directory
//! at all: a mark is a NAME that points at a run, so it lives in a file named for the mark under
//! `vike_paths::state_path::MARKS_SUBDIR`, a SIBLING of the runs root. [`Mark`] and that constant carry
//! the argument.
//!
//! # Persisting is additive, never a new way to fail
//!
//! Every function here returns its failure as a value. A caller prints its result FIRST and
//! persists afterwards, so a run whose directory cannot be created still hands the operator the
//! numbers it computed, with the failure named on stderr rather than swallowed. Saving a run is
//! worth doing; it is not worth losing a run over.
//!
//! # Where this crate lives, and why so low
//!
//! It is the crate `vike-runs`, at layer 15 (a leaf naming `vike-model`, for the trades document's
//! `Trade` and the one time formatter, and `vike-utility`, for the atomic writer), so a consumer that
//! may not link the engine can still read a run: `vike-cli` reads a run directory for
//! `backtest ls`/`show`/`path`, and a normal `vike-backtest` edge would drag `vike-execution` (tokio
//! `rt-multi-thread`, `core_affinity`) into a binary whose whole identity is being light and
//! DataFusion-free, which `crates/vike-cli/Cargo.toml`'s `vike-model` rationale refuses BY NAME.
//! (History: born in `vike-backtest` beside the first producer, it moved down whole to `vike-model`'s
//! `runs` module for that consumer, and out of `vike-model` into this crate on 2026-10-12, decision
//! 0149 — each time a move, never a copy, with no `pub use` shim behind it: two definitions of a
//! common manifest is the one outcome this shape cannot survive.)
//! The producers call it from here (`crates/vike-backtesthub/src/backtest_cli.rs`'s `persist_run`,
//! `crates/vike-study-runner/src/study_run.rs`'s `run_study_with`), and both readers read through the ONE
//! scan here, `crates/vike-runs/src/scan.rs`'s `scan_runs`: `vike-cli`'s `runs` verbs directly, and
//! the Studio's results surface (`crates/vike-study-runner/src/listing.rs`'s `list_runs`) on top of
//! it. Why the study RUNNER lives above this crate rather than in it is
//! `docs/decisions/0031-the-study-runner-lives-above-run-persistence.md`.
//!
//! ⚠ **One thing changed in the move out of `vike-backtest`, and it is worth knowing:**
//! [`utc_rfc3339`](vike_model::time::utc_rfc3339) no longer calls `chrono`. It calls [`vike_utility::calendar::civil_from_days`], which is exact over the same range, and
//! `the_chrono_free_formatter_is_byte_identical_to_the_one_it_replaced` pins the two equal on
//! instants measured from the implementation this replaced.
//!
//! Layout and the traps a change here meets: `crates/vike-runs/CLAUDE.md`.

mod forward;
mod manifest;
mod marks;
mod meta;
mod read;
mod retention;
mod scan;
mod write;

pub use forward::{
    FORWARD_RUN_KIND, FORWARD_RUN_SCHEMA, ForwardRun, ForwardRunEnd, end_forward_run,
    read_forward_run, write_forward_run,
};
pub use manifest::{
    BuildStamp, RunConfig, RunDiagnostics, RunDir, RunEquityCurve, RunExtras, RunForecasts,
    RunManifest, RunSeries, RunTrades,
};
pub use marks::{
    MARK_HISTORY_MAX, Mark, MarkError, MarkMove, read_mark, valid_mark_name, write_mark,
};
pub use meta::{META_SCHEMA, RunMeta, RunNote, add_tags, read_meta};
pub use read::{
    RunReadError, finished_run_dir, read_config_toml, read_day_stamps, read_equity_curve,
    read_equity_log, read_forecasts, read_kept_answer, read_manifest, read_model_text, read_series,
    read_trades,
};
pub use retention::{RunRetention, remove_run, remove_unfinished_run};
pub use scan::{RunScan, ScanDiagnostic, ScannedRun, scan_runs, scan_runs_common};
pub use write::{
    RunPersistError, create_run_dir, run_id_at, write_json, write_run, write_run_with,
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

/// The FORECASTS file inside a run directory: what the run's strategy published beside its orders
/// (book W35, [`RunForecasts`]). Written only by a run that published one.
pub const FORECASTS_FILE: &str = "forecasts.json";

/// The WHOLE equity curve of a run the backtesthub answered as a summary ([`RunEquityCurve`], decision
/// 0078's amendment of 2026-11-18): every step, never thinned, where [`SERIES_FILE`] keeps a sample.
/// Binary, 16 bytes a step. Written only by a run that keeps one.
pub const EQUITY_CURVE_FILE: &str = "equity_curve.bin";

/// The EVENT LOG a summarized single BAR run keeps in place of [`EQUITY_CURVE_FILE`] (decision 0078's amendment of
/// 2026-11-19): every event that moved its cash or a position, from which the backtesthub rebuilds the whole curve on
/// demand over the stored bars, through the simulator's own fold. Binary and opaque here: its bytes are
/// `vike_simulator::encode_ledger_events`', what the rebuild needs beside them is the manifest's
/// `detail.equity_log` (`crates/vike-backtest/src/run/equity_log.rs`'s `EquityLogRecipe`). A run keeps one of the
/// two files, never both; a run saved before the log existed keeps its curve, and reads as it always did.
pub const EQUITY_LOG_FILE: &str = "equity_log.bin";

/// The DAY STAMPS of the bars a run's [`EQUITY_LOG_FILE`] rebuilds over (decision 0078's amendment of 2026-11-19, the
/// owner's yes to binary files read on every request): the datahub's stamp of every UTC day of each series, which the
/// manifest held inline (about 140 bytes a day, 145 kB for three years) and every scan of the runs root parsed. Binary
/// and opaque here, as the log is: `crates/vike-backtesthub/src/backtesthub_server/saved_run.rs`'s `day_stamps_apart`
/// writes it and the manifest records its hash and count. Only a curve rebuild reads it. A run saved with its stamps
/// inline (before this file) keeps them there, and reads as it always did.
pub const DAY_STAMPS_FILE: &str = "day_stamps.bin";

/// What a backtest ANSWERED and what it READ, kept so an IDENTICAL request is answered from this run (decision 0078's
/// amendment of 2026-11-20): the answer as its client got it (without the names of the run it was held and saved as),
/// what a summarized run holds for its later reads, and every history read its sandboxed child made with that read's
/// witness (the store's day stamps of the read, or a hash of the answer it got: no market data). Binary and opaque
/// here: `crates/vike-backtesthub/src/backtesthub_server/answer_from_saved.rs`'s `KeptAnswer` is its shape, and the
/// manifest's `detail` records its input key, hash and length. Written only by a run that may answer again; a run
/// saved before the file existed keeps none and never answers again.
pub const KEPT_ANSWER_FILE: &str = "kept_answer.msgpack";

/// The RESOLVED CONFIG the run was driven by, verbatim as TEXT rather than as a re-serialized
/// struct. `crates/vike-backtest/src/run/profile.rs`'s `ProfileBacktest` and its nine nested
/// config types derive `Deserialize` ONLY — there is no `Serialize` anywhere in that tree, and
/// `vike_model::ProfileRisk` is in another crate again — so "the resolved config" as a struct would
/// be eleven new derives across two crates for a document the text already answers exactly. The
/// text is also what already crosses the wire (`crates/vike-backtesthub/src/backtesthub_server.rs` ships
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

/// The END of a FORWARD run ([`ForwardRunEnd`]), written by [`end_forward_run`] when its mount
/// closes, AFTER the completion marker: optional by construction, as [`META_FILE`] is (this module's
/// doc).
pub const FORWARD_END_FILE: &str = "forward_end.json";

/// The name a study gives the model it fitted (`StudyOutcome::artifact("model.txt", …)`): the file
/// [`read_model_text`] reads for a strategy file's `type = "model"` key. NOT in [`RESERVED_FILES`]: this
/// crate never writes it, a study does.
pub const MODEL_ARTIFACT: &str = "model.txt";

/// The name of a trained NEURAL NETWORK in a run: the ONNX model's bytes, as any ONNX tool writes and
/// reads them (W6). [`read_model_text`] hands it to a strategy file as the base64 of those bytes, which
/// `vike_ml_onnx_prediction::OnnxModel::from_model_text` decodes. A run holds ONE model: this or
/// [`MODEL_ARTIFACT`], never both. NOT in [`RESERVED_FILES`], for [`MODEL_ARTIFACT`]'s reason.
pub const MODEL_ONNX_ARTIFACT: &str = "model.onnx";

/// Every file name this module writes, so a producer that also writes artifacts of its own can
/// refuse a collision against ONE roster rather than against a list it maintains itself —
/// `crates/vike-study-runner/src/study_run.rs`'s `persist` is that producer, and it checked two
/// names when there were two.
///
/// ⚠ **"Writes" means this MODULE, not [`write_run_with`] alone.** [`META_FILE`] is written by
/// [`add_tags`], long after the run finished, and [`FORWARD_END_FILE`] by [`end_forward_run`] when a
/// forward run's mount closes; both are reserved for exactly the same reason the others are.
pub const RESERVED_FILES: &[&str] = &[
    MANIFEST_FILE,
    REPORT_FILE,
    SERIES_FILE,
    TRADES_FILE,
    FORECASTS_FILE,
    EQUITY_CURVE_FILE,
    EQUITY_LOG_FILE,
    DAY_STAMPS_FILE,
    KEPT_ANSWER_FILE,
    CONFIG_FILE,
    META_FILE,
    FORWARD_END_FILE,
];

/// The version of [`SERIES_FILE`]'s shape.
///
/// ⚠ **A version here is not the contradiction it looks like.** `crates/vike-backtesthub/src/binutil.rs`'s
/// `stats_provenance` argues at length AGAINST a schema version, and the argument is correct for
/// the documents it is about: "this tree does version a payload where a DECODER has to know how to
/// read bytes it did not write… Nothing decodes THESE documents", whose readers are a human and an
/// ad-hoc `jq` filter. A run directory is the OTHER case by that test's own terms —
/// [`read_manifest`], [`read_series`] and [`read_trades`] are real decoders,
/// `crates/vike-runs/src/scan.rs`'s `scan_runs` is a real reader of bytes it did not
/// write, and a document missing a common field already fails to parse. So that argument does not
/// apply, and the version ships WITH the document rather than after it, because a schema tag
/// retrofitted onto documents already in people's scripts gets strictly more expensive every week.
pub const SERIES_SCHEMA: u32 = 1;

/// The version of [`TRADES_FILE`]'s shape. See [`SERIES_SCHEMA`] for why these exist at all.
pub const TRADES_SCHEMA: u32 = 1;

/// The version of [`FORECASTS_FILE`]'s shape. See [`SERIES_SCHEMA`] for why these exist at all.
pub const FORECASTS_SCHEMA: u32 = 1;

/// The version of [`MANIFEST_FILE`]'s shape — [`SERIES_SCHEMA`] carries the argument for versioning
/// these documents at all, and for why `crates/vike-backtesthub/src/binutil.rs`'s opposite conclusion
/// about the stats documents does not reach here.
///
/// `0` is not a version this build ever writes: it is what [`RunManifest::schema`] reads as in a
/// document written BEFORE the field existed, which is a real state and a reportable one.
pub const MANIFEST_SCHEMA: u32 = 1;

/// The [`RunManifest::kind`] a backtest run carries.
///
/// Exported because it was already being spelled twice: `crates/vike-studio/src/panes/research/mod.rs`
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
/// nowhere: `crates/vike-studio/src/panes/research/mod.rs`'s `kind_status` draws an unrecognised kind in the
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
/// `crates/vike-simulator/src/engine/sim_broker/knobs.rs`'s `EquitySampling` carries the MEASURED
/// in-memory figure — the curve grows by 16 bytes per priced tick, so a 100M-tick tape is 1.6 GB
/// resident per run — and `crates/vike-backtest/src/search/paramscan/threads.rs`'s
/// `DEFAULT_PARAMSCAN_THREADS` is small BECAUSE that figure multiplies. Written as JSON the same curve
/// is roughly 3 GB, on a box that also hosts the live daemon. That knob is TICK-LANE ONLY
/// (`crates/vike-backtest/src/run/profile/validate.rs`'s `ProfileBacktest::validate` refuses it
/// on a bar profile), so it bounds neither a bar run's memory nor anybody's disk.
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

/// The most forecasts a run record keeps, as a CHRONOLOGICAL PREFIX with [`RunForecasts::source_len`]
/// beside it, for [`MAX_TRADES`]' reason: a forecast series thinned by a stride would pair each kept
/// forecast with returns it was never measured against, so the bound cuts, and says it cut. One
/// forecast a bar of a year of minute bars is about half a million rows; this keeps a year of one
/// symbol's 5-minute bars, about 15 MB of JSON.
pub const MAX_FORECASTS: usize = 120_000;

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
