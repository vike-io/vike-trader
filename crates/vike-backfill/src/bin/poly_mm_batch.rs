//! `poly_mm_batch` — sweep the Group-B market-making models over a universe of Polymarket outcome
//! tokens and aggregate the results IN-PROCESS, reusing the exact same Rust machinery the single-run
//! `poly_ch_backtest` bin uses: [`vike_backtest::harness::run_backtest`] over a
//! [`ClickHousePolyHistStore`]. There is NO external JSON/aggregation tooling — every backtest, the
//! metric extraction, and the group aggregation are Rust; the only non-Rust process is
//! `clickhouse-client`, which is the store's own DB driver (the same one `poly_ch_backtest` shells).
//!
//! For each token it runs the [`CONFIGS`] grid — a small SWEEP over each model's key knob (A-S `γ`,
//! LMSR `b`, LS-LMSR `α`, Glosten–Milgrom `μ`) — over the recorded `kind = "tick"` lane (quotes +
//! trades + books, so the `prob_power` queue model fills resting quotes against the real taker tape).
//! It sums the fractional return + trade count and averages the win rate per `(lane, family, config)`
//! and prints one table grouped by lane, then family, then config.
//!
//! ```sh
//! poly_mm_batch --universe universe.tsv [--fee] [--floor N] [--latency-ms N] [--lane l1|l2|both] [--strategy spread|trailing|both] [--db polymarket] [--clickhouse-bin clickhouse-client] [--store DIR | --archive PATH]
//! ```
//!
//! - **`--archive PATH` is the preferred way to run against downloaded archive data.** It reads the
//!   `data.vike.io` Parquet day files IN PLACE via [`ArchiveParquetHistStore`] — no import, no
//!   second copy. `PATH` may be a directory (every `*.parquet` under it) or a single day file.
//!   Measured on the same 1.20 GiB / 74.4M-row / 562-token day
//!   (`.superpowers/sdd/2026-07-28-poly-mm-latency-batch/archive-store-report.md`): **1.2 s** for a
//!   one-market slice — 6 of 73 row groups, 8.3% of compressed bytes, 92 MB RSS — against **128.6 s**
//!   (23.6 s download + ~105 s import) to get the same day into a `--store` DataFusion store first.
//!   The crossover is ~195 token-scans, above which the one-time import amortises; a 20-token run is
//!   13.2 s here, still ~10x ahead. `--archive` WINS over a simultaneous `--store`.
//!
//! - `--store DIR` swaps the read-side `HistStore` from the live `ClickHousePolyHistStore` (one
//!   `clickhouse-client` shell per scan verb per market, ~1.5-2s overhead each) to a local
//!   `vike_data::DataFusionHist` Parquet store at `DIR` — the fast path for a token universe
//!   already one-time-exported via `clickhouse_poly_backfill --kind all --store DIR` (see that
//!   bin's module doc for the exact export invocation). Per-token scans drop from ~seconds to tens
//!   of milliseconds. Absent (the default) is byte-identical to before this flag existed: the live
//!   ClickHouse bridge, `--db`/`--clickhouse-bin` honored exactly as today. The per-market
//!   `CachedHistStore` memoizing wrapper (below) sits in front of EITHER backend unchanged.
//!
//! - `--fee` adds the real Polymarket `0.072·p(1−p)` per-fill taker cost (`[engine.fee]`
//!   `probability_scaled`); absent ⇒ gross (no fee).
//! - `--floor N` sets `min_half_spread_ticks` (default **0** — NO floor, so the LS-LMSR / GM spread
//!   formulas are not collapsed onto the same tick; the earlier `=1` floor made them identical).
//! - `--latency-ms N` (default **0**) arms `[engine] order_latency_ms` — every order action (place/
//!   modify/cancel) reaches the venue that much later; `0` emits no line, byte-identical to before.
//! - `--lane l1|l2|both` (default **l2**) selects the fill-realism lane: `l2` keeps the queue-position
//!   model against the real taker tape (`queue_model = "prob_power"`, today's behavior); `l1` is the
//!   optimistic spread-crossing Tick fill (no queue model); `both` runs each config through both.
//! - `--strategy spread|trailing|both` (default **spread**) selects the strategy family: `spread` is
//!   today's 12-config `spread_maker` grid; `trailing` is the single `trail-d2000` scalper, whose 2s
//!   exit delay is IN-STRATEGY and stacks with `--latency-ms`; `both` runs the union.
//!
//! The `--universe` file is a plain TSV, one market per line: `<family>\t<token_id>\t<end_date_ms>`.
//! The per-market window is the market's exact life, `[end_date − tenor, end_date]` (5m ⇒ tenor
//! `300_000`, 15m ⇒ `900_000` — see [`window_for`]). The former `[end−tenor−60s, end+60s]` padding
//! was CONTAMINATED (60s pre-open flat + 60s post-close settlement) and has been dropped.
//!
//! Every config in a market's run matrix replays the SAME `(token, window)` tick slice, so
//! [`CachedHistStore`] wraps the shared store once per market (inside the `par_iter` closure) and
//! memoizes each scan verb's result — turning what was 26x redundant `clickhouse-client` shells
//! per market into one.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use rayon::prelude::*;

use vike_backfill::archive_store::ArchiveParquetHistStore;
use vike_backfill::backtest_bridge::{ClickHousePolyHistStore, DB};
use vike_backfill::cli::{CliSpec, arg, has_flag, log_config, scratch_root};
use vike_backtest::harness::{self, BacktestProfile};
use vike_data::{DataError, DataFusionHist, ExecFillRow, ExecOrderRow, HistStore, TsRange};
use vike_model::{Bar, BookUpdate, EquitySample, QuoteTick, SymbolProperties, TradeTick};

/// The sweep grid: `(label, [strategy.params] body)`. Each body carries the maker's `γ` plus the
/// model selector + its swept knob. Labels sort so a family's rows group by model then knob.
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

