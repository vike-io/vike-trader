//! The tools that dial the backtesthub — `run_backtest`, `run_sweep`, `run_walk_forward` and
//! `list_strategies` — and the argument helpers they share.
//!
//! None of them takes `&Server`: each is handed the daemon address, the box's hub keys and the JSON
//! arguments, so the plane a tool dials is visible at its signature (`Server::call_tool` passes
//! `Server::backtest_addr`, never the data server's). Split out of `src/lib.rs` (code-layout
//! phase 2, task 9).
//!
//! Each dials through the one dial `vike-cli` makes too (`vike_backtesthub_client::BacktesthubClient::dial`, its
//! failure the one `DialRefusal::shown` sentence): under this box's device key when it holds one, plain when it holds
//! none, and a run verb asks for [`Scope::Write`] because it compiles client-supplied Rhai.
//!
//! The three RUN tools also take a [`CancelToken`], and that is what lets them leave the event
//! loop's thread: `serve` runs them on a worker (`super::protocol`'s `run_deferred`), and a
//! `notifications/cancelled` closes their daemon socket through the token. `list_strategies` is a
//! one-frame metadata read and stays inline.

use schemars::JsonSchema;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use vike_backtesthub_client::BacktesthubClient;
use vike_backtesthub_client::client::ParamscanProfileAnswer;
use vike_backtesthub_client::proto::SearchSelectionWire;
use vike_client::refusals::{BackgroundRunRefusal, BacktestCallRefusal};
use vike_hub_protocol::auth::{HubKeys, Scope};

use super::cancel::CancelToken;
use super::tool_args::{RunBacktestArgs, RunSweepArgs, RunWalkForwardArgs, parse_args};

/// A run tool's arguments read into its struct `T` ([`parse_args`]), each refusal in the words the
/// tool has always used. `script` and `rank_by` read as absent when they are no text, so they refuse
/// nothing.
pub(super) fn run_args<T: DeserializeOwned>(tool: &str, args: &Value) -> Result<T, String> {
    parse_args(args, |key, got| match key {
        "profile" => no_profile(tool),
        "fresh" | "include_curve" => format!("`{key}` must be true or false, got {got}"),
        "background" => BackgroundRunRefusal::NotABoolean { got: got.to_string() }.rendered(),
        "optimizer" => format!("{tool}: `optimizer` must be a string, got {got}"),
        // `trials`, `seed` and `euler_depth`: the search TOKENS (`tool_args.rs`'s `search_token`).
        _ => format!("{tool}: `{key}` must be an integer (or its decimal string), got {got}"),
    })
}

/// What a run tool says of a `profile` it cannot read, absent or not text.
fn no_profile(tool: &str) -> String {
    match tool {
        "run_backtest" => "run_backtest requires a `profile` string argument".to_string(),
        _ => format!("{tool} requires a `profile` string argument (a backtest profile TOML)"),
    }
}

/// The three cancellable tools' one way to open their daemon connection: connect, then hand the
/// connection's [`BacktesthubClient::cancel_handle`](vike_hub_protocol::conn::HubConnection::cancel_handle) to `token`, so `notifications/cancelled` can close
/// it while the verb is blocked on the reply. The call's `fresh` argument asks every run fresh
/// ([`BacktesthubClient::set_fresh`]: decision 0078's amendment of 2026-11-20).
///
/// ⚠ A token cancelled BEFORE this point refuses the registration, and the fresh client is dropped
/// here with NOTHING sent — the cancel-before-connect race `super::cancel`'s module doc names. The
/// `Err` text is never shown to the client: a cancelled call gets no response.
fn connect_cancellable(
    backtest_addr: &str,
    keys: Option<&HubKeys>,
    fresh: Option<bool>,
    token: &CancelToken,
) -> Result<BacktesthubClient, String> {
    let fresh = fresh.unwrap_or(false);
    let mut client = BacktesthubClient::dial(backtest_addr, keys, Scope::Write)
        .map_err(|refusal| refusal.shown())?;
    let handle = client
        .cancel_handle()
        .map_err(|e| format!("cannot arm the cancel for the backtest daemon connection: {e}"))?;
    token
        .register(Box::new(handle))
        .map_err(|_| "cancelled by the client before the request was sent".to_string())?;
    client.set_fresh(fresh);
    Ok(client)
}

/// Run ONE backtest on the remote backtesthub over its `run_backtest` verb.
///
/// ⚠ **A profile that declares a `[paramscan]` grid is REFUSED, by the daemon.**
/// `crates/vike-backtesthub/src/backtesthub_server/verbs.rs`'s `run_backtest` answers such a profile with
/// an error naming `run_sweep` rather than a single backtest that silently ignored the grid, which
/// is what this tool used to hand an agent as the answer. The refusal is the SERVER's, like every
/// other profile-shape error here (one parser, one error source); the tool description says so,
/// so a model reading `tools/list` reaches for `run_sweep` first.
pub(super) fn tool_run_backtest(
    backtest_addr: &str,
    keys: Option<&HubKeys>,
    args: &Value,
    token: &CancelToken,
) -> Result<Value, String> {
    let tool = "run_backtest";
    let args: RunBacktestArgs = run_args(tool, args)?;
    let profile_toml = shipped_profile(tool, args.profile, args.script)?;
    let mut client = connect_cancellable(backtest_addr, keys, args.fresh, token)?;
    let report_json = client.run_backtest(&profile_toml)?;
    let report: Value = serde_json::from_str(&report_json)
        .map_err(|e| format!("server report was not valid JSON: {e}"))?;
    Ok(json!({ "report": report }))
}

