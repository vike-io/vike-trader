//! **`poly_mm` — the Polymarket market-making sweep, as a USER STUDY.**
//!
//! This is the ENTRY FILE `vike-user-research`'s `build.rs` finds and compiles.
//!
//! NOTE it lives under `tests/fixture_user_data/`, a directory named for the two ~60-line
//! fixtures beside it, and it is not a fixture. The name is kept and the placement is the point:
//! that tree is the ONLY user_data a CI checkout has, scanned unconditionally, so a study here is
//! the one kind of study a COMPILER ever sees. The real `user_data/` is gitignored, which the
//! cohort study accepts — it exists on one box and nothing type-checks it. This sweep was ported
//! from a workspace member (`crates/vike-poly-research`, deleted with decision 0076) and keeping
//! it compiler-checked was chosen over keeping it private, deliberately and at a stated price:
//! it is in version control, and `cargo test -p vike-user-research` now compiles 1,568 lines that
//! no shipped binary contains. It reaches no release and no dispatcher; `tests/` code never does.
//! `crates/vike-user-research/src/contract.rs` is the contract it satisfies:
//!
//! ```ignore
//! pub fn run(ctx: &StudyContext, params: &toml::Value) -> Result<StudyOutcome, StudyError>
//! ```
//!
//! # What it ports
//!
//! The `poly_mm_batch` BINARY (`crates/vike-poly-research/src/main.rs`, deleted by the same change
//! that landed this file). That program swept the Group-B market-making models over a universe of
//! Polymarket outcome tokens, ran one event-driven backtest per `(market, lane, strategy, config)`
//! cell, and printed one aggregate table grouped by lane, family and config.
//!
//! Every number this file computes is the same number that binary computed. What changed is
//! everything AROUND the numbers, because a study is not a program: it owns no argv, no store, no
//! logger, no stdout and no exit code. Each of those is translated or dropped below, and a drop is
//! stated as a drop.
//!
//! # The grid, unchanged
//!
//! For each market it runs the [`CONFIGS`] grid — a sweep over each model's key knob (A–S `γ`,
//! LMSR `b`, LS-LMSR `α`, Glosten–Milgrom `μ`) — plus the [`TRAILING_CONFIGS`] scalper pair, over
//! the recorded `kind = "tick"` lane (quotes + trades + books, so the `prob_power` queue model
//! fills resting quotes against the real taker tape). It sums the fractional return and the trade
//! count and averages the win rate per `(lane, family, config)`.
//!
//! # What the BINARY had and this study does NOT
//!
//! | binary concern | what happened to it |
//! |---|---|
//! | `vike_backfill::cli::CliSpec`, `USAGE`, `arg`/`has_flag`, `ExitCode` | **DROPPED.** A study has no argv and no exit code; every knob became a `params` key (below). `vike-backfill` is also not in this crate's `[dependencies]` and never will be — `crates/vike-user-research/src/lib.rs` argues its absence as the side door ADR 0029 closes. |
//! | `--archive PATH` / `--store DIR`, `parse_backtest_store`, `open_backtest_store` | **DROPPED.** The store ARRIVES. `contract.rs`: *"The study never resolves a path, never reads an environment variable, never opens a socket and never constructs a store."* Both of those functions live behind `vike-data`'s `hist-datafusion` feature, which this crate deliberately does not enable, so the code would not compile even if the contract allowed it. Which backend the operator chose, and the archive-vs-import crossover the binary's doc measured, are now the CALLER's decision and the caller's documentation. |
//! | `window_clock_banner` | **DROPPED with the store flags.** It printed which of the recorder's two timestamps the chosen backend means. That is a property of the store somebody else opened, it lives in the same feature-gated module, and a study has no stderr to print it on. It is a real loss: a five-minute window is exactly the size at which the two stamps disagree most, and the study cannot warn about it. The host that builds the `StudyContext` is where that banner belongs now. |
//! | `vike_log::init` + `log_config` | **DROPPED.** A study does not initialise logging; only binaries do (root `CLAUDE.md`, and `vike-log` is not a dependency here anyway). |
//! | `eprintln!` progress (`n/total markets done …`) | **DROPPED.** A study writes to no stream. Progress across a long sweep is genuinely lost; nothing in the contract carries it. |
//! | `eprintln!` per-failure lines and the final summary | **TRANSLATED**, not dropped — see `failures.tsv` below. The counts also ride as metrics. |
//! | `println!` aggregate table | **TRANSLATED** to metrics plus the `per_cell.tsv` artifact. |
//! | `harness::run_backtest` + `BacktestReport::from_result` + `periods_per_year` | **TRANSLATED** to ONE [`vike_user_research::StudySim::run_one`] call. The report's other fields — Sharpe, its annualisation, drawdown, the equity curve — do NOT cross the seam; [`vike_user_research::SimOutcome`] carries the three numbers the binary's table actually used (`total_return`, `n_trades`, `win_rate`) and nothing else. That is a real narrowing and the seam's own doc argues it: naming the report type would put `vike-backtest` (layer 50) in a layer-35 manifest. |
//! | `BacktestProfile::from_toml_str` | **DROPPED as a TYPE, kept as a CHECK.** The seam takes `&toml::Value`, so the profile is parsed here with `toml::from_str` and handed over as data; the simulator's own deserializer — the thing that knows what a `[strategy]` table means — runs on the HOST side and its refusal comes back as `Err(String)`. The binary's tests asserted `from_toml_str(...).expect("must parse")`; the ported tests assert the text is well-formed TOML instead, which is strictly weaker and is stated as such on each test. |
//! | `CachedHistStore` (the memoizing wrapper) | **KEPT, and promoted** — see the next section. |
//!
//! # ⚠ The memoizing wrapper is no longer an optimisation. It is the only way to call the seam.
//!
//! [`vike_user_research::StudySim::run_one`] takes `store: Arc<dyn HistStore + Send + Sync>` — and
//! [`vike_user_research::StudyContext`] exposes **no store handle**. Its field is private and every
//! accessor is a read VERB (`bars`/`quotes`/`trades`/`book_updates`/`depth`/`cohort`/
//! `perp_metrics`). So a study cannot forward the store it was handed; it has to PRESENT one, and
//! the only material it has to build one out of is those verbs.
//!
//! [`ContextStore`] is that adapter, and it is the binary's `CachedHistStore` with its backing
//! changed from `inner: Arc<dyn HistStore>` to `ctx: StudyContext`. The memoization survives
//! verbatim and for the same measured reason: every cell of one market's run matrix replays the
//! IDENTICAL `(venue = polymarket, token, [from, to])` tick slice — only `[strategy]`/`[engine]`
//! differ, never `[data]` — so without it the host re-reads and re-decodes the same rows once per
//! cell. One wrapper per market, inside the `par_iter` closure, so memory stays bounded to one
//! market's slice.
//!
//! ⚠ **The adapter is narrower than the wrapper was, in three places, and each is a behaviour
//! change rather than a tidy-up:**
//!
//! 1. **The append half and `resample_*` REFUSE.** They are required trait methods with no
//!    default, so they must have bodies, and the contract gives a study nothing to write through.
//!    They return `DataError::Query` naming the refusal rather than `Ok(0)`: an `Ok(0)` would tell
//!    a caller its write succeeded and stored nothing.
//! 2. **`scan_symbol_properties`, `scan_equity`, `scan_exec_fills` and `scan_exec_orders` REFUSE
//!    too** — required reads with no verb behind them on `StudyContext`. This is the single
//!    largest risk in the port and it is stated rather than hidden: if the host's simulator
//!    pre-fetches symbol properties (the default `properties_as_of` is derived from
//!    `scan_symbol_properties`, so it refuses with it), every run fails. An empty `Ok` was the
//!    alternative and was refused on this workspace's own rule — an absent thing is silent, a
//!    present-but-unreachable thing is an error, and "no properties were ever recorded" must not
//!    wear the same answer as "this contract cannot ask". If it does fire, the cure is a
//!    `properties` verb on `StudyContext`, not a softer body here.
//! 3. **`list_series`/`inventory` inherit the trait's REFUSAL, reversing the wrapper's argument.**
//!    `HistStore::list_series`' doc names `poly_mm_batch`'s `CachedHistStore` as the
//!    forward-to-inner shape, because "cannot enumerate" is false for a type fronting a store that
//!    can. Here it is TRUE: this adapter fronts a contract with no catalog verb, so the default
//!    refusal is the honest answer and overriding it would be the fabrication that doc describes.
//!
//! And one place it is WIDER: `scan_depth`, `scan_cohort` and `scan_perp_metrics` FORWARD to the
//! matching context verb. The binary forwarded depth and left cohort/perp on their defaults; here
//! all three have a verb behind them, and inheriting a refusal where the context can answer would
//! be the same false statement in the other direction.
//!
//! # `params` — every knob the binary had
//!
//! ```toml
//! fee        = false     # bool. the real Polymarket 0.072·p(1−p) per-fill taker cost
//! floor      = 0.0       # number. `min_half_spread_ticks`; 0 = NO floor
//! latency_ms = 0         # integer ≥ 0. `[engine] order_latency_ms`
//! lane       = "l2"      # "l1" | "l2" | "both"
//! strategy   = "spread"  # "spread" | "trailing" | "both"
//!
//! [[universe]]           # required, ≥ 1 row
//! family      = "btc-5m" # the slug; a `-15m` substring picks the 15-minute tenor
//! token       = "…"      # the Polymarket outcome-token id, the store's `symbol`
//! end_date_ms = 0        # the market's close, epoch ms
//! ```
//!
//! **The universe is the token list ITSELF, in `params` — not a path.** The binary read a TSV the
//! operator named. A study may not: `contract.rs` says it *"never resolves a path"* in as many
//! words, and a path in `params` is still a path the study opens. The TSV columns became the three
//! keys above one for one, so converting a universe file is mechanical and belongs to whatever
//! writes the recipe. The cost is real — a 562-token universe is 562 inline tables in the recipe
//! — and it is the price of the study staying a pure function of `(store, params)`.
//!
//! ⚠ **Two readers are STRICTER than the binary's, deliberately.** `--floor` was
//! `.parse().ok().unwrap_or(0.0)`, so `--floor banana` ran the whole sweep with NO floor and said
//! nothing; a malformed TSV line was skipped and the market silently left the universe. Both are
//! the hazard `--latency-ms` was already strict about (*"a silently-zero latency run would defeat
//! the whole experiment"*), so every reader here refuses instead: a wrong-typed knob and a
//! malformed universe row are both `StudyError::Study`, naming the key and what was found.
//!
//! # What it hands back
//!
//! Headline metrics first (`markets`, `runs`, `failed`, `cells`, the resolved parameters), then one
//! group of four per `(lane, family, config)` cell — `sum_ret`, `trades`, `avg_win`, `n` — under
//! the stable name `"{lane}/{family}/{config}/{metric}"`. Two artifacts: `per_cell.tsv` (the table
//! the binary printed, same columns) and `failures.tsv` (the lines it sent to stderr).
//!
//! # Determinism
//!
//! `rayon` fans out over MARKETS and nothing else. `par_iter().map(..).collect()` preserves input
//! order, so the sequential fold afterwards adds each market's return in universe order on every
//! run — a parallel fold would not. The per-cell accumulation stays a naive `+=`, matching the
//! binary and the workspace rule that *"explicit `+=` loops stay naive folds"*; it is deliberately
//! NOT `vike_model::py_sum`, because changing the fold would change the numbers this port exists
//! to preserve. The `BTreeMap` cell ordering is what makes metric emission order stable, and
//! `HashMap` is used only for the scan cache, where iteration order reaches no arithmetic.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use rayon::prelude::*;