/// One config run's result row: `(lane, family, config label, total_return, n_trades, win_rate)`.
type ConfigRow = (String, String, String, f64, u64, f64);

/// Trailing-scalper configs: the user-fixed 2s IN-STRATEGY reaction gap (`exit_delay_ms`),
/// mid-following flatten (`profit_target = 0`). NOTE: the engine's `order_latency_ms` stacks on
/// top of this 2s (deliberate — see the 2026-07-28 latency-realism spec).
///
/// `trail-d2000` is today's baseline — no entry-timing cutoffs, byte-identical to before this
/// const grew a second row. `trail-cut` arms BOTH entry-timing cutoffs
/// (`crates/vike-strategy/src/trailing_scalper.rs`'s "Entry-timing cutoffs" section): suppress
/// entries for the first 5s after open (`entry_open_delay_ms = 5000`, avoiding the chaotic
/// just-opened book) and stop posting/re-pricing fresh entries in the last 30s before close
/// (`entry_cutoff_before_close_ms = 30000`, so a late fill under venue latency still has time to
/// exit before resolution). `market_open_ms`/`market_close_ms` are NOT baked in here — every
/// `RunSpec` built from these bodies gets its OWN market's window appended per-token by
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

/// One (lane, strategy-config) cell of the run matrix.
struct RunSpec {
    lane: Lane,
    label: &'static str,
    strategy: &'static str,
    /// the full `[strategy.params]` body
    params: String,
}

/// Expand lanes x strategies into the per-market run list: 12 spread configs (each carrying the
/// shared qty/tick_size/floor prefix) + 1 trailing config, per lane.
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

/// `--lane` reader: absent/`l2` keeps today's queue-model lane; `l1` the optimistic Tick lane;
/// `both` runs each config through the two lanes. Unknown ⇒ `None` (caller exits 2).
fn parse_lanes(s: Option<&str>) -> Option<Vec<Lane>> {
    match s {
        None | Some("l2") => Some(vec![Lane::L2]),
        Some("l1") => Some(vec![Lane::L1]),
        Some("both") => Some(vec![Lane::L1, Lane::L2]),
        _ => None,
    }
}

/// `--strategy` reader: absent/`spread` keeps today's grid; `trailing` the scalper; `both` = union.
fn parse_strats(s: Option<&str>) -> Option<Vec<Strat>> {
    match s {
        None | Some("spread") => Some(vec![Strat::Spread]),
        Some("trailing") => Some(vec![Strat::Trailing]),
        Some("both") => Some(vec![Strat::Spread, Strat::Trailing]),
        _ => None,
    }
}

/// `--latency-ms` reader: absent ⇒ `Some(0)` (today's zero-latency default); present ⇒ a
/// non-negative integer or `None` (caller exits 2 — a silently-zero latency run would defeat
/// the whole experiment).
fn parse_latency_ms(s: Option<&str>) -> Option<i64> {
    match s {
        None => Some(0),
        Some(s) => s.parse::<i64>().ok().filter(|n| *n >= 0),
    }
}

/// Which read-side `HistStore` backend the store flags select. `ClickHouse` (no flag, the default)
/// is today's live bridge; `Local(dir)` is the one-time-imported `DataFusionHist`; `Archive(path)`
/// reads downloaded archive Parquet **in place**, with no import at all.
///
/// **Prefer `Archive` for an archive-backed run.** `Local` requires the day to be imported first,
/// and that import is a pure format round-trip — the archive is already Parquet and the store is
/// already Parquet, so it decodes and re-encodes what it just decoded, paying a per-series commit
/// floor for each of the day's ~562 tokens. Measured on the same 1.20 GiB / 74.4M-row / 562-token
/// day (`.superpowers/sdd/2026-07-28-poly-mm-latency-batch/archive-store-report.md`):
///
/// | path | one-market slice |
/// |---|---|
/// | `Archive` (read in place) | **1.2 s** — 6/73 row groups, 8.3% of compressed bytes, 92 MB RSS |
/// | `Local` (download + import first) | **128.6 s** — 23.6 s download + ~105 s import |
///
/// The crossover is ~195 token-scans: below that, reading in place wins outright; above it the
/// one-time import amortises. A 20-token run is 13.2 s on `Archive`, still ~10x ahead.
enum StoreSel {
    ClickHouse,
    Local(PathBuf),
    Archive(PathBuf),
}

/// Pure store-flag reader. `--archive` WINS over `--store` when both are given: it is the cheaper
/// path and naming it is an explicit request for it, so silently preferring the import would be the
/// surprising reading. Absent both -> [`StoreSel::ClickHouse`] (today's default, byte-identical).
/// No validation here — a bad/missing path surfaces as an `open_store` error, not a parse error.
fn parse_store_sel(store_arg: Option<&str>, archive_arg: Option<&str>) -> StoreSel {
    match (archive_arg, store_arg) {
        (Some(p), _) => StoreSel::Archive(PathBuf::from(p)),
        (None, Some(dir)) => StoreSel::Local(PathBuf::from(dir)),
        (None, None) => StoreSel::ClickHouse,
    }
}

