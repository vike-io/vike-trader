//! `vike-cli strategy sdk-diff <FILE> --from N --to M --profile P` — **does moving this file's SDK pin
//! silently change its results?** (decision 0296; the research's "silently changes results" half of the
//! diff report: a build that breaks is the builder's refusal, a result that moves is this.)
//!
//! # What it runs
//!
//! The SAME backtest twice through the backtesthub, over ONE profile: `P` with its `[strategy] file`
//! set to `FILE` and its `[strategy] sdk` set to N, then to M (`crate::cmd::backtest`'s
//! `set_profile_key`, the `--set` applier). So the data window, the costs and every engine knob are the
//! profile's on both sides; only the SDK moves. Each run is the plain `RunBacktest` a `backtest run
//! --whole` sends ([`BacktesthubClient::run_backtest`]): its report carries every closed trade, which a
//! summary keeps on the hub. A profile that routes to anything but one backtest (a `[paramscan]` grid, a
//! `[walkforward]` or `[permutation]` table, a data plan) is refused before the dial.
//!
//! # What it compares
//!
//! - **Every metric**, side by side, through the ONE comparison of two results the owner ruled
//!   (`vike_analytics::results_comparison::TradingResultsComparison`, which `report --compare` renders).
//! - **The trades**: both counts and the first closed trade that differs.
//! - **Every recorded field**: the two report documents leaf by leaf (`crate::cmd::backtest::read::diff`'s
//!   `diff_docs`, `backtest diff`'s walk), so a field no metric reads still counts. The two are
//!   IDENTICAL only when no leaf differs; the SDK the result records is the one key left out, since it
//!   differs by construction. No tolerance: a value that moved in its last bit moved (decision 0021).
//!
//! # ⚠ Each side must SAY it ran its SDK
//!
//! A result that records no SDK (`TradingResults::strategy_sdk`), or another one than asked, fails the
//! file: a hub that dropped the override would build both sides against one SDK and answer
//! "identical", which is the false confidence this verb exists to remove.
//!
//! # ⚠ Two runs that traded nothing compare NOTHING
//!
//! A file that opens no position on either SDK answers two flat, identical reports, which say nothing
//! about its logic. So identical with no trade on either side (no closed trade and no lot open at the end,
//! [`Comparison::traded`]) is its own answer: the line [`NO_TRADES`], and exit `7`, never `0`. A warning
//! beside a `0` was the other choice and was refused: the exit code is what a script and the cut PR's
//! `--all` read, and the ladder's `7` exists so that "checked nothing" never shares a number with
//! "passed" (`crate::exit`'s `Exit::Empty`). Two reports that DIFFER without a trade (an open lot whose
//! mark moved) still breach: a difference is a finding with or without trades.
//!
//! # Exit codes (`crate::exit`)
//!
//! `0` identical, and trading; `6` ([`Exit::Breach`]) the results differ: the command worked and the
//! answer is no; `1` a run failed (a build the compiler refused, an SDK the box does not keep, the check
//! above); `2` a bad command line; `3` no backtesthub; `7` ([`Exit::Empty`]) nothing was compared: no
//! trade on either SDK, or `--all` found no `.rs` file. Under `--all` a failure outranks a difference,
//! which outranks a file with no trades, which outranks identical.
//!
//! # `--all` (the cut PR's report)
//!
//! Every `.rs` file under a folder of the HUB's `strategies/` folder (its roster,
//! [`BacktesthubClient::list_strategies`]; the whole folder when none is named), one line a file as it
//! completes, then the counts. It costs two backtests a file and leaves two saved runs a file on the hub
//! (kept by its `config.saved_runs_per_strategy`), so nothing runs it but a person's command line: no
//! CI job, deploy step or cut calls it. The profile's `[strategy.params]` reach every file, so the
//! profile for `--all` should carry the costs alone.

use std::process::ExitCode;

use serde_json::{Value, json};
use vike_analytics::results::TradingResults;
use vike_analytics::results_comparison::TradingResultsComparison;
use vike_backtesthub_client::BacktesthubClient;
use vike_client::resolved::backtesthub_dial_address;
use vike_hub_protocol::auth::{HubKeys, Scope};

