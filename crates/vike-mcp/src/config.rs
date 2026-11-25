//! The program's flag grammar, and the agent-transcript request it resolves at startup.

use std::path::{Path, PathBuf};

use vike_utility::cli::{CliExit, Program};

use super::scope::{Profile, ToolAccess};
use super::tool_schemas::tools_spec;
use super::trace::McpTrace;
#[cfg(doc)]
use super::{Server, node_writes::unattended_refusal, run};

/// The program name. ⚠ `vike-mcp`'s STDOUT is a JSON-RPC stream: every refusal here goes to stderr
/// (`CliExit::refusal`, printed by `CliExit::finish`), and only `--help`/`--version` — requests to
/// print, made before any session exists — reach stdout.
pub(super) const BIN: &str = "vike-mcp";

/// The program's command line, as `clap` reads it.
///
/// ⚠ Both scope flags REFUSE a name they do not recognise rather than ignoring it, and that is the
/// same rule twice: a mistyped `--profile` must not serve more than the operator asked for, and a
/// mistyped `--deny-tool` must not leave them believing a tool was withheld when nothing was. Those
/// two checks name the SERVED roster, so they stay in [`McpCli::into_config`], after the parse.
#[derive(clap::Parser, Debug)]
#[command(about = "Serve the create+backtest tools over stdio MCP (for Claude / an agent)", long_about = None)]
pub(super) struct McpCli {
    // ⚠ The DATA daemon (list_series, coverage, backfill). Default: `config.datahub_addr`, then
    // `vike_config::DEFAULT_DATAHUB_ADDRESS`.
    #[arg(
        long,
        value_name = "HOST:PORT",
        help = format!(
            "the vike-datahub the data tools dial [default: config.datahub_addr, then {}]",
            vike_config::DEFAULT_DATAHUB_ADDRESS
        )
    )]
    addr: Option<String>,
    // ⚠ A SECOND address flag since ruling 7 split the served surface: `--addr` still names the
    // DATA daemon and this one names the backtesthub (run_backtest, run_sweep, run_walk_forward,
    // list_strategies). One MCP server holds tools on both planes, so it needs both.
    #[arg(
        long,
        value_name = "HOST:PORT",
        help = format!(
            "the backtesthub the run_* tools dial [default: config.backtest_addr, then {}]",
            vike_config::DEFAULT_BACKTEST_ADDRESS
        )
    )]
    backtest_addr: Option<String>,
    #[arg(
        long,
        value_name = "HOST:PORT",
        help = "the vike-tradehub the trade tools control [default: config.node_addr]"
    )]
    node: Option<String>,
    #[arg(
        long,
        value_name = "PROFILE",
        help = "which tools are served: full (the default) | read-only | offline"
    )]
    profile: Option<String>,
    // ⚠ Repeatable, and validated against the SERVED roster after the parse.
    #[arg(
        long,
        value_name = "NAME",
        help = "withhold one tool by name; repeatable. A name this server does not serve is refused"
    )]
    deny_tool: Vec<String>,
    #[arg(
        long,
        help = "record the agent transcript (arguments redacted) under <project>/settings/state/agent"
    )]
    trace: bool,
    // ⚠ `--trace` and `--trace-dir` are two spellings of one destination. They used to be
    // last-one-wins; a `clap` struct cannot see the order, so the explicit directory now wins.
    #[arg(long, value_name = "DIR", help = "record the agent transcript exactly there")]
    trace_dir: Option<PathBuf>,
    // A BOOLEAN, and it takes no value: `--unattended=false` would be a way to write the flag and
    // mean its opposite, so a value is refused like every other valueless flag.
    #[arg(
        long,
        help = "no person attends this session: every set_setting is refused; other writes keep their preview gate"
    )]
    unattended: bool,
}

/// Read the argv (without the program name). `configured_backtest_addr` / `configured_datahub_addr` /
/// `configured_node_addr` are the MIDDLE rung of each address ladder — `--backtest-addr` → the setting → the
/// constant; `--node` → `config.node_addr` → none — which only the composition root may read.
pub(super) fn parse_config(
    argv: impl Iterator<Item = String>,
    configured_backtest_addr: Option<&str>,
    configured_datahub_addr: Option<&str>,
    configured_node_addr: Option<&str>,
) -> Result<Config, CliExit> {
    let version = vike_buildinfo::version_line(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
    let cli: McpCli = Program::new(BIN, &version).try_parse_from(argv)?;
    cli.into_config(configured_backtest_addr, configured_datahub_addr, configured_node_addr)
        .map_err(|msg| CliExit::refusal(BIN, msg))
}

impl McpCli {
    fn into_config(
        self,
        configured_backtest_addr: Option<&str>,
        configured_datahub_addr: Option<&str>,
        configured_node_addr: Option<&str>,
    ) -> Result<Config, String> {
        // ⚠ Both ladders are the ones every `vike-cli` dialer folds, through the same two functions
        // below both programs, so one setting cannot give two clients two answers (a box whose
        // datahub is not on the compiled-in default once reached it from the GUI and dialled
        // `127.0.0.1:7878` from here).
        let datahub_addr = vike_client::resolved::datahub_dial_address(
            self.addr.as_deref(),
            configured_datahub_addr,
        );
        let backtest_addr = vike_client::resolved::backtesthub_dial_address(
            self.backtest_addr.as_deref(),
            configured_backtest_addr,
        );
        let profile = match self.profile.as_deref() {
            Some(name) => Profile::parse(name)?,
            None => Profile::Full,
        };
        // ⚠ A deny name is validated against the SERVED roster here rather than being applied
        // blindly: a `--deny-tool submit-order` (hyphen) that quietly denied nothing would leave
        // an operator believing a write tool was withheld when it was not.
        for name in &self.deny_tool {
            // A name this server served once denies nothing now: refused naming the new one.
            if let Some(renamed) = super::refusals::renamed(name) {
                return Err(renamed.rendered());
            }
            if !served_tool_names().contains(name) {
                return Err(format!(
                    "unknown --deny-tool {name:?} — this server serves: {}",
                    served_tool_names().join(", ")
                ));
            }
        }
        let trace = match (self.trace_dir, self.trace) {
            (Some(dir), _) => TraceRequest::Dir(dir),
            (None, true) => TraceRequest::ProjectState,
            (None, false) => TraceRequest::Off,
        };
        Ok(Config {
            datahub_addr,
            backtest_addr,
            // `vike-cli trade`'s ladder (variant A). No node is no start refusal: the server serves its other tools, and
            // each node tool answers `dial.node.unset` (`node_reads::missing_node`).
            node_addr: vike_client::resolved::node_dial_address(
                self.node.as_deref(),
                configured_node_addr,
            )
            .ok(),
            access: ToolAccess::new(profile, self.deny_tool),
            trace,
            unattended: self.unattended,
        })
    }
}

/// What [`parse_config`] resolved: the THREE addresses (data daemon, backtesthub, node), the
/// tool scope, and whether a transcript was
/// asked for. A struct rather than a tuple because the tuple was already two elements and this
/// change would have made it four positional values with two `Option`s among them.
#[derive(Debug)]
pub(super) struct Config {
    pub(super) datahub_addr: String,
    /// The backtesthub the `Run*`/`list_strategies` tools dial (ruling 7) — a separate address
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
