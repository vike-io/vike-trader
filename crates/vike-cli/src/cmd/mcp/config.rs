//! The `mcp` verb's flag grammar, and the agent-transcript request it resolves at startup.

use std::path::{Path, PathBuf};

use super::DEFAULT_ADDR;
use super::scope::{Profile, ToolAccess};
use super::tool_schemas::tools_spec;
use super::trace::McpTrace;
#[cfg(doc)]
use super::{Server, node_writes::unattended_refusal, run};
use crate::cmd::args::{self, Flags};

/// Parse `--addr <datahub>` (default [`DEFAULT_ADDR`]), the optional `--node <host:port>` (the
/// vike-tradehub node the trade tools control), the tool SCOPE (`--profile`, repeatable
/// `--deny-tool`), the transcript request (`--trace` / `--trace-dir`) and `--unattended` (no person
/// attends this session — see [`unattended_refusal`]), via the shared
/// [`crate::cmd::args`] glue. A `--help`/`-h` short-circuits through [`args::help_requested`];
/// [`args::exit_for_parse_error`] in [`run`] is what turns that back into a stdout usage and an
/// exit 0.
///
/// ⚠ Both scope flags REFUSE a name they do not recognise rather than ignoring it, and that is the
/// same rule twice: a mistyped `--profile` must not serve more than the operator asked for, and a
/// mistyped `--deny-tool` must not leave them believing a tool was withheld when nothing was.
/// `--trace` and `--trace-dir` are last-one-wins, the ordinary CLI expectation for two spellings of
/// one destination.
pub(super) fn parse_config(
    args: impl Iterator<Item = String>,
    configured_backtest_addr: Option<&str>,
    configured_datahub_addr: Option<&str>,
) -> Result<Config, String> {
    // ⚠ THE MIDDLE RUNG on the DATA plane, and it was missing for exactly as long as its
    // compute twin below was. `crates/vike-desktop/src/app_methods.rs` read `config.datahub_addr`
    // and was its ONLY reader, so a box whose datahub is not on the compiled-in default reached it
    // from the GUI and dialled `127.0.0.1:7878` from here — one setting, two clients, two answers.
    // A BLANK rung is skipped rather than honoured, same as the compute side: an `Environment=`
    // line that set nothing must not aim this at an empty address.
    let mut addr = configured_datahub_addr
        .filter(|s| !s.trim().is_empty())
        .map_or_else(|| DEFAULT_ADDR.to_string(), str::to_string);
    // ⚠ THE MIDDLE RUNG, and its absence was a recorded residual of the backtest-CLI-surface
    // design (§18 row 9): the dispatcher handed `config.backtest_addr` to `backtest` and
    // `research` and NOT here, so a box that set the key moved some of its compute dialers and
    // silently left this one on the compiled-in default. The ladder is now the same three rungs
    // every other compute dialer has — `--backtest-addr` → the setting →
    // `vike_config::DEFAULT_BACKTEST_ADDR`.
    let mut backtest_addr = configured_backtest_addr
        .filter(|s| !s.trim().is_empty())
        .map_or_else(|| vike_config::DEFAULT_BACKTEST_ADDR.to_string(), str::to_string);
    let mut node: Option<String> = None;
    let mut profile = Profile::Full;
    let mut denied: Vec<String> = Vec::new();
    let mut trace = TraceRequest::Off;
    let mut unattended = false;
    let mut flags = Flags::new(args);
    while let Some((flag, inline)) = flags.next_flag() {
        match flag.as_str() {
            "--addr" => addr = flags.value(&flag, inline)?,
            // ⚠ A SECOND address flag since ruling 7 split the served surface: `--addr` still names
            // the DATA daemon (list_series, coverage, backfill) and this one names the COMPUTE
            // daemon (run_backtest, run_sweep, run_walk_forward, list_strategies). One MCP server
            // holds tools on both planes, so it needs both.
            "--backtest-addr" => backtest_addr = flags.value(&flag, inline)?,
            "--node" => node = Some(flags.value(&flag, inline)?),
            "--profile" => profile = Profile::parse(&flags.value(&flag, inline)?)?,
            // ⚠ A deny name is validated against the SERVED roster here rather than being applied
            // blindly: a `--deny-tool submit-order` (hyphen) that quietly denied nothing would
            // leave an operator believing a write tool was withheld when it was not.
            "--deny-tool" => {
                let name = flags.value(&flag, inline)?;
                if !served_tool_names().contains(&name) {
                    return Err(format!(
                        "unknown --deny-tool {name:?} — this server serves: {}",
                        served_tool_names().join(", ")
                    ));
                }
                denied.push(name);
            }
            "--trace" => {
                args::no_value(&flag, inline)?;
                trace = TraceRequest::ProjectState;
            }
            "--trace-dir" => trace = TraceRequest::Dir(PathBuf::from(flags.value(&flag, inline)?)),
            // A BOOLEAN, and it takes no value: `--unattended=false` would be a way to write the
            // flag and mean its opposite, so a value is refused like every other valueless flag.
            "--unattended" => {
                args::no_value(&flag, inline)?;
                unattended = true;
            }
            "-h" | "--help" => return args::help_requested(),
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    Ok(Config {
        datahub_addr: addr,
        backtest_addr,
        node_addr: node,
        access: ToolAccess::new(profile, denied),
        trace,
        unattended,
    })
}

/// What [`parse_config`] resolved: the THREE addresses (data daemon, compute daemon, node), the
/// tool scope, and whether a transcript was
/// asked for. A struct rather than a tuple because the tuple was already two elements and this
/// change would have made it four positional values with two `Option`s among them.
#[derive(Debug)]
pub(super) struct Config {
    pub(super) datahub_addr: String,
    /// The COMPUTE daemon the `Run*`/`list_strategies` tools dial (ruling 7) — a separate address
    /// from `datahub_addr` because this server holds tools on both planes.
    pub(super) backtest_addr: String,
    pub(super) node_addr: Option<String>,
    pub(super) access: ToolAccess,
    pub(super) trace: TraceRequest,
    /// `--unattended` — see [`Server`]'s field of the same name.
    pub(super) unattended: bool,
}

/// Whether the operator asked for an agent transcript, and where.
///
/// Three states rather than an `Option<PathBuf>`, because "the default location" cannot be resolved
/// by the parser: it is the composition root's already-resolved state directory, and re-deriving it
/// here would be a second walk (see [`resolve_trace`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum TraceRequest {
    /// No `--trace`: nothing is written and no directory is created. The default.
    Off,
    /// `--trace`: `<project>/settings/state/agent`, beside the change journal.
    ProjectState,
    /// `--trace-dir <dir>`: exactly there.
    Dir(PathBuf),
}

/// Turn a [`TraceRequest`] into a writer, or into the reason it cannot be honoured.
///
/// ⚠ **A `--trace` that cannot resolve a project REFUSES the process rather than starting silently
/// without a record.** That is the opposite of `vike_boot::journal_boot_settings`, which records
/// nothing when no project is above the working directory — and the difference is who asked: an
/// anchor nobody requested is right to be silent, while an operator who typed `--trace` and got a
/// server with no transcript would find out after the incident that there was nothing to read. The
/// message names the two ways out, so the refusal is actionable rather than merely correct.
pub(super) fn resolve_trace(
    request: TraceRequest,
    state_dir: Option<&Path>,
) -> Result<Option<McpTrace>, String> {
    match request {
        TraceRequest::Off => Ok(None),
        TraceRequest::Dir(dir) => Ok(Some(McpTrace::new(dir))),
        TraceRequest::ProjectState => match state_dir {
            Some(dir) => Ok(Some(McpTrace::in_state_dir(dir))),
            None => Err(
                "--trace: no project directory above the working directory, so there is nowhere \
                 to write the agent transcript. Run from inside a project, set $VIKE_SETTINGS_DIR, \
                 or name a directory outright with --trace-dir <dir>."
                    .to_string(),
            ),
        },
    }
}

/// Every tool name this server implements, in [`tools_spec`] order — the roster `--deny-tool`
/// validates against and the refusal message quotes.
pub(super) fn served_tool_names() -> Vec<String> {
    tools_spec()
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .map(|t| t["name"].as_str().unwrap_or_default().to_string())
        .collect()
}
