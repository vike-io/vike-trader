//! The tools that dial the COMPUTE daemon — `run_backtest`, `run_sweep`, `run_walk_forward` and
//! `list_strategies` — and the argument helpers they share.
//!
//! None of them takes `&Server`: each is handed the daemon address and the JSON arguments, so the
//! plane a tool dials is visible at its signature (`Server::call_tool` passes
//! `Server::backtest_addr`, never the data server's). Split out of `cmd/mcp.rs` (code-layout
//! phase 2, task 9).

use serde_json::{Value, json};
use vike_datahub_client::{DatahubClient, WireSearch};

pub(super) fn tool_run_backtest(backtest_addr: &str, args: &Value) -> Result<Value, String> {
    let profile = args
        .get("profile")
        .and_then(Value::as_str)
        .ok_or("run_backtest requires a `profile` string argument")?;
    let mut profile_toml = profile.to_string();
    if let Some(script) = args.get("script").and_then(Value::as_str) {
        profile_toml = crate::cmd::backtest::inject_script_src(&profile_toml, script)?;
    }
    let mut client = DatahubClient::connect(backtest_addr)
        .map_err(|e| format!("cannot connect to the backtest daemon at {backtest_addr}: {e} (start it with `vike-backend backtest --addr`)"))?;
    let report_json = client.run_backtest(&profile_toml)?;
    let report: Value = serde_json::from_str(&report_json)
        .map_err(|e| format!("server report was not valid JSON: {e}"))?;
    Ok(json!({ "report": report }))
}

/// List the compiled NATIVE backtest-strategy roster the remote `vike-backend backtest --addr` daemon advertises
/// (`vike_backtest::harness::STRATEGIES` over RPC) — the names a profile's `strategy.name` can
/// resolve, so an agent can discover which strategies exist before authoring a profile. Connects
/// to the compute daemon like `run_backtest`; a connect failure or a server-side error is a clean tool
/// error.
pub(super) fn tool_list_strategies(backtest_addr: &str) -> Result<Value, String> {
    let mut client = DatahubClient::connect(backtest_addr)
        .map_err(|e| format!("cannot connect to the backtest daemon at {backtest_addr}: {e} (start it with `vike-backend backtest --addr`)"))?;
    let strategies = client.list_strategies()?;
    Ok(json!({ "strategies": strategies }))
}

/// Resolve the profile TOML a run tool ships from its arguments: the required `profile` string,
/// with the optional `script` injected as `[strategy.params].src` (the `run_backtest`/`backtest
/// --script` idiom). Returned as TEXT — the run tools ship it verbatim, exactly like the
/// subcommands, so the SERVER's `BacktestProfile::from_toml_str` is the only profile parser.
fn profile_from_args(args: &Value, tool: &str) -> Result<String, String> {
    let profile = args.get("profile").and_then(Value::as_str).ok_or_else(|| {
        format!("{tool} requires a `profile` string argument (a backtest profile TOML)")
    })?;
    let mut profile_toml = profile.to_string();
    if let Some(script) = args.get("script").and_then(Value::as_str) {
        profile_toml = crate::cmd::backtest::inject_script_src(&profile_toml, script)?;
    }
    Ok(profile_toml)
}

/// One tool argument that the wire carries as a TOKEN, read from whatever JSON a model sent.
///
/// ⚠ `Value::as_str` ALONE was the trap: a model passing `"trials": 128` (a JSON number, which is
/// what the schema asks for) would have answered `None` and the budget would have vanished in
/// silence — the same defect class this whole stage exists to end, wearing a JSON type. A number is
/// rendered, a string is passed through, and anything else is refused by name.
fn scalar_arg(args: &Value, name: &str) -> Result<Option<String>, String> {
    match args.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(n)) => Ok(Some(n.to_string())),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(other) => Err(format!(
            "run_sweep: `{name}` must be an integer (or its decimal string), got {other}"
        )),
    }
}