/// Build the `Arc<dyn HistStore + Send + Sync>` `main` hands to every market's `CachedHistStore`
/// wrapper, per `sel`. `ch_bin`/`db` are only consulted for the `ClickHouse` selection (and
/// constructing that variant touches no network — it opens lazily per scan, exactly as before this
/// flag existed).
/// `scratch` is `<project>/tmp`, where the ClickHouse bridge stages each export on its way into the
/// decoder (`vike_backfill::cli::scratch_root`). Only the `ClickHouse` selection consults it; the
/// other two read files the caller named. It is a PARAMETER rather than something the bridge
/// resolves, because a library must not reach for global state its caller cannot see.
fn open_store(
    sel: StoreSel,
    ch_bin: String,
    db: String,
    scratch: PathBuf,
) -> Result<Arc<dyn HistStore + Send + Sync>, String> {
    match sel {
        StoreSel::ClickHouse => Ok(Arc::new(ClickHousePolyHistStore::new(ch_bin, db, scratch))),
        StoreSel::Local(dir) => DataFusionHist::open(&dir)
            .map(|h| Arc::new(h) as Arc<dyn HistStore + Send + Sync>)
            .map_err(|e| format!("open local store at {}: {e}", dir.display())),
        // A directory takes every `*.parquet` in it (one file per UTC day is the archive's shape);
        // a single file path takes just that file — so `--archive dl/` and
        // `--archive dl/btc5m_2026-07-28.parquet` both do the obvious thing.
        StoreSel::Archive(path) => {
            let store = if path.is_dir() {
                ArchiveParquetHistStore::from_dir(&path)
                    .map_err(|e| format!("open archive dir {}: {e}", path.display()))?
            } else {
                ArchiveParquetHistStore::from_files([path.clone()])
            };
            if store.files().is_empty() {
                return Err(format!("no .parquet files under {}", path.display()));
            }
            Ok(Arc::new(store) as Arc<dyn HistStore + Send + Sync>)
        }
    }
}

/// Build the profile TOML for one `(token, window, run-spec)`. `latency_ms > 0` arms the engine's
/// order-latency gate (every place/modify/cancel reaches matching that much later); `0` emits no
/// line, keeping the no-flag TOML byte-identical to the pre-flag builder. The L2 lane keeps
/// `queue_model = "prob_power"` (resting quotes fill against the real taker tape); the L1 lane
/// omits it (optimistic Tick crossing).
///
/// For the `trailing_scalper` strategy ONLY, the market's exact `[from, to]` window — already
/// computed by [`window_for`] as this market's real open/close — is ALSO appended to
/// `[strategy.params]` as `market_open_ms`/`market_close_ms`: the reference timestamps
/// `TrailingScalper`'s `entry_open_delay_ms`/`entry_cutoff_before_close_ms` cutoffs need (see that
/// module's "Entry-timing cutoffs" doc). Appended unconditionally (even for `trail-d2000`, whose
/// own cutoff knobs are 0) because `TrailingScalper::entries_allowed` only consults an open/close
/// timestamp when its OWN delay/cutoff knob is armed — so this addition is inert for any trailing
/// config that doesn't ask for a cutoff, and the `spread_maker` strategy never receives these keys
/// at all (`SpreadMaker::from_params` would just ignore them, but omitting them entirely keeps the
/// existing spread-grid TOML byte-for-byte unchanged).
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

// ---- per-market memoizing HistStore wrapper --------------------------------------------------
//
// The 26 `CONFIGS`/`TRAILING_CONFIGS` x lane runs for one market all replay the IDENTICAL
// (venue, token, window) tick slice — only `[strategy]`/`[engine]` differ across runs, never
// `[data]` — so `harness::run_backtest` re-fetching `scan_quotes`/`scan_trades`/
// `scan_book_updates` from `ClickHousePolyHistStore` 26 times per market was 26x redundant
// `clickhouse-client` shells + Parquet export + Arrow decode for the exact same rows. This wrapper
// memoizes each read verb's result the first time it is asked for a given
// `(venue, symbol, range)` and clones the cached `Vec`/`Option` on every later call.

/// Verb discriminant folded into the cache key (see [`CachedHistStore`]) — plain `u8` codes rather
/// than a keyed enum so the key tuple stays `Hash`/`Eq` with zero extra derive plumbing.
const VERB_QUOTES: u8 = 0;
const VERB_TRADES: u8 = 1;
const VERB_BOOKS: u8 = 2;
const VERB_BARS: u8 = 3;
const VERB_PROPERTIES: u8 = 4;

/// One memoized read verb's decoded result.
#[derive(Clone)]
enum CachedRows {
    Quotes(Vec<QuoteTick>),
    Trades(Vec<TradeTick>),
    Books(Vec<BookUpdate>),
    Bars(Vec<Bar>),
    Properties(Option<SymbolProperties>),
}

/// The memoization cache key: `(verb, venue, symbol, from, to)` — `load_bars`' extra `interval`
/// axis is folded into the symbol slot (`"{symbol}\0{interval}"`) rather than widening the tuple,
/// since every other verb has no such axis. A named alias (clippy's `type_complexity` gate).
type CacheKey = (u8, String, String, i64, i64);

/// Per-market memoizing [`HistStore`] wrapper: the 26 configs of one market all replay the
/// identical (token, window) slice, so each scan verb hits the wrapped store once and every later
/// call clones the cached rows. One instance per market (bounded memory, no cross-market reuse).
struct CachedHistStore {
    inner: Arc<dyn HistStore + Send + Sync>,
    cache: Mutex<HashMap<CacheKey, CachedRows>>,
}

impl CachedHistStore {
    fn new(inner: Arc<dyn HistStore + Send + Sync>) -> Self {
        Self { inner, cache: Mutex::new(HashMap::new()) }
    }

    /// `TsRange`'s open bounds resolve to the widest representable `i64` pair, mirroring
    /// `backtest_bridge::range_bounds` — so an unbounded scan still gets a stable cache key.
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
}