/// List the strategy files of the remote `vike-backend backtesthub --addr` daemon's `strategies/` folder
/// (`Request::ListStrategies`) — each by its path under that folder, the value a profile's
/// `[strategy] file` names, so an agent can discover which strategies exist before authoring a profile. Connects
/// to the backtesthub like `run_backtest`; a connect failure or a server-side error is a clean tool
/// error.
pub(super) fn tool_list_strategies(
    backtest_addr: &str,
    keys: Option<&HubKeys>,
    reply_wait: std::time::Duration,
) -> Result<Value, String> {
    let mut client = BacktesthubClient::dial(backtest_addr, keys, Scope::Read)
        .map_err(|refusal| refusal.shown())?;
    client.set_reply_timeout(Some(reply_wait));
    let strategies = client.list_strategies()?;
    Ok(json!(ListStrategiesAnswer { strategies }))
}

/// `list_strategies`' answer, its `outputSchema` too.
#[derive(Serialize, JsonSchema)]
pub(super) struct ListStrategiesAnswer {
    /// each file's path under the daemon's strategies/ folder: what a profile's [strategy] file names
    strategies: Vec<String>,
}

/// What run tool `tool` refuses before it dials, in its own words, so a `background: true` start is refused at once
/// rather than answered a run id for a call that cannot run (`super::background_runs`).
pub(super) fn refused_before_the_dial(tool: &str, args: &Value) -> Result<(), String> {
    let (profile, script) = match tool {
        "run_backtest" => run_args::<RunBacktestArgs>(tool, args).map(|a| (a.profile, a.script))?,
        "run_sweep" => run_args::<RunSweepArgs>(tool, args).map(|a| (a.profile, a.script))?,
        _ => run_args::<RunWalkForwardArgs>(tool, args).map(|a| (a.profile, a.script))?,
    };
    shipped_profile(tool, profile, script).map(drop)
}

/// The profile TOML a run tool ships: the required `profile` string, as TEXT — the run tools ship it
/// verbatim, exactly like the subcommands, so the SERVER's `BacktestProfile::from_toml_str` is the only
/// profile parser.
///
/// ⚠ A `script` is REFUSED first, before any socket: a strategy is a FILE the profile names by
/// `[strategy] file`, and the inline source this argument injected as `[strategy.params].src` was dropped
/// unread by the run, which then went ahead on the file and said nothing
/// (`vike_client::refusals::BacktestCallRefusal`).
fn shipped_profile(
    tool: &str,
    profile: Option<String>,
    script: Option<Value>,
) -> Result<String, String> {
    if script.is_some() {
        let argument = "the `script` argument";
        return Err(BacktestCallRefusal::ScriptUnsupported { argument }.rendered());
    }
    profile.ok_or_else(|| no_profile(tool))
}

/// The search SELECTION a tool call asks for, or `None` when it asks for none.
///
/// The four values cross the wire as tokens (the numbers a model sent rendered, by `tool_args.rs`'s
/// `search_token`) and are judged ONCE, by
/// `vike_backtest::search::select` on the server — including the ownership rule (`trials`
/// belongs to tpe, `euler_depth` to euler, `seed` to tpe and genetic). ⚠ That module's refusals name
/// the CLI FLAG rather than this tool's argument (`--trials is a tpe or genetic flag`), which is the
/// accepted cost of one spelling for two surfaces; the schema below names the flag beside each
/// argument so a model can connect the two.
pub(super) fn search_from(args: &RunSweepArgs) -> Option<SearchSelectionWire> {
    let search = SearchSelectionWire {
        optimizer: args.optimizer.clone(),
        euler_depth: args.euler_depth.clone(),
        trials: args.trials.clone(),
        seed: args.seed.clone(),
    };
    (!search.is_empty()).then_some(search)
}