use vike_data::{
    CohortRow, DataError, ExecFillRow, ExecOrderRow, HistStore, PerpMetricRow, TsRange,
};
use vike_model::{Bar, BookUpdate, EquitySample, QuoteTick, SymbolProperties, TradeTick};
use vike_user_research::{StudyContext, StudyError, StudyOutcome};

/// The sweep grid: `(label, [strategy.params] body)`. Each body carries the maker's `γ` plus the
/// model selector + its swept knob. Labels sort so a family's rows group by model then knob.
///
/// Copied VERBATIM from the binary. Nothing here is re-derived, re-tuned or re-ordered — the whole
/// proof strategy of this port is that the grid and the profile text are byte-identical, so "does
/// the study produce the binary's numbers" reduces to "does the seam run the binary's backtest".
const CONFIGS: &[(&str, &str)] = &[
    // Avellaneda–Stoikov baseline — risk-aversion γ sweep.
    ("as-g0.05", "gamma = 0.05\n"),
    ("as-g0.10", "gamma = 0.1\n"),
    ("as-g0.30", "gamma = 0.3\n"),
    // LMSR reservation — liquidity-depth b sweep (γ fixed at 0.1).
    ("lmsr-b05", "gamma = 0.1\nreservation_model = \"lmsr\"\nlmsr_b = 5.0\n"),
    ("lmsr-b20", "gamma = 0.1\nreservation_model = \"lmsr\"\nlmsr_b = 20.0\n"),
    ("lmsr-b50", "gamma = 0.1\nreservation_model = \"lmsr\"\nlmsr_b = 50.0\n"),
    // LS-LMSR spread — α sweep.
    ("lslmsr-a0.004", "gamma = 0.1\nspread_source = \"ls_lmsr\"\nls_lmsr_alpha = 0.004\n"),
    ("lslmsr-a0.02", "gamma = 0.1\nspread_source = \"ls_lmsr\"\nls_lmsr_alpha = 0.02\n"),
    ("lslmsr-a0.08", "gamma = 0.1\nspread_source = \"ls_lmsr\"\nls_lmsr_alpha = 0.08\n"),
    // Glosten–Milgrom spread — informed-fraction μ sweep.
    ("gm-m0.005", "gamma = 0.1\nspread_source = \"glosten_milgrom\"\ngm_mu = 0.005\n"),
    ("gm-m0.02", "gamma = 0.1\nspread_source = \"glosten_milgrom\"\ngm_mu = 0.02\n"),
    ("gm-m0.08", "gamma = 0.1\nspread_source = \"glosten_milgrom\"\ngm_mu = 0.08\n"),
];

/// Trailing-scalper configs: the user-fixed 2s IN-STRATEGY reaction gap (`exit_delay_ms`),
/// mid-following flatten (`profit_target = 0`). NOTE: the engine's `order_latency_ms` stacks on
/// top of this 2s (deliberate — see the 2026-07-28 latency-realism spec).
///
/// `trail-d2000` is the baseline — no entry-timing cutoffs. `trail-cut` arms BOTH entry-timing
/// cutoffs: suppress entries for the first 5s after open (`entry_open_delay_ms = 5000`, avoiding
/// the chaotic just-opened book) and stop posting/re-pricing fresh entries in the last 30s before
/// close (`entry_cutoff_before_close_ms = 30000`, so a late fill under venue latency still has
/// time to exit before resolution). `market_open_ms`/`market_close_ms` are NOT baked in here —
/// every [`RunSpec`] built from these bodies gets its OWN market's window appended per-token by
/// [`profile_toml`] (via [`window_for`]), since the const table has no per-market knowledge.
const TRAILING_CONFIGS: &[(&str, &str)] = &[
    ("trail-d2000", "qty = 1.0\nhalf_spread = 0.01\nexit_delay_ms = 2000\nprofit_target = 0.0\n"),
    (
        "trail-cut",
        "qty = 1.0\nhalf_spread = 0.01\nexit_delay_ms = 2000\nprofit_target = 0.0\n\
         entry_open_delay_ms = 5000\nentry_cutoff_before_close_ms = 30000\n",
    ),
];

/// Fill-realism lane: `L2` = the queue-position model against the real taker tape (realistic);
/// `L1` = no queue model, the default optimistic spread-crossing Tick fill.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Lane {
    L1,
    L2,
}

impl Lane {
    fn label(self) -> &'static str {
        match self {
            Lane::L1 => "l1",
            Lane::L2 => "l2",
        }
    }
}

/// Strategy family to run.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Strat {
    Spread,
    Trailing,
}

/// One `(lane, strategy-config)` cell of the run matrix.
struct RunSpec {
    lane: Lane,
    label: &'static str,
    strategy: &'static str,
    /// the full `[strategy.params]` body
    params: String,
}

/// One market of the universe — the binary's TSV columns `family\ttoken\tend_date_ms`, as data.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Market {
    family: String,
    token: String,
    end_date_ms: i64,
}

/// One config run's result row: `(lane, family, config label, total_return, n_trades, win_rate)`.
type ConfigRow = (String, String, String, f64, u64, f64);

/// What one market's closure hands back: its rows, its failure count, and the failure LINES that
/// used to be `eprintln!`s. Aliased because it is a `par_iter` element type and clippy's
/// `type_complexity` gate reads the written type.
type MarketResult = (Vec<ConfigRow>, u64, Vec<String>);

/// Expand lanes × strategies into the per-market run list: the 12 spread configs (each carrying
/// the shared qty/tick_size/floor prefix) + the 2 trailing configs, per lane.
fn run_matrix(lanes: &[Lane], strats: &[Strat], floor: f64) -> Vec<RunSpec> {
    let mut out = Vec::new();
    for &lane in lanes {
        for &strat in strats {
            match strat {
                Strat::Spread => {
                    for (label, params) in CONFIGS {
                        out.push(RunSpec {
                            lane,
                            label,
                            strategy: "spread_maker",
                            params: format!(
                                "qty = 1.0\ntick_size = 0.001\nmin_half_spread_ticks = {floor}\n{params}"
                            ),
                        });
                    }
                }
                Strat::Trailing => {
                    for (label, params) in TRAILING_CONFIGS {
                        out.push(RunSpec {
                            lane,
                            label,
                            strategy: "trailing_scalper",
                            params: (*params).to_string(),
                        });
                    }
                }
            }
        }
    }
    out
}

