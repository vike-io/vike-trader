//! What a run LEAVES BEHIND: one directory under `<project>/user_data/runs/`, holding a manifest
//! every producer writes the same way beside a report only that producer's kind understands.
//!
//! # Why a run has to leave anything behind
//!
//! A backtest that only PRINTS has no history. There is nothing to list, nothing to compare a
//! second run against, and nothing a UI could ever show — the result exists for as long as the
//! terminal scrollback does. `<project>/user_data/runs/` (`crate::state_path::RUNS_SUBDIR`)
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
//! Nothing here resolves a path. The BINARY calls `crate::state_path::user_runs_dir` and hands
//! the answer down, because a library that resolves its own project root reads global state its
//! caller can neither see nor override — the rule `crates/vike-ops/tests/settings_registry.rs`'s
//! `LIBRARY_PIN` ratchets down, and the reason ONE walk decides which project a process is in.
//!
//! Nothing here reads the clock either, for the stronger reason
//! `crates/vike-ops/tests/clock_pin.rs`'s `CLOCK_PIN` states: in the crates that carry
//! `backtest == paper == live`, TIME IS AN INPUT, and this crate contains no ambient clock read at
//! all. So [`create_run_dir`] takes the run's start SECOND and [`utc_rfc3339`] takes any second —
//! which is also what lets a test mint an id against a fixed instant instead of racing one.
//!
//! # One document is written AFTER the completion marker, and only one
//!
//! [`META_FILE`] — the tag sidecar — is written by [`add_tags`] whenever somebody labels a run,
//! which is by definition after the run finished. That does not break the rule above, and the
//! reason is the direction of the implication: a directory holding a manifest is a finished run,
//! and the sidecar is OPTIONAL by construction (absent means "no tags", which is every run minted
//! before tagging existed). A document a reader must have would have to go before the marker; one
//! whose absence is an ordinary answer may go after it.
//!
//! The MARK store is the other half of the same feature and is deliberately NOT in a run directory
//! at all: a mark is a NAME that points at a run, so it lives in a file named for the mark under
//! `crate::state_path::MARKS_SUBDIR`, a SIBLING of the runs root. [`Mark`] and that constant carry
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
//! ⚠ **One thing changed in the move and it is worth knowing:** [`utc_rfc3339`] no longer calls
//! `chrono`. It calls [`crate::time::civil_from_days`], which is exact over the same range, and
//! `the_chrono_free_formatter_is_byte_identical_to_the_one_it_replaced` pins the two equal on
//! instants measured from the implementation this replaced.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

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
/// Exported because it was already being spelled twice: `crates/vike-studio/src/research.rs`
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
/// nowhere: `crates/vike-studio/src/research.rs`'s `kind_color` renders an unrecognised kind as
/// `WARN` under its own name, deliberately, and a later comparison verb would treat a walked-forward
/// backtest as a different species from the single-slice backtest it exists to be compared against.
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
/// `crates/vike-backtest/src/engine/sim_broker.rs`'s `EquitySampling` carries the MEASURED
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

/// What the PRODUCING BINARY can say about its own build — the parameter shape that lets a library
/// record a commit it cannot resolve itself.
///
/// ⚠ **A parameter rather than a `vike-buildinfo` dependency, and that is an argued decision rather
/// than an omission.** `vike-buildinfo` resolves its facts at COMPILE time from its own build
/// script, which reruns whenever HEAD moves — so a normal dependency from this crate would rebuild
/// the simulator and everything stacked on it on every commit, with the cost landing on the inner
/// loop. `crates/vike-buildinfo/tests/identity_adoption.rs`'s `WITHOUT_IDENTITY` declares that
/// exemption and states that reason. `crates/vike-studio-core/src/study_run.rs`'s
/// `StudyRunRequest` already solves the same problem the same way.
///
/// ⚠ Shelling `git rev-parse` here instead would answer about the WORKING DIRECTORY rather than
/// about the binary — exactly the confusion `vike-buildinfo` exists to end.
///
/// Both halves are `Option` because a producer can know one and not the other, and because
/// `vike_buildinfo::GIT_SHA` is the literal `"unknown"` when it could not be resolved — a caller
/// converts that to `None` rather than writing the word into a manifest field a reader would take
/// for a commit.
#[derive(Clone, Copy, Debug, Default)]
pub struct BuildStamp<'a> {
    /// The commit, short form. Fills [`RunManifest::git_sha`] — the COMMON field a listing renders
    /// a column from.
    pub git_sha: Option<&'a str>,
    /// The whole identity line — commit, tree state, build timestamp, rustc and target. Nests under
    /// [`RunManifest::detail`], because only the sha is a question every kind answers.
    pub summary: Option<&'a str>,
}

/// Which config produced a run — the identity question every listing asks and no kind can answer
/// for another. Both halves are optional: a producer driven by flags alone has no file to name, and
/// a config file need not carry a label.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunConfig {
    /// The config FILE, spelled as the operator spelled it on the command line — deliberately NOT
    /// canonicalized. A canonical path answers about the box the run happened on; the operator's
    /// own spelling is what they can paste back to reproduce it.
    pub path: Option<String>,
    /// A short human label for that config when it carries one (a backtest profile's `name`).
    pub name: Option<String>,
}

/// One order the engine's gates DROPPED: `(symbol, reason, size, weight)`, named rather than a
/// tuple because a run record is a document a human opens.
///
/// The raw channel is `vike_analytics::result::BacktestResult::dropped`, and
/// `vike_analytics::zero_trade::aggregate_denials` is what turns it into a ranked diagnosis. Both
/// halves are worth keeping: the aggregate is what a reader reads, the rows are what an
/// investigation needs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DroppedOrder {
    /// Which instrument the order was for.
    pub symbol: String,
    /// The gate's own reason string — a `RiskGate` reason, an order-kind cash-gate drop,
    /// `"volume_cap"` or `"latency_reject"`.
    pub reason: String,
    /// The order size that was refused.
    pub size: f64,
    /// The sizer's weight for it.
    pub weight: f64,
}

/// The counters that tell a ZERO-TRADE run apart from a BROKEN one.
///
/// ⚠ These are the half a "keep the curve and the ledger" record would silently throw away, and
/// they are the half an operator actually needs: every one of them exists because the engine's
/// answer ("no trades") is identical whether the strategy never signalled, the tape never printed,
/// the venue was closed, the warm-up never completed or a gate refused every order. They are
/// already computed and already carried on the result; today they die when `run` returns.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RunDiagnostics {
    /// The warm-up requirement the run GATED on, in bars/ticks; `0` = none. The EFFECTIVE number
    /// — the engines resolve `max(configured floor, Strategy::warmup())` once and gate on that —
    /// so a profile-declared warm-up reads here as well as a strategy-declared one.
    #[serde(default)]
    pub warmup: usize,
    /// SL+TP both-hit resolutions (event engines only).
    #[serde(default)]
    pub intrabar_both_hit: u32,
    /// Market orders the stale-price wait discipline DEFERRED rather than filling against stale
    /// data. Always `0` when that knob is unset.
    #[serde(default)]
    pub stale_deferrals: u64,
    /// Fills at which a CONFIGURED impact model charged nothing because the market context it
    /// needs could not be measured from the series — the engine falls back to flat slippage, so the
    /// run comes back priced without the model and nothing says so.
    #[serde(default)]
    pub impact_unpriced: u64,
    /// Fills the session gate skipped because the venue was closed.
    #[serde(default)]
    pub session_deferrals: u64,
    /// Reversals refused for falling below the instrument's minimum size.
    #[serde(default)]
    pub below_min_reversals: u64,
    /// Every gate-dropped order, in the order the engine pushed them.
    #[serde(default)]
    pub dropped: Vec<DroppedOrder>,
    /// Order fills the engine booked as MAKER, and its taker twin below — the REALISED maker/taker
    /// mix, and the only thing on disk that can tell a fee schedule that MATTERED from one that was
    /// resolved and never touched.
    ///
    /// ⚠ It records the classification the FEE was charged under, which the engines set from the
    /// order KIND rather than from crossing aggressiveness. Reporting a second derivation would be
    /// a second opinion about a number already spent. `0` for every vector-kernel run.
    #[serde(default)]
    pub maker_fills: u64,
    /// Order fills the engine booked as TAKER — the other half of [`Self::maker_fills`].
    #[serde(default)]
    pub taker_fills: u64,
    /// TOTAL per-fill commission charged, in cash units, signed (a rebate-bearing schedule
    /// contributes negative terms). Summed from what the engine debited, never recomputed — so a
    /// reader comparing it against a claimed fee schedule is reading what the run charged.
    ///
    /// Distinct from the run report's funding figure, which is a CARRY cashflow and not a
    /// commission, and from the per-trade `fees` apportioned across closed round-trips (empty when
    /// a kernel ran with `build_trades = false`).
    #[serde(default)]
    pub fees_paid: f64,
}

/// A run's EQUITY SERIES and its diagnostic counters — [`SERIES_FILE`]'s content.
///
/// # The invariant, and why it is checkable rather than conventional
///
/// [`Self::equity`] and [`Self::equity_ts`] are parallel vectors, which is the shape that produces
/// silently-misaligned artifacts. So the document declares ONE rule and [`Self::is_aligned`] lets
/// any reader check it: `equity_ts` is either the SAME LENGTH as `equity` or EMPTY. Empty is a real
/// state — `vike_analytics::result::BacktestResult::equity_ts` is documented as "empty when not
/// tracked", which the vector kernels are — and it is a different answer from "timestamped at
/// zero".
///
/// # Per-bar returns are DERIVED, never stored
///
/// `vike_analytics::metrics::returns` SKIPS zero-denominator steps, so a returns vector is NOT
/// index-alignable with either vector above: `returns(&c).len()` is not `c.len() - 1` in general.
/// Persisting it beside timestamps of a different length would be a misaligned artifact by
/// construction, and persisting it WITH its own timestamps would be a third copy of information the
/// curve already carries exactly. A reader derives returns from [`Self::equity`] with the same
/// function the report's Sharpe was computed through, which is also the only way the two can be
/// guaranteed to agree.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RunSeries {
    /// [`SERIES_SCHEMA`] at write time; `0` in a document written before the field existed.
    #[serde(default)]
    pub schema: u32,
    /// The kept equity samples. Thinned by [`decimate`] when the run exceeded
    /// [`MAX_EQUITY_SAMPLES`] — see [`Self::stride`].
    pub equity: Vec<f64>,
    /// The timestamp of each kept sample, or EMPTY — see this type's invariant.
    #[serde(default)]
    pub equity_ts: Vec<i64>,
    /// Per-symbol cumulative PnL curves, thinned at the SAME stride so their indices still line up
    /// with [`Self::equity`]. Empty for a single-symbol or vector run.
    #[serde(default)]
    pub per_symbol_equity: Vec<(String, Vec<f64>)>,
    /// The stride [`decimate`] used. `1` means the curve is WHOLE and a statistic recomputed from
    /// it is exact; anything higher means it was thinned and a recomputed drawdown can be shallower
    /// than [`REPORT_FILE`]'s. `0` means nothing was kept.
    pub stride: usize,
    /// How many samples the run actually produced, before thinning.
    pub source_len: usize,
    /// See [`RunDiagnostics`].
    #[serde(default)]
    pub diagnostics: RunDiagnostics,
}

impl RunSeries {
    /// The document's one invariant: `equity_ts` is either the same length as `equity` or empty.
    /// A reader asserts this rather than trusting it, because two parallel vectors of different
    /// lengths are the failure mode this shape exists to make visible.
    pub fn is_aligned(&self) -> bool {
        self.equity_ts.is_empty() || self.equity_ts.len() == self.equity.len()
    }
}

/// A run's closed-trade LEDGER — [`TRADES_FILE`]'s content.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RunTrades {
    /// [`TRADES_SCHEMA`] at write time; `0` in a document written before the field existed.
    #[serde(default)]
    pub schema: u32,
    /// The kept trades, a CHRONOLOGICAL PREFIX when [`Self::source_len`] exceeds this length.
    /// Never a sample — [`MAX_TRADES`] carries the argument.
    pub trades: Vec<crate::Trade>,
    /// How many trades the run actually closed.
    pub source_len: usize,
}