use crate::cmd::backtest::profile::set_profile_key;
use crate::cmd::backtest::read::diff::{Row, diff_docs, rows_json, section};
use crate::cmd::backtest::route::{Route, asks_for_a_plan, route_of};
use crate::cmd::report::comparison::{ComparisonSide, document};
use crate::exit::{CliError, CommandResult, Exit};
use crate::refusals::FlagRefusal;

/// The program name a refusal prints under.
const BIN: &str = "vike-cli strategy sdk-diff";

/// The report key a result records the SDK it ran under (`TradingResults::strategy_sdk`).
const RECORDED_SDK: &str = "strategy_sdk";

/// What every closed-trade row's leaf key starts with (`crate::cmd::backtest::read::jsondoc`'s `leaves`).
const TRADE_ROWS: &str = "closed_trades.trades[";

/// What a file that traded on neither SDK is told (the module doc's "Two runs that traded nothing").
pub(crate) const NO_TRADES: &str =
    "no trades on either SDK: the comparison shows nothing about the strategy's logic";

/// `sdk-diff`'s arguments.
#[derive(clap::Args, Debug)]
pub(crate) struct SdkDiffCommand {
    #[arg(
        value_name = "FILE",
        required_unless_present = "all",
        help = "the strategy file's path under the backtesthub's strategies/ folder; with --all, a \
                FOLDER of it (absent: the whole folder)"
    )]
    target: Option<String>,
    #[arg(
        long,
        help = "every .rs file under the folder, one line each: two backtests a file, run only when \
                this is typed"
    )]
    all: bool,
    #[arg(long, value_name = "N", help = "the SDK the reference run is built against")]
    from: u32,
    #[arg(long, value_name = "M", help = "the SDK the run held against it is built against")]
    to: u32,
    #[arg(
        long,
        value_name = "PATH",
        help = "the backtest profile both runs share (the data window, the costs, the engine); its \
                [strategy] file and sdk are set by this command"
    )]
    profile: String,
    #[arg(
        long,
        value_name = "HOST:PORT",
        help = "the backtesthub to dial; unwritten, config.backtest_addr, then 127.0.0.1:7880"
    )]
    addr: Option<String>,
    #[arg(long, help = "print one JSON document instead of the tables")]
    json: bool,
}

/// One side's run: the SDK it was asked for and the report the hub answered.
pub(crate) struct Side {
    pub(crate) sdk: u32,
    pub(crate) report: Value,
}

/// Where two closed-trade lists part: both counts (`None` where a report carries no closed-trade
/// block) and the index of the first row that differs (`None`: the same, or a side has none).
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct TradesDiff {
    pub(crate) from: Option<usize>,
    pub(crate) to: Option<usize>,
    pub(crate) first: Option<usize>,
}

/// Two reports held against each other.
pub(crate) struct Comparison {
    /// Every metric, the reference first, each side named by its SDK.
    pub(crate) metrics: TradingResultsComparison,
    pub(crate) trades: TradesDiff,
    /// The recorded leaves that differ outside the closed-trade rows.
    pub(crate) fields: Vec<Row>,
    /// How many closed-trade leaves differ (summarized by [`Self::trades`]).
    pub(crate) trade_leaves: usize,
}

impl Comparison {
    /// No recorded leaf differs.
    pub(crate) fn identical(&self) -> bool {
        self.fields.is_empty() && self.trade_leaves == 0
    }

    /// Either side traded: a closed trade, or a lot still open at the end (a held position is the
    /// strategy's logic too).
    pub(crate) fn traded(&self) -> bool {
        [&self.metrics.a, &self.metrics.b].iter().any(|side| {
            side.n_trades > 0
                || side
                    .closed_trades
                    .as_ref()
                    .is_some_and(|c| !c.trades.is_empty() || !c.open_trades.is_empty())
        })
    }

    /// Identical, and nothing traded on either side: nothing was compared.
    pub(crate) fn compared_nothing(&self) -> bool {
        self.identical() && !self.traded()
    }
}

/// One file's answer.
pub(crate) enum Verdict {
    Compared(Box<Comparison>),
    /// A run, a build or the recorded-SDK check failed, in words.
    Failed(String),
}

/// The first index where `a` and `b` differ, a shorter list differing where it ends.
fn first_difference<T: PartialEq>(a: &[T], b: &[T]) -> Option<usize> {
    a.iter()
        .zip(b)
        .position(|(x, y)| x != y)
        .or_else(|| (a.len() != b.len()).then(|| a.len().min(b.len())))
}