impl HistStore for CachedHistStore {
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
            || self.inner.load_bars(venue, symbol, interval, range),
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
            || self.inner.scan_quotes(venue, symbol, range),
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
            || self.inner.scan_trades(venue, symbol, range),
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
            || self.inner.scan_book_updates(venue, symbol, range),
        )
    }

    /// FORWARDED rather than inherited, and NOT memoized: this bin never reads depth, so there is
    /// no `CachedRows` variant to key it on.
    ///
    /// Both halves of the trait's depth seam default to REFUSING — "this store serves no depth
    /// lane" — which is the honest answer for a LEAF store and a false one for this type, which
    /// serves no lane of its own because it fronts one. Whether depth is available is `inner`'s
    /// fact to state, so both verbs pass straight through and `inner` answers for itself.
    fn scan_depth(
        &self,
        venue: &str,
        symbol: &str,
        range: TsRange,
    ) -> Result<Vec<BookUpdate>, DataError> {
        self.inner.scan_depth(venue, symbol, range)
    }

    /// The write twin of [`CachedHistStore::scan_depth`], forwarded for the same reason.
    fn append_depth(
        &self,
        venue: &str,
        symbol: &str,
        updates: &[BookUpdate],
        commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        self.inner.append_depth(venue, symbol, updates, commit_key)
    }

    /// FORWARDED rather than inherited, and NOT memoized — the same shape, for the same reason, as
    /// the depth pair above: this bin never lists the catalog, so there is no `CachedRows` variant
    /// to key it on, and the trait's default — a refusal, "this store cannot enumerate its
    /// inventory" — is the honest answer for a LEAF store without the verbs and a false one for
    /// this type, which enumerates nothing of its own because it fronts a store that can. What the
    /// catalog holds is `inner`'s fact to state, so both verbs pass straight through.
    fn list_series(&self) -> Result<Vec<vike_data::SeriesId>, DataError> {
        self.inner.list_series()
    }

    /// The coverage twin of [`CachedHistStore::list_series`], forwarded for the same reason.
    fn inventory(
        &self,
    ) -> Result<Vec<(vike_data::SeriesId, vike_data::SeriesCoverage)>, DataError> {
        self.inner.inventory()
    }

    /// Point-in-time lookup: memoized on `(venue, symbol, ts)` directly (rather than left to the
    /// trait's `scan_symbol_properties`-derived default), since a fresh `ts` per run-spec would
    /// otherwise miss the range-keyed cache below even when the underlying series never changes.
    fn properties_as_of(
        &self,
        venue: &str,
        symbol: &str,
        ts: i64,
    ) -> Result<Option<SymbolProperties>, DataError> {
        let key = (VERB_PROPERTIES, venue.to_string(), symbol.to_string(), ts, ts);
        self.memoize(
            key,
            CachedRows::Properties,
            |c| if let CachedRows::Properties(v) = c { Some(*v) } else { None },
            || self.inner.properties_as_of(venue, symbol, ts),
        )
    }

    // ---- everything else passes straight through (writes + the rarely-hit reads this bin never
    // exercises in tick mode) — no memoization, no behavior change from the wrapped store. ----

    fn scan_symbol_properties(
        &self,
        venue: &str,
        symbol: &str,
        range: TsRange,
    ) -> Result<Vec<(i64, SymbolProperties)>, DataError> {
        self.inner.scan_symbol_properties(venue, symbol, range)
    }

    fn scan_equity(
        &self,
        venue: &str,
        symbol: &str,
        range: TsRange,
    ) -> Result<Vec<EquitySample>, DataError> {
        self.inner.scan_equity(venue, symbol, range)
    }

    fn scan_exec_fills(&self, venue: &str, symbol: &str) -> Result<Vec<ExecFillRow>, DataError> {
        self.inner.scan_exec_fills(venue, symbol)
    }

    fn scan_exec_orders(&self, venue: &str, symbol: &str) -> Result<Vec<ExecOrderRow>, DataError> {
        self.inner.scan_exec_orders(venue, symbol)
    }

    fn append_bars(
        &self,
        venue: &str,
        symbol: &str,
        interval: &str,
        bars: &[Bar],
        commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        self.inner.append_bars(venue, symbol, interval, bars, commit_key)
    }

    fn append_quotes(
        &self,
        venue: &str,
        symbol: &str,
        ticks: &[QuoteTick],
        commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        self.inner.append_quotes(venue, symbol, ticks, commit_key)
    }

    fn append_trades(
        &self,
        venue: &str,
        symbol: &str,
        ticks: &[TradeTick],
        commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        self.inner.append_trades(venue, symbol, ticks, commit_key)
    }

    fn append_book_updates(
        &self,
        venue: &str,
        symbol: &str,
        updates: &[BookUpdate],
        commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        self.inner.append_book_updates(venue, symbol, updates, commit_key)
    }

    fn append_symbol_properties(
        &self,
        venue: &str,
        symbol: &str,
        rows: &[(i64, SymbolProperties)],
        commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        self.inner.append_symbol_properties(venue, symbol, rows, commit_key)
    }

    fn append_equity(
        &self,
        venue: &str,
        symbol: &str,
        rows: &[EquitySample],
        commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        self.inner.append_equity(venue, symbol, rows, commit_key)
    }

    fn append_exec_fills(
        &self,
        venue: &str,
        symbol: &str,
        rows: &[ExecFillRow],
        commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        self.inner.append_exec_fills(venue, symbol, rows, commit_key)
    }

    fn append_exec_orders(
        &self,
        venue: &str,
        symbol: &str,
        rows: &[ExecOrderRow],
        commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        self.inner.append_exec_orders(venue, symbol, rows, commit_key)
    }

    fn resample_quotes_to_bars(
        &self,
        venue: &str,
        symbol: &str,
        interval: &str,
        range: TsRange,
        commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        self.inner.resample_quotes_to_bars(venue, symbol, interval, range, commit_key)
    }

    fn resample_trades_to_bars(
        &self,
        venue: &str,
        symbol: &str,
        interval: &str,
        range: TsRange,
        commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        self.inner.resample_trades_to_bars(venue, symbol, interval, range, commit_key)
    }

    // list_series / inventory / series_gaps / append_funding / scan_funding /
    // append_chain_snapshot / scan_chain / chain_as_of / chain_as_of_within stay on the trait's
    // own defaults (empty/no-op) — this bin never calls them.
}

const USAGE: &str = "\
usage: poly_mm_batch --universe PATH [--store DIR | --archive DIR] [--db NAME]
                     [--clickhouse-bin BIN] [--lane l1|l2|both] [--strategy spread|trailing|both]
                     [--latency-ms N] [--floor F] [--fee]

