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
//! Nothing here resolves a path. The BINARY calls `crate::state_path::user_runs_dir` and hands
//! the answer down, because a library that resolves its own project root reads global state its
//! caller can neither see nor override — the rule `crates/vike-ops/tests/settings_registry.rs`'s
//! `LIBRARY_PIN` ratchets down, and the reason ONE walk decides which project a process is in.
//!
//! Nothing here reads the clock either, for the stronger reason
//! `crates/vike-ops/tests/clock_pin.rs`'s `CLOCK_PIN` states: in the crates that carry
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
//! `crate::state_path::MARKS_SUBDIR`, a SIBLING of the runs root. [`Mark`](crate::runs::Mark) and that constant carry
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
/// nowhere: `crates/vike-studio/src/research.rs`'s `kind_status` draws an unrecognised kind in the
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

#[path = "runs_tests.rs"]
#[cfg(test)]
mod runs_tests;