/// The optional documents a producer writes BESIDE its report.
///
/// A struct rather than four parameters so [`write_run_with`] keeps the ORDERING decision — the
/// manifest LAST, always — instead of handing a producer a side door it could call afterwards.
#[derive(Default)]
pub struct RunExtras<'a> {
    /// The resolved config, verbatim. See [`CONFIG_FILE`].
    pub config_toml: Option<&'a str>,
    /// See [`RunSeries`].
    pub series: Option<&'a RunSeries>,
    /// See [`RunTrades`].
    pub trades: Option<&'a RunTrades>,
}

/// The COMMON half of a run directory: what every producer of every kind writes identically, so a
/// listing renders a row without knowing which kind it is holding.
///
/// # `run_id` cannot collide, and the FILESYSTEM is what guarantees it
///
/// The id is `<unix-seconds>-<address|pid>-<seq>` — [`run_id_at`] is the one spelling of it.
/// Seconds say WHEN, which is what a human sorting a directory wants and what keeps a plain name
/// sort chronological; `seq` breaks the rest.
///
/// The MIDDLE segment is the INPUT ADDRESS ([`Self::fingerprint`], truncated to
/// [`ID_FINGERPRINT_LEN`]) for a producer that can compute one, and the PID for a producer that
/// cannot. The address is the more useful of the two — it says at a glance which runs are
/// comparable — and the pid form is what every producer without one keeps, so the study producer
/// and every run minted before addressing existed read and sort exactly as they did.
///
/// ⚠ This paragraph said `<unix-seconds>-<pid>-<seq>` flatly until the address landed, and
/// `crates/vike-studio-core/src/listing.rs`'s module doc cites it BY NAME for the sort rule — so
/// the two are corrected together, and only the SECONDS prefix was ever load-bearing for that sort.
///
/// ⚠ **None of those parts is the guarantee.** Two processes in different pid namespaces sharing
/// one bind-mounted project can hold the same pid at the same instant, and a discriminator drawn
/// from a clock is a probability rather than a rule. The guarantee is that [`create_run_dir`] mints
/// the id by CREATING the directory: `create_dir` is atomic on every platform this ships to and
/// fails with `AlreadyExists` rather than opening what is already there, so the first creator of a
/// given id owns it and every later one is told to try the next `seq`. The id is not checked and
/// then used — checking and then using is the race. Creating IS the check.
///
/// The research producer minted its `run_id` from a bare unix-seconds clock read
/// (`crates/vike-research/src/bin/research.rs`'s `now_secs`), so two of its runs starting in the
/// same second collided. That rule is deliberately NOT inherited: this namespace is shared, and a
/// producer arriving second must be able to mint into it without asking what the first one did.
/// ⚠ That binary is GONE — it went with the research crate, and the citation is kept as the
/// evidence for the rule rather than as a file to go and read (it is filed in
/// `crates/vike-ops/tests/citation_gate.rs`'s `DEAD_PATH_EXCEPTIONS`). The rule outlived it: the
/// producer that did arrive, `crates/vike-studio-core/src/study_run.rs`'s `run_study`, mints
/// through [`create_run_dir`] and inherits the guarantee instead of a clock.
///
/// # Fields are public and there is no constructor
///
/// A struct literal cannot omit a field. That is the point: when this manifest gains a common
/// field, every producer stops compiling until it decides what to put there — exactly the review a
/// shared shape needs, and exactly what a `new()` with a default would have hidden.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunManifest {
    /// [`MANIFEST_SCHEMA`] at write time; `0` in a document written before this field existed.
    ///
    /// ⚠ **`#[serde(default)]` is load-bearing and must not be removed.** Every other field here is
    /// REQUIRED, and `crates/vike-studio-core/src/listing.rs`'s `list_runs` turns a parse failure
    /// into a dropped row — so a required version field would have made every manifest already on
    /// disk vanish from the listing on the day this shipped.
    #[serde(default)]
    pub schema: u32,
    /// The directory name this manifest sits in. Duplicated INTO the file on purpose: a listing
    /// that has read the manifest should not have to re-derive identity from the path it read it
    /// through, and a manifest copied out of its directory should still say which run it is.
    pub run_id: String,
    /// What KIND of run this was — `"backtest"` for this producer. The one field a reader may
    /// branch on, and a plain string rather than an enum because the set of kinds grows in crates
    /// that cannot see each other; a reader that does not recognise a kind still renders every
    /// field above and beside it.
    pub kind: String,
    /// The BINARY that produced the run, spelled literally. Not `CARGO_PKG_NAME`, which is the
    /// PACKAGE (`vike-backtest`) rather than the binary (`backtest`) — the same distinction
    /// `backtest --version` makes, for the same reason: a name the operator never invoked is a name
    /// a bug report cannot use.
    pub produced_by: String,
    /// When the run STARTED, RFC-3339 UTC to the second (`2026-08-24T09:15:04Z`) — see
    /// [`utc_rfc3339`], which is the one spelling so two producers cannot disagree about the format
    /// of a common field. A string rather than a number because a manifest is a document a human
    /// opens and every language a listing could be written in parses RFC-3339 in one line. Derived
    /// from the SAME clock read that minted [`Self::run_id`], so the two can never disagree about
    /// when the run began.
    pub started_at: String,
    /// When the run FINISHED, same format. Read after the work and before the result is printed, so
    /// it measures the run rather than the terminal.
    pub finished_at: String,
    /// The commit the producing binary was BUILT from, or `None` when the producer cannot name one.
    ///
    /// ⚠ **Always serialized, `null` included** — never skipped. A listing renders a column off a
    /// key set it can rely on, and "this producer could not name a build" is an answer worth
    /// showing; a missing key would make it indistinguishable from a manifest written before the
    /// field existed.
    ///
    /// ⚠ **Which producer can fill it is a property of the BINARY, not of this crate** — see
    /// [`BuildStamp`], which is how it arrives. `crates/vike/src/main.rs`'s `backtest_main` fills
    /// it (that root already depends on `vike-buildinfo` for its own `--version`); the standalone
    /// `crates/vike-backtest/src/bin/backtest.rs` writes `None`, because that bin lives IN the
    /// `vike-backtest` package and so could only name `vike-buildinfo` if the PACKAGE did — which
    /// `crates/vike-buildinfo/tests/identity_adoption.rs`'s `WITHOUT_IDENTITY` refuses on
    /// build-cost grounds. That asymmetry is real and SHIPPED:
    /// `crates/vike-cli/src/cmd/engine.rs`'s search order makes the standalone engine the rung that
    /// answers on an ordinary Linux install, so the COMMON path still records `null` here.
    ///
    /// ⚠ This doc said flatly that "`backtest` writes `None`", and stopped being true when the
    /// stamp became a parameter one of the two roots fills. The dependency fact below is still
    /// true and is kept, because it is what a reader will otherwise re-derive: naming a commit
    /// means `vike-buildinfo`, whose
    /// `build.rs` resolves it at COMPILE time rather than shelling `git rev-parse` at runtime (which
    /// would answer about the working directory instead of about the binary), and `vike-backtest`
    /// does not depend on that crate. A producer whose BINARY can answer fills this in —
    /// `crates/vike-studio-core/src/study_run.rs`'s `StudyRunRequest` takes the sha as a parameter
    /// for exactly that reason.
    pub git_sha: Option<String>,
    /// The run's INPUTS as one address — **never its results and never its build.**
    ///
    /// `None` when the producer cannot compute one, written as `null` for the same reason
    /// [`Self::git_sha`] is: "cannot address its inputs" and "wrote no such field" are different
    /// answers. ⚠ It ALSO means "this producer's STORE could not be read" — the backtest producer
    /// refuses to address a run whose data slice it could not inventory rather than addressing it
    /// wrongly — and a reader holding the document alone cannot tell that apart from a producer
    /// that never computes one; the reason was named on stderr when the run happened and is not in
    /// the file. Lowercase hex, and this module neither computes nor validates it — the producer
    /// does, because what an input IS differs per kind
    /// (`crates/vike-backtest/src/run_fingerprint.rs`'s `input_fingerprint` is the backtest's).
    ///
    /// ⚠ **Why inputs and not inputs-plus-build.** The question this exists to answer is "did my
    /// engine edit change the result?" — which means finding the BASELINE run that had the same
    /// inputs and a DIFFERENT build. An address that included the build would never match across
    /// the commit boundary, so every run would be its own baseline and nothing could ever be
    /// compared. The build rides separately in [`Self::git_sha`] and under [`Self::detail`].
    ///
    /// ⚠ **Why not the results.** `docs/decisions/0032-transcendentals-come-from-the-libm-crate-not-the-platform.md`
    /// records that this workspace's equity fold still differs between MSVC and glibc at the last
    /// bit. An address over `(config, data)` is reproducible on both boxes; one over the curve is
    /// not.
    #[serde(default)]
    pub fingerprint: Option<String>,
    /// Which config drove the run. See [`RunConfig`].
    pub config: RunConfig,
    /// Everything KIND-SPECIFIC, nested so it cannot crowd the common fields above. `Null` for a
    /// producer with nothing to add. A reader that does not recognise [`Self::kind`] ignores this
    /// whole subtree and still renders a complete listing row — which is the entire argument for
    /// the nesting.
    #[serde(default)]
    pub detail: serde_json::Value,
}

/// A run directory that now EXISTS, and the id its creation minted.
#[derive(Clone, Debug)]
pub struct RunDir {
    /// The minted id — [`RunManifest::run_id`] carries the rule and why it cannot collide.
    pub run_id: String,
    /// The created directory: `<runs_root>/<run_id>`.
    pub path: PathBuf,
}

/// Why a run could not be persisted. Every variant names the PATH it failed on, because "the run
/// was not saved" without a location is a message an operator cannot act on.
#[derive(Debug)]
pub enum RunPersistError {
    /// A directory could not be created.
    Dir {
        /// The directory that could not be created.
        path: PathBuf,
        /// The operating system's own words.
        why: String,
    },
    /// A file could not be written.
    Write {
        /// The file that could not be written.
        path: PathBuf,
        /// The operating system's own words.
        why: String,
    },
    /// A value could not be turned into JSON.
    Serialize {
        /// Which document it was — one of [`RESERVED_FILES`].
        file: &'static str,
        /// serde_json's own words.
        why: String,
    },
    /// [`MINT_ATTEMPTS`] ids in a row were already taken — in practice a runs directory that
    /// refuses creation while reporting `AlreadyExists`, not a genuine flood of runs.
    IdExhausted {
        /// The runs directory that yielded no free id.
        root: PathBuf,
        /// How many ids were tried.
        attempts: u32,
    },
}

impl fmt::Display for RunPersistError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Dir { path, why } => write!(f, "cannot create {}: {why}", path.display()),
            Self::Write { path, why } => write!(f, "cannot write {}: {why}", path.display()),
            Self::Serialize { file, why } => write!(f, "cannot serialize {file}: {why}"),
            Self::IdExhausted { root, attempts } => {
                write!(f, "no free run id under {} after {attempts} tries", root.display())
            }
        }
    }
}

impl std::error::Error for RunPersistError {}