/// `lane` reader: absent/`l2` keeps the binary's default queue-model lane; `l1` the optimistic Tick
/// lane; `both` runs each config through the two lanes.
fn parse_lanes(s: Option<&str>) -> Option<Vec<Lane>> {
    match s {
        None | Some("l2") => Some(vec![Lane::L2]),
        Some("l1") => Some(vec![Lane::L1]),
        Some("both") => Some(vec![Lane::L1, Lane::L2]),
        _ => None,
    }
}

/// `strategy` reader: absent/`spread` keeps the binary's default grid; `trailing` the scalper;
/// `both` = union.
fn parse_strats(s: Option<&str>) -> Option<Vec<Strat>> {
    match s {
        None | Some("spread") => Some(vec![Strat::Spread]),
        Some("trailing") => Some(vec![Strat::Trailing]),
        Some("both") => Some(vec![Strat::Spread, Strat::Trailing]),
        _ => None,
    }
}

/// Build the profile TOML for one `(token, window, run-spec)`. `latency_ms > 0` arms the engine's
/// order-latency gate (every place/modify/cancel reaches matching that much later); `0` emits no
/// line, keeping the no-knob TOML byte-identical to the binary's. The L2 lane keeps
/// `queue_model = "prob_power"` (resting quotes fill against the real taker tape); the L1 lane
/// omits it (optimistic Tick crossing).
///
/// For the `trailing_scalper` strategy ONLY, the market's exact `[from, to]` window — already
/// computed by [`window_for`] as this market's real open/close — is ALSO appended to
/// `[strategy.params]` as `market_open_ms`/`market_close_ms`: the reference timestamps the
/// scalper's `entry_open_delay_ms`/`entry_cutoff_before_close_ms` cutoffs need. Appended
/// unconditionally (even for `trail-d2000`, whose own cutoff knobs are 0) because the scalper only
/// consults an open/close timestamp when its OWN delay/cutoff knob is armed — so this addition is
/// inert for any trailing config that doesn't ask for a cutoff, and the `spread_maker` strategy
/// never receives these keys at all, which keeps its TOML byte-for-byte unchanged.
///
/// ⚠ Still a STRING, not a `toml::Value` assembled programmatically, even though the seam takes a
/// `Value`. The string is what the binary's tests pin byte for byte, and those pins are the only
/// mechanical evidence that this port did not quietly re-tune a knob; building a `Value` by hand
/// would throw them away in exchange for nothing — the text is parsed one line later either way.
fn profile_toml(
    token: &str,
    from: i64,
    to: i64,
    fee: bool,
    latency_ms: i64,
    spec: &RunSpec,
) -> String {
    let fee_block =
        if fee { "[engine.fee]\nkind = \"probability_scaled\"\ntaker_rate = 0.072\n" } else { "" };
    let queue_line = match spec.lane {
        Lane::L2 => "queue_model = \"prob_power\"\n",
        Lane::L1 => "",
    };
    let latency_line =
        if latency_ms != 0 { format!("order_latency_ms = {latency_ms}\n") } else { String::new() };
    let window_params = if spec.strategy == "trailing_scalper" {
        format!("market_open_ms = {from}\nmarket_close_ms = {to}\n")
    } else {
        String::new()
    };
    format!(
        "name = \"batch\"\n\
         [data]\nkind = \"tick\"\nfrom = \"{from}\"\nto = \"{to}\"\n\
         [[data.series]]\nvenue = \"polymarket\"\nsymbol = \"{token}\"\nkind = \"tick\"\n\
         [engine]\ncash = 1000.0\nslippage = 0.0\n{queue_line}{latency_line}{fee_block}\
         [strategy]\nname = \"{strategy}\"\n\
         [strategy.params]\n{params}{window_params}",
        strategy = spec.strategy,
        params = spec.params,
    )
}

/// The honest per-market replay window: exactly the market's life `[end - tenor, end]`.
/// The former `[end - tenor - 60s, end + 60s]` padding is CONTAMINATED — 60s of pre-open
/// flat plus 60s of post-close settlement (price pins to 0/1 and rests get run over),
/// which distorts maker PnL. 15m families keyed off the `-15m` slug suffix.
fn window_for(family: &str, end_date: i64) -> (i64, i64) {
    let tenor = if family.contains("-15m") { 900_000 } else { 300_000 };
    (end_date - tenor, end_date)
}

/// Running aggregate for one `(lane, family, config)` cell.
#[derive(Default)]
struct Agg {
    ret: f64,
    trades: u64,
    win_sum: f64,
    n: u64,
}

// ---- params -----------------------------------------------------------------------------------
//
// The binary's `vike_backfill::cli` arg readers, re-expressed over `params`. The `params.get(..)`
// plus a default idiom is the same lenient-reader convention the strategy tier's `build` and the
// built-in `from_params` arms use — but ONLY for an ABSENT key. A key that is PRESENT and the
// wrong type is refused, because the alternative is the silent-default hazard the binary's own
// `--latency-ms` reader was already written to avoid.

/// The refusal a wrong-typed knob produces: names the key, what was wanted, and what was found.
fn wrong_type(key: &str, want: &str, got: &toml::Value) -> StudyError {
    StudyError::Study(format!("`{key}` must be {want}, found a {}", got.type_str()))
}

fn opt_bool(params: &toml::Value, key: &str) -> Result<Option<bool>, StudyError> {
    match params.get(key) {
        None => Ok(None),
        Some(v) => v.as_bool().map(Some).ok_or_else(|| wrong_type(key, "a boolean", v)),
    }
}

/// Accepts a TOML integer as well as a float, because `floor = 0` is an Integer to the parser and
/// an operator writing a whole number should not have to know that.
fn opt_f64(params: &toml::Value, key: &str) -> Result<Option<f64>, StudyError> {
    match params.get(key) {
        None => Ok(None),
        Some(v) => v
            .as_float()
            .or_else(|| v.as_integer().map(|n| n as f64))
            .map(Some)
            .ok_or_else(|| wrong_type(key, "a number", v)),
    }
}

fn opt_i64(params: &toml::Value, key: &str) -> Result<Option<i64>, StudyError> {
    match params.get(key) {
        None => Ok(None),
        Some(v) => v.as_integer().map(Some).ok_or_else(|| wrong_type(key, "an integer", v)),
    }
}

fn opt_str<'a>(params: &'a toml::Value, key: &str) -> Result<Option<&'a str>, StudyError> {
    match params.get(key) {
        None => Ok(None),
        Some(v) => v.as_str().map(Some).ok_or_else(|| wrong_type(key, "a string", v)),
    }
}

/// The universe, read as DATA rather than from a path — see the module doc for why a path is not
/// available to a study at all. Every malformed row is a refusal naming its index; the binary
/// silently dropped the market, which shrinks the denominator of every aggregate below it with no
/// trace in the output.
fn read_universe(params: &toml::Value) -> Result<Vec<Market>, StudyError> {
    let Some(raw) = params.get("universe") else {
        return Err(StudyError::Study(
            "`universe` is required: an array of tables, one per market, each with `family` \
             (the slug whose `-15m` suffix picks the tenor), `token` (the Polymarket \
             outcome-token id, which is the store's `symbol`) and `end_date_ms` (the market's \
             close, epoch ms) — the three columns of the batch binary's TSV"
                .to_string(),
        ));
    };
    let Some(rows) = raw.as_array() else {
        return Err(wrong_type("universe", "an array of tables", raw));
    };
    if rows.is_empty() {
        return Err(StudyError::Study(
            "`universe` is empty — there is nothing to sweep, and an empty sweep would report \
             zeroes that look like a result"
                .to_string(),
        ));
    }
    let mut out = Vec::with_capacity(rows.len());
    for (i, row) in rows.iter().enumerate() {
        if !row.is_table() {
            return Err(wrong_type(&format!("universe[{i}]"), "a table", row));
        }
        let family = opt_str(row, "family")?
            .ok_or_else(|| StudyError::Study(format!("universe[{i}] has no `family`")))?;
        let token = opt_str(row, "token")?
            .ok_or_else(|| StudyError::Study(format!("universe[{i}] has no `token`")))?;
        let end_date_ms = opt_i64(row, "end_date_ms")?
            .ok_or_else(|| StudyError::Study(format!("universe[{i}] has no `end_date_ms`")))?;
        out.push(Market {
            family: family.trim().to_string(),
            token: token.trim().to_string(),
            end_date_ms,
        });
    }
    Ok(out)
}

/// Everything the binary's flags resolved to, resolved once before any work starts.
///
/// `Debug` is not decoration: the knob tests use `Result::expect_err`, which requires the OK type
/// to be `Debug`.
#[derive(Debug)]
struct Knobs {
    fee: bool,
    floor: f64,
    latency_ms: i64,
    lanes: Vec<Lane>,
    strats: Vec<Strat>,
}