/// That `side`'s report records the SDK it was asked for (the module doc's ⚠).
fn check_recorded_sdk(side: &Side) -> Result<(), String> {
    match side.report.get(RECORDED_SDK).and_then(Value::as_u64) {
        Some(ran) if ran == u64::from(side.sdk) => Ok(()),
        Some(ran) => Err(format!(
            "asked for SDK {}, and the result records SDK {ran}: the override did not reach the build",
            side.sdk
        )),
        None => Err(format!(
            "the result records no SDK (`{RECORDED_SDK}`), so it cannot show it ran SDK {}: a pair \
             built against one SDK would read as identical",
            side.sdk
        )),
    }
}

/// `from` and `to` compared (the module doc's "What it compares"), or why they cannot be.
pub(crate) fn compare(from: &Side, to: &Side) -> Result<Comparison, String> {
    check_recorded_sdk(from)?;
    check_recorded_sdk(to)?;
    let results = |side: &Side| {
        let mut results: TradingResults = serde_json::from_value(side.report.clone())
            .map_err(|e| format!("sdk {}: the answer is not a backtest report: {e}", side.sdk))?;
        results.name = Some(format!("sdk {}", side.sdk));
        Ok::<_, String>(results)
    };
    let (a, b) = (results(from)?, results(to)?);
    let rows = |r: &TradingResults| r.closed_trades.as_ref().map(|c| c.trades.len());
    let first = match (&a.closed_trades, &b.closed_trades) {
        (Some(x), Some(y)) => first_difference(&x.trades, &y.trades),
        _ => None,
    };
    let trades = TradesDiff { from: rows(&a), to: rows(&b), first };
    let (trade_rows, fields): (Vec<Row>, Vec<Row>) =
        diff_docs(&from.report, &to.report, &[RECORDED_SDK])
            .into_iter()
            .filter(Row::changed)
            .partition(|r| r.key.starts_with(TRADE_ROWS));
    Ok(Comparison {
        metrics: TradingResultsComparison::of(a, b),
        trades,
        fields,
        trade_leaves: trade_rows.len(),
    })
}

/// The exit rung of a set of verdicts (the module doc's "Exit codes"): none, or a file that traded on
/// neither SDK, is [`Exit::Empty`].
pub(crate) fn rung<'a>(verdicts: impl IntoIterator<Item = &'a Verdict>) -> Exit {
    let (mut compared, mut breach, mut nothing) = (false, false, false);
    for verdict in verdicts {
        match verdict {
            Verdict::Failed(_) => return Exit::Failed,
            Verdict::Compared(c) if !c.identical() => breach = true,
            Verdict::Compared(c) if c.compared_nothing() => nothing = true,
            Verdict::Compared(_) => compared = true,
        }
    }
    match (breach, nothing, compared) {
        (true, _, _) => Exit::Breach,
        (false, true, _) | (false, false, false) => Exit::Empty,
        (false, false, true) => Exit::Ok,
    }
}

/// The profile one side runs: `base` with `[strategy] file = file` and `sdk = sdk`.
pub(crate) fn profile_for(base: &toml::Value, file: &str, sdk: u32) -> Result<String, String> {
    let mut doc = base.clone();
    set_profile_key(&mut doc, "strategy.file", toml::Value::String(file.to_string()))?;
    set_profile_key(&mut doc, "strategy.sdk", toml::Value::Integer(i64::from(sdk)))?;
    toml::to_string(&doc).map_err(|e| format!("cannot re-serialize the profile: {e}"))
}

/// The `.rs` files of `roster` under `folder` (`None`, empty or `.`: all of them).
pub(crate) fn rust_files_under(roster: &[String], folder: Option<&str>) -> Vec<String> {
    let prefix = folder.map(|f| f.trim_end_matches('/')).filter(|f| !f.is_empty() && *f != ".");
    roster
        .iter()
        .filter(|f| f.ends_with(".rs"))
        .filter(|f| prefix.is_none_or(|p| f.starts_with(&format!("{p}/"))))
        .cloned()
        .collect()
}