/// Run a remote parameter-grid search on the remote `vike-backend backtesthub --addr` daemon — the
/// absorbed `vike-mcp` `run_sweep`, gone remote: EXACTLY the request `vike-cli backtest` builds for
/// a profile carrying a `[sweep]` table (the SAME profile TOML over the SAME `run_paramscan_profile`
/// wire verb), so the tool and the human command can never drift. The optional `rank_by` argument
/// names the server-side metric.
///
/// ⚠ **The TOOL keeps its name while the human VERB was deleted, and that is ruling 13's own
/// split.** `vike-cli sweep` is retired (`crates/vike-cli/src/lib.rs`'s `RETIRED_COMMANDS`) because a second verb is a
/// second word for one operation; `RunSweep` is the PROTOCOL, not the prompt, and the ruling says
/// outright that the wire verb keeps its own name. Nothing here routed through that subcommand —
/// this dials the daemon directly — so the deletion changes no behaviour on this surface.
///
/// NOTE the trade this shares with `run_backtest`: shipping the TOML verbatim means a profile-SHAPE
/// error (no `[paramscan]` table, bad range, unknown strategy) now surfaces from the SERVER rather
/// than before the connect. One parser, one error source.
///
/// ⚠ **The tool names its own METHOD since stage 7, and before that every agent was a grid-search
/// user.** `optimizer`/`trials`/`seed`/`euler_depth` cross as the TOKENS a model sent (rendered
/// from JSON numbers by `tool_args.rs`'s `search_token`) and are judged ONCE, server-side, by
/// `vike_backtest::search::select` — including the ownership rule, so a `trials` under
/// `optimizer: "euler"` is refused BY NAME rather than discarded. An agent talking to a daemon that
/// does not advertise `vike_backtesthub_client::proto::FEATURE_SEARCH_METHOD` is told so by name, with nothing
/// sent, rather than handed a grid it did not ask for.
pub(super) fn tool_run_paramscan(
    backtest_addr: &str,
    keys: Option<&HubKeys>,
    args: &Value,
    token: &CancelToken,
) -> Result<Value, String> {
    let tool = "run_sweep";
    // ⚠ BEFORE the connect: an argument this boundary cannot read is a bad CALL, not a bad daemon,
    // and it costs no socket.
    let args: RunSweepArgs = run_args(tool, args)?;
    let search = search_from(&args);
    let profile_toml = shipped_profile(tool, args.profile, args.script)?;
    let mut client = connect_cancellable(backtest_addr, keys, args.fresh, token)?;
    let rank_by = args.rank_by.as_deref();
    // ⚠ The SUMMARIZED search where the daemon has it (decision 0078's amendment of 2026-11-19): every point's
    // overrides and metric table under the whole report's own keys, and `saved_run`, the run that keeps each point
    // whole; a daemon without it answers the whole report, as before (the client's one fallback, `vike-cli`'s too).
    match client.run_paramscan_profile_answer(&profile_toml, rank_by, search.as_ref(), false)? {
        ParamscanProfileAnswer::Summary(summary) => Ok(json!({ "sweep": summary })),
        ParamscanProfileAnswer::Whole(report_json) => {
            let report: Value = serde_json::from_str(&report_json)
                .map_err(|e| format!("server sweep report was not valid JSON: {e}"))?;
            Ok(json!({ "sweep": report }))
        }
    }
}

/// Run a remote out-of-sample walk-forward on the remote `vike-backend backtesthub --addr` daemon — the absorbed `vike-mcp`
/// `run_walk_forward`, gone remote: the SAME request the `walkforward` subcommand builds (the
/// profile TOML over the `run_walkforward_profile` wire verb).
///
/// The WHOLE protocol rides in the profile's `[walkforward]` table — the split count, and since the
/// optimizing driver landed, whether each window re-searches its own training half (`search`), the
/// training shape (`mode`) and how a window scores its candidates (`rank_by`). This function
/// therefore gains no argument and needs none: `crates/vike-backtesthub/src/backtesthub_server/verbs.rs`'s
/// `run_walkforward_profile` reads that table and picks the driver, so a tool argument here would
/// be a second place to say it and a second thing for an agent to get wrong.
///
/// ⚠ The consequence for the ADVERTISED description (`tools_spec` below), which is what a model
/// actually reads: the default — `n_splits` alone — is still the FIXED-parameter stability walk, so
/// the description has to say both what this can do and what it did not do, or a model reports an
/// optimization it never ran. The answer distinguishes them: only a window that searched carries
/// `chosen_params`.
pub(super) fn tool_run_walk_forward(
    backtest_addr: &str,
    keys: Option<&HubKeys>,
    args: &Value,
    token: &CancelToken,
) -> Result<Value, String> {
    let tool = "run_walk_forward";
    let args: RunWalkForwardArgs = run_args(tool, args)?;
    let profile_toml = shipped_profile(tool, args.profile, args.script)?;
    let mut client = connect_cancellable(backtest_addr, keys, args.fresh, token)?;
    let report_json = client.run_walkforward_profile(&profile_toml)?;
    let report: Value = serde_json::from_str(&report_json)
        .map_err(|e| format!("server walk-forward report was not valid JSON: {e}"))?;
    Ok(bounded_walkforward(report, args.include_curve.unwrap_or(false)))
}

/// The walk-forward answer an agent reads: the daemon's report, its stitched `equity_curve` (one point a bar,
/// nearly all of the text) left out unless the call asked for it, as `run_sweep` answers no curve (decision
/// 0078's amendments of 2026-11-19 and 2026-11-24). Every other key of the report stays.
pub(super) fn bounded_walkforward(mut report: Value, include_curve: bool) -> Value {
    if !include_curve && let Some(fields) = report.as_object_mut() {
        fields.remove("equity_curve");
    }
    json!({ "walkforward": report })
}