fn read_knobs(params: &toml::Value) -> Result<Knobs, StudyError> {
    let fee = opt_bool(params, "fee")?.unwrap_or(false);
    // Default 0 (NO floor) so the spread-source formulas are not collapsed onto the same tick —
    // the `=1` floor this default replaced made LS-LMSR and GM produce identical quotes.
    let floor = opt_f64(params, "floor")?.unwrap_or(0.0);
    if !floor.is_finite() || floor < 0.0 {
        return Err(StudyError::Study(format!(
            "`floor` must be a finite, non-negative number of ticks, found {floor}"
        )));
    }
    let latency_ms = opt_i64(params, "latency_ms")?.unwrap_or(0);
    if latency_ms < 0 {
        return Err(StudyError::Study(format!(
            "`latency_ms` must be a non-negative integer of milliseconds, found {latency_ms}"
        )));
    }
    let lane = opt_str(params, "lane")?;
    let lanes = parse_lanes(lane).ok_or_else(|| {
        StudyError::Study(format!("unknown `lane` {:?} (l1 | l2 | both)", lane.unwrap_or_default()))
    })?;
    let strategy = opt_str(params, "strategy")?;
    let strats = parse_strats(strategy).ok_or_else(|| {
        StudyError::Study(format!(
            "unknown `strategy` {:?} (spread | trailing | both)",
            strategy.unwrap_or_default()
        ))
    })?;
    Ok(Knobs { fee, floor, latency_ms, lanes, strats })
}

// ---- the context-backed, memoizing HistStore ---------------------------------------------------
//
// The binary's `CachedHistStore`, with its backing swapped from a store it was handed to the
// StudyContext's read verbs — see the module doc's "no longer an optimisation" section for why a
// study has to PRESENT a store rather than forward one, and for the three narrowings this adapter
// carries that the wrapper did not.

/// Verb discriminant folded into the cache key — plain `u8` codes rather than a keyed enum so the
/// key tuple stays `Hash`/`Eq` with zero extra derive plumbing.
const VERB_QUOTES: u8 = 0;
const VERB_TRADES: u8 = 1;
const VERB_BOOKS: u8 = 2;
const VERB_BARS: u8 = 3;

/// One memoized read verb's decoded result.
///
/// ⚠ The binary had a fifth variant, `Properties(Option<SymbolProperties>)`, memoizing
/// `properties_as_of` on `(venue, symbol, ts)`. It is GONE with the verb: this adapter cannot
/// serve symbol properties at all, so there is nothing to cache.
#[derive(Clone)]
enum CachedRows {
    Quotes(Vec<QuoteTick>),
    Trades(Vec<TradeTick>),
    Books(Vec<BookUpdate>),
    Bars(Vec<Bar>),
}

/// The memoization cache key: `(verb, venue, symbol, from, to)` — `load_bars`' extra `interval`
/// axis is folded into the symbol slot (`"{symbol}\0{interval}"`) rather than widening the tuple,
/// since every other verb has no such axis. A named alias (clippy's `type_complexity` gate).
type CacheKey = (u8, String, String, i64, i64);

/// The store a study PRESENTS to [`vike_user_research::StudySim::run_one`]: every read forwarded to
/// the [`StudyContext`] it was built from, memoized per `(verb, venue, symbol, range)`.
///
/// It owns a CLONE of the context rather than borrowing one, because the seam takes
/// `Arc<dyn HistStore + Send + Sync>` and that is `'static`. The clone is cheap by the contract's
/// own design note — two `Arc`s, a `TsRange` and a `PathBuf`.
struct ContextStore {
    ctx: StudyContext,
    cache: Mutex<HashMap<CacheKey, CachedRows>>,
}

impl ContextStore {
    fn new(ctx: StudyContext) -> Self {
        Self { ctx, cache: Mutex::new(HashMap::new()) }
    }

    /// `TsRange`'s open bounds resolve to the widest representable `i64` pair — so an unbounded
    /// scan still gets a stable cache key.
    fn range_bounds(range: TsRange) -> (i64, i64) {
        (range.start.unwrap_or(i64::MIN), range.end.unwrap_or(i64::MAX))
    }

    /// Shared memoize-or-fetch: look up `key` in the cache; on miss, call `fetch`, cache the
    /// wrapped rows, and return the freshly fetched value.
    fn memoize<T: Clone>(
        &self,
        key: CacheKey,
        wrap: impl Fn(T) -> CachedRows,
        unwrap: impl Fn(&CachedRows) -> Option<T>,
        fetch: impl FnOnce() -> Result<T, DataError>,
    ) -> Result<T, DataError> {
        if let Some(cached) = self.cache.lock().unwrap().get(&key).and_then(&unwrap) {
            return Ok(cached);
        }
        let value = fetch()?;
        self.cache.lock().unwrap().insert(key, wrap(value.clone()));
        Ok(value)
    }

    /// The one refusal text every unreachable verb shares. `DataError` has exactly two variants —
    /// `Query` and `Io` — and neither means "refused by contract"; `Query` is the closer of the
    /// two (a question this store cannot answer) and is used uniformly so the message, not the
    /// variant, carries the reason.
    fn unreachable(verb: &str) -> DataError {
        DataError::Query(format!(
            "{verb}: a study's store is the StudyContext read verbs, which expose no {verb} — \
             see crates/vike-user-research/src/contract.rs. This is a REFUSAL, not an empty \
             result: nothing here looked."
        ))
    }

    /// The write half's refusal, kept separate so the message names the rule rather than a gap.
    fn read_only(verb: &str) -> DataError {
        DataError::Query(format!(
            "{verb}: a study's store is READ-ONLY — \
             docs/decisions/0029-a-study-reads-the-store-never-a-vendor-api.md. Filling a gap is \
             an ingest command's job, and reporting a write that did not happen as `Ok(0)` would \
             hide it."
        ))
    }
}

impl HistStore for ContextStore {
    fn load_bars(
        &self,
        venue: &str,
        symbol: &str,
        interval: &str,
        range: TsRange,
    ) -> Result<Vec<Bar>, DataError> {
        let (from, to) = Self::range_bounds(range);
        let key = (VERB_BARS, venue.to_string(), format!("{symbol}\0{interval}"), from, to);
        self.memoize(
            key,
            CachedRows::Bars,
            |c| if let CachedRows::Bars(v) = c { Some(v.clone()) } else { None },
            || self.ctx.bars(venue, symbol, interval, range),
        )
    }

    fn scan_quotes(
        &self,
        venue: &str,
        symbol: &str,
        range: TsRange,
    ) -> Result<Vec<QuoteTick>, DataError> {
        let (from, to) = Self::range_bounds(range);
        let key = (VERB_QUOTES, venue.to_string(), symbol.to_string(), from, to);
        self.memoize(
            key,
            CachedRows::Quotes,
            |c| if let CachedRows::Quotes(v) = c { Some(v.clone()) } else { None },
            || self.ctx.quotes(venue, symbol, range),
        )
    }

    fn scan_trades(
        &self,
        venue: &str,
        symbol: &str,
        range: TsRange,
    ) -> Result<Vec<TradeTick>, DataError> {
        let (from, to) = Self::range_bounds(range);
        let key = (VERB_TRADES, venue.to_string(), symbol.to_string(), from, to);
        self.memoize(
            key,
            CachedRows::Trades,
            |c| if let CachedRows::Trades(v) = c { Some(v.clone()) } else { None },
            || self.ctx.trades(venue, symbol, range),
        )
    }

    fn scan_book_updates(
        &self,
        venue: &str,
        symbol: &str,
        range: TsRange,
    ) -> Result<Vec<BookUpdate>, DataError> {
        let (from, to) = Self::range_bounds(range);
        let key = (VERB_BOOKS, venue.to_string(), symbol.to_string(), from, to);
        self.memoize(
            key,
            CachedRows::Books,
            |c| if let CachedRows::Books(v) = c { Some(v.clone()) } else { None },
            || self.ctx.book_updates(venue, symbol, range),
        )
    }

    /// FORWARDED rather than inherited, and NOT memoized: this study never reads depth, so there
    /// is no `CachedRows` variant to key it on.
    ///
    /// The trait's default REFUSES — "this store serves no depth lane" — which is the honest
    /// answer for a LEAF store and a false one for this type, which serves no lane of its own
    /// because it fronts one. Whether depth is available is the real store's fact to state, and
    /// `StudyContext::depth` passes that store's refusal through unchanged, so this verb does too.
    fn scan_depth(
        &self,
        venue: &str,
        symbol: &str,
        range: TsRange,
    ) -> Result<Vec<BookUpdate>, DataError> {
        self.ctx.depth(venue, symbol, range)
    }