Run the pluggable maker models over a universe of Polymarket tokens and aggregate the resulting
BacktestReports in process, fanned across all cores. All-Rust: no external aggregation tooling.

  --universe PATH       TSV universe, one line per market: family<TAB>token<TAB>end_date_ms
  --store DIR           read from a DataFusion hist store at DIR
  --archive DIR         read the downloaded archive Parquet in place instead
                        (neither: read the ClickHouse tables)
  --db NAME             the ClickHouse database to read
  --clickhouse-bin BIN  the clickhouse client to spawn (default clickhouse-client)
  --lane L              l1 | l2 | both — which quote lane to simulate
  --strategy S          spread | trailing | both
  --latency-ms N        simulated round-trip latency, non-negative integer ms. This is the knob the
                        edge is most sensitive to: the measured up/down MM edge is +60%/day at 0 ms
                        and -42% by 300 ms.
  --floor F             minimum spread floor (default 0 = no floor, so the spread-source formulas
                        are not all collapsed onto the same tick)
  --fee                 charge the venue fee
  -h, --help            print this and exit 0
  -V, --version         print the version and exit 0";

const SPEC: CliSpec = CliSpec {
    bin: "poly_mm_batch",
    usage: USAGE,
    valued: &[
        "--universe",
        "--store",
        "--archive",
        "--db",
        "--clickhouse-bin",
        "--lane",
        "--strategy",
        "--latency-ms",
        "--floor",
    ],
    toggles: &["--fee"],
    positionals: 0,
};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if let Some(code) = SPEC.short_circuit(&args) {
        return code;
    }
    let _log_guards = vike_log::init(log_config("poly-mm-batch"));

    let Some(universe_path) = arg(&args, "--universe") else {
        eprintln!("poly_mm_batch: --universe is required\n\n{USAGE}");
        return ExitCode::from(2);
    };
    let db = arg(&args, "--db").unwrap_or_else(|| DB.to_string());
    let ch_bin = arg(&args, "--clickhouse-bin").unwrap_or_else(|| "clickhouse-client".to_string());
    let fee = has_flag(&args, "--fee");
    // Default floor is 0 (NO floor) so the spread-source formulas are not collapsed to the same tick.
    let floor: f64 = arg(&args, "--floor").and_then(|s| s.parse().ok()).unwrap_or(0.0);
    let latency_arg = arg(&args, "--latency-ms");
    let Some(latency_ms) = parse_latency_ms(latency_arg.as_deref()) else {
        eprintln!(
            "poly_mm_batch: invalid --latency-ms {:?} (non-negative integer milliseconds)",
            latency_arg.unwrap_or_default()
        );
        return ExitCode::from(2);
    };
    let lane_arg = arg(&args, "--lane");
    let Some(lanes) = parse_lanes(lane_arg.as_deref()) else {
        eprintln!("poly_mm_batch: unknown --lane {:?} (l1|l2|both)", lane_arg.unwrap_or_default());
        return ExitCode::from(2);
    };
    let strat_arg = arg(&args, "--strategy");
    let Some(strats) = parse_strats(strat_arg.as_deref()) else {
        eprintln!(
            "poly_mm_batch: unknown --strategy {:?} (spread|trailing|both)",
            strat_arg.unwrap_or_default()
        );
        return ExitCode::from(2);
    };

    let universe = match std::fs::read_to_string(&universe_path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("poly_mm_batch: failed to read universe {universe_path:?}: {e}");
            return ExitCode::FAILURE;
        }
    };

    let store_sel =
        parse_store_sel(arg(&args, "--store").as_deref(), arg(&args, "--archive").as_deref());
    // `<project>/tmp` — resolved HERE, at the composition root, and swept on the way. Only the
    // ClickHouse selection stages anything into it; the other two read files the operator named.
    let scratch = scratch_root(&std::env::vars().collect());
    let store: Arc<dyn HistStore + Send + Sync> = match open_store(store_sel, ch_bin, db, scratch) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("poly_mm_batch: {e}");
            return ExitCode::FAILURE;
        }
    };

    // Parallelize over MARKETS (rayon): each market runs its CONFIGS independently and returns its
    // per-config results, fanned across the box's cores. The store is `Arc<dyn HistStore + Send +
    // Sync>` cloned per run, and `ClickHousePolyHistStore` shells an INDEPENDENT `clickhouse-client`
    // per scan, so concurrent markets never race. Cap concurrency with `RAYON_NUM_THREADS=N` if the
    // ClickHouse server saturates. Aggregation is done sequentially afterward (cheap).
    let lines: Vec<&str> = universe.lines().collect();
    let total_markets = lines.len();
    let done = std::sync::atomic::AtomicU64::new(0);
    let matrix = run_matrix(&lanes, &strats, floor);
    let per_market: Vec<(Vec<ConfigRow>, u64)> = lines
        .par_iter()
        .map(|line| {
            let mut out: Vec<ConfigRow> = Vec::new();
            let mut failed = 0u64;
            let cols: Vec<&str> = line.split('\t').collect();
            if cols.len() < 3 {
                return (out, failed);
            }
            let family = cols[0].trim();
            let token = cols[1].trim();
            let Ok(end_date) = cols[2].trim().parse::<i64>() else {
                return (out, failed);
            };
            let (from, to) = window_for(family, end_date);

            // ONE cache per market, shared by all 26 (lane x strategy x config) runs below: every
            // run replays the identical (venue=polymarket, token, [from, to]) tick slice, so the
            // wrapped store's `scan_quotes`/`scan_trades`/`scan_book_updates` each hit ClickHouse
            // once here instead of once per run (26x). Never shared ACROSS markets (a fresh
            // wrapper per `map` call), so memory stays bounded to one market's slice at a time.
            let market_store: Arc<dyn HistStore + Send + Sync> =
                Arc::new(CachedHistStore::new(store.clone()));

            for spec in &matrix {
                let toml = profile_toml(token, from, to, fee, latency_ms, spec);
                let profile = match BacktestProfile::from_toml_str(&toml) {
                    Ok(p) => p,
                    Err(e) => {
                        eprintln!("poly_mm_batch: profile parse ({}, {token}): {e}", spec.label);
                        failed += 1;
                        continue;
                    }
                };
                let result = match harness::run_backtest(&profile, market_store.clone()) {
                    Ok(r) => r,
                    Err(e) => {
                        eprintln!("poly_mm_batch: run failed ({}, {token}): {e}", spec.label);
                        failed += 1;
                        continue;
                    }
                };
                let report = harness::BacktestReport::from_result(
                    Some("batch".to_string()),
                    &result,
                    harness::report::periods_per_year(&profile),
                );
                out.push((
                    spec.lane.label().to_string(),
                    family.to_string(),
                    spec.label.to_string(),
                    report.total_return,
                    report.n_trades as u64,
                    report.win_rate,
                ));
            }
            let n = done.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
            if n.is_multiple_of(20) {
                eprintln!("poly_mm_batch: {n}/{total_markets} markets done …");
            }
            (out, failed)
        })
        .collect();

    let mut agg: BTreeMap<(String, String, String), Agg> = BTreeMap::new();
    let (mut markets, mut runs, mut failed) = (0u64, 0u64, 0u64);
    for (results, f) in per_market {
        failed += f;
        if !results.is_empty() {
            markets += 1;
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

    println!(
        "\nfee={} floor={} latency_ms={}  (sum_ret over each family's markets)\n{:<4} {:<18} {:<14} {:>10} {:>8} {:>9} {:>5}",
        fee, floor, latency_ms, "lane", "family", "config", "sum_ret%", "trades", "avg_win%", "n"
    );
    println!("{}", "-".repeat(75));
    for ((lane, family, label), cell) in &agg {
        let avg_win = if cell.n > 0 { cell.win_sum / cell.n as f64 } else { 0.0 };
        println!(
            "{:<4} {:<18} {:<14} {:>+10.4} {:>8} {:>9.1} {:>5}",
            lane,
            family,
            label,
            cell.ret * 100.0,
            cell.trades,
            avg_win * 100.0,
            cell.n
        );
    }
    eprintln!(
        "\npoly_mm_batch: {markets} markets, {runs} runs, {failed} failed (fee={fee} floor={floor})"
    );
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The no-flags run (lane l2, latency 0, spread grid) must generate EXACTLY yesterday's TOML —
    /// the refactor is byte-identical for existing invocations.
    #[test]
    fn default_profile_is_byte_identical_to_the_pre_flag_builder() {
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

    /// `--latency-ms 250` lands as `[engine] order_latency_ms = 250`, and the profile still parses.
    #[test]
    fn latency_flag_emits_the_engine_order_latency_knob() {
        let matrix = run_matrix(&[Lane::L2], &[Strat::Spread], 0.0);
        let toml = profile_toml("TOK", 100, 200, false, 250, &matrix[0]);
        assert!(toml.contains("order_latency_ms = 250\n"), "{toml}");
        BacktestProfile::from_toml_str(&toml).expect("must parse");
    }

    /// The L1 lane omits queue_model entirely — the tick lane then uses the default optimistic
    /// spread-crossing Tick fill model.
    #[test]
    fn l1_lane_has_no_queue_model() {
        let matrix = run_matrix(&[Lane::L1], &[Strat::Spread], 0.0);
        let toml = profile_toml("TOK", 100, 200, false, 250, &matrix[0]);
        assert!(!toml.contains("queue_model"), "{toml}");
        BacktestProfile::from_toml_str(&toml).expect("must parse");
    }

    /// The trailing profile mounts `trailing_scalper` with the user-fixed 2s in-strategy exit delay
    /// and carries NO spread_maker-only params.
    #[test]
    fn trailing_profile_names_the_scalper_with_its_exit_delay() {
        // Two trailing configs now: baseline `trail-d2000` (no cutoffs) + `trail-cut` (both armed).
        let matrix = run_matrix(&[Lane::L2], &[Strat::Trailing], 0.0);
        assert_eq!(matrix.len(), 2);
        let toml = profile_toml("TOK", 100, 200, false, 250, &matrix[0]);
        assert!(toml.contains("name = \"trailing_scalper\"\n"), "{toml}");
        assert!(toml.contains("exit_delay_ms = 2000\n"), "{toml}");
        assert!(!toml.contains("tick_size"), "{toml}");
        assert!(!toml.contains("min_half_spread_ticks"), "{toml}");
        BacktestProfile::from_toml_str(&toml).expect("must parse");
    }

    /// Every trailing config's profile carries the market's real `[from, to]` window as
    /// `market_open_ms`/`market_close_ms` — the reference timestamps `TrailingScalper`'s cutoffs
    /// need — for BOTH the baseline (whose own cutoff knobs are 0, so this is inert) and `trail-cut`
    /// (whose knobs are actually armed).
    #[test]
    fn trailing_configs_carry_the_markets_window_as_open_close_params() {
        let matrix = run_matrix(&[Lane::L2], &[Strat::Trailing], 0.0);
        assert_eq!(matrix.len(), 2);
        for spec in &matrix {
            let toml = profile_toml("TOK", 111, 222, false, 250, spec);
            assert!(toml.contains("market_open_ms = 111\n"), "{}: {toml}", spec.label);
            assert!(toml.contains("market_close_ms = 222\n"), "{}: {toml}", spec.label);
            BacktestProfile::from_toml_str(&toml).expect("must parse");
        }
    }

    /// `trail-cut` names both entry-timing cutoff knobs at the user-fixed target values (5s open
    /// delay, 30s close cutoff) — the config this program measures against `trail-d2000`.
    #[test]
    fn trail_cut_config_arms_both_entry_timing_cutoffs() {
        let matrix = run_matrix(&[Lane::L2], &[Strat::Trailing], 0.0);
        let spec = matrix.iter().find(|s| s.label == "trail-cut").expect("trail-cut present");
        let toml = profile_toml("TOK", 100, 200, false, 250, spec);
        assert!(toml.contains("entry_open_delay_ms = 5000\n"), "{toml}");
        assert!(toml.contains("entry_cutoff_before_close_ms = 30000\n"), "{toml}");
        BacktestProfile::from_toml_str(&toml).expect("must parse");
    }

    /// The `spread_maker` grid NEVER receives `market_open_ms`/`market_close_ms` — those params
    /// mean nothing to it, and omitting them keeps its TOML byte-for-byte unchanged from before
    /// this feature existed (see `default_profile_is_byte_identical_to_the_pre_flag_builder`).
    #[test]
    fn spread_maker_configs_never_carry_the_window_params() {
        let matrix = run_matrix(&[Lane::L2], &[Strat::Spread], 0.0);
        let toml = profile_toml("TOK", 100, 200, false, 0, &matrix[0]);
        assert!(!toml.contains("market_open_ms"), "{toml}");
        assert!(!toml.contains("market_close_ms"), "{toml}");
    }

    /// both lanes x both strategies = (12 spread + 2 trailing) x 2 lanes = 28 runs per market.
    #[test]
    fn full_matrix_is_28_runs() {
        let matrix = run_matrix(&[Lane::L1, Lane::L2], &[Strat::Spread, Strat::Trailing], 0.0);
        assert_eq!(matrix.len(), 28);
    }

    /// Flag parsing: defaults preserve today's behavior; "both" expands; junk is None (exit 2).
    #[test]
    fn lane_and_strategy_flags_parse_with_backcompat_defaults() {
        assert_eq!(parse_lanes(None), Some(vec![Lane::L2]));
        assert_eq!(parse_lanes(Some("l1")), Some(vec![Lane::L1]));
        assert_eq!(parse_lanes(Some("both")), Some(vec![Lane::L1, Lane::L2]));
        assert_eq!(parse_lanes(Some("l3")), None);
        assert_eq!(parse_strats(None), Some(vec![Strat::Spread]));
        assert_eq!(parse_strats(Some("trailing")), Some(vec![Strat::Trailing]));
        assert_eq!(parse_strats(Some("both")), Some(vec![Strat::Spread, Strat::Trailing]));
        assert_eq!(parse_strats(Some("maker")), None);
    }

    /// `--latency-ms` strict-parses: absent ⇒ 0; garbage or negative ⇒ None (exit 2), never a
    /// silent zero-latency run.
    #[test]
    fn latency_flag_strict_parses() {
        assert_eq!(parse_latency_ms(None), Some(0));
        assert_eq!(parse_latency_ms(Some("250")), Some(250));
        assert_eq!(parse_latency_ms(Some("0")), Some(0));
        assert_eq!(parse_latency_ms(Some("250ms")), None);
        assert_eq!(parse_latency_ms(Some("-5")), None);
    }

    /// 5m ⇒ exactly [end-300s, end]; 15m ⇒ [end-900s, end] — no pre-open or settlement padding.
    #[test]
    fn window_is_the_markets_exact_life() {
        assert_eq!(window_for("btc-5m", 1_000_000), (700_000, 1_000_000));
        assert_eq!(window_for("xrp-15m", 1_000_000), (100_000, 1_000_000));
    }

    // ---- CachedHistStore --------------------------------------------------------------------

    /// A tiny counting `HistStore` double: every `scan_quotes` call increments an `AtomicU64` and
    /// returns an empty `Vec` (the row VALUES don't matter for a hit-count proof). Every other
    /// trait verb this bin never drives through `CachedHistStore` is stubbed to the cheapest
    /// legal answer.
    struct CountingStore {
        quote_calls: std::sync::atomic::AtomicU64,
    }

    impl CountingStore {
        fn new() -> Self {
            Self { quote_calls: std::sync::atomic::AtomicU64::new(0) }
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

    /// Two identical `scan_quotes` calls through the wrapper hit the inner store exactly ONCE
    /// (the second is served from the cache); a DIFFERENT range hits it again — proving the
    /// memoization is keyed on the full `(venue, symbol, range)` tuple, not just `(venue, symbol)`.
    #[test]
    fn identical_scan_hits_inner_once_different_range_hits_again() {
        let inner = Arc::new(CountingStore::new());
        let cached = CachedHistStore::new(inner.clone());

        let r1 = cached.scan_quotes("polymarket", "TOK", TsRange::of(100, 200)).unwrap();
        let r2 = cached.scan_quotes("polymarket", "TOK", TsRange::of(100, 200)).unwrap();
        assert_eq!(r1, r2);
        assert_eq!(
            inner.quote_calls.load(std::sync::atomic::Ordering::Relaxed),
            1,
            "the second identical scan must be served from the cache, not the inner store"
        );

        let _r3 = cached.scan_quotes("polymarket", "TOK", TsRange::of(300, 400)).unwrap();
        assert_eq!(
            inner.quote_calls.load(std::sync::atomic::Ordering::Relaxed),
            2,
            "a different range is a cache miss and must reach the inner store again"
        );
    }

    /// A run's 26 configs each call `scan_quotes` on the SAME (venue, symbol, range) — the exact
    /// shape `poly_mm_batch`'s per-market loop produces. Simulate it directly: the inner store must
    /// see exactly one call no matter how many times the wrapper is asked.
    #[test]
    fn many_repeated_calls_still_hit_inner_once() {
        let inner = Arc::new(CountingStore::new());
        let cached = CachedHistStore::new(inner.clone());
        for _ in 0..26 {
            cached.scan_quotes("polymarket", "TOK", TsRange::of(1, 2)).unwrap();
        }
        assert_eq!(inner.quote_calls.load(std::sync::atomic::Ordering::Relaxed), 1);
    }

    // ---- --store DIR (StoreSel / open_store) -------------------------------------------------

    /// Absent both flags selects `ClickHouse` (today's default); `--store` alone selects `Local` at
    /// that exact path.
    #[test]
    fn store_sel_parses_clickhouse_default_and_local_dir() {
        assert!(matches!(parse_store_sel(None, None), StoreSel::ClickHouse));
        match parse_store_sel(Some("/tmp/poly_hist"), None) {
            StoreSel::Local(p) => assert_eq!(p, PathBuf::from("/tmp/poly_hist")),
            other => panic!("expected Local, got {}", store_sel_name(&other)),
        }
    }

    /// `--archive` selects the read-in-place store, and WINS over a simultaneous `--store`: naming
    /// it is an explicit request for the cheaper path (1.2 s vs 128.6 s for a one-market slice), so
    /// silently preferring the import would be the surprising reading of "both were given".
    #[test]
    fn store_sel_archive_wins_over_store_and_takes_the_given_path() {
        match parse_store_sel(None, Some("/tmp/dl")) {
            StoreSel::Archive(p) => assert_eq!(p, PathBuf::from("/tmp/dl")),
            other => panic!("expected Archive, got {}", store_sel_name(&other)),
        }
        match parse_store_sel(Some("/tmp/poly_hist"), Some("/tmp/dl")) {
            StoreSel::Archive(p) => {
                assert_eq!(p, PathBuf::from("/tmp/dl"), "archive path, not store")
            }
            other => panic!("expected Archive to win, got {}", store_sel_name(&other)),
        }
    }

    /// An `--archive` path with no `.parquet` under it is a CLEAR startup error, not a store that
    /// silently answers every scan empty — an empty result set from a mistyped path would read as
    /// "this strategy never traded", which is the most expensive possible way to be wrong here.
    #[test]
    fn open_store_archive_with_no_parquet_files_is_an_error_not_an_empty_store() {
        let dir = tempfile::tempdir().unwrap();
        // `Arc<dyn HistStore>` is not `Debug`, so `expect_err` cannot be used — match instead.
        match open_store(
            StoreSel::Archive(dir.path().to_path_buf()),
            "clickhouse-client".to_string(),
            "polymarket".to_string(),
            dir.path().join("scratch"),
        ) {
            Err(e) => assert!(e.contains("no .parquet files"), "{e}"),
            Ok(_) => panic!("an archive dir with no parquet must not open"),
        }
    }

    /// `--archive FILE` (a single day file, not a directory) opens that one file.
    #[test]
    fn open_store_archive_accepts_a_single_file_path() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("btc5m_2026-07-28.parquet");
        std::fs::write(&f, b"not really parquet").unwrap();
        // Opening is lazy — the file is only decoded on a scan, so construction succeeds and it is
        // the FILE LIST that this asserts (a bad file surfaces at scan time, like every other store).
        open_store(
            StoreSel::Archive(f),
            "clickhouse-client".to_string(),
            "polymarket".to_string(),
            dir.path().join("scratch"),
        )
        .expect("a single archive file path opens");
    }

    /// Name a selection for a test failure message (the enum is not `Debug` — it holds no data worth
    /// printing beyond its path, which the assertions above check directly).
    fn store_sel_name(s: &StoreSel) -> &'static str {
        match s {
            StoreSel::ClickHouse => "ClickHouse",
            StoreSel::Local(_) => "Local",
            StoreSel::Archive(_) => "Archive",
        }
    }

    /// `StoreSel::Local` really opens a working `DataFusionHist` at the given directory — a fresh
    /// empty store answers scans cleanly (empty, not an error), proving `--store DIR` produces a
    /// usable `HistStore`, not just a type-checked branch.
    #[test]
    fn open_store_local_opens_a_real_datafusionhist_at_the_given_dir() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_store(
            StoreSel::Local(dir.path().to_path_buf()),
            "clickhouse-client".to_string(),
            "polymarket".to_string(),
            dir.path().join("scratch"),
        )
        .expect("open local store");
        let rows = store.scan_quotes("polymarket", "TOK", TsRange::all()).unwrap();
        assert!(rows.is_empty(), "a fresh local store has no rows yet, not an error");
    }

    /// `StoreSel::ClickHouse` constructs without touching the filesystem or network — the exact
    /// pre-flag behavior for the no-`--store` path.
    #[test]
    fn open_store_clickhouse_selection_constructs_without_io() {
        // The scratch root is never touched here — construction is what this asserts, and the
        // bridge only stages a file once a scan shells `clickhouse-client`. A relative name makes
        // that explicit: if this test ever started writing, it would write where it was run.
        let store = open_store(
            StoreSel::ClickHouse,
            "clickhouse-client".to_string(),
            "polymarket".to_string(),
            PathBuf::from("unused-scratch-root"),
        )
        .expect("construct the ClickHouse bridge");
        // Type-check only — calling a scan verb here would shell a real clickhouse-client.
        let _: Arc<dyn HistStore + Send + Sync> = store;
    }

    /// A bogus local directory (e.g. a path DataFusionHist can't open) is a clean `Err`, not a
    /// panic — `main` turns this into `ExitCode::FAILURE` with a message naming the path.
    #[test]
    fn open_store_local_bad_dir_is_a_clean_error() {
        // A file (not a directory) where DataFusionHist expects to create/open a directory store.
        let dir = tempfile::tempdir().unwrap();
        let bad_path = dir.path().join("not_a_dir_but_a_file");
        std::fs::write(&bad_path, b"nope").unwrap();
        let result = open_store(
            StoreSel::Local(bad_path.clone()),
            "clickhouse-client".to_string(),
            "polymarket".to_string(),
            dir.path().join("scratch"),
        );
        let Err(err) = result else {
            panic!("a file in place of a store directory must not open");
        };
        assert!(err.contains(&bad_path.display().to_string()), "{err}");
    }
}