/// One side run on the hub.
fn run_side(
    client: &mut BacktesthubClient,
    base: &toml::Value,
    file: &str,
    sdk: u32,
) -> Result<Side, String> {
    let profile = profile_for(base, file, sdk)?;
    let answer = client.run_backtest(&profile).map_err(|e| format!("sdk {sdk}: {e}"))?;
    let report = serde_json::from_str(&answer)
        .map_err(|e| format!("sdk {sdk}: the backtesthub's answer is not JSON: {e}"))?;
    Ok(Side { sdk, report })
}

/// One file: both sides, then the comparison; a failed reference skips the second run.
fn diff_file(
    client: &mut BacktesthubClient,
    base: &toml::Value,
    file: &str,
    cmd: &SdkDiffCommand,
) -> Verdict {
    let compared = run_side(client, base, file, cmd.from)
        .and_then(|from| Ok((from, run_side(client, base, file, cmd.to)?)))
        .and_then(|(from, to)| compare(&from, &to));
    match compared {
        Ok(comparison) => Verdict::Compared(Box::new(comparison)),
        Err(why) => Verdict::Failed(why),
    }
}

/// The verb, end to end; its exit rung.
pub(crate) fn run(
    cmd: &SdkDiffCommand,
    configured_address: Option<&str>,
    keys: Option<&HubKeys>,
) -> ExitCode {
    match execute(cmd, configured_address, keys) {
        Ok(exit) => exit.into(),
        Err(e) => {
            eprintln!("{BIN}: {}", e.message);
            e.exit.into()
        }
    }
}

fn execute(
    cmd: &SdkDiffCommand,
    configured_address: Option<&str>,
    keys: Option<&HubKeys>,
) -> CommandResult<Exit> {
    if cmd.from == cmd.to {
        return Err(CliError::usage(format!(
            "--from and --to both name SDK {}: there is nothing to compare",
            cmd.from
        )));
    }
    let text = std::fs::read_to_string(&cmd.profile).map_err(|e| {
        CliError::failed(
            FlagRefusal::ProfileUnreadable { path: cmd.profile.clone(), e: e.to_string() }
                .rendered(),
        )
    })?;
    let base: toml::Value = toml::from_str(&text)
        .map_err(|e| CliError::usage(format!("profile {} is not valid TOML: {e}", cmd.profile)))?;
    if route_of(&text, false) != Route::Single || asks_for_a_plan(&text) {
        return Err(CliError::usage(format!(
            "profile {} does not run ONE backtest (it declares a [paramscan], [walkforward] or \
             [permutation] table, or asks for its data plan): sdk-diff runs one backtest per SDK",
            cmd.profile
        )));
    }
    let addr = backtesthub_dial_address(cmd.addr.as_deref(), configured_address);
    let mut client = BacktesthubClient::dial(&addr, keys, Scope::Write)?;
    let files = if cmd.all {
        let roster = client.list_strategies().map_err(CliError::failed)?;
        let files = rust_files_under(&roster, cmd.target.as_deref());
        if files.is_empty() {
            return Err(CliError::empty(format!(
                "no .rs strategy file under {:?} of the backtesthub's strategies/ folder: nothing \
                 to compare",
                cmd.target.as_deref().unwrap_or(".")
            )));
        }
        files
    } else {
        vec![cmd.target.clone().unwrap_or_default()]
    };
    let mut verdicts: Vec<(String, Verdict)> = Vec::new();
    for file in files {
        let verdict = diff_file(&mut client, &base, &file, cmd);
        if cmd.all && !cmd.json {
            println!("{}", one_line(&file, &verdict));
        }
        verdicts.push((file, verdict));
    }
    if cmd.json {
        let text = serde_json::to_string_pretty(&batch_document(cmd.from, cmd.to, &verdicts))
            .map_err(|e| CliError::failed(format!("cannot render the JSON document: {e}")))?;
        println!("{text}");
    } else if cmd.all {
        println!("{}", counts_line(&verdicts));
    } else if let Some((file, verdict)) = verdicts.first() {
        print_file(file, cmd.from, cmd.to, verdict);
    }
    Ok(rung(verdicts.iter().map(|(_, v)| v)))
}

/// `--all`'s line for one file.
pub(crate) fn one_line(file: &str, verdict: &Verdict) -> String {
    match verdict {
        Verdict::Failed(why) => format!("FAILED     {file}  {}", why.lines().next().unwrap_or("")),
        Verdict::Compared(c) if c.compared_nothing() => format!("NO TRADES  {file}  {NO_TRADES}"),
        Verdict::Compared(c) if c.identical() => format!("identical  {file}"),
        Verdict::Compared(c) => format!(
            "DIFFERENT  {file}  final_equity {} -> {}; {}; {} fields differ",
            c.metrics.a.final_equity,
            c.metrics.b.final_equity,
            trades_words(&c.trades),
            c.fields.len() + c.trade_leaves
        ),
    }
}