    /// FORWARDED, where the binary's wrapper left this on the trait default. The context has the
    /// verb, so inheriting "this store holds no cohort panel" would be a false statement about a
    /// store that may well hold one.
    fn scan_cohort(
        &self,
        venue: &str,
        asset: &str,
        range: TsRange,
    ) -> Result<Vec<CohortRow>, DataError> {
        self.ctx.cohort(venue, asset, range)
    }

    /// FORWARDED for the same reason as [`ContextStore::scan_cohort`].
    fn scan_perp_metrics(
        &self,
        venue: &str,
        symbol: &str,
        range: TsRange,
    ) -> Result<Vec<PerpMetricRow>, DataError> {
        self.ctx.perp_metrics(venue, symbol, range)
    }

    // ---- the reads with no context verb behind them: REFUSE, never answer empty ----------------

    /// ⚠ The one that can break a run. The trait's `properties_as_of` default is derived from this
    /// verb, so a simulator that pre-fetches an instrument grid gets this refusal rather than a
    /// permissive fallback. That is deliberate — see the module doc — and the cure is a
    /// `properties` verb on `StudyContext`, not a softer body here.
    fn scan_symbol_properties(
        &self,
        _venue: &str,
        _symbol: &str,
        _range: TsRange,
    ) -> Result<Vec<(i64, SymbolProperties)>, DataError> {
        Err(Self::unreachable("scan_symbol_properties"))
    }

    fn scan_equity(
        &self,
        _venue: &str,
        _symbol: &str,
        _range: TsRange,
    ) -> Result<Vec<EquitySample>, DataError> {
        Err(Self::unreachable("scan_equity"))
    }

    fn scan_exec_fills(&self, _venue: &str, _symbol: &str) -> Result<Vec<ExecFillRow>, DataError> {
        Err(Self::unreachable("scan_exec_fills"))
    }

    fn scan_exec_orders(
        &self,
        _venue: &str,
        _symbol: &str,
    ) -> Result<Vec<ExecOrderRow>, DataError> {
        Err(Self::unreachable("scan_exec_orders"))
    }

    // ---- the write half: required methods with nothing to write through ------------------------

    fn append_bars(
        &self,
        _venue: &str,
        _symbol: &str,
        _interval: &str,
        _bars: &[Bar],
        _commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        Err(Self::read_only("append_bars"))
    }

    fn append_quotes(
        &self,
        _venue: &str,
        _symbol: &str,
        _ticks: &[QuoteTick],
        _commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        Err(Self::read_only("append_quotes"))
    }

    fn append_trades(
        &self,
        _venue: &str,
        _symbol: &str,
        _ticks: &[TradeTick],
        _commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        Err(Self::read_only("append_trades"))
    }

    fn append_book_updates(
        &self,
        _venue: &str,
        _symbol: &str,
        _updates: &[BookUpdate],
        _commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        Err(Self::read_only("append_book_updates"))
    }

    fn append_symbol_properties(
        &self,
        _venue: &str,
        _symbol: &str,
        _rows: &[(i64, SymbolProperties)],
        _commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        Err(Self::read_only("append_symbol_properties"))
    }

    fn append_equity(
        &self,
        _venue: &str,
        _symbol: &str,
        _rows: &[EquitySample],
        _commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        Err(Self::read_only("append_equity"))
    }

    fn append_exec_fills(
        &self,
        _venue: &str,
        _symbol: &str,
        _rows: &[ExecFillRow],
        _commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        Err(Self::read_only("append_exec_fills"))
    }

    fn append_exec_orders(
        &self,
        _venue: &str,
        _symbol: &str,
        _rows: &[ExecOrderRow],
        _commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        Err(Self::read_only("append_exec_orders"))
    }

    fn resample_quotes_to_bars(
        &self,
        _venue: &str,
        _symbol: &str,
        _interval: &str,
        _range: TsRange,
        _commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        Err(Self::read_only("resample_quotes_to_bars"))
    }

    fn resample_trades_to_bars(
        &self,
        _venue: &str,
        _symbol: &str,
        _interval: &str,
        _range: TsRange,
        _commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        Err(Self::read_only("resample_trades_to_bars"))
    }

    // `list_series` / `inventory` / `series_gaps` / `coverage_report` / the funding, chain and
    // depth-append verbs stay on the trait's own defaults. For the two ENUMERATION verbs that is a
    // REVERSAL of the binary wrapper's choice and it is argued in the module doc: the trait's
    // refusal reads "this store cannot enumerate its inventory", which was false for a type
    // fronting a store that can and is TRUE for a type fronting a contract with no catalog verb.
}

// ---- the study ---------------------------------------------------------------------------------

/// Sanitize one failure message into a single TSV field: the simulator's `Err` is free text and
/// may carry a newline or a tab, either of which would silently split a row of `failures.tsv`.
fn one_line(s: &str) -> String {
    s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect()
}