/// Unix seconds as RFC-3339 UTC to the second — the ONE spelling every manifest timestamp takes, so
/// two producers cannot disagree about the format of a common field.
///
/// ⚠ **Chrono-free, and that is load-bearing rather than tidiness.** This module lives in the crate
/// at the BOTTOM of the dependency graph so that `vike-cli` can read a run without linking the
/// engine (`crates/vike-cli/Cargo.toml`'s `vike-model` rationale argues the same trade for the
/// client-order-id generator). `chrono` may not follow it here, so the calendar math is
/// [`crate::time::civil_from_days`] — Howard Hinnant's proleptic-Gregorian algorithm, integer-exact
/// across the whole `i64` range, which that module was consolidated to be the one home for.
///
/// A second no calendar date can hold falls back to the raw number rather than panicking: a manifest
/// is metadata about a run that already succeeded, and must never be the thing that kills it.
pub fn utc_rfc3339(unix_secs: i64) -> String {
    let days = unix_secs.div_euclid(86_400);
    let rem = unix_secs.rem_euclid(86_400);
    let (y, mo, d) = crate::time::civil_from_days(days);
    // The one unrepresentable case: a year outside four digits has no RFC-3339 spelling, so the raw
    // number is the honest answer. `i64::MIN`/`i64::MAX` seconds are both far outside it.
    if !(0..=9999).contains(&y) {
        return unix_secs.to_string();
    }
    let (h, mi, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

/// The id a run started at `started_at` takes at attempt `seq`.
///
/// `<unix-seconds>-<address|pid>-<seq>`. Public and pure so a test can plant the id the minter
/// WOULD produce rather than re-spelling the format, and so a reader can recognise one.
///
/// ⚠ **The address is SANITIZED, because this becomes a DIRECTORY NAME.** A fingerprint carrying
/// `/`, `\` or `..` would be a path traversal assembled out of a value some producer computed, and
/// slicing a `&str` by BYTES would panic on a non-char-boundary — so this filters to ASCII
/// alphanumerics, takes at most [`ID_FINGERPRINT_LEN`] CHARACTERS, and falls back to the pid form
/// when nothing usable survives.
pub fn run_id_at(started_at: i64, fingerprint: Option<&str>, seq: u32) -> String {
    let addr: Option<String> = fingerprint.map(|fp| {
        fp.chars().filter(char::is_ascii_alphanumeric).take(ID_FINGERPRINT_LEN).collect()
    });
    match addr {
        Some(a) if !a.is_empty() => format!("{started_at}-{a}-{seq}"),
        // No address, or nothing usable in it: the pre-address form, pid and all. Every producer
        // that cannot content-address its inputs keeps exactly the ids it minted before.
        _ => format!("{started_at}-{}-{seq}", std::process::id()),
    }
}

/// Create a fresh run directory under `runs_root`, minting an id that cannot collide with another
/// run's — [`RunManifest::run_id`] carries the rule and why creation IS the check.
///
/// `started_at` is the run's own clock read in unix seconds, passed IN rather than read here so the
/// id and [`RunManifest::started_at`] name the same instant.
///
/// `fingerprint` is the run's INPUT ADDRESS ([`RunManifest::fingerprint`]) or `None` for a producer
/// that cannot compute one. It rides in the id as a SUFFIX, never as the whole name: the seconds
/// stay in front because `crates/vike-studio-core/src/listing.rs`'s `list_runs` sorts by directory
/// NAME and its module doc calls that chronological for exactly that reason, and
/// `crates/vike-studio/src/research.rs` reverses the same list for "Newest first". A bare content
/// hash would make both arbitrary with nothing going red.
///
/// `runs_root` and its parents are created when absent: a project that has run nothing has no
/// `user_data/runs/`, which is a fresh install rather than an error.
pub fn create_run_dir(
    runs_root: &Path,
    started_at: i64,
    fingerprint: Option<&str>,
) -> Result<RunDir, RunPersistError> {
    std::fs::create_dir_all(runs_root)
        .map_err(|e| RunPersistError::Dir { path: runs_root.to_path_buf(), why: e.to_string() })?;
    for seq in 0..MINT_ATTEMPTS {
        let run_id = run_id_at(started_at, fingerprint, seq);
        let path = runs_root.join(&run_id);
        match std::fs::create_dir(&path) {
            Ok(()) => return Ok(RunDir { run_id, path }),
            // The whole collision rule, in one arm: somebody else owns that id, so try the next.
            // Reached by a second run in the same second of the same process, by a second PROCESS
            // that shares this one's pid — which is the case a clock-derived discriminator would
            // have called impossible — and now by a second run over the SAME INPUTS in one second,
            // which is two runs and must be two directories.
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(RunPersistError::Dir { path, why: e.to_string() }),
        }
    }
    Err(RunPersistError::IdExhausted { root: runs_root.to_path_buf(), attempts: MINT_ATTEMPTS })
}

/// Write the documents of a run directory, the manifest LAST — this module's doc carries the
/// argument for that order, and [`write_run_with`] is where it is enforced.
///
/// Kept as the two-document door because that is what a producer with nothing else to write wants,
/// and because its two callers should not have to say "no extras" to mean it.
///
/// `report` is generic rather than a named type because the report is the KIND-SPECIFIC half: this
/// function's contract is the file names and the ordering, never the schema of what a particular
/// producer computed.
pub fn write_run<R>(dir: &Path, manifest: &RunManifest, report: &R) -> Result<(), RunPersistError>
where
    R: Serialize + ?Sized,
{
    write_run_with(dir, manifest, report, &RunExtras::default())
}

/// [`write_run`] plus the optional documents in [`RunExtras`].
///
/// ORDER: config, series, trades, report, manifest — every irreplaceable document before the
/// COMPLETION MARKER, so a disk that fills part-way through costs the metadata rather than the
/// result, and a listing can still tell a half-written run from a broken one with no lock file.
pub fn write_run_with<R>(
    dir: &Path,
    manifest: &RunManifest,
    report: &R,
    extras: &RunExtras<'_>,
) -> Result<(), RunPersistError>
where
    R: Serialize + ?Sized,
{
    if let Some(toml) = extras.config_toml {
        write_text(&dir.join(CONFIG_FILE), toml)?;
    }
    if let Some(series) = extras.series {
        write_json(&dir.join(SERIES_FILE), SERIES_FILE, series)?;
    }
    if let Some(trades) = extras.trades {
        write_json(&dir.join(TRADES_FILE), TRADES_FILE, trades)?;
    }
    write_json(&dir.join(REPORT_FILE), REPORT_FILE, report)?;
    write_json(&dir.join(MANIFEST_FILE), MANIFEST_FILE, manifest)
}

/// The config is TEXT, not JSON — it is the operator's own file, byte for byte.
fn write_text(path: &Path, value: &str) -> Result<(), RunPersistError> {
    std::fs::write(path, value)
        .map_err(|e| RunPersistError::Write { path: path.to_path_buf(), why: e.to_string() })
}

fn write_json<T>(path: &Path, file: &'static str, value: &T) -> Result<(), RunPersistError>
where
    T: Serialize + ?Sized,
{
    let mut json = serde_json::to_string_pretty(value)
        .map_err(|e| RunPersistError::Serialize { file, why: e.to_string() })?;
    json.push('\n');
    std::fs::write(path, json)
        .map_err(|e| RunPersistError::Write { path: path.to_path_buf(), why: e.to_string() })
}

/// Why a run directory's manifest could not be READ back. The counterpart of [`RunPersistError`],
/// and it lives here rather than in whatever crate happens to list runs first: the schema is
/// [`RunManifest`]'s, so the code that turns bytes into one belongs beside the code that turns one
/// into bytes. A reader written up the dependency graph would be a second definition of a common
/// manifest, which this module's doc names as the one outcome that shape cannot survive.
///
/// ⚠ [`RunReadError::Missing`] is deliberately NOT folded into [`RunReadError::Read`], even though
/// both are "no manifest came back". The manifest is written LAST (see this module's doc), so its
/// absence means the run is being written RIGHT NOW or a process died between the two writes —
/// while an unreadable one means a file that exists and cannot be opened. A listing renders those
/// as different rows because they have different fixes: wait, versus go and look at the file.
#[derive(Debug)]
pub enum RunReadError {
    /// The directory holds no such document.
    ///
    /// For [`MANIFEST_FILE`] that is an unfinished run rather than a broken one. For
    /// [`SERIES_FILE`], [`TRADES_FILE`] and [`CONFIG_FILE`] it is ORDINARY: every run written
    /// before those documents existed has none, and a producer that keeps no curve never will.
    Missing {
        /// Where the document was looked for.
        path: PathBuf,
    },
    /// The document is there and could not be read as text — permissions, or not UTF-8.
    Read {
        /// The file that could not be read.
        path: PathBuf,
        /// The operating system's own words.
        why: String,
    },
    /// The text is not a [`RunManifest`]: not JSON at all, or JSON missing a COMMON field. Both are
    /// one variant because a listing acts on them identically — the document cannot produce a row,
    /// and `why` carries which of the two it was in the parser's own words.
    Parse {
        /// The file that could not be parsed.
        path: PathBuf,
        /// serde_json's own words.
        why: String,
    },
}

impl fmt::Display for RunReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            // ⚠ The FILE comes off the path rather than being the manifest by assumption: this
            // error serves four documents since the run record grew past two, and a message
            // that said `no manifest.json` when a caller asked for `series.json` would send a
            // reader looking for the wrong absence. The half-written-run clause is
            // manifest-ONLY for the same reason: it is true of the completion marker and of
            // nothing else.
            Self::Missing { path } => {
                let file = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| MANIFEST_FILE.to_string());
                if file == MANIFEST_FILE {
                    write!(
                        f,
                        "no {MANIFEST_FILE} at {} — the run is still being written, or it \
                         stopped between its report and its manifest",
                        path.display()
                    )
                } else {
                    write!(f, "no {file} at {}", path.display())
                }
            }
            Self::Read { path, why } => write!(f, "cannot read {}: {why}", path.display()),
            Self::Parse { path, why } => write!(f, "cannot parse {}: {why}", path.display()),
        }
    }
}

impl std::error::Error for RunReadError {}

/// Read one run directory's [`MANIFEST_FILE`] — the READING half of this module, and the call any
/// listing of `<project>/user_data/runs/` is written on top of
/// (`crates/vike-studio-core/src/listing.rs`'s `list_runs`).
///
/// Takes the RUN DIRECTORY rather than the manifest path, symmetrically with [`write_run`], so the
/// file name stays this module's business: a caller that spelled `manifest.json` itself would be a
/// second place the name lives.
///
/// Every failure is a value naming the path — nothing here panics, and nothing is silently skipped.
/// A run that vanishes from a listing is worse than one that shows as broken: the first is
/// unanswerable, the second names its own fix.
pub fn read_manifest(dir: &Path) -> Result<RunManifest, RunReadError> {
    read_doc(dir, MANIFEST_FILE)
}

/// Read one JSON document out of a run directory. The three readers below differ only in the file
/// name and the type, so the error mapping — and the `Missing` / `Read` / `Parse` distinction this
/// module's [`RunReadError`] exists for — is written once.
fn read_doc<T: serde::de::DeserializeOwned>(dir: &Path, file: &str) -> Result<T, RunReadError> {
    let text = read_run_text(dir, file)?;
    let path = dir.join(file);
    serde_json::from_str(&text).map_err(|e| RunReadError::Parse { path, why: e.to_string() })
}

/// The raw text of one file in a run directory, with the same three-way failure distinction.
fn read_run_text(dir: &Path, file: &str) -> Result<String, RunReadError> {
    let path = dir.join(file);
    match std::fs::read_to_string(&path) {
        Ok(text) => Ok(text),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Err(RunReadError::Missing { path }),
        Err(e) => Err(RunReadError::Read { path, why: e.to_string() }),
    }
}

/// Read one run directory's [`SERIES_FILE`].
///
/// ⚠ [`RunReadError::Missing`] is an ORDINARY answer here, unlike for the manifest: every run
/// written before this document existed has none, and a producer that keeps no curve never will.
/// A caller renders "no series" rather than "broken run".
pub fn read_series(dir: &Path) -> Result<RunSeries, RunReadError> {
    read_doc(dir, SERIES_FILE)
}

/// Read one run directory's [`TRADES_FILE`]. Same `Missing`-is-ordinary rule as [`read_series`].
pub fn read_trades(dir: &Path) -> Result<RunTrades, RunReadError> {
    read_doc(dir, TRADES_FILE)
}

/// Read one run directory's [`CONFIG_FILE`] — the resolved config as TEXT, unparsed. Parsing it is
/// `crates/vike-backtest/src/harness/profile.rs`'s `BacktestProfile::from_toml_str`'s job and this
/// module does not depend on that tree. Same `Missing`-is-ordinary rule.
pub fn read_config_toml(dir: &Path) -> Result<String, RunReadError> {
    read_run_text(dir, CONFIG_FILE)
}

// ─── the per-run TAG SIDECAR, and the MARK store beside the runs root ──────────────────────────
//
// Two documents, because they answer two different questions and one store could not answer both. A
// TAG is a LABEL that belongs to one run and travels with it, so it lives INSIDE the run directory.
// A MARK is a POINTER — a name that must resolve to a run without opening every run directory in
// the tree — so it lives in a file NAMED for the mark, under a root that is a SIBLING of the runs
// root (`crate::state_path::MARKS_SUBDIR` carries why a child would be wrong).
//
// ⚠ **Neither is a field on [`RunManifest`], and that is a decision rather than an omission.**
// [`write_run_with`] is write-once and there is no rewrite entry point: the manifest IS the
// completion marker a listing relies on, so rewriting it to add a tag would mean a run momentarily
// has none. And `RunManifest`'s fields are public with no constructor precisely so that a new COMMON
// field stops every producer compiling until it decides what to put there — which is right for a
// field every producer must answer, and wrong for one only a later verb ever writes.