/// The trades half in words.
fn trades_words(trades: &TradesDiff) -> String {
    match (trades.from, trades.to, trades.first) {
        (Some(a), Some(b), None) => format!("trades {a} -> {b}, every closed trade the same"),
        (Some(a), Some(b), Some(i)) => {
            format!("trades {a} -> {b}, the first that differs is [{i}]")
        }
        _ => "no closed-trade rows on a side (engine.closed_trades is off)".to_string(),
    }
}

/// `--all`'s closing line: how many files came out each way.
fn counts_line(verdicts: &[(String, Verdict)]) -> String {
    let (mut same, mut idle, mut moved, mut failed) = (0, 0, 0, 0);
    for (_, verdict) in verdicts {
        match verdict {
            Verdict::Failed(_) => failed += 1,
            Verdict::Compared(c) if c.compared_nothing() => idle += 1,
            Verdict::Compared(c) if c.identical() => same += 1,
            Verdict::Compared(_) => moved += 1,
        }
    }
    format!(
        "{} files: {same} identical, {idle} with no trades, {moved} different, {failed} failed",
        verdicts.len()
    )
}

/// One file, whole: the verdict, every metric side by side, the trades and every other field that moved.
fn print_file(file: &str, from: u32, to: u32, verdict: &Verdict) {
    let c = match verdict {
        Verdict::Failed(why) => {
            println!("{file}: sdk {from} -> sdk {to}: FAILED\n  {why}");
            return;
        }
        Verdict::Compared(c) => c,
    };
    let word = if c.identical() { "identical" } else { "DIFFERENT" };
    println!("{file}: sdk {from} -> sdk {to}: {word}");
    if c.compared_nothing() {
        println!("WARNING: {NO_TRADES}");
    }
    print!("{}", c.metrics);
    println!("{}", trades_words(&c.trades));
    if let Some(i) = c.trades.first {
        for side in [&c.metrics.a, &c.metrics.b] {
            let row = side.closed_trades.as_ref().and_then(|t| t.trades.get(i));
            let text =
                row.and_then(|r| serde_json::to_string(r).ok()).unwrap_or_else(|| "-".to_string());
            println!("  {}: {text}", side.name.as_deref().unwrap_or(""));
        }
    }
    if !c.fields.is_empty() {
        section("recorded fields that differ (the trade rows aside)", &c.fields, false);
    }
}

/// One file's JSON object.
pub(crate) fn file_document(file: &str, from: u32, to: u32, verdict: &Verdict) -> Value {
    match verdict {
        Verdict::Failed(why) => {
            json!({ "file": file, "from": from, "to": to, "verdict": "failed", "error": why })
        }
        Verdict::Compared(c) => {
            let sides = [from, to]
                .map(|sdk| ComparisonSide { source: format!("sdk {sdk}"), notes: Vec::new() });
            json!({
                "file": file,
                "from": from,
                "to": to,
                "verdict": match (c.identical(), c.compared_nothing()) {
                    (_, true) => "no_trades",
                    (true, false) => "identical",
                    (false, false) => "different",
                },
                "warning": c.compared_nothing().then_some(NO_TRADES),
                "metrics": document(&sides, &c.metrics),
                "trades": {
                    "from": c.trades.from,
                    "to": c.trades.to,
                    "first_difference": c.trades.first,
                    "changed_fields": c.trade_leaves,
                },
                "changed": rows_json(&c.fields, false),
            })
        }
    }
}

/// The `--json` document: every file's object, and the counts.
fn batch_document(from: u32, to: u32, verdicts: &[(String, Verdict)]) -> Value {
    json!({
        "schema": 1,
        "from": from,
        "to": to,
        "exit": rung(verdicts.iter().map(|(_, v)| v)) as u8,
        "files": verdicts.iter().map(|(f, v)| file_document(f, from, to, v)).collect::<Vec<_>>(),
    })
}

#[path = "sdk_diff_tests.rs"]
#[cfg(test)]
mod sdk_diff_tests;