pub fn run(ctx: &StudyContext, params: &toml::Value) -> Result<StudyOutcome, StudyError> {
    let markets = read_universe(params)?;
    let knobs = read_knobs(params)?;
    let matrix = run_matrix(&knobs.lanes, &knobs.strats, knobs.floor);

    // The host seam, resolved BEFORE any work: a study that needs an event-driven run says so and
    // stops, naming the work, rather than degrading into a number from a different machine. The
    // parameters are read first so the message can state the size of what was refused.
    let Some(sim) = ctx.sim() else {
        return Err(StudyError::NoSim(format!(
            "{} Polymarket maker backtests — {} markets × {} (lane × strategy × config) cells \
             over the recorded tick lane",
            markets.len() * matrix.len(),
            markets.len(),
            matrix.len()
        )));
    };

    // How many markets ask for history outside the window this run was handed. NOT clamped: a
    // market's window is its exact life and truncating it would silently change what the numbers
    // below mean. It is counted and emitted instead, so a run over a half-covered universe is
    // visible in the listing rather than only in the returns.
    let window = ctx.window();
    let outside = markets
        .iter()
        .filter(|m| {
            let (from, to) = window_for(&m.family, m.end_date_ms);
            window.start.is_some_and(|s| from < s) || window.end.is_some_and(|e| to > e)
        })
        .count() as u64;

    // Parallelize over MARKETS: each market runs its own matrix independently and returns its
    // per-config rows. `par_iter().map(..).collect()` preserves input order, which is what makes
    // the sequential fold below add the same f64s in the same order on every run — a parallel
    // reduction would not. `&dyn StudySim` crosses into the closures because `StudySim: Sync`.
    let per_market: Vec<MarketResult> = markets
        .par_iter()
        .map(|market| {
            let mut out: Vec<ConfigRow> = Vec::new();
            let mut failed = 0u64;
            let mut notes: Vec<String> = Vec::new();
            let (from, to) = window_for(&market.family, market.end_date_ms);

            // ONE cache per market, shared by every cell below: they all replay the identical
            // (venue = polymarket, token, [from, to]) tick slice, so the context's read verbs are
            // driven once here instead of once per cell. Never shared ACROSS markets (a fresh
            // adapter per `map` call), so memory stays bounded to one market's slice at a time.
            let market_store: Arc<dyn HistStore + Send + Sync> =
                Arc::new(ContextStore::new(ctx.clone()));

            for spec in &matrix {
                let text = profile_toml(&market.token, from, to, knobs.fee, knobs.latency_ms, spec);
                let profile = match toml::from_str::<toml::Value>(&text) {
                    Ok(p) => p,
                    Err(e) => {
                        // The binary parsed with `BacktestProfile::from_toml_str` and this is the
                        // weaker half of that check — well-formedness only. The SCHEMA half now
                        // runs on the host side and arrives as the `Err(String)` below.
                        notes.push(format!(
                            "{}\t{}\tprofile\t{}",
                            market.token,
                            spec.label,
                            one_line(&e.to_string())
                        ));
                        failed += 1;
                        continue;
                    }
                };
                let outcome = match sim.run_one(&profile, market_store.clone()) {
                    Ok(o) => o,
                    Err(e) => {
                        notes.push(format!(
                            "{}\t{}\trun\t{}",
                            market.token,
                            spec.label,
                            one_line(&e)
                        ));
                        failed += 1;
                        continue;
                    }
                };
                out.push((
                    spec.lane.label().to_string(),
                    market.family.clone(),
                    spec.label.to_string(),
                    outcome.total_return,
                    outcome.n_trades,
                    outcome.win_rate,
                ));
            }
            (out, failed, notes)
        })
        .collect();

    // The aggregation, sequential and in universe order. `+=` stays a naive fold, matching the
    // binary: `vike_model::py_sum` here would be a different number from the one this port exists
    // to reproduce.
    let mut agg: BTreeMap<(String, String, String), Agg> = BTreeMap::new();
    let (mut n_markets, mut runs, mut failed) = (0u64, 0u64, 0u64);
    let mut failures = String::from("token\tconfig\tstage\tmessage\n");
    for (results, f, notes) in per_market {
        failed += f;
        for note in notes {
            failures.push_str(&note);
            failures.push('\n');
        }
        if !results.is_empty() {
            n_markets += 1;
        }
        for (lane, family, label, ret, trades, win) in results {
            let cell = agg.entry((lane, family, label)).or_default();
            cell.ret += ret;
            cell.trades += trades;
            cell.win_sum += win;
            cell.n += 1;
            runs += 1;
        }
    }

    let mut outcome = StudyOutcome::new();
    // Headline first — the contract's own convention, so a run listing shows the shape of the
    // sweep without opening an artifact.
    outcome.metric("markets", n_markets as f64)?;
    outcome.metric("markets_requested", markets.len() as f64)?;
    outcome.metric("markets_outside_window", outside as f64)?;
    outcome.metric("runs", runs as f64)?;
    outcome.metric("failed", failed as f64)?;
    outcome.metric("cells", agg.len() as f64)?;
    outcome.metric("configs_per_market", matrix.len() as f64)?;
    // The three knobs the binary printed in its table header, so the manifest records the
    // configuration a number was produced under rather than leaving it to the recipe.
    outcome.metric("param/fee", if knobs.fee { 1.0 } else { 0.0 })?;
    outcome.metric("param/floor", knobs.floor)?;
    outcome.metric("param/latency_ms", knobs.latency_ms as f64)?;

    // One group of four per cell, in `BTreeMap` order so emission is stable across runs.
    //
    // ⚠ The name FLATTENS a tuple key, and `StudyOutcome::metric` refuses a duplicate. A family
    // slug containing `/` could therefore collide with another cell — which surfaces as a
    // `StudyError::Study` naming the metric, not as a silently merged row. That is the correct
    // failure and it is why the separator is `/` rather than `.`: a config label like `as-g0.05`
    // already contains a dot.
    let mut table = String::from("lane\tfamily\tconfig\tsum_ret\ttrades\tavg_win\tn\n");
    for ((lane, family, label), cell) in &agg {
        let avg_win = if cell.n > 0 { cell.win_sum / cell.n as f64 } else { 0.0 };
        let stem = format!("{lane}/{family}/{label}");
        outcome.metric(&format!("{stem}/sum_ret"), cell.ret)?;
        outcome.metric(&format!("{stem}/trades"), cell.trades as f64)?;
        outcome.metric(&format!("{stem}/avg_win"), avg_win)?;
        outcome.metric(&format!("{stem}/n"), cell.n as f64)?;
        table.push_str(&format!(
            "{lane}\t{family}\t{label}\t{}\t{}\t{}\t{}\n",
            cell.ret, cell.trades, avg_win, cell.n
        ));
    }

    // The table the binary printed, as a file. Deliberately RAW values rather than the binary's
    // `{:>+10.4}` / `×100` presentation: a `.tsv` is read by a program, and rounding a return to
    // four decimals on the way out is a lossy choice a formatter should make, not a study.
    outcome.artifact("per_cell.tsv", table)?;
    // ...and the lines that used to go to stderr. Always emitted, even with no failures, so a
    // missing file means the study did not get this far rather than "nothing went wrong".
    outcome.artifact("failures.tsv", failures)?;
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use vike_user_research::{SimOutcome, StudySim};

    // ---- the profile text: the pins that prove the port re-tuned nothing --------------------
    //
    // Carried from the binary verbatim, minus each one's `BacktestProfile::from_toml_str(..)
    // .expect("must parse")` line — that type lives in `vike-backtest`, which this crate may not
    // name. Where the binary proved "the simulator's own deserializer accepts this", these prove
    // only "this is well-formed TOML"; the schema half now runs host-side.

    /// The no-knob run (lane l2, latency 0, spread grid) must generate EXACTLY the binary's TOML.
    #[test]
    fn default_profile_is_byte_identical_to_the_batch_binary() {
        let matrix = run_matrix(&[Lane::L2], &[Strat::Spread], 0.0);
        let got = profile_toml("TOK", 100, 200, false, 0, &matrix[0]); // as-g0.05
        let want = "name = \"batch\"\n\
                    [data]\nkind = \"tick\"\nfrom = \"100\"\nto = \"200\"\n\
                    [[data.series]]\nvenue = \"polymarket\"\nsymbol = \"TOK\"\nkind = \"tick\"\n\
                    [engine]\ncash = 1000.0\nslippage = 0.0\nqueue_model = \"prob_power\"\n\
                    [strategy]\nname = \"spread_maker\"\n\
                    [strategy.params]\nqty = 1.0\ntick_size = 0.001\nmin_half_spread_ticks = 0\ngamma = 0.05\n";
        assert_eq!(got, want);
    }

    /// `latency_ms = 250` lands as `[engine] order_latency_ms = 250`, and the profile is still
    /// well-formed TOML.
    #[test]
    fn latency_knob_emits_the_engine_order_latency_line() {
        let matrix = run_matrix(&[Lane::L2], &[Strat::Spread], 0.0);
        let text = profile_toml("TOK", 100, 200, false, 250, &matrix[0]);
        assert!(text.contains("order_latency_ms = 250\n"), "{text}");
        toml::from_str::<toml::Value>(&text).expect("must parse as TOML");
    }

    /// The L1 lane omits queue_model entirely — the tick lane then uses the default optimistic
    /// spread-crossing Tick fill model.
    #[test]
    fn l1_lane_has_no_queue_model() {
        let matrix = run_matrix(&[Lane::L1], &[Strat::Spread], 0.0);
        let text = profile_toml("TOK", 100, 200, false, 250, &matrix[0]);
        assert!(!text.contains("queue_model"), "{text}");
        toml::from_str::<toml::Value>(&text).expect("must parse as TOML");
    }

    /// The trailing profile mounts `trailing_scalper` with the user-fixed 2s in-strategy exit
    /// delay and carries NO spread_maker-only params.
    #[test]
    fn trailing_profile_names_the_scalper_with_its_exit_delay() {
        let matrix = run_matrix(&[Lane::L2], &[Strat::Trailing], 0.0);
        assert_eq!(matrix.len(), 2);
        let text = profile_toml("TOK", 100, 200, false, 250, &matrix[0]);
        assert!(text.contains("name = \"trailing_scalper\"\n"), "{text}");
        assert!(text.contains("exit_delay_ms = 2000\n"), "{text}");
        assert!(!text.contains("tick_size"), "{text}");
        assert!(!text.contains("min_half_spread_ticks"), "{text}");
        toml::from_str::<toml::Value>(&text).expect("must parse as TOML");
    }

    /// Every trailing config's profile carries the market's real `[from, to]` window as
    /// `market_open_ms`/`market_close_ms` — for BOTH the baseline (whose own cutoff knobs are 0,
    /// so this is inert) and `trail-cut` (whose knobs are actually armed).
    #[test]
    fn trailing_configs_carry_the_markets_window_as_open_close_params() {
        let matrix = run_matrix(&[Lane::L2], &[Strat::Trailing], 0.0);
        assert_eq!(matrix.len(), 2);
        for spec in &matrix {
            let text = profile_toml("TOK", 111, 222, false, 250, spec);
            assert!(text.contains("market_open_ms = 111\n"), "{}: {text}", spec.label);
            assert!(text.contains("market_close_ms = 222\n"), "{}: {text}", spec.label);
            toml::from_str::<toml::Value>(&text).expect("must parse as TOML");
        }
    }

    /// `trail-cut` names both entry-timing cutoff knobs at the user-fixed target values (5s open
    /// delay, 30s close cutoff) — the config this study measures against `trail-d2000`.
    #[test]
    fn trail_cut_config_arms_both_entry_timing_cutoffs() {
        let matrix = run_matrix(&[Lane::L2], &[Strat::Trailing], 0.0);
        let spec = matrix.iter().find(|s| s.label == "trail-cut").expect("trail-cut present");
        let text = profile_toml("TOK", 100, 200, false, 250, spec);
        assert!(text.contains("entry_open_delay_ms = 5000\n"), "{text}");
        assert!(text.contains("entry_cutoff_before_close_ms = 30000\n"), "{text}");
    }

    /// The `spread_maker` grid NEVER receives `market_open_ms`/`market_close_ms`.
    #[test]
    fn spread_maker_configs_never_carry_the_window_params() {
        let matrix = run_matrix(&[Lane::L2], &[Strat::Spread], 0.0);
        let text = profile_toml("TOK", 100, 200, false, 0, &matrix[0]);
        assert!(!text.contains("market_open_ms"), "{text}");
        assert!(!text.contains("market_close_ms"), "{text}");
    }

    /// both lanes × both strategies = (12 spread + 2 trailing) × 2 lanes = 28 runs per market.
    #[test]
    fn full_matrix_is_28_runs() {
        let matrix = run_matrix(&[Lane::L1, Lane::L2], &[Strat::Spread, Strat::Trailing], 0.0);
        assert_eq!(matrix.len(), 28);
    }

    /// 5m ⇒ exactly [end-300s, end]; 15m ⇒ [end-900s, end] — no pre-open or settlement padding.
    #[test]
    fn window_is_the_markets_exact_life() {
        assert_eq!(window_for("btc-5m", 1_000_000), (700_000, 1_000_000));
        assert_eq!(window_for("xrp-15m", 1_000_000), (100_000, 1_000_000));
    }

    // ---- params ------------------------------------------------------------------------------

    fn params(text: &str) -> toml::Value {
        toml::from_str::<toml::Value>(text).expect("test params must parse")
    }

    /// Absent knobs preserve the binary's defaults; `both` expands; junk is refused.
    #[test]
    fn lane_and_strategy_selectors_keep_the_binarys_defaults() {
        assert_eq!(parse_lanes(None), Some(vec![Lane::L2]));
        assert_eq!(parse_lanes(Some("l1")), Some(vec![Lane::L1]));
        assert_eq!(parse_lanes(Some("both")), Some(vec![Lane::L1, Lane::L2]));
        assert_eq!(parse_lanes(Some("l3")), None);
        assert_eq!(parse_strats(None), Some(vec![Strat::Spread]));
        assert_eq!(parse_strats(Some("trailing")), Some(vec![Strat::Trailing]));
        assert_eq!(parse_strats(Some("both")), Some(vec![Strat::Spread, Strat::Trailing]));
        assert_eq!(parse_strats(Some("maker")), None);
    }

    /// An EMPTY params table is the binary's no-flags invocation, minus the universe it always
    /// required.
    #[test]
    fn knob_defaults_match_the_binarys_no_flag_invocation() {
        let k = read_knobs(&params("")).expect("defaults");
        assert!(!k.fee);
        assert_eq!(k.floor, 0.0);
        assert_eq!(k.latency_ms, 0);
        assert_eq!(k.lanes, vec![Lane::L2]);
        assert_eq!(k.strats, vec![Strat::Spread]);
    }

    /// `floor = 1` is an Integer to the TOML parser; an operator writing a whole number of ticks
    /// must not have to know that.
    #[test]
    fn floor_accepts_an_integer_as_well_as_a_float() {
        assert_eq!(read_knobs(&params("floor = 1")).expect("int floor").floor, 1.0);
        assert_eq!(read_knobs(&params("floor = 0.5")).expect("float floor").floor, 0.5);
    }

    /// STRICTER than the binary, deliberately: `--floor banana` used to run the whole sweep with
    /// no floor and say nothing.
    #[test]
    fn a_wrong_typed_knob_is_refused_rather_than_defaulted() {
        for bad in ["floor = \"banana\"", "latency_ms = \"250ms\"", "fee = 1", "lane = 7"] {
            let err = read_knobs(&params(bad)).expect_err(bad);
            assert!(matches!(err, StudyError::Study(_)), "{bad}: {err}");
        }
        assert!(read_knobs(&params("latency_ms = -5")).is_err());
        assert!(read_knobs(&params("lane = \"l3\"")).is_err());
        assert!(read_knobs(&params("strategy = \"maker\"")).is_err());
    }

    /// The universe is DATA. A missing or malformed row is a refusal naming its index — where the
    /// binary silently dropped the market and shrank every denominator below it.
    #[test]
    fn the_universe_is_read_from_params_and_a_bad_row_is_refused() {
        let good = params(
            "[[universe]]\nfamily = \"btc-5m\"\ntoken = \"TOK\"\nend_date_ms = 1000000\n",
        );
        assert_eq!(
            read_universe(&good).expect("one market"),
            vec![Market {
                family: "btc-5m".to_string(),
                token: "TOK".to_string(),
                end_date_ms: 1_000_000,
            }]
        );
        assert!(read_universe(&params("")).is_err(), "absent universe");
        assert!(read_universe(&params("universe = []")).is_err(), "empty universe");
        assert!(
            read_universe(&params("[[universe]]\nfamily = \"f\"\ntoken = \"T\"\n")).is_err(),
            "row with no end_date_ms"
        );
        assert!(
            read_universe(&params("universe = [\"f\\tT\\t1\"]")).is_err(),
            "a TSV line is not a table — the study takes the columns, not the file"
        );
    }

    // ---- the context-backed store --------------------------------------------------------------

    /// A counting `HistStore` double: every `scan_quotes` call increments an `AtomicU64` and
    /// returns an empty `Vec` (the row VALUES don't matter for a hit-count proof). Every other
    /// verb is stubbed to the cheapest legal answer. This is the store the *caller* would have
    /// opened — it sits under the `StudyContext`, which the adapter under test sits on top of.
    struct CountingStore {
        quote_calls: std::sync::atomic::AtomicU64,
    }

    impl CountingStore {
        fn new() -> Self {
            Self { quote_calls: std::sync::atomic::AtomicU64::new(0) }
        }
        fn quotes_seen(&self) -> u64 {
            self.quote_calls.load(std::sync::atomic::Ordering::Relaxed)
        }
    }

    impl HistStore for CountingStore {
        fn load_bars(
            &self,
            _venue: &str,
            _symbol: &str,
            _interval: &str,
            _range: TsRange,
        ) -> Result<Vec<Bar>, DataError> {
            Ok(Vec::new())
        }

        fn scan_quotes(
            &self,
            _venue: &str,
            _symbol: &str,
            _range: TsRange,
        ) -> Result<Vec<QuoteTick>, DataError> {
            self.quote_calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Ok(Vec::new())
        }

        fn scan_trades(
            &self,
            _venue: &str,
            _symbol: &str,
            _range: TsRange,
        ) -> Result<Vec<TradeTick>, DataError> {
            Ok(Vec::new())
        }

        fn scan_book_updates(
            &self,
            _venue: &str,
            _symbol: &str,
            _range: TsRange,
        ) -> Result<Vec<BookUpdate>, DataError> {
            Ok(Vec::new())
        }

        fn scan_symbol_properties(
            &self,
            _venue: &str,
            _symbol: &str,
            _range: TsRange,
        ) -> Result<Vec<(i64, SymbolProperties)>, DataError> {
            Ok(Vec::new())
        }

        fn scan_equity(
            &self,
            _venue: &str,
            _symbol: &str,
            _range: TsRange,
        ) -> Result<Vec<EquitySample>, DataError> {
            Ok(Vec::new())
        }

        fn scan_exec_fills(
            &self,
            _venue: &str,
            _symbol: &str,
        ) -> Result<Vec<ExecFillRow>, DataError> {
            Ok(Vec::new())
        }

        fn scan_exec_orders(
            &self,
            _venue: &str,
            _symbol: &str,
        ) -> Result<Vec<ExecOrderRow>, DataError> {
            Ok(Vec::new())
        }

        fn append_bars(
            &self,
            _venue: &str,
            _symbol: &str,
            _interval: &str,
            _bars: &[Bar],
            _commit_key: Option<&str>,
        ) -> Result<usize, DataError> {
            Ok(0)
        }

        fn append_quotes(
            &self,
            _venue: &str,
            _symbol: &str,
            _ticks: &[QuoteTick],
            _commit_key: Option<&str>,
        ) -> Result<usize, DataError> {
            Ok(0)
        }

        fn append_trades(
            &self,
            _venue: &str,
            _symbol: &str,
            _ticks: &[TradeTick],
            _commit_key: Option<&str>,
        ) -> Result<usize, DataError> {
            Ok(0)
        }

        fn append_book_updates(
            &self,
            _venue: &str,
            _symbol: &str,
            _updates: &[BookUpdate],
            _commit_key: Option<&str>,
        ) -> Result<usize, DataError> {
            Ok(0)
        }

        fn append_symbol_properties(
            &self,
            _venue: &str,
            _symbol: &str,
            _rows: &[(i64, SymbolProperties)],
            _commit_key: Option<&str>,
        ) -> Result<usize, DataError> {
            Ok(0)
        }

        fn append_equity(
            &self,
            _venue: &str,
            _symbol: &str,
            _rows: &[EquitySample],
            _commit_key: Option<&str>,
        ) -> Result<usize, DataError> {
            Ok(0)
        }

        fn append_exec_fills(
            &self,
            _venue: &str,
            _symbol: &str,
            _rows: &[ExecFillRow],
            _commit_key: Option<&str>,
        ) -> Result<usize, DataError> {
            Ok(0)
        }

        fn append_exec_orders(
            &self,
            _venue: &str,
            _symbol: &str,
            _rows: &[ExecOrderRow],
            _commit_key: Option<&str>,
        ) -> Result<usize, DataError> {
            Ok(0)
        }

        fn resample_quotes_to_bars(
            &self,
            _venue: &str,
            _symbol: &str,
            _interval: &str,
            _range: TsRange,
            _commit_key: Option<&str>,
        ) -> Result<usize, DataError> {
            Ok(0)
        }

        fn resample_trades_to_bars(
            &self,
            _venue: &str,
            _symbol: &str,
            _interval: &str,
            _range: TsRange,
            _commit_key: Option<&str>,
        ) -> Result<usize, DataError> {
            Ok(0)
        }
    }

    fn ctx_over(store: Arc<dyn HistStore + Send + Sync>) -> StudyContext {
        StudyContext::new(store, TsRange::all(), PathBuf::from("."))
    }

    /// Two identical `scan_quotes` calls through the adapter reach the underlying store exactly
    /// ONCE; a DIFFERENT range reaches it again — proving the memoization is keyed on the full
    /// `(venue, symbol, range)` tuple, not just `(venue, symbol)`.
    #[test]
    fn identical_scan_hits_the_store_once_different_range_hits_again() {
        let inner = Arc::new(CountingStore::new());
        let cached = ContextStore::new(ctx_over(inner.clone()));

        let r1 = cached.scan_quotes("polymarket", "TOK", TsRange::of(100, 200)).unwrap();
        let r2 = cached.scan_quotes("polymarket", "TOK", TsRange::of(100, 200)).unwrap();
        assert_eq!(r1, r2);
        assert_eq!(
            inner.quotes_seen(),
            1,
            "the second identical scan must be served from the cache, not the store"
        );

        let _r3 = cached.scan_quotes("polymarket", "TOK", TsRange::of(300, 400)).unwrap();
        assert_eq!(inner.quotes_seen(), 2, "a different range is a miss and must reach the store");
    }

    /// One market's 28 cells each call `scan_quotes` on the SAME `(venue, symbol, range)` — the
    /// exact shape the per-market closure produces. The store must see exactly one call.
    #[test]
    fn a_whole_markets_matrix_still_hits_the_store_once() {
        let inner = Arc::new(CountingStore::new());
        let cached = ContextStore::new(ctx_over(inner.clone()));
        for _ in 0..28 {
            cached.scan_quotes("polymarket", "TOK", TsRange::of(1, 2)).unwrap();
        }
        assert_eq!(inner.quotes_seen(), 1);
    }

    /// The narrowings are asserted, not merely documented: the verbs with no context behind them
    /// REFUSE. An `Ok(empty)` here would let "this contract cannot ask" wear the same answer as
    /// "the store holds nothing", and `properties_as_of` inherits the refusal through the trait's
    /// own default.
    #[test]
    fn the_unreachable_verbs_refuse_rather_than_answer_empty() {
        let cached = ContextStore::new(ctx_over(Arc::new(CountingStore::new())));
        assert!(cached.scan_symbol_properties("polymarket", "TOK", TsRange::all()).is_err());
        assert!(cached.properties_as_of("polymarket", "TOK", 1).is_err());
        assert!(cached.scan_equity("polymarket", "TOK", TsRange::all()).is_err());
        assert!(cached.scan_exec_fills("polymarket", "TOK").is_err());
        assert!(cached.scan_exec_orders("polymarket", "TOK").is_err());
        assert!(cached.append_quotes("polymarket", "TOK", &[], None).is_err());
        assert!(cached.list_series().is_err(), "the trait's own refusal, inherited on purpose");
    }

    // ---- the seam ------------------------------------------------------------------------------

    /// A [`StudySim`] double: records every profile it was handed and answers a fixed outcome.
    ///
    /// No `#[derive(Default)]`: `SimOutcome` is `Copy` but NOT `Default`, so the derive would not
    /// compile — the answer is always supplied explicitly by [`scripted`].
    struct ScriptedSim {
        seen: Mutex<Vec<toml::Value>>,
        answer: SimOutcome,
    }

    impl StudySim for ScriptedSim {
        fn run_one(
            &self,
            profile: &toml::Value,
            _store: Arc<dyn HistStore + Send + Sync>,
        ) -> Result<SimOutcome, String> {
            self.seen.lock().unwrap().push(profile.clone());
            Ok(self.answer)
        }
    }

    fn scripted(answer: SimOutcome) -> Arc<ScriptedSim> {
        Arc::new(ScriptedSim { seen: Mutex::new(Vec::new()), answer })
    }

    /// No simulator ⇒ `NoSim`, naming the work, never a number.
    #[test]
    fn a_host_with_no_simulator_is_refused_by_name() {
        let ctx = ctx_over(Arc::new(CountingStore::new()));
        let p = params("[[universe]]\nfamily = \"btc-5m\"\ntoken = \"TOK\"\nend_date_ms = 1000\n");
        match run(&ctx, &p) {
            Err(StudyError::NoSim(what)) => {
                assert!(what.contains("12 Polymarket maker backtests"), "{what}");
                assert!(what.contains("1 markets"), "{what}");
            }
            other => panic!("expected NoSim, got {other:?}"),
        }
    }

    /// The whole sweep, end to end, through the seam: the full matrix runs once per market and the
    /// aggregate lands as metrics under the stable `{lane}/{family}/{config}/{metric}` name.
    #[test]
    fn the_sweep_aggregates_one_metric_group_per_cell() {
        let sim = scripted(SimOutcome { total_return: 0.25, n_trades: 4, win_rate: 0.5 });
        let ctx = ctx_over(Arc::new(CountingStore::new())).with_sim(sim.clone());
        let p = params(
            "lane = \"both\"\nstrategy = \"both\"\nfee = true\nlatency_ms = 250\n\
             [[universe]]\nfamily = \"btc-5m\"\ntoken = \"A\"\nend_date_ms = 1000000\n\
             [[universe]]\nfamily = \"btc-5m\"\ntoken = \"B\"\nend_date_ms = 2000000\n",
        );
        let out = run(&ctx, &p).expect("the sweep runs");

        assert_eq!(out.metric_value("markets"), Some(2.0));
        assert_eq!(out.metric_value("runs"), Some(56.0)); // 2 markets x 28 cells
        assert_eq!(out.metric_value("failed"), Some(0.0));
        assert_eq!(out.metric_value("cells"), Some(28.0));
        assert_eq!(out.metric_value("param/fee"), Some(1.0));
        assert_eq!(out.metric_value("param/latency_ms"), Some(250.0));
        // one cell, both markets folded into it
        assert_eq!(out.metric_value("l2/btc-5m/as-g0.05/n"), Some(2.0));
        assert_eq!(out.metric_value("l2/btc-5m/as-g0.05/sum_ret"), Some(0.5));
        assert_eq!(out.metric_value("l2/btc-5m/as-g0.05/trades"), Some(8.0));
        assert_eq!(out.metric_value("l2/btc-5m/as-g0.05/avg_win"), Some(0.5));
        assert_eq!(out.metric_value("l1/btc-5m/trail-cut/n"), Some(2.0));
        assert_eq!(sim.seen.lock().unwrap().len(), 56);

        let names: Vec<&str> = out.artifacts().iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["per_cell.tsv", "failures.tsv"]);
    }

    /// A host that refuses a run is counted and REPORTED, not swallowed: the binary's stderr line
    /// becomes a row of `failures.tsv`.
    #[test]
    fn a_refused_run_lands_in_the_failures_artifact() {
        struct AlwaysRefuses;
        impl StudySim for AlwaysRefuses {
            fn run_one(
                &self,
                _profile: &toml::Value,
                _store: Arc<dyn HistStore + Send + Sync>,
            ) -> Result<SimOutcome, String> {
                Err("no strategy named spread_maker\nsecond line".to_string())
            }
        }
        let ctx = ctx_over(Arc::new(CountingStore::new())).with_sim(Arc::new(AlwaysRefuses));
        let p = params("[[universe]]\nfamily = \"btc-5m\"\ntoken = \"A\"\nend_date_ms = 1000\n");
        let out = run(&ctx, &p).expect("a refused RUN is not a refused STUDY");
        assert_eq!(out.metric_value("failed"), Some(12.0));
        assert_eq!(out.metric_value("runs"), Some(0.0));
        assert_eq!(out.metric_value("markets"), Some(0.0));
        let (_, body) = out.artifacts().iter().find(|(n, _)| n == "failures.tsv").expect("present");
        assert!(body.contains("A\tas-g0.05\trun\tno strategy named spread_maker second line\n"));
    }

    /// A market whose life falls outside the window the run was handed is COUNTED, never clamped:
    /// truncating a market's window would silently change what its return means.
    #[test]
    fn markets_outside_the_handed_window_are_counted_not_clamped() {
        let sim = scripted(SimOutcome { total_return: 0.0, n_trades: 0, win_rate: 0.0 });
        let store: Arc<dyn HistStore + Send + Sync> = Arc::new(CountingStore::new());
        let ctx = StudyContext::new(store, TsRange::of(0, 1_000), PathBuf::from("."))
            .with_sim(sim.clone());
        let p = params("[[universe]]\nfamily = \"btc-5m\"\ntoken = \"A\"\nend_date_ms = 9000000\n");
        let out = run(&ctx, &p).expect("still runs");
        assert_eq!(out.metric_value("markets_outside_window"), Some(1.0));
        assert_eq!(out.metric_value("markets_requested"), Some(1.0));
        assert_eq!(sim.seen.lock().unwrap().len(), 12, "not clamped away");
    }
}