/// The schema version [`RunMeta`] and [`Mark`] open with.
///
/// Shipped WITH the documents rather than after them — §13 of
/// `docs/superpowers/specs/2026-09-12-backtest-cli-surface-design.md`: a schema tag retrofitted onto
/// documents already in people's scripts is the one item that gets strictly more expensive every
/// week. [`SERIES_SCHEMA`] carries the argument for versioning a run directory's documents at all.
pub const META_SCHEMA: u32 = 1;

/// A note somebody attached to a run, with when.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunNote {
    /// RFC-3339 UTC to the second — [`utc_rfc3339`], the one spelling every timestamp in this module
    /// takes.
    pub at: String,
    /// The note, verbatim.
    pub text: String,
}

/// What a user attached to a run after it finished: labels and notes. The content of [`META_FILE`].
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunMeta {
    /// [`META_SCHEMA`] at write time; `0` in a document written before this field existed, carried
    /// for the same reason [`RunManifest::schema`] carries it.
    #[serde(default)]
    pub schema: u32,
    /// Labels, deduped, in FIRST-INSERT order so a rendered row does not shuffle between calls.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Notes, APPEND-ONLY. A note is evidence; the second one must not delete the first.
    #[serde(default)]
    pub notes: Vec<RunNote>,
}

impl Default for RunMeta {
    fn default() -> Self {
        Self { schema: META_SCHEMA, tags: Vec::new(), notes: Vec::new() }
    }
}

/// Read a run's tag sidecar.
///
/// An ABSENT file is an EMPTY [`RunMeta`], never an error — see [`META_FILE`]: a run nobody has
/// tagged, and every run minted before tagging existed, is the ordinary case. A file that EXISTS and
/// will not parse IS an error, which is the line every reader in this workspace draws between "not
/// configured" and "broken".
pub fn read_meta(dir: &Path) -> Result<RunMeta, RunReadError> {
    match read_doc::<RunMeta>(dir, META_FILE) {
        Ok(meta) => Ok(meta),
        Err(RunReadError::Missing { .. }) => Ok(RunMeta::default()),
        Err(other) => Err(other),
    }
}

/// Add tags and/or a note to a run, in place. The ONE writer of [`META_FILE`].
///
/// ⚠ **Both `vike-cli backtest run --tag/--note` and `vike-cli backtest tag --add/--note` call
/// this**, and that is a decision rather than a coincidence: they write the same document, and two
/// writers with two formats is how a sidecar comes to mean different things depending on which verb
/// made it. §5.8 and §7.2 of the CLI-surface design each give a verb that flag pair and neither says
/// who owns the file; this function is the answer.
///
/// `at` is the clock second, passed IN — this crate contains no ambient clock read, the same rule
/// [`create_run_dir`] follows and for the same reason.
///
/// Tags DEDUPE and keep first-insert order; notes APPEND. Passing neither is not an error here —
/// "you asked for no change" is a question about the command line, and belongs where the command
/// line is parsed.
///
/// ⚠ **An existing sidecar that will not parse is a REFUSAL, not an overwrite.** The file is the
/// only copy of somebody's notes; re-minting it from an empty document would delete them silently,
/// which is the one outcome a metadata write may not have.
pub fn add_tags(
    dir: &Path,
    tags: &[String],
    note: Option<&str>,
    at: i64,
) -> Result<RunMeta, RunPersistError> {
    let path = dir.join(META_FILE);
    let mut meta = read_meta(dir).map_err(|e| RunPersistError::Write {
        path: path.clone(),
        why: format!(
            "the sidecar already there could not be read ({e}) — refusing to overwrite it, because \
             it is the only copy of whatever is in it"
        ),
    })?;
    meta.schema = META_SCHEMA;
    for tag in tags {
        let tag = tag.trim();
        if tag.is_empty() || meta.tags.iter().any(|t| t == tag) {
            continue;
        }
        meta.tags.push(tag.to_string());
    }
    if let Some(text) = note {
        meta.notes.push(RunNote { at: utc_rfc3339(at), text: text.to_string() });
    }
    write_json(&path, META_FILE, &meta)?;
    Ok(meta)
}

/// How many previous pointers a mark keeps.
///
/// A mark moved on every CI run would otherwise grow one file without bound — the same failure the
/// log retention in this workspace exists for, on a smaller scale. Most-recent-first, oldest
/// dropped.
pub const MARK_HISTORY_MAX: usize = 32;

/// The deepest a mark name may nest. A mark is a LABEL, not a tree: `baseline/momentum` and
/// `nightly/eu/open` are names; anything deeper is somebody using the marks root as a filesystem.
const MARK_MAX_DEPTH: usize = 3;

/// The Windows RESERVED DEVICE NAMES, which are unopenable there under ANY extension.
///
/// ⚠ No test in this workspace runs on Windows (`CLAUDE.md`: "No Windows TEST runs anywhere"), so
/// this list has to be right by construction rather than by measurement. A mark called `con` would
/// write here, resolve here, and be unopenable on the one platform nothing would catch it on.
const WINDOWS_DEVICE_NAMES: &[&str] = &[
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// Where a mark pointed before it was moved.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MarkMove {
    /// The run it pointed at.
    pub run_id: String,
    /// When it was set, RFC-3339 UTC.
    pub marked_at: String,
    /// The note it carried, if any.
    pub note: Option<String>,
}

/// A NAME that points at a run, and the moves it has made.
///
/// # Why a mark exists at all
///
/// A run id moves every time you run. A gate written against one is a gate that passes once and then
/// names a run nobody is comparing to. §7.2 of
/// `docs/superpowers/specs/2026-09-12-backtest-cli-surface-design.md`: "A **mark** is what makes
/// `gate` and `diff` usable: a stable second operand that does not move when you run again."
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Mark {
    /// [`META_SCHEMA`] at write time.
    #[serde(default)]
    pub schema: u32,
    /// The mark's own name, duplicated into the file for the same reason [`RunManifest::run_id`] is:
    /// a document copied out of its directory should still say what it is. The FILE NAME decides,
    /// exactly as the run DIRECTORY does.
    pub name: String,
    /// The run it points at now.
    pub run_id: String,
    /// When it was pointed there, RFC-3339 UTC.
    pub marked_at: String,
    /// The note given when it was set.
    #[serde(default)]
    pub note: Option<String>,
    /// Where it pointed before, MOST RECENT FIRST, capped at [`MARK_HISTORY_MAX`].
    #[serde(default)]
    pub history: Vec<MarkMove>,
}

/// Refuse a mark name that could not be, or should not be, a file path.
///
/// The name becomes `<marks_root>/<name>.json`, so this is a SECURITY boundary and a PORTABILITY one
/// at once rather than tidiness:
///
/// * traversal (`..`, a leading or doubled `/`, an absolute path) would write OUTSIDE the marks
///   root, from a string somebody typed on a command line;
/// * a leading `.` collides with the dot-entry skip every directory scan in this workspace applies,
///   so such a mark would be written and then be invisible;
/// * `\`, `:`, `*`, `?`, `"`, `<`, `>`, `|`, whitespace and control bytes are unwritable or
///   ambiguous on Windows;
/// * the Windows device names (`con`, `prn`, `aux`, `nul`, `com1`–`com9`, `lpt1`–`lpt9`,
///   case-insensitive, and under ANY extension) are unopenable there;
/// * depth is capped at three `/`-separated parts, because a mark is a label and not a tree.
///
/// The `Err` is the SENTENCE a caller prints, so it names what was wrong rather than saying
/// "invalid".
pub fn valid_mark_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("a mark name is required (for example `baseline/momentum`, or `prod`)".into());
    }
    if name != name.trim() {
        return Err(format!("'{name}': a mark name may not begin or end with whitespace"));
    }
    let segments: Vec<&str> = name.split('/').collect();
    if segments.len() > MARK_MAX_DEPTH {
        return Err(format!(
            "'{name}': a mark name may have at most {MARK_MAX_DEPTH} '/'-separated parts — a mark \
             is a label, not a directory tree"
        ));
    }
    // ⚠ `.copied()` gives a `&str` rather than the `&&str` a plain `&segments` loop yields — the
    // device-name check below hands one to `unwrap_or`, which needs the same type the split returns.
    for segment in segments.iter().copied() {
        if segment.is_empty() {
            return Err(format!(
                "'{name}': an empty part — a mark name may not start or end with '/' and may not \
                 contain '//'"
            ));
        }
        if segment.starts_with('.') {
            return Err(format!(
                "'{name}': the part '{segment}' starts with '.' — that is a traversal ('..'), or a \
                 dot-entry every directory scan in this workspace skips, so the mark would be \
                 written and then be invisible"
            ));
        }
        if let Some(bad) =
            segment.chars().find(|c| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')))
        {
            return Err(format!(
                "'{name}': the part '{segment}' contains '{bad}' — a mark name takes ASCII letters, \
                 digits, '.', '_', '-' and '/' only, because it becomes a file path on every \
                 platform this ships to"
            ));
        }
        // Windows refuses a device name under ANY extension, so the STEM is what matters: `con` and
        // `con.baseline` are both unopenable there once `.json` is appended.
        let stem = segment.split('.').next().unwrap_or(segment).to_ascii_lowercase();
        if WINDOWS_DEVICE_NAMES.contains(&stem.as_str()) {
            return Err(format!(
                "'{name}': the part '{segment}' is a reserved Windows device name — a file called \
                 that cannot be opened on Windows under any extension"
            ));
        }
    }
    Ok(())
}

/// Why a mark could not be read or written. Every variant names the PATH or the NAME, because "no
/// such mark" without one is a message an operator cannot act on.
#[derive(Debug)]
pub enum MarkError {
    /// The name is not usable as a path — [`valid_mark_name`] says why, verbatim.
    BadName {
        /// The name as it was typed.
        name: String,
        /// [`valid_mark_name`]'s own sentence.
        why: String,
    },
    /// No such mark.
    Missing {
        /// The name that was looked up.
        name: String,
        /// Where it was looked for.
        path: PathBuf,
    },
    /// It is there and could not be read, or will not parse.
    Read {
        /// The file.
        path: PathBuf,
        /// The operating system's or the parser's own words.
        why: String,
    },
    /// It could not be written.
    Write {
        /// The file.
        path: PathBuf,
        /// The operating system's own words.
        why: String,
    },
}

impl fmt::Display for MarkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadName { why, .. } => write!(f, "{why}"),
            Self::Missing { name, path } => write!(
                f,
                "no mark named '{name}' — nothing at {}. Set one with \
                 `vike-cli backtest tag <run> --as {name}`.",
                path.display()
            ),
            Self::Read { path, why } => write!(f, "cannot read {}: {why}", path.display()),
            Self::Write { path, why } => write!(f, "cannot write {}: {why}", path.display()),
        }
    }
}

impl std::error::Error for MarkError {}

/// The file one mark lives in: `<marks_root>/<name>.json`.
///
/// ⚠ Joined SEGMENT BY SEGMENT rather than as one string, so a name that somehow reached here
/// absolute could not replace the root — `Path::join` with an absolute argument DISCARDS the base.
/// [`valid_mark_name`] already refuses one; this is the belt beside that brace, because the cost of
/// being wrong is a write outside the marks root.
fn mark_path(marks_root: &Path, name: &str) -> PathBuf {
    let mut path = marks_root.to_path_buf();
    let segments: Vec<&str> = name.split('/').collect();
    for (i, segment) in segments.iter().enumerate() {
        if i + 1 == segments.len() {
            path.push(format!("{segment}.json"));
        } else {
            path.push(segment);
        }
    }
    path
}

/// Read one mark by name.
pub fn read_mark(marks_root: &Path, name: &str) -> Result<Mark, MarkError> {
    valid_mark_name(name).map_err(|why| MarkError::BadName { name: name.to_string(), why })?;
    let path = mark_path(marks_root, name);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Err(MarkError::Missing { name: name.to_string(), path });
        }
        Err(e) => return Err(MarkError::Read { path, why: e.to_string() }),
    };
    serde_json::from_str(&text).map_err(|e| MarkError::Read { path, why: e.to_string() })
}