/// The search SELECTION a tool call asks for, or `None` when it asks for none.
///
/// The four values cross the wire as tokens and are judged ONCE, by
/// `vike_backtest::harness::search_select` on the server — including the ownership rule (`trials`
/// belongs to tpe, `euler_depth` to euler, `seed` to tpe and genetic). ⚠ That module's refusals name
/// the CLI FLAG rather than this tool's argument (`--trials is a tpe or genetic flag`), which is the
/// accepted cost of one spelling for two surfaces; the schema below names the flag beside each
/// argument so a model can connect the two.
pub(super) fn search_from_args(args: &Value) -> Result<Option<WireSearch>, String> {
    let optimizer = match args.get("optimizer") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.clone()),
        Some(other) => {
            return Err(format!("run_sweep: `optimizer` must be a string, got {other}"));
        }
    };
    let search = WireSearch {
        optimizer,
        euler_depth: scalar_arg(args, "euler_depth")?,
        trials: scalar_arg(args, "trials")?,
        seed: scalar_arg(args, "seed")?,
    };
    Ok((!search.is_empty()).then_some(search))
}

/// Run a remote parameter-grid search on the remote `vike-backend backtest --addr` daemon — the
/// absorbed `vike-mcp` `run_sweep`, gone remote: EXACTLY the request `vike-cli backtest` builds for
/// a profile carrying a `[sweep]` table (the SAME profile TOML over the SAME `run_paramscan_profile`
/// wire verb), so the tool and the human command can never drift. The optional `rank_by` argument
/// names the server-side metric.
///
/// ⚠ **The TOOL keeps its name while the human VERB was deleted, and that is ruling 13's own
/// split.** `vike-cli sweep` is retired (`crate::RETIRED_COMMANDS`) because a second verb is a
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
/// from JSON numbers by [`scalar_arg`]) and are judged ONCE, server-side, by
/// `vike_backtest::harness::search_select` — including the ownership rule, so a `trials` under
/// `optimizer: "euler"` is refused BY NAME rather than discarded. An agent talking to a daemon that
/// does not advertise `vike_datahub_client::FEATURE_SEARCH_METHOD` is told so by name, with nothing
/// sent, rather than handed a grid it did not ask for.
pub(super) fn tool_run_paramscan(backtest_addr: &str, args: &Value) -> Result<Value, String> {
    let profile_toml = profile_from_args(args, "run_sweep")?;
    let rank_by = args.get("rank_by").and_then(Value::as_str);
    // ⚠ BEFORE the connect: an argument this boundary cannot read is a bad CALL, not a bad daemon,
    // and it costs no socket — the same rule `profile_from_args` already follows one line up.
    let search = search_from_args(args)?;
    let mut client = DatahubClient::connect(backtest_addr)
        .map_err(|e| format!("cannot connect to the backtest daemon at {backtest_addr}: {e} (start it with `vike-backend backtest --addr`)"))?;
    let report_json = client.run_paramscan_profile(&profile_toml, rank_by, search.as_ref())?;
    let report: Value = serde_json::from_str(&report_json)
        .map_err(|e| format!("server sweep report was not valid JSON: {e}"))?;
    Ok(json!({ "sweep": report }))
}

/// Run a remote out-of-sample walk-forward on the remote `vike-backend backtest --addr` daemon — the absorbed `vike-mcp`
/// `run_walk_forward`, gone remote: the SAME request the `walkforward` subcommand builds (the
/// profile TOML over the `run_walkforward_profile` wire verb).
///
/// The WHOLE protocol rides in the profile's `[walkforward]` table — the split count, and since the
/// optimizing driver landed, whether each window re-searches its own training half (`search`), the
/// training shape (`mode`) and how a window scores its candidates (`rank_by`). This function
/// therefore gains no argument and needs none: `crates/vike-backtest/src/compute_server.rs`'s
/// `run_walkforward_profile` reads that table and picks the driver, so a tool argument here would
/// be a second place to say it and a second thing for an agent to get wrong.
///
/// ⚠ The consequence for the ADVERTISED description (`tools_spec` below), which is what a model
/// actually reads: the default — `n_splits` alone — is still the FIXED-parameter stability walk, so
/// the description has to say both what this can do and what it did not do, or a model reports an
/// optimization it never ran. The answer distinguishes them: only a window that searched carries
/// `chosen_params`.
pub(super) fn tool_run_walk_forward(backtest_addr: &str, args: &Value) -> Result<Value, String> {
    let profile_toml = profile_from_args(args, "run_walk_forward")?;
    let mut client = DatahubClient::connect(backtest_addr)
        .map_err(|e| format!("cannot connect to the backtest daemon at {backtest_addr}: {e} (start it with `vike-backend backtest --addr`)"))?;
    let report_json = client.run_walkforward_profile(&profile_toml)?;
    let report: Value = serde_json::from_str(&report_json)
        .map_err(|e| format!("server walk-forward report was not valid JSON: {e}"))?;
    Ok(json!({ "walkforward": report }))
}
