//! The CLI's argv and the `ClaudeCli` driver value that builds the `Command` it spawns.

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use super::{OAUTH_TOKEN_ENV, RUN_TIMEOUT};

/// Everything the argv is built from.
pub struct Invocation<'a> {
    pub mcp_config: &'a Path,
    pub allowed_tools: &'a [String],
    /// `None` leaves the flag off entirely, so the subscription's own default model answers — which
    /// is the point of this driver, and which the `system`/`init` line then names.
    pub model: Option<&'a str>,
    pub system: &'a str,
}

/// The CLI arguments, verbatim.
///
/// Every flag here was read off the real `claude --help` and then RUN against the real CLI. Three
/// of them are not decoration:
///
///   * `--verbose` is MANDATORY with `--output-format stream-json` under `--print` — without it the
///     CLI refuses with `When using --print, --output-format=stream-json requires --verbose` and
///     produces no stream at all.
///   * `--allowedTools` is VARIADIC and is therefore LAST, with nothing after it. A variadic option
///     consumes every following token that does not start with `-`.
///   * there is NO positional prompt: the prompt is written to the child's stdin. That keeps it out
///     of a process listing and, more importantly, out of reach of the variadic above.
pub fn argv(inv: &Invocation<'_>) -> Vec<String> {
    let mut out: Vec<String> = vec![
        "-p".into(),
        "--verbose".into(),
        "--output-format".into(),
        "stream-json".into(),
        // The SAME instruction the API driver gives, so the two measurements differ in the model
        // and the billing rather than in what the agent was told.
        "--system-prompt".into(),
        inv.system.to_string(),
        "--mcp-config".into(),
        inv.mcp_config.display().to_string(),
        // Only ours: the box's own MCP servers must not join a trading evaluation.
        "--strict-mcp-config".into(),
        // No user, project or local settings file — the box's hooks and permissions are not part of
        // the surface under test.
        "--setting-sources".into(),
        String::new(),
        // Nothing of this run is written into the operator's session history.
        "--no-session-persistence".into(),
        "--disable-slash-commands".into(),
        // Anything that would prompt is DENIED rather than bypassed. `--dangerously-skip-permissions`
        // would be the other way to make a headless run not stall, and it is the wrong way here.
        "--permission-prompts".into(),
        "none".into(),
        // No built-in tools at all: the agent may reach the system ONLY through the MCP roster it is
        // being evaluated against.
        "--tools".into(),
        String::new(),
    ];
    if let Some(model) = inv.model {
        out.push("--model".into());
        out.push(model.to_string());
    }
    out.push("--allowedTools".into());
    out.extend(inv.allowed_tools.iter().cloned());
    out
}

/// The Claude Code CLI as a driver.
pub struct ClaudeCli {
    pub(super) binary: PathBuf,
    token: String,
    /// What `--model` was asked for, if anything.
    pub(super) requested: Option<String>,
    /// What the CLI said actually answered, once a case has run.
    pub(super) observed: Option<String>,
    pub(super) max_tool_calls: usize,
    pub(super) timeout: Duration,
    /// The executable the `--mcp-config` names as the bridge. `None` means
    /// `std::env::current_exe()`, which is the right answer for the shipped binary and the WRONG
    /// one for a test harness — a test binary has no `mcp-bridge` verb — so the live smoke names it
    /// outright. See [`ClaudeCli::with_bridge_exe`].
    pub(super) bridge_exe: Option<PathBuf>,
}

impl fmt::Debug for ClaudeCli {
    /// Redacts the token. The manual impl is the point: a derived one would print a live
    /// subscription credential into any error, panic message or debug log that formats this struct.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClaudeCli")
            .field("binary", &self.binary)
            .field("requested", &self.requested)
            .field("observed", &self.observed)
            .field("max_tool_calls", &self.max_tool_calls)
            .field("timeout", &self.timeout)
            .field("token", &"<redacted>")
            .finish()
    }
}

impl ClaudeCli {
    /// `token` comes from `main.rs`'s one environment read and is never stored anywhere else.
    pub fn new(
        binary: PathBuf,
        token: String,
        requested: Option<String>,
        max_tool_calls: usize,
    ) -> Self {
        Self {
            binary,
            token,
            requested,
            observed: None,
            max_tool_calls,
            timeout: RUN_TIMEOUT,
            bridge_exe: None,
        }
    }

    /// Shorten the whole-run ceiling. Used by the bounded-wait test, which has to outlive nothing.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Name the executable the CLI will spawn as the MCP bridge, instead of this process.
    ///
    /// ⚠ It exists because `std::env::current_exe()` answers the RUNNING executable — the
    /// `vike-agent-eval` binary in production, and a `tests/` binary under `cargo test`. A test
    /// binary has no `mcp-bridge` verb, so without this override the live smoke could only ever
    /// have failed for a reason that had nothing to do with the bridge. It is an OVERRIDE, never a
    /// default: taking the shipped path from `current_exe` is what keeps the bridge and the harness
    /// the same build, which is the property that lets the bridge forward frames unread.
    #[must_use]
    pub fn with_bridge_exe(mut self, exe: PathBuf) -> Self {
        self.bridge_exe = Some(exe);
        self
    }

    /// The `Command` this driver spawns — BUILT and returned rather than spawned, so a test can
    /// read the argv and the environment the child would be given without a process existing.
    ///
    /// ⚠ The token is SET here, explicitly. It would also be inherited (this process holds it), and
    /// that is exactly why it is set: inheritance is invisible, and a reader could not tell the one
    /// child that must have the token from the two children `main.rs`'s `SCRUB_FROM_CHILDREN`
    /// removes it from.
    pub fn command(&self, inv: &Invocation<'_>, cwd: &Path) -> Command {
        let mut cmd = Command::new(&self.binary);
        cmd.args(argv(inv));
        cmd.current_dir(cwd);
        cmd.env(OAUTH_TOKEN_ENV, &self.token);
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped());
        cmd
    }
}