/// Point a mark at a run, creating it or MOVING it.
///
/// Moving RECORDS the previous pointer in [`Mark::history`] — §7.2's "Re-marking is explicit and
/// recorded", which is only true if the move is kept somewhere a person can see it.
///
/// The write is atomic (temp file beside the target, then rename), so a CI job interrupted mid-write
/// leaves the OLD mark rather than a truncated file — a half-written pointer is worse than a stale
/// one, because a gate would then judge against nothing.
///
/// ⚠ On Windows `rename` FAILS when the destination exists, unlike POSIX, so the replace is
/// `remove_file`-then-`rename` with the removal's `NotFound` treated as success. That is the one
/// platform difference in this module and no test in this workspace can execute it — nothing here
/// runs on Windows — so it is written from the rule rather than from a measurement.
pub fn write_mark(
    marks_root: &Path,
    name: &str,
    run_id: &str,
    note: Option<&str>,
    at: i64,
) -> Result<Mark, MarkError> {
    valid_mark_name(name).map_err(|why| MarkError::BadName { name: name.to_string(), why })?;
    let path = mark_path(marks_root, name);

    // The PREVIOUS pointer, if there is one. A mark that is there and will not parse is a REFUSAL
    // rather than a silent re-mint: the history in it is the evidence §7.2 asks for.
    let previous = match read_mark(marks_root, name) {
        Ok(m) => Some(m),
        Err(MarkError::Missing { .. }) => None,
        Err(other) => return Err(other),
    };

    let mut history = Vec::new();
    if let Some(prev) = previous {
        history.push(MarkMove { run_id: prev.run_id, marked_at: prev.marked_at, note: prev.note });
        history.extend(prev.history);
        history.truncate(MARK_HISTORY_MAX);
    }

    let mark = Mark {
        schema: META_SCHEMA,
        name: name.to_string(),
        run_id: run_id.to_string(),
        marked_at: utc_rfc3339(at),
        note: note.map(str::to_string),
        history,
    };

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| MarkError::Write { path: path.clone(), why: e.to_string() })?;
    }
    let mut json = serde_json::to_string_pretty(&mark)
        .map_err(|e| MarkError::Write { path: path.clone(), why: e.to_string() })?;
    json.push('\n');

    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json)
        .map_err(|e| MarkError::Write { path: tmp.clone(), why: e.to_string() })?;
    // ⚠ Windows `rename` refuses an existing destination; POSIX replaces it. Removing first is
    // correct on both, and a `NotFound` here is the ordinary first-write case.
    match std::fs::remove_file(&path) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(MarkError::Write { path: path.clone(), why: e.to_string() }),
    }
    std::fs::rename(&tmp, &path)
        .map_err(|e| MarkError::Write { path: path.clone(), why: e.to_string() })?;
    Ok(mark)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn a_manifest(run_id: &str) -> RunManifest {
        RunManifest {
            schema: MANIFEST_SCHEMA,
            run_id: run_id.to_string(),
            kind: BACKTEST_RUN_KIND.to_string(),
            produced_by: "backtest".to_string(),
            started_at: utc_rfc3339(1_756_000_000),
            finished_at: utc_rfc3339(1_756_000_012),
            git_sha: None,
            fingerprint: None,
            config: RunConfig {
                path: Some("profiles/sma.toml".to_string()),
                name: Some("sma cross".to_string()),
            },
            detail: json!({ "strategy": "sma_cross" }),
        }
    }

    /// The collision rule, stated as the property that matters: the SAME clock second, minted
    /// twice, must not name one directory. The research producer's bare-seconds id failed exactly
    /// this, which is why the rule is decided here rather than inherited.
    #[test]
    fn two_runs_starting_in_the_same_second_cannot_share_a_run_id() {
        let root = tempfile::tempdir().unwrap();
        let runs = root.path().join("runs");

        let first = create_run_dir(&runs, 1_756_000_000, None).unwrap();
        let second = create_run_dir(&runs, 1_756_000_000, None).unwrap();

        assert_ne!(first.run_id, second.run_id, "one clock second must not mint one id twice");
        assert_ne!(first.path, second.path);
        assert!(first.path.is_dir(), "minting a run id CREATES the directory — that is the check");
        assert!(second.path.is_dir());
    }

    /// Creation is the check, so an id whose directory already exists must be refused rather than
    /// reused: handing one out twice lets the second run overwrite the first one's report. The
    /// taken id is built with the SAME function the minter uses, so this test cannot drift away
    /// from the id format.
    #[test]
    fn an_id_whose_directory_already_exists_is_never_handed_out() {
        let root = tempfile::tempdir().unwrap();
        let runs = root.path().join("runs");
        std::fs::create_dir_all(&runs).unwrap();
        let taken = run_id_at(1_756_000_000, Some(FP), 0);
        std::fs::create_dir(runs.join(&taken)).unwrap();
        std::fs::write(runs.join(&taken).join(REPORT_FILE), b"the first run's result").unwrap();

        let minted = create_run_dir(&runs, 1_756_000_000, Some(FP)).unwrap();

        assert_ne!(minted.run_id, taken);
        assert!(
            minted.run_id.ends_with("-1"),
            "a taken id must advance to the NEXT seq, not to some unrelated name: {}",
            minted.run_id
        );
        assert_eq!(
            std::fs::read(runs.join(&taken).join(REPORT_FILE)).unwrap(),
            b"the first run's result",
            "the run already holding that id keeps its report"
        );
    }

    /// The id opens with the second the run started, so a plain directory listing sorts by when —
    /// the one ordering a human scanning `user_data/runs/` actually wants. True in BOTH forms;
    /// that is what makes a content address safe to put in the name at all.
    #[test]
    fn a_run_id_opens_with_the_second_its_run_started() {
        let root = tempfile::tempdir().unwrap();
        let runs = root.path().join("runs");

        for fp in [None, Some(FP)] {
            let minted = create_run_dir(&runs, 1_756_000_000, fp).unwrap();
            assert!(
                minted.run_id.starts_with("1756000000-"),
                "run id {} must open with its start second",
                minted.run_id
            );
        }
    }

    /// The whole reason this manifest is common: a listing renders a row off the TOP level, without
    /// knowing what kind of run produced the file.
    #[test]
    fn every_common_field_sits_at_the_top_level_so_a_listing_needs_no_per_kind_parser() {
        let v = serde_json::to_value(a_manifest("r-1")).unwrap();
        let obj = v.as_object().expect("a manifest is a JSON object");

        for key in [
            "schema",
            "run_id",
            "kind",
            "produced_by",
            "started_at",
            "finished_at",
            "git_sha",
            "fingerprint",
            "config",
        ] {
            assert!(obj.contains_key(key), "a listing row needs `{key}` at the top level");
        }
    }

    /// Kind-specific detail nests BELOW the common fields. Beside them is the defect the nesting
    /// exists to refuse — it forces every reader to know the kind before it can read the first key.
    #[test]
    fn kind_specific_detail_nests_below_the_common_fields_rather_than_beside_them() {
        let v = serde_json::to_value(a_manifest("r-1")).unwrap();
        let obj = v.as_object().unwrap();

        assert_eq!(obj["detail"]["strategy"], json!("sma_cross"));
        assert!(
            !obj.contains_key("strategy"),
            "`strategy` is a backtest's business and must not sit beside the common fields"
        );
    }

    /// `git_sha` is written even when the producer cannot name a build. A missing key would be
    /// indistinguishable from a manifest predating the field; `null` says "this producer does not
    /// know", which is a different and reportable answer.
    #[test]
    fn a_producer_that_cannot_name_a_build_still_writes_the_git_sha_key() {
        let v = serde_json::to_value(a_manifest("r-1")).unwrap();

        assert_eq!(v.as_object().unwrap().get("git_sha"), Some(&serde_json::Value::Null));
    }

    /// A run directory holds BOTH documents, and the report is stored verbatim — a report's JSON is
    /// a machine contract, and persisting it must not reshape it.
    #[test]
    fn a_written_run_holds_the_report_verbatim_beside_its_manifest() {
        let root = tempfile::tempdir().unwrap();
        let run = create_run_dir(&root.path().join("runs"), 1_756_000_000, None).unwrap();
        let report = json!({ "name": "sma cross", "sharpe": 1.25, "trades": 42 });

        write_run(&run.path, &a_manifest(&run.run_id), &report).unwrap();

        let back: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(run.path.join(REPORT_FILE)).unwrap())
                .unwrap();
        assert_eq!(back, report);
        let m: RunManifest =
            serde_json::from_str(&std::fs::read_to_string(run.path.join(MANIFEST_FILE)).unwrap())
                .unwrap();
        assert_eq!(m.run_id, run.run_id);
        assert_eq!(m.kind, "backtest");
    }

    /// Persisting is additive, never a new way to fail: a runs root that cannot be created comes
    /// back as a VALUE naming the path, so the caller can print the result it already computed and
    /// report the failure beside it.
    #[test]
    fn a_runs_root_that_cannot_be_created_is_reported_rather_than_panicking() {
        let root = tempfile::tempdir().unwrap();
        let blocked = root.path().join("blocked");
        std::fs::write(&blocked, b"a file where the runs directory would go").unwrap();

        let err = create_run_dir(&blocked, 1_756_000_000, None).unwrap_err();

        assert!(
            matches!(err, RunPersistError::Dir { .. }),
            "expected a directory failure, got {err:?}"
        );
        assert!(err.to_string().contains("blocked"), "the message must name the path: {err}");
    }

    /// The round trip that makes a listing possible at all: what [`write_run`] wrote comes back as
    /// the same COMMON fields, through the reader that lives beside the writer rather than through
    /// a second spelling of the schema somewhere up the dependency graph.
    #[test]
    fn a_written_manifest_reads_back_with_every_common_field_intact() {
        let root = tempfile::tempdir().unwrap();
        let run = create_run_dir(&root.path().join("runs"), 1_756_000_000, None).unwrap();
        let written = a_manifest(&run.run_id);
        write_run(&run.path, &written, &json!({ "sharpe": 1.25 })).unwrap();

        let back = read_manifest(&run.path).unwrap();

        assert_eq!(back.run_id, written.run_id);
        assert_eq!(back.kind, "backtest");
        assert_eq!(back.produced_by, "backtest");
        assert_eq!(back.started_at, written.started_at);
        assert_eq!(back.finished_at, written.finished_at);
        assert_eq!(back.git_sha, None);
        assert_eq!(back.config.path.as_deref(), Some("profiles/sma.toml"));
        assert_eq!(back.detail["strategy"], json!("sma_cross"));
    }

    /// The manifest is written LAST, so its ABSENCE is a distinct answer rather than a corrupt
    /// file: the run is being written right now, or a process died between the two writes. A reader
    /// that collapsed this into "unreadable" would make a listing call a running backtest broken.
    #[test]
    fn a_directory_whose_manifest_is_absent_reads_as_missing_not_as_unreadable() {
        let root = tempfile::tempdir().unwrap();
        let run = create_run_dir(&root.path().join("runs"), 1_756_000_000, None).unwrap();
        // The half-written shape exactly: the report landed, the manifest has not.
        std::fs::write(run.path.join(REPORT_FILE), b"{}\n").unwrap();

        let err = read_manifest(&run.path).unwrap_err();

        assert!(matches!(err, RunReadError::Missing { .. }), "expected Missing, got {err:?}");
        assert!(
            err.to_string().contains(MANIFEST_FILE),
            "the message must name the file it looked for: {err}"
        );
    }

    /// A corrupt manifest is REPORTED, never treated as absent — a run that vanishes from a listing
    /// is worse than one that shows as broken, and the two have different fixes.
    #[test]
    fn a_manifest_that_is_not_json_is_reported_with_the_parser_error_and_the_path() {
        let root = tempfile::tempdir().unwrap();
        let run = create_run_dir(&root.path().join("runs"), 1_756_000_000, None).unwrap();
        std::fs::write(run.path.join(MANIFEST_FILE), b"{ not json").unwrap();

        let err = read_manifest(&run.path).unwrap_err();

        match &err {
            RunReadError::Parse { path, why } => {
                assert!(path.ends_with(MANIFEST_FILE), "the path must be the manifest: {path:?}");
                assert!(!why.is_empty(), "serde_json's own words must be carried through");
            }
            other => panic!("expected Parse, got {other:?}"),
        }
    }

    /// A manifest that parses as JSON but is missing a COMMON field is a parse failure too: the
    /// common fields are what a listing renders a row from, so a document without them is not a
    /// manifest, whatever it is.
    #[test]
    fn a_json_document_missing_a_common_field_is_a_parse_failure() {
        let root = tempfile::tempdir().unwrap();
        let run = create_run_dir(&root.path().join("runs"), 1_756_000_000, None).unwrap();
        std::fs::write(run.path.join(MANIFEST_FILE), br#"{ "run_id": "r-1" }"#).unwrap();

        let err = read_manifest(&run.path).unwrap_err();

        assert!(matches!(err, RunReadError::Parse { .. }), "expected Parse, got {err:?}");
    }

    /// A manifest that exists and cannot be READ is its own answer — a permissions bug must not
    /// wear the "not written yet" reply, for the same reason the credential store refuses to.
    #[test]
    fn a_manifest_that_cannot_be_read_is_distinct_from_one_that_is_absent() {
        let root = tempfile::tempdir().unwrap();
        let run = create_run_dir(&root.path().join("runs"), 1_756_000_000, None).unwrap();
        // A DIRECTORY where the file belongs: unreadable as text on every platform this ships to,
        // and reached without asking a test to change file permissions.
        std::fs::create_dir(run.path.join(MANIFEST_FILE)).unwrap();

        let err = read_manifest(&run.path).unwrap_err();

        assert!(matches!(err, RunReadError::Read { .. }), "expected Read, got {err:?}");
        assert!(err.to_string().contains(MANIFEST_FILE), "the message must name the path: {err}");
    }

    /// Under the cap nothing is touched: a stride of `1` is the claim "this is the whole curve",
    /// and a reader keys on it to know whether a statistic recomputed from the file is exact.
    #[test]
    fn a_curve_that_fits_under_the_cap_is_kept_whole_at_stride_one() {
        let samples: Vec<f64> = (0..100).map(f64::from).collect();

        let (kept, stride) = decimate(&samples, 1_000);

        assert_eq!(kept, samples, "nothing may be dropped under the cap");
        assert_eq!(stride, 1, "stride 1 IS the exactness claim");
    }

    /// The bound is the whole point: a curve far over the cap comes back bounded, and the stride
    /// says by how much it was thinned.
    #[test]
    fn a_curve_over_the_cap_is_thinned_to_the_cap_and_says_by_how_much() {
        let samples: Vec<u32> = (0..100_000).collect();

        let (kept, stride) = decimate(&samples, 1_000);

        assert!(stride > 1, "a thinned curve must not claim stride 1");
        assert!(
            kept.len() <= 1_001,
            "the cap is soft by exactly one — the appended last sample: {}",
            kept.len()
        );
        assert_eq!(kept[0], 0, "the first sample is the run's opening equity");
    }

    /// The LAST sample is the run's OUTCOME. A decimation that dropped it would make
    /// `final_equity` derived from the file disagree with the one in `report.json`, which is the
    /// single comparison a reader is most likely to make.
    #[test]
    fn the_final_sample_survives_a_stride_that_does_not_land_on_it() {
        // 8 samples into a cap of 3 gives stride 3: indices 0, 3, 6 — and 7 is the one that must
        // be appended, because the stride does not land on it.
        let samples: Vec<u32> = (0..8).collect();

        let (kept, stride) = decimate(&samples, 3);

        assert_eq!(stride, 3);
        assert_eq!(*kept.last().unwrap(), 7, "the last sample must survive: {kept:?}");
        assert_eq!(kept, vec![0, 3, 6, 7]);
    }

    /// A cap of zero is "keep nothing" — the shape a future `--keep-series none` spells — and it
    /// reports stride `0` so a reader can tell it apart from an empty run.
    #[test]
    fn a_cap_of_zero_keeps_nothing_and_says_so_with_stride_zero() {
        let samples: Vec<u32> = (0..10).collect();

        let (kept, stride) = decimate(&samples, 0);

        assert!(kept.is_empty());
        assert_eq!(stride, 0, "stride 0 means NOTHING was kept, not `kept everything`");
    }

    /// An empty curve is a real run (a slice with no rows), not an error, and it claims exactness.
    #[test]
    fn an_empty_curve_is_exact_rather_than_thinned() {
        let samples: Vec<f64> = Vec::new();

        let (kept, stride) = decimate(&samples, 1_000);

        assert!(kept.is_empty());
        assert_eq!(stride, 1);
    }

    fn a_series() -> RunSeries {
        RunSeries {
            schema: SERIES_SCHEMA,
            equity: vec![10_000.0, 10_100.0, 9_950.0],
            equity_ts: vec![1_756_000_000_000, 1_756_000_060_000, 1_756_000_120_000],
            per_symbol_equity: vec![("BTCUSDT".to_string(), vec![0.0, 100.0, -50.0])],
            stride: 1,
            source_len: 3,
            diagnostics: RunDiagnostics {
                warmup: 20,
                intrabar_both_hit: 1,
                stale_deferrals: 2,
                impact_unpriced: 3,
                session_deferrals: 4,
                below_min_reversals: 5,
                maker_fills: 6,
                taker_fills: 7,
                fees_paid: 8.5,
                dropped: vec![DroppedOrder {
                    symbol: "BTCUSDT".to_string(),
                    reason: "risk_gate:max_notional".to_string(),
                    size: 0.5,
                    weight: 1.0,
                }],
            },
        }
    }

    fn a_trade(pnl: f64) -> crate::Trade {
        crate::Trade {
            entry_price: 100.0,
            exit_price: 100.0 + pnl,
            size: 1.0,
            pnl,
            fees: 0.1,
            entry_ts: 1_756_000_000_000,
            exit_ts: 1_756_000_060_000,
            symbol: "BTCUSDT".to_string(),
            mae: 0.0,
            mfe: pnl.max(0.0),
            is_long: true,
        }
    }

    /// The whole point of the stage: what the run computed comes BACK, typed, through the reader
    /// that lives beside the writer. A blob would have made the schema version meaningless.
    #[test]
    fn a_written_series_reads_back_with_every_sample_and_every_counter_intact() {
        let root = tempfile::tempdir().unwrap();
        let run = create_run_dir(&root.path().join("runs"), 1_756_000_000, None).unwrap();
        let written = a_series();
        let extras = RunExtras { series: Some(&written), ..Default::default() };

        write_run_with(&run.path, &a_manifest(&run.run_id), &json!({}), &extras).unwrap();
        let back = read_series(&run.path).unwrap();

        assert_eq!(back.schema, SERIES_SCHEMA);
        assert_eq!(back.equity, written.equity);
        assert_eq!(back.equity_ts, written.equity_ts);
        assert_eq!(back.per_symbol_equity, written.per_symbol_equity);
        assert_eq!(back.stride, 1);
        assert_eq!(back.source_len, 3);
        assert_eq!(back.diagnostics.warmup, 20);
        assert_eq!(back.diagnostics.stale_deferrals, 2);
        assert_eq!(back.diagnostics.dropped.len(), 1);
        assert_eq!(back.diagnostics.dropped[0].reason, "risk_gate:max_notional");
    }

    /// The parallel-vector hazard, refused by an invariant a reader can CHECK rather than by a
    /// convention it has to trust: `vike_analytics::metrics::returns` SKIPS zero-denominator steps,
    /// so a returns vector is not index-alignable with a timestamp vector — which is exactly why
    /// this record persists the CURVE and lets a reader derive returns from it.
    #[test]
    fn a_series_is_aligned_when_its_timestamps_match_its_samples_or_are_absent() {
        assert!(a_series().is_aligned(), "equal lengths are aligned");

        let untimed = RunSeries { equity_ts: Vec::new(), ..a_series() };
        assert!(untimed.is_aligned(), "an untracked-timestamp run is the other valid shape");

        let ragged = RunSeries { equity_ts: vec![1, 2], ..a_series() };
        assert!(
            !ragged.is_aligned(),
            "two lengths that are neither equal nor empty is not a document"
        );
    }

    /// A ledger is a PREFIX when it is bounded, never a sample: `source_len` is what tells a reader
    /// it is holding part of a ledger rather than all of a short one.
    #[test]
    fn a_bounded_trade_ledger_says_how_many_trades_the_run_actually_closed() {
        let root = tempfile::tempdir().unwrap();
        let run = create_run_dir(&root.path().join("runs"), 1_756_000_000, None).unwrap();
        let written = RunTrades {
            schema: TRADES_SCHEMA,
            trades: vec![a_trade(1.0), a_trade(-2.0)],
            source_len: 900,
        };
        let extras = RunExtras { trades: Some(&written), ..Default::default() };

        write_run_with(&run.path, &a_manifest(&run.run_id), &json!({}), &extras).unwrap();
        let back = read_trades(&run.path).unwrap();

        assert_eq!(back.trades.len(), 2);
        assert_eq!(back.source_len, 900, "the ledger is a prefix and must say so");
        assert_eq!(back.trades[1].pnl, -2.0);
    }

    /// A run that kept no series is the ordinary shape of every run written before this stage, and
    /// of any producer with no curve. It must read as MISSING — the same distinct answer the
    /// manifest's absence already carries — never as a corrupt document.
    #[test]
    fn a_run_with_no_series_reads_as_missing_rather_than_broken() {
        let root = tempfile::tempdir().unwrap();
        let run = create_run_dir(&root.path().join("runs"), 1_756_000_000, None).unwrap();
        write_run(&run.path, &a_manifest(&run.run_id), &json!({ "sharpe": 1.0 })).unwrap();

        let err = read_series(&run.path).unwrap_err();

        assert!(matches!(err, RunReadError::Missing { .. }), "expected Missing, got {err:?}");
        assert!(err.to_string().contains(SERIES_FILE), "the message must name the file: {err}");
    }

    /// The ORDERING contract, extended to the new documents and proved rather than asserted in
    /// prose: the manifest is the COMPLETION MARKER, so a failure part-way through must leave the
    /// documents that DID land and no manifest. A directory where `report.json` belongs is an
    /// unwritable path on every platform this ships to, reached without changing permissions.
    #[test]
    fn a_failure_writing_the_report_leaves_the_extras_and_no_manifest() {
        let root = tempfile::tempdir().unwrap();
        let run = create_run_dir(&root.path().join("runs"), 1_756_000_000, None).unwrap();
        std::fs::create_dir(run.path.join(REPORT_FILE)).unwrap();
        let series = a_series();
        let extras = RunExtras { series: Some(&series), ..Default::default() };

        let err =
            write_run_with(&run.path, &a_manifest(&run.run_id), &json!({}), &extras).unwrap_err();

        assert!(matches!(err, RunPersistError::Write { .. }), "expected Write, got {err:?}");
        assert!(run.path.join(SERIES_FILE).is_file(), "the series landed before the report");
        assert!(
            !run.path.join(MANIFEST_FILE).is_file(),
            "the manifest is the completion marker and must NOT exist after a failed write"
        );
    }

    /// Every `*_FILE` const named between `fn <name>(` and the first line that is a lone `}`.
    ///
    /// Deliberately a scan over this module's OWN SOURCE rather than a list: the point of the test
    /// below is that the roster cannot fall behind the writer, and a hand-written list of what the
    /// writer writes is the thing that falls behind.
    fn file_consts_in(text: &str) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let mut tok = String::new();
        for c in text.chars() {
            if c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_' {
                tok.push(c);
                continue;
            }
            if tok.ends_with("_FILE") && !out.contains(&tok) {
                out.push(tok.clone());
            }
            tok.clear();
        }
        if tok.ends_with("_FILE") && !out.contains(&tok) {
            out.push(tok);
        }
        out.sort();
        out
    }

    /// The source between `fn <name>(` and the first line that is a lone `}` at column zero — the
    /// shape `crates/vike-backtest/tests/run_record_completeness.rs` already uses.
    fn fn_body_of(text: &str, name: &str) -> String {
        // ⚠ `fn <name>` then `(` OR `<`. A needle carrying the paren (`fn write_run_with(`) matches
        // NOTHING on a GENERIC function — `write_run_with<R>` is one — and the harvest then returns an
        // empty body, which is a gate that has silently gone blind rather than one that fails.
        // Measured: the first spelling of this test reported `left: []`.
        let needle = format!("fn {name}");
        let mut out = String::new();
        let mut inside = false;
        for line in text.lines() {
            if !inside {
                // ⚠ The line must BE a definition, not merely mention one. The floor below
                // catches a ZERO harvest; it cannot catch a MIS-ANCHORED one, and this file
                // already contains the literal `fn write_run_with(` inside a comment. Anchored
                // there, the scan would sweep a region that happens to contain every name it
                // looks for, and the assertion would pass while measuring nothing.
                let def = line.trim_start();
                let is_definition = def.starts_with("fn ") || def.starts_with("pub fn ");
                // ONE condition, not three nested `if`s: clippy's `collapsible_if` refuses the
                // nested spelling at `-D warnings`, which is the merge gate. Edition 2024
                // let-chains are what make the whole test one expression.
                if is_definition
                    && let Some(i) = line.find(&needle)
                    && matches!(line[i + needle.len()..].chars().next(), Some('(') | Some('<'))
                {
                    inside = true;
                }
                continue;
            }
            if line == "}" {
                break;
            }
            out.push_str(line);
            out.push('\n');
        }
        out
    }

    /// Every function in this module that writes a file INTO a run directory, by name.
    ///
    /// ⚠ **It is a LIST because it stopped being one function**, and the change is the whole reason
    /// this array exists. [`write_run_with`] writes the five documents a run is made of; [`add_tags`]
    /// writes [`META_FILE`] long AFTER the run finished, which is exactly what lets a tag be
    /// optional. Both land in the same namespace, so both must be reserved against, and the roster
    /// is therefore "every name this MODULE writes" rather than "every name the run WRITER writes".
    /// A third writer added here and not to [`RESERVED_FILES`] reddens
    /// `every_document_this_module_writes_is_in_the_reserved_roster`; a third writer added to
    /// NEITHER is the hole this array cannot see, and is why each entry is a deliberate act.
    const RUN_DIRECTORY_WRITERS: &[&str] = &["write_run_with", "add_tags"];

    /// One roster of reserved names, so a producer writing its own artifacts cannot collide with a
    /// document this module writes. `crates/vike-studio-core/src/study_run.rs`'s `persist` checked
    /// exactly two names and there are six.
    ///
    /// ⚠ **DERIVED from the writers' own bodies, on both sides.** This test first compared a
    /// hand-written array against [`RESERVED_FILES`] — which IS that array, so it compared a copy
    /// with itself and a SIXTH document added to the writer and not to the roster passed it. That
    /// is precisely the failure the roster exists to prevent, so the test reads the writers instead.
    #[test]
    fn every_document_this_module_writes_is_in_the_reserved_roster() {
        let src = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join("runs.rs"),
        )
        .expect(
            "this module's own source — read at run time, never `include_str!`, so the \
                 published mirror can withhold a file without breaking the build",
        );

        let mut written: Vec<String> = Vec::new();
        for writer in RUN_DIRECTORY_WRITERS {
            let body = fn_body_of(&src, writer);
            let found = file_consts_in(&body);
            // ⚠ PER-WRITER, not only over the union: a name that was renamed or reshaped yields an
            // empty body, and the union's floor below would still pass on the other writer's five.
            assert!(
                !found.is_empty(),
                "the harvest found nothing in `{writer}` — it was renamed or reshaped, and this \
                 gate is now measuring one writer fewer than it claims"
            );
            for name in found {
                if !written.contains(&name) {
                    written.push(name);
                }
            }
        }
        written.sort();
        let reserved = file_consts_in(&decl_of(&src, "pub const RESERVED_FILES"));

        // The floor: a harvester that has stopped matching passes every assertion by seeing
        // nothing, which is how a derived gate quietly becomes a no-op.
        assert!(
            written.len() >= 6,
            "the writer harvest found {written:?} across {RUN_DIRECTORY_WRITERS:?}"
        );
        assert!(reserved.len() >= 6, "the roster harvest found {reserved:?}");

        for name in &written {
            assert!(
                reserved.contains(name),
                "`{name}` is written by one of {RUN_DIRECTORY_WRITERS:?} and is NOT in \
                 RESERVED_FILES — a producer writing its own artifact under that name would \
                 silently overwrite a document this module writes. Add it to the roster.\n  \
                 writers: {written:?}\n  roster:  {reserved:?}"
            );
        }
        assert_eq!(
            written, reserved,
            "the roster and the writers must name the SAME set — a reserved name nothing writes \
             refuses a producer's artifact for no reason"
        );
    }

    /// The source between `head` and the first `;` — [`RESERVED_FILES`]'s declaration.
    fn decl_of(text: &str, head: &str) -> String {
        let i = text.find(head).expect("declaration not found — was it renamed?");
        let rest = &text[i..];
        let end = rest.find(';').map(|e| i + e).unwrap_or(text.len());
        text[i..end].to_string()
    }

    /// The harvesters' own proof, over planted text: a scan that has gone blind passes the test
    /// above by finding nothing on BOTH sides, and the length floors are a blunt instrument beside
    /// this.
    #[test]
    fn the_writer_harvest_reads_a_body_and_takes_only_file_consts() {
        let planted = "\
pub fn write_run_with<R>(dir: &Path) -> u8 {
    write_text(&dir.join(CONFIG_FILE), toml)?;
    write_json(&dir.join(SERIES_FILE), SERIES_FILE, series)?;
    write_json(&dir.join(MANIFEST_FILE), MANIFEST_FILE, manifest)
}

