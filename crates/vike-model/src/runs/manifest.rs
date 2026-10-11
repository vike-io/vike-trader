//! The run directory's document TYPES — what each file holds, and the manifest's common half.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[cfg(doc)]
use super::{
    CONFIG_FILE, ID_FINGERPRINT_LEN, MANIFEST_SCHEMA, MAX_EQUITY_SAMPLES, MAX_TRADES, REPORT_FILE,
    SERIES_FILE, SERIES_SCHEMA, TRADES_FILE, TRADES_SCHEMA, create_run_dir, decimate, run_id_at,
    utc_rfc3339, write_run_with,
};

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
/// `vike_buildinfo::GIT_SHA` is not always a commit: it is the literal `"unknown"` when it could not
/// be resolved, and the literal `"frozen"` in a CI or verification-lane DEBUG build under the
/// freeze switch (`crates/vike-buildinfo/build.rs`'s module doc). A caller converts BOTH to `None`
/// rather than writing either word into a manifest field a reader would take for a commit —
/// `crates/vike/src/lib.rs`'s `manifest_git_sha` is that filter for the one producer that links the
/// identity crate.
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
/// The raw channel is `vike_analytics::BacktestResult::dropped`, and
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
/// state — `vike_analytics::BacktestResult::equity_ts` is documented as "empty when not
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
/// `crates/vike-ops/tests/docs/citation_gate/dead_paths.rs`'s `DEAD_PATH_EXCEPTIONS`). The rule outlived it: the
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
