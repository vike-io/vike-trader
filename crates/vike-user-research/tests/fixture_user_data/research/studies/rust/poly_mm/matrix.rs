//! The run matrix: the sweep grid, lanes, strategies, the per-cell profile TOML, the aggregate.

/// The sweep grid: `(label, [strategy.params] body)`. Each body carries the maker's `γ` plus the
/// model selector + its swept knob. Labels sort so a family's rows group by model then knob.
///
/// Copied VERBATIM from the binary: with the grid and the profile text byte-identical, "does the
/// study produce the binary's numbers" reduces to "does the seam run the binary's backtest".
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
/// `trail-d2000` is the baseline, no entry-timing cutoffs. `trail-cut` arms both: no entries in
/// the first 5s after open (the chaotic just-opened book) nor in the last 30s before close (a late
/// fill under venue latency still has time to exit). `market_open_ms`/`market_close_ms` are
/// appended per market by [`profile_toml`] (via [`window_for`]), not baked in here.
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
pub(super) enum Lane {
    L1,
    L2,
}

impl Lane {
    pub(super) fn label(self) -> &'static str {
        match self {
            Lane::L1 => "l1",
            Lane::L2 => "l2",
        }
    }
}

/// Strategy family to run.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Strat {
    Spread,
    Trailing,
}

/// One `(lane, strategy-config)` cell of the run matrix.
pub(super) struct RunSpec {
    pub(super) lane: Lane,
    pub(super) label: &'static str,
    strategy: &'static str,
    /// the full `[strategy.params]` body
    params: String,
}

/// One market of the universe — the binary's TSV columns `family\ttoken\tend_date_ms`, as data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Market {
    pub(super) family: String,
    pub(super) token: String,
    pub(super) end_date_ms: i64,
}

/// One config run's result row: `(lane, family, config label, total_return, n_trades, win_rate)`.
pub(super) type ConfigRow = (String, String, String, f64, u64, f64);

/// What one market's closure hands back: its rows, its failure count, and the failure LINES that
/// used to be `eprintln!`s. Aliased because it is a `par_iter` element type and clippy's
/// `type_complexity` gate reads the written type.
pub(super) type MarketResult = (Vec<ConfigRow>, u64, Vec<String>);

/// Expand lanes × strategies into the per-market run list: the 12 spread configs (each carrying
/// the shared qty/tick_size/floor prefix) + the 2 trailing configs, per lane.
pub(super) fn run_matrix(lanes: &[Lane], strats: &[Strat], floor: f64) -> Vec<RunSpec> {
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
pub(super) fn parse_lanes(s: Option<&str>) -> Option<Vec<Lane>> {
    match s {
        None | Some("l2") => Some(vec![Lane::L2]),
        Some("l1") => Some(vec![Lane::L1]),
        Some("both") => Some(vec![Lane::L1, Lane::L2]),
        _ => None,
    }
}

/// `strategy` reader: absent/`spread` keeps the binary's default grid; `trailing` the scalper;
/// `both` = union.
pub(super) fn parse_strats(s: Option<&str>) -> Option<Vec<Strat>> {
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
/// For `trailing_scalper` ONLY, the market's [`window_for`] `[from, to]` is also appended to
/// `[strategy.params]` as `market_open_ms`/`market_close_ms`, the timestamps its entry cutoffs
/// read. Unconditional (inert where no cutoff knob is armed); `spread_maker` never gets these keys,
/// so its TOML stays byte-for-byte the binary's.
///
/// ⚠ Still a STRING, not a hand-built `toml::Value`: the binary's tests pin this text byte for
/// byte, the only mechanical evidence that the port did not quietly re-tune a knob.
pub(super) fn profile_toml(
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
pub(super) fn window_for(family: &str, end_date: i64) -> (i64, i64) {
    let tenor = if family.contains("-15m") { 900_000 } else { 300_000 };
    (end_date - tenor, end_date)
}

/// Running aggregate for one `(lane, family, config)` cell.
#[derive(Default)]
pub(super) struct Agg {
    pub(super) ret: f64,
    pub(super) trades: u64,
    pub(super) win_sum: f64,
    pub(super) n: u64,
}