pub fn something_else() {
    let _ = NOT_MINE_FILE;
}
";

        assert_eq!(
            file_consts_in(&fn_body_of(planted, "write_run_with")),
            vec!["CONFIG_FILE".to_string(), "MANIFEST_FILE".to_string(), "SERIES_FILE".to_string()],
            "every `*_FILE` const the body names, deduplicated and sorted, and nothing from the \
             function after it"
        );
        assert_eq!(
            file_consts_in(&decl_of(
                "pub const RESERVED_FILES: &[&str] = &[MANIFEST_FILE, CONFIG_FILE];\nnext",
                "pub const RESERVED_FILES"
            )),
            vec!["CONFIG_FILE".to_string(), "MANIFEST_FILE".to_string()],
            "and the roster side reads the same way"
        );
        assert!(
            !file_consts_in("let x = MAX_TRADES;").contains(&"MAX_TRADES".to_string()),
            "a SCREAMING const that is not a `*_FILE` must not join either side"
        );
    }

    /// ⚠ **THE BACK-COMPATIBILITY PROOF, and the reason both new fields carry
    /// `#[serde(default)]`.** Every field but `detail` is required at deserialize time, so a
    /// document written before these fields existed becomes `RunReadError::Parse` the moment one is
    /// required — which `crates/vike-studio-core/src/listing.rs`'s `list_runs` renders as
    /// `RunUnreadable` and DROPS. Written as TEXT rather than through the writer, deliberately: the
    /// bytes on somebody's disk are what this test is about, and a round trip through the current
    /// struct could never see the loss.
    #[test]
    fn a_manifest_written_before_the_schema_field_existed_still_reads() {
        let root = tempfile::tempdir().unwrap();
        let run = create_run_dir(&root.path().join("runs"), 1_756_000_000, None).unwrap();
        std::fs::write(
            run.path.join(MANIFEST_FILE),
            br#"{
  "run_id": "1756000000-4242-0",
  "kind": "backtest",
  "produced_by": "backtest",
  "started_at": "2026-08-24T09:15:04Z",
  "finished_at": "2026-08-24T09:15:16Z",
  "git_sha": null,
  "config": { "path": "profiles/sma.toml", "name": "sma cross" },
  "detail": { "strategy": "sma_cross" }
}
"#,
        )
        .unwrap();

        let back = read_manifest(&run.path).unwrap();

        assert_eq!(
            back.schema, 0,
            "absence means PRE-SCHEMA, which is a real and reportable answer"
        );
        assert_eq!(back.fingerprint, None, "a manifest predating the address names none");
        assert_eq!(back.kind, "backtest", "and every field it DID carry is untouched");
    }

    /// A manifest this build writes states its own version, so a decoder never has to guess.
    #[test]
    fn a_freshly_written_manifest_states_the_schema_this_build_writes() {
        let v = serde_json::to_value(a_manifest("r-1")).unwrap();

        assert_eq!(v["schema"], json!(MANIFEST_SCHEMA));
    }

    /// `fingerprint` follows `git_sha`'s rule exactly: written even when the producer cannot name
    /// one, because "does not know" and "wrote no such field" are different answers to a reader.
    #[test]
    fn a_producer_that_cannot_address_its_inputs_still_writes_the_fingerprint_key() {
        let v = serde_json::to_value(a_manifest("r-1")).unwrap();

        assert_eq!(v.as_object().unwrap().get("fingerprint"), Some(&serde_json::Value::Null));
    }

    /// The kind a backtest writes is EXPORTED now, so the two places that spell it cannot drift —
    /// `crates/vike-studio/src/research.rs` carried its own copy and said in a comment that this
    /// is what should replace it.
    #[test]
    fn the_backtest_kind_is_exported_and_is_what_this_producer_writes() {
        assert_eq!(BACKTEST_RUN_KIND, "backtest");
        assert_eq!(a_manifest("r-1").kind, BACKTEST_RUN_KIND);
    }

    const FP: &str = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";

    /// The content-addressing property, stated as what it is FOR: two runs over the same inputs
    /// carry the same address in their names, so a human scanning `user_data/runs/` sees at a
    /// glance which runs are comparable. The full digest lives in the manifest; this is the
    /// legible prefix of it.
    #[test]
    fn two_runs_with_the_same_inputs_carry_the_same_address_in_their_ids() {
        let root = tempfile::tempdir().unwrap();
        let runs = root.path().join("runs");

        let first = create_run_dir(&runs, 1_756_000_000, Some(FP)).unwrap();
        let second = create_run_dir(&runs, 1_756_000_000, Some(FP)).unwrap();

        assert_ne!(first.run_id, second.run_id, "two runs are still two directories");
        assert!(first.run_id.starts_with("1756000000-9f86d081884c7d65-"), "{}", first.run_id);
        assert!(second.run_id.starts_with("1756000000-9f86d081884c7d65-"), "{}", second.run_id);
        assert!(second.run_id.ends_with("-1"), "the second takes the next seq: {}", second.run_id);
    }

    /// ⚠ **THE ORDERING CONTRACT.** `crates/vike-studio-core/src/listing.rs`'s `list_runs` sorts by
    /// DIRECTORY NAME and its module doc says that is chronological because the id opens with unix
    /// seconds; `crates/vike-studio/src/research.rs` reverses that list to get "Newest first". A
    /// bare content hash would make both arbitrary and silently wrong, which is why the address is
    /// a SUFFIX.
    #[test]
    fn a_content_address_does_not_disturb_the_chronological_name_sort() {
        let mut ids = vec![
            run_id_at(1_756_000_200, Some("ffffffffffffffff"), 0),
            run_id_at(1_756_000_000, Some("0000000000000000"), 0),
            run_id_at(1_756_000_100, Some("aaaaaaaaaaaaaaaa"), 0),
        ];
        ids.sort();

        assert_eq!(
            ids,
            vec![
                "1756000000-0000000000000000-0".to_string(),
                "1756000100-aaaaaaaaaaaaaaaa-0".to_string(),
                "1756000200-ffffffffffffffff-0".to_string(),
            ],
            "a plain name sort must still be chronological whatever the addresses are"
        );
    }

    /// A producer that cannot address its inputs keeps the shape that was there before, pid and
    /// all — so the study producer and every pre-address run sort and read exactly as they did.
    #[test]
    fn a_run_with_no_address_keeps_the_pid_form() {
        let pid = std::process::id();

        assert_eq!(run_id_at(1_756_000_000, None, 3), format!("1756000000-{pid}-3"));
    }

    /// The id is a DIRECTORY NAME, so an address is sanitized before it becomes one: a value
    /// carrying separators would otherwise be a path traversal built out of something a producer
    /// computed.
    #[test]
    fn an_address_that_is_not_bare_hex_cannot_escape_the_runs_root() {
        let root = tempfile::tempdir().unwrap();
        let runs = root.path().join("runs");

        let minted = create_run_dir(&runs, 1_756_000_000, Some("../../etc/passwd")).unwrap();

        assert_eq!(minted.path.parent(), Some(runs.as_path()), "minted outside the runs root");
        assert!(!minted.run_id.contains('/') && !minted.run_id.contains('\\'));
        assert!(!minted.run_id.contains(".."), "run id {}", minted.run_id);
    }

    /// An address with nothing usable in it is the same answer as no address at all, rather than
    /// an empty segment that would make two ids collide on a name a human cannot read.
    #[test]
    fn an_address_with_no_usable_characters_falls_back_to_the_pid_form() {
        let pid = std::process::id();

        assert_eq!(run_id_at(1_756_000_000, Some("///"), 0), format!("1756000000-{pid}-0"));
    }

    /// ⚠ The formatter is no longer `chrono`'s — it is [`crate::time::civil_from_days`], because
    /// this module moved into the crate at the BOTTOM of the graph and `chrono` may not follow it
    /// there (`crates/vike-cli/Cargo.toml`'s whole identity is being light). These instants are the
    /// pins: each was MEASURED against the chrono implementation this replaced, in the commit before
    /// the move, so a divergence is a REGRESSION in a document already on people's disks rather than
    /// a formatting preference.
    #[test]
    fn the_chrono_free_formatter_is_byte_identical_to_the_one_it_replaced() {
        for (secs, expect) in [
            (0_i64, "1970-01-01T00:00:00Z"),
            (1_756_000_000, "2025-08-24T01:46:40Z"),
            (1_756_000_012, "2025-08-24T01:46:52Z"),
            (951_782_400, "2000-02-29T00:00:00Z"),
            (-1, "1969-12-31T23:59:59Z"),
            (-86_400, "1969-12-31T00:00:00Z"),
            (253_402_300_799, "9999-12-31T23:59:59Z"),
        ] {
            assert_eq!(utc_rfc3339(secs), expect, "{secs}");
        }
    }

    /// The fallback the old doc promised: a second no calendar date can hold must not panic. With
    /// integer-exact civil math there is no such second inside `i64::MIN/86_400`, so the guard is
    /// the four-digit-year boundary instead — and it must still return a string rather than abort.
    #[test]
    fn an_unrepresentable_second_falls_back_to_the_raw_number_instead_of_panicking() {
        assert_eq!(utc_rfc3339(i64::MIN), i64::MIN.to_string());
        assert_eq!(utc_rfc3339(i64::MAX), i64::MAX.to_string());
    }

    // ─── the tag sidecar and the mark store ─────────────────────────────────────────────────────

    /// A FINISHED run directory: a manifest, which is the completion marker this module's doc
    /// describes. Written as TEXT rather than through `write_run`, so these cases pin the on-disk
    /// document a later reader has to survive rather than a round trip of our own struct.
    fn plant(runs: &Path, run_id: &str, kind: &str) -> PathBuf {
        let dir = runs.join(run_id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(MANIFEST_FILE),
            format!(
                r#"{{"run_id":"{run_id}","kind":"{kind}","produced_by":"backtest",
                     "started_at":"2025-08-24T01:46:40Z","finished_at":"2025-08-24T01:46:41Z",
                     "git_sha":null,"config":{{"path":"p.toml","name":null}},"detail":null}}"#
            ),
        )
        .unwrap();
        dir
    }

    /// Tags are a SET with a stable order: adding one twice does not duplicate it, and the order is
    /// FIRST-INSERT so a rendered row does not shuffle between calls.
    #[test]
    fn tags_dedupe_and_keep_first_insert_order() {
        let root = tempfile::tempdir().unwrap();
        let dir = plant(&root.path().join("runs"), "1756000000-1-0", "backtest");

        let m = add_tags(&dir, &["ci".into(), "fee-fix".into()], None, 1_756_000_000).unwrap();
        assert_eq!(m.tags, vec!["ci".to_string(), "fee-fix".to_string()]);
        let m = add_tags(&dir, &["fee-fix".into(), "green".into()], None, 1_756_000_001).unwrap();
        assert_eq!(m.tags, vec!["ci".to_string(), "fee-fix".to_string(), "green".to_string()]);
        assert_eq!(m.schema, META_SCHEMA, "the schema key ships WITH the document, never after it");
        // …and it is on DISK with the schema, not merely in the returned value.
        let on_disk = read_meta(&dir).unwrap();
        assert_eq!(on_disk.tags, m.tags);
        assert_eq!(on_disk.schema, META_SCHEMA);
    }

    /// Notes APPEND and never overwrite. A note is evidence — §7.2's "re-marking is explicit and
    /// recorded" is the same instinct — so the second one must not delete the first.
    #[test]
    fn notes_append_with_their_own_timestamps() {
        let root = tempfile::tempdir().unwrap();
        let dir = plant(&root.path().join("runs"), "1756000000-1-0", "backtest");

        add_tags(&dir, &[], Some("first look"), 1_756_000_000).unwrap();
        let m = add_tags(&dir, &[], Some("after the fee fix"), 1_756_000_060).unwrap();
        assert_eq!(m.notes.len(), 2);
        assert_eq!(m.notes[0].text, "first look");
        assert_eq!(m.notes[1].at, utc_rfc3339(1_756_000_060));
    }

    /// An ABSENT sidecar is an EMPTY one, never an error: a run minted before tagging existed, or
    /// one nobody has tagged, is the ordinary case and not a broken run.
    #[test]
    fn a_run_with_no_sidecar_reads_as_empty() {
        let root = tempfile::tempdir().unwrap();
        let dir = plant(&root.path().join("runs"), "1756000000-1-0", "backtest");
        let m = read_meta(&dir).unwrap();
        assert!(m.tags.is_empty() && m.notes.is_empty());
        assert!(!dir.join(META_FILE).exists(), "reading must not create one");
    }

    /// ⚠ A sidecar that EXISTS and will not parse is a REFUSAL, not a re-mint. The file is the only
    /// copy of whatever somebody wrote in it, and overwriting it from an empty document would
    /// delete their notes silently — the one outcome a metadata write may not have.
    #[test]
    fn an_unparseable_sidecar_is_refused_rather_than_overwritten() {
        let root = tempfile::tempdir().unwrap();
        let dir = plant(&root.path().join("runs"), "1756000000-1-0", "backtest");
        std::fs::write(dir.join(META_FILE), "not json at all").unwrap();

        let err = add_tags(&dir, &["ci".into()], None, 1_756_000_000).unwrap_err();
        assert!(err.to_string().contains(META_FILE), "the message names the file: {err}");
        assert_eq!(
            std::fs::read_to_string(dir.join(META_FILE)).unwrap(),
            "not json at all",
            "and the bytes on disk are untouched"
        );
    }

    /// A mark is a POINTER, and moving it RECORDS where it pointed — §7.2: "Re-marking is explicit
    /// and recorded", which is only true if the move is kept where somebody can see it.
    #[test]
    fn re_marking_records_what_moved() {
        let root = tempfile::tempdir().unwrap();
        let marks = root.path().join("marks");

        let first =
            write_mark(&marks, "baseline/momentum", "1756000000-1-0", Some("v1"), 1_756_000_000)
                .unwrap();
        assert!(first.history.is_empty(), "the first mark has nothing to record");
        assert_eq!(first.schema, META_SCHEMA);

        let second =
            write_mark(&marks, "baseline/momentum", "1799999999-2-0", None, 1_799_999_999).unwrap();
        assert_eq!(second.run_id, "1799999999-2-0");
        assert_eq!(second.history.len(), 1, "the previous pointer is kept");
        assert_eq!(second.history[0].run_id, "1756000000-1-0");
        assert_eq!(second.history[0].note.as_deref(), Some("v1"));

        assert_eq!(read_mark(&marks, "baseline/momentum").unwrap().run_id, "1799999999-2-0");
        assert!(
            marks.join("baseline").join("momentum.json").is_file(),
            "a mark is a file NAMED for the mark"
        );
    }

    /// A mark NAME becomes a PATH, so traversal, invisibility and reserved device names are refused
    /// at the door rather than left to the filesystem.
    ///
    /// ⚠ The Windows device names matter here even though NO test in this workspace runs on Windows:
    /// a mark called `con` would be written and resolved here and be unopenable there, and nothing
    /// downstream would ever discover it.
    #[test]
    fn a_mark_name_that_would_escape_or_break_a_path_is_refused() {
        for ok in ["baseline/momentum", "prod", "v1.2_rc-3", "nightly/eu/open"] {
            assert!(valid_mark_name(ok).is_ok(), "`{ok}` must be accepted");
        }
        for bad in [
            "",
            "/leading",
            "trailing/",
            "a//b",
            "../escape",
            "a/../b",
            ".hidden",
            "with space",
            "with\\backslash",
            "with:colon",
            "a/b/c/d/e",
            "con",
            "COM1",
            "nul",
            "nul.baseline",
        ] {
            assert!(valid_mark_name(bad).is_err(), "`{bad}` must be refused");
        }
    }

    /// ⚠ A refused name never reaches the filesystem, in EITHER direction. `valid_mark_name` being
    /// right is only half of it — the check has to be the first thing both doors do, or a traversal
    /// typed on a command line becomes a write outside the marks root.
    #[test]
    fn a_refused_name_writes_nothing_and_reads_nothing() {
        let root = tempfile::tempdir().unwrap();
        let marks = root.path().join("marks");
        let outside = root.path().join("escaped.json");

        let err = write_mark(&marks, "../escaped", "1756000000-1-0", None, 1_756_000_000)
            .expect_err("a traversal is refused");
        assert!(matches!(&err, MarkError::BadName { .. }), "got {err:?}");
        assert!(!outside.exists(), "nothing was written outside the marks root");
        assert!(!marks.exists(), "…and the marks root was not even created");

        assert!(matches!(
            read_mark(&marks, "../escaped").expect_err("and the read door refuses too"),
            MarkError::BadName { .. }
        ));
    }

    /// A mark pointing at a run that is gone is DANGLING, which is a different answer from "no such
    /// mark": the first says a prune or an `rm` took the run, the second says the name was never
    /// set. They have different fixes, so they are different errors — and the SELECTOR layer that
    /// distinguishes them lives in `crates/vike-cli/src/cmd/runs/selector.rs`, which is why this
    /// case asserts only the two halves this module owns.
    #[test]
    fn a_missing_mark_and_a_bad_name_are_different_answers() {
        let root = tempfile::tempdir().unwrap();
        let marks = root.path().join("marks");
        write_mark(&marks, "baseline", "1799999999-2-0", None, 1_799_999_999).unwrap();

        assert!(matches!(
            read_mark(&marks, "nope").expect_err("never set"),
            MarkError::Missing { .. }
        ));
        let err = read_mark(&marks, "nope").unwrap_err();
        assert!(err.to_string().contains("tag"), "it names the verb that sets one: {err}");
        assert_eq!(read_mark(&marks, "baseline").unwrap().run_id, "1799999999-2-0");
    }

    /// The history is BOUNDED. A mark moved on every CI run would otherwise grow one file forever,
    /// which is the failure the log retention in this workspace already exists for.
    #[test]
    fn the_mark_history_is_capped() {
        let root = tempfile::tempdir().unwrap();
        let marks = root.path().join("marks");
        for i in 0..(MARK_HISTORY_MAX + 5) {
            write_mark(
                &marks,
                "baseline",
                &format!("1756000000-1-{i}"),
                None,
                1_756_000_000 + i as i64,
            )
            .unwrap();
        }
        let m = read_mark(&marks, "baseline").unwrap();
        assert_eq!(m.history.len(), MARK_HISTORY_MAX);
        assert_eq!(
            m.history[0].run_id,
            format!("1756000000-1-{}", MARK_HISTORY_MAX + 3),
            "most recent first — the oldest moves fall off the end"
        );
        assert_eq!(
            m.run_id,
            format!("1756000000-1-{}", MARK_HISTORY_MAX + 4),
            "…and the pointer itself is the last write, not a history row"
        );
    }

    /// The write is ATOMIC and leaves no droppings: an interrupted CI job must leave the OLD mark
    /// rather than a truncated file, and a successful one must not leave the temp file behind for a
    /// listing to trip over.
    #[test]
    fn a_written_mark_leaves_no_temporary_file_behind() {
        let root = tempfile::tempdir().unwrap();
        let marks = root.path().join("marks");
        write_mark(&marks, "baseline/m", "1756000000-1-0", None, 1_756_000_000).unwrap();
        write_mark(&marks, "baseline/m", "1756000001-1-0", None, 1_756_000_001).unwrap();

        let kept: Vec<String> = std::fs::read_dir(marks.join("baseline"))
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(kept, vec!["m.json".to_string()], "one file, and no `.tmp` beside it");
    }
}
