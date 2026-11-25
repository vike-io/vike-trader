//! Finding and driving the BACKTEST ENGINE — the `backtesthub` tool of the multicall `vike-backend`
//! binary that a release attaches beside `vike-cli` on **Linux** (the container image carries the
//! same binary), or a standalone `backtesthub` binary where one is installed — and the STORE TOOL,
//! `vike-backend datahub store` or a standalone `vike-datahub`, found by the same rungs under its own
//! names ([`Tool`]). "The engine" below reads for both.
//!
//! # ⚠ THE ENGINE IS A LINUX / CONTAINER ASSET, AND `vike-cli.exe` IS NOT
//!
//! This is the one asymmetry a reader has to carry, because it decides whether the five rungs below
//! can answer at all. `.github/workflows/release.yml` attaches the engine — inside `vike-backend`
//! — from its HOST (Linux) lane only and builds nothing for Windows; `vike-cli.exe` is cross-built on
//! request with the desktop by `release-desktop.yml` (since 2026-11-24; the desktop's `.exe` since
//! decision 0177), and there is no `backtesthub.exe` or `vike-backend.exe` in any published manifest. So on Windows, rungs 2
//! (`<project>/bin`), 3 (beside this executable), 4 (the multicall) and 5 (`PATH`) find nothing that came from a
//! release, and `--local` and the whole `data` verb are unavailable until the user
//! supplies an engine themselves. [`MISSING_HINT`] says so in those words, per platform.
//!
//! Why the engine is not simply cross-built too, stated so it is a decision rather than a gap: that
//! binary is `vike-backend`, whose `datahub` tool links the DataFusion store and the starter
//! download's HTTPS client (`crates/vike-backend/Cargo.toml`'s `vike-datahub` row), i.e.
//! the whole Arrow/DataFusion/Parquet closure, and nothing in this repository has ever compiled
//! that tree for `x86_64-pc-windows-gnu`. Adding an unproven cross-build of it to a release job is
//! the failure the `windows-cross` CI lane exists to prevent, not an instance of preventing it. The
//! condition that closes this: that closure proved to cross-compile in the `windows-cross` lane
//! first — at which point a Windows build of the engine joins a release workflow and this paragraph
//! shrinks to a sentence.
//!
//! # ⚠ This SPAWNS rather than links, and that is not a shortcut
//!
//! `vike_backtesthub::backtest_cli::run` carries the whole harness and the store tool opens a concrete
//! `HistoryStoreDataFusion`, and **this crate's whole
//! identity is being DataFusion-free and transport-free** — argued edge by edge in
//! `crates/vike-cli/Cargo.toml`, and machine-checked by CI's `light-consumers` lane. Linking either
//! would end the property that makes this binary small enough to be the thing an agent
//! installs, in exchange for saving one process spawn on a command whose real cost is reading a
//! multi-gigabyte tape.
//!
//! So `--local` runs the engine as a CHILD: its stdout and stderr are inherited (the report reaches
//! the terminal or the pipe exactly as if the engine had been invoked directly — nothing is
//! captured, reformatted, or buffered by this process) and its exit status is folded onto this
//! crate's own ladder by [`fold_status`].
//!
//! # Where the engine is looked for, in order
//!
//! 1. `--engine PATH`, if given. The specific consumer's own knob, which is the grain
//!    `crates/vike-paths/src/state_path.rs`'s `PROJECT_TMP_DIR` argues for — and the ONLY way to
//!    name it here, because a `$VIKE_BACKTEST_BIN` read under `src/cmd/` would be a new
//!    `Layer::Library` row on `crates/vike-ops/tests/settings_secrets/settings_registry.rs`'s `LIBRARY_PIN`, a
//!    ratchet that may shrink and never grow.
//! 2. `<project>/bin/backtesthub` — `vike_paths::state_path::PROJECT_BIN_DIR`, the runtime home every
//!    project tool is installed into. The project root arrives as a PARAMETER from the dispatcher,
//!    which resolved it once for the whole process; nothing here walks anything.
//! 3. Beside THIS executable — the place a standalone engine sits when it was installed next to
//!    `vike-cli` by hand. A release no longer attaches one (see rung 4), so on an ordinary release
//!    install neither this rung nor rung 2 has anything to find.
//! 4. **The `vike-backend` MULTICALL** ([`MULTICALL_BIN`]), in those same two places and that
//!    order, invoked as `vike-backend backtesthub …`. ⚠ This rung is BELOW the standalone ones
//!    deliberately: a box that has a real `backtesthub` behaves exactly as it did before this rung
//!    existed, and the dispatcher answers only where no standalone engine is on disk. It is what
//!    lets a release stop shipping 629 MB of duplicated tools without a symlink and without
//!    breaking `data` or `--local` — and since 2026-09-09, when `.github/workflows/release.yml`
//!    began attaching `vike-cli` and `vike-backend` and no standalone `backtesthub`, it is the rung
//!    that answers on an ordinary LINUX install. ⚠ It looked for a binary named `vike` until
//!    2026-09-26: the multicall's PRE-RENAME name (v0.1.21 and earlier), which no current release
//!    ships, so on a release install every rung missed and `--local` and `data` reported the
//!    engine missing while it sat beside `vike-cli` under its real name.
//! 5. `PATH`, by bare name — the developer case, where `cargo build --bin backtesthub` put it in a
//!    directory that is already on the path.
//!
//! ⚠ Rung 4 is why [`locate`] returns a COMMAND rather than a path. "Where is the engine" stopped
//! having a filename as its answer the moment one executable could BE several tools, and a
//! `PathBuf` cannot carry "this program, but first say which tool you are".
//!
//! A miss on all five is a [`Exit::Connect`] failure naming what is missing and how to get it, for
//! the same reason an unreachable datahub is: the command line was right, the thing it needs is not
//! there, and a caller can fix that without editing the command.
//!
//! # ⚠ The second tool this finds: the STORE TOOL
//!
//! `vike-cli data history rm`, `repair`, `fetch --source starter` and the Parquet `export` spawn
//! `vike-datahub store <verb>` rather than the engine: only the datahub's code may open the store
//! (`docs/decisions/0084-only-the-datahub-touches-the-store.md`), and each of those verbs has a case
//! the wire cannot serve. [`locate_store_tool`] walks the SAME five rungs for that tool
//! ([`STORE_TOOL`]): `--engine PATH`, a standalone `vike-datahub` in `<project>/bin`, beside this
//! executable, the multicall as `vike-backend datahub`, then `PATH`. `--engine` keeps its spelling on
//! those verbs and names this tool there. The rest of this page says "engine" for both.
//!
//! # The third: the TRADING DAEMON, for one question
//!
//! `vike-cli config check` asks `vike-tradehub --check-profile` whether the ACTIVE daemon profile
//! loads ([`locate_tradehub`], [`TRADEHUB_TOOL`]: `--tradehub PATH`, a standalone `vike-tradehub`,
//! then the multicall as `vike-backend tradehub`, then `PATH`). Spawned for the reason the engine is:
//! the daemon's loader consults the strategy gallery, which this crate may not link. Its output is
//! CAPTURED ([`output`]) and quoted in the check's report, never inherited.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::exit::{CliError, CommandResult};
use crate::refusals::FlagRefusal;

/// The engine binary's name, without a platform suffix. It is the `[[bin]] name` of
/// `crates/vike-backtesthub`'s `backtesthub` target AND the multicall's tool name for it — the lead
/// argument rung 4 passes — so the spellings cannot differ without one of them failing to find it.
pub(crate) const ENGINE_BIN: &str = "backtesthub";

/// The multicall's file name, without a platform suffix: `crates/vike-backend/Cargo.toml`'s `name`. That
/// manifest declares no `[[bin]]`, so the implicit `src/main.rs` bin takes the package name: the
/// package name IS the executable and the release asset.
///
/// A macro underneath so [`MISSING_HINT`] can `concat!` the name rather than carry a second copy
/// that a rename leaves behind — a copy left behind is exactly how rung 4 came to look for `vike`,
/// the pre-rename name, from the rename until 2026-09-26.
/// `the_multicall_file_name_is_the_backend_package_name` holds it to the manifest.
macro_rules! multicall_bin {
    () => {
        "vike-backend"
    };
}
pub(crate) const MULTICALL_BIN: &str = multicall_bin!();

/// What to tell a user who does not have the engine. Kept beside the search order it describes, so
/// a rung added above is a rung this sentence can be updated for in the same edit.
///
/// ⚠ **Two spellings, because the honest answer differs by platform** (see this module's doc). On
/// Linux the engine is in the release the user already downloaded, so "put it into
/// `<project>/bin/`" is an instruction they can follow. On Windows no release publishes one, so
/// telling them to install it would name a file that does not exist anywhere — the message says
/// that outright and names the one command that produces one. `#[cfg]` rather than a runtime
/// branch: the platform is known at compile time, and the `windows-cross` CI lane compiles this
/// crate for Windows on every PR, so both arms are type-checked.
///
/// ⚠ The Linux arm said "the standalone engine is a separate binary attached to the same release"
/// until 2026-09-26, and no release had attached one since 2026-09-09: the engine ships inside the
/// multicall, as its `backtesthub` tool.
#[cfg(not(windows))]
const MISSING_HINT: &str = concat!(
    "the engine ships as the `backtesthub` tool of ",
    multicall_bin!(),
    ", the backend binary attached to the same release as vike-cli; put it into <project>/bin/ or \
     beside vike-cli, put a standalone `backtesthub` on your PATH, or name one with --engine PATH"
);
#[cfg(windows)]
const MISSING_HINT: &str = concat!(
    "no vike release publishes a Windows build of the engine - it is a Linux/container asset \
     today. Build one with `cargo build --release -p vike-backtesthub --bin backtesthub`, then install \
     it into <project>/bin/, put it on your PATH, or \
     name it with --engine PATH. (The remote path needs no engine at all: `vike-cli backtest run \
     --addr HOST:PORT` runs on the backtesthub, `",
    multicall_bin!(),
    " backtesthub --addr`, instead.)"
);

/// [`MISSING_HINT`], for the store tool, and with its two spellings for the same reason.
#[cfg(not(windows))]
const STORE_MISSING_HINT: &str = concat!(
    "the store tool ships as the `datahub` tool of ",
    multicall_bin!(),
    ", the backend binary attached to the same release as vike-cli; put it into <project>/bin/ or \
     beside vike-cli, put a standalone `vike-datahub` on your PATH, or name one with --engine PATH"
);
#[cfg(windows)]
const STORE_MISSING_HINT: &str = "no vike release publishes a Windows build of the store tool - it is \
     a Linux/container asset today. Build one with `cargo build --release -p vike-datahub --features \
     fetch-starter --bin vike-datahub`, then install it into <project>/bin/, put it on your PATH, or \
     name it with --engine PATH. (A datahub reached over `--addr` needs no store tool at all.)";

/// [`MISSING_HINT`], for the trading daemon. One spelling: the daemon is a Linux/container asset on
/// every platform, and on a box with no daemon there is no daemon profile to start either.
const TRADEHUB_MISSING_HINT: &str = concat!(
    "the trading daemon ships as the `tradehub` tool of ",
    multicall_bin!(),
    ", the backend binary attached to the same release as vike-cli; put it into <project>/bin/ or \
     beside vike-cli, put a standalone `vike-tradehub` on your PATH, or name one with --tradehub PATH"
);

/// One program this module finds and spawns.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Tool {
    /// The standalone binary's name, without a platform suffix.
    bin: &'static str,
    /// The multicall's word for it, the lead argument rung 4 passes.
    multicall_word: &'static str,
    /// What a message calls it.
    title: &'static str,
    missing_hint: &'static str,
}

/// The backtest engine, which `backtest run --local` spawns.
pub(crate) const ENGINE: Tool = Tool {
    bin: ENGINE_BIN,
    multicall_word: ENGINE_BIN,
    title: "the backtesthub engine",
    missing_hint: MISSING_HINT,
};

/// The store tool, which the local `data history` verbs spawn.
pub(crate) const STORE_TOOL: Tool = Tool {
    bin: "vike-datahub",
    multicall_word: "datahub",
    title: "the vike-datahub store tool",
    missing_hint: STORE_MISSING_HINT,
};

/// The trading daemon, which `config check` spawns as `--check-profile` to judge the ACTIVE daemon
/// profile with the daemon's own loader.
pub(crate) const TRADEHUB_TOOL: Tool = Tool {
    bin: "vike-tradehub",
    multicall_word: "tradehub",
    title: "the vike-tradehub daemon",
    missing_hint: TRADEHUB_MISSING_HINT,
};

/// The engine as a COMMAND rather than a path: the program, plus the leading arguments that select
/// it when the program is the multicall.
///
/// ⚠ This was a bare `PathBuf` and could not stay one. `vike-backend` carries the engine as one of
/// its tools and dispatches on a SUBCOMMAND (`vike-backend backtesthub …`), so "where is the engine"
/// stopped having a filename as its answer — and a `PathBuf` cannot express "this program, but
/// first say which tool you are". Keeping the old type would have meant either shipping a
/// standalone `backtesthub` asset forever (629 MB of duplicated code per release, the thing the
/// multicall exists to remove) or a symlink, which this deployment has rejected twice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EngineCommand {
    program: PathBuf,
    /// The multicall tool selector. EMPTY for a standalone engine binary, which is why the
    /// pre-multicall behaviour is preserved byte for byte rather than merely closely.
    lead: Vec<String>,
    /// The `user_data` directory THIS process resolved, handed to the child as
    /// `vike_paths::state_path::USER_DATA_DIR_ENV` so the two processes cannot answer "which
    /// project's runs" differently. `None` hands the child nothing and leaves its environment
    /// exactly as it was inherited. See [`EngineCommand::with_user_data_dir`] for why it has to be told.
    user_data_dir: Option<PathBuf>,
    /// Which program this answers for: what a failure says it could not run.
    tool: &'static Tool,
}

impl EngineCommand {
    /// A plain engine binary — no multicall selector. The shape every rung but one produces, and
    /// the one an `--engine PATH` always produces.
    #[cfg(test)]
    pub(crate) fn standalone(program: impl Into<PathBuf>) -> Self {
        Self::standalone_of(&ENGINE, program)
    }

    /// [`EngineCommand::standalone`] of any tool.
    fn standalone_of(tool: &'static Tool, program: impl Into<PathBuf>) -> Self {
        Self { program: program.into(), lead: Vec::new(), user_data_dir: None, tool }
    }

    /// Tell the child which `user_data` directory to use: the one `Resolved::user_data_dir` holds,
    /// and so the one `vike-cli backtest ls|show|path` READ runs from.
    ///
    /// # ⚠ Why the child has to be TOLD
    ///
    /// The engine resolves its runs root for itself, through
    /// `vike_paths::state_path::user_runs_dir_from` — the `VIKE_USER_DATA_DIR` override, else a
    /// WALK UP FROM ITS WORKING DIRECTORY. That walk does not read `VIKE_SETTINGS_DIR`. The reader
    /// does: this process's directory is `user_data_dir_beside` over the settings directory the
    /// boot resolved, override included. So with `VIKE_SETTINGS_DIR` naming one project and the
    /// working directory inside another, `backtest run --local` SAVED its run in the project it
    /// was standing in while `backtest ls` looked beside the named settings — a run that finished
    /// and could not be found. Measured on #2179 by a reviewer; the comment at the `backtest` arm
    /// of `crates/vike-cli/src/dispatch.rs`'s `dispatch` had claimed the opposite, because it checked
    /// only the `VIKE_USER_DATA_DIR` half.
    ///
    /// Setting the variable is the whole fix, and it is exact rather than approximate: the engine
    /// honours a non-blank value outright, so the child resolves THIS directory, whatever its
    /// working directory is. It changes nothing for an operator who set `VIKE_USER_DATA_DIR`
    /// themselves: `user_data_dir_beside` answers with that value trimmed, and the engine would have
    /// resolved that same path from the inherited one. A BLANK value is ignored by both resolvers,
    /// so it is the unset case and is treated like it.
    ///
    /// ⚠ The working directory is deliberately NOT changed: the `--profile` path the child is
    /// handed may be the operator's own relative one, and a relative override is relative to it too.
    ///
    /// ⚠ Called by the `--local` backtest arm alone, and the `data` verbs are right without it: the
    /// engine routes its `data` subcommand BEFORE it reads any `user_data` path (the user-indicator
    /// install and the runs root both come later in `vike_backtesthub::backtest_cli::run`), and no
    /// `data` verb reads a run. A future engine verb that reads a run and is spawned from
    /// `crate::cmd::data` is the case that would need this call there too.
    pub(crate) fn with_user_data_dir(mut self, dir: Option<&Path>) -> Self {
        self.user_data_dir = dir.map(Path::to_path_buf);
        self
    }

    fn command(&self) -> Command {
        let mut c = Command::new(&self.program);
        c.args(&self.lead);
        // ⚠ Only a UTF-8 path is passed. The engine sweeps `std::env::vars()`, which PANICS on a
        // non-Unicode value, and a non-UTF-8 answer can only have come from this process's own walk
        // from the working directory (both variables it could come from are `String`s) — which is
        // the walk the child performs from that same directory anyway.
        if let Some(dir) = self.user_data_dir.as_deref().and_then(Path::to_str) {
            c.env(vike_paths::state_path::USER_DATA_DIR_ENV, dir);
        }
        c
    }

    /// What a message should call this engine. The lead is included because
    /// `cannot run the backtest engine (/…/vike-backend)` would name a program the operator never
    /// configured and hide which tool was actually being asked for.
    pub(crate) fn display(&self) -> String {
        if self.lead.is_empty() {
            self.program.display().to_string()
        } else {
            format!("{} {}", self.program.display(), self.lead.join(" "))
        }
    }
}

/// ⚠ THE LADDER HAS FOUR RUNGS NOW, and the ORDER is the compatibility guarantee: a standalone
/// `backtesthub` is preferred wherever one exists, so a box that has one behaves exactly as it did
/// before the multicall rung was added. The multicall is consulted only when no standalone engine
/// is on disk, and `PATH` remains the last word.
pub(crate) fn locate(explicit: Option<&str>, project_root: Option<&Path>) -> EngineCommand {
    locate_tool(&ENGINE, explicit, project_root)
}

/// [`locate`], for the store tool: the same rungs, the same order, its own names.
pub(crate) fn locate_store_tool(
    explicit: Option<&str>,
    project_root: Option<&Path>,
) -> EngineCommand {
    locate_tool(&STORE_TOOL, explicit, project_root)
}

/// [`locate`], for the trading daemon: the same rungs, the same order, its own names.
pub(crate) fn locate_tradehub(
    explicit: Option<&str>,
    project_root: Option<&Path>,
) -> EngineCommand {
    locate_tool(&TRADEHUB_TOOL, explicit, project_root)
}

/// Run `engine` with `args` and `envs`, stdin empty and BOTH output streams captured, for a caller
/// whose product is its own report: `config check` QUOTES the child's verdict in a finding, so the
/// child's lines must not land on this process's stdout, which under `--json` is the document.
///
/// # Errors
///
/// The child could not be started: [`cannot_spawn`]'s sentence, naming the program and how to get
/// one.
pub(crate) fn output(
    engine: &EngineCommand,
    args: &[&str],
    envs: &[(&str, &OsStr)],
) -> Result<std::process::Output, String> {
    engine
        .command()
        .args(args)
        .envs(envs.iter().copied())
        .stdin(Stdio::null())
        .output()
        .map_err(|e| cannot_spawn(engine, e).message)
}

fn locate_tool(
    tool: &'static Tool,
    explicit: Option<&str>,
    project_root: Option<&Path>,
) -> EngineCommand {
    if let Some(named) = explicit {
        // NOT probed for existence: a named path that is not there must fail with the name the
        // operator typed, never by falling through to a different engine they did not ask for.
        // ⚠ Nor is it ever treated as a multicall: `--engine` names the thing to RUN, and silently
        // appending a tool selector to an operator's own path would run something they did not type.
        return EngineCommand::standalone_of(tool, named);
    }
    let file = format!("{}{}", tool.bin, std::env::consts::EXE_SUFFIX);
    let standalone = [
        project_root.map(|r| r.join(vike_paths::state_path::PROJECT_BIN_DIR).join(&file)),
        std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join(&file))),
    ];
    if let Some(p) = standalone.into_iter().flatten().find(|p| p.is_file()) {
        return EngineCommand::standalone_of(tool, p);
    }
    // The multicall, in the same two places and that order. ⚠ The selector is the tool's name as
    // `crates/vike-backend/src/main.rs`'s tool table registers it, matched by EXACT string with no aliases
    // (`crates/vike-backend/src/lib.rs`'s `resolve`) — a wrong one prints the dispatcher's own tool list
    // and exits 2. ⚠ The FILE name has to be exact too: it read `vike` here until 2026-09-26, the
    // pre-rename name, and every release since the rename was invisible to this rung
    // ([`MULTICALL_BIN`]).
    let multicall = format!("{MULTICALL_BIN}{}", std::env::consts::EXE_SUFFIX);
    let carrier = [
        project_root.map(|r| r.join(vike_paths::state_path::PROJECT_BIN_DIR).join(&multicall)),
        std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join(&multicall))),
    ];
    if let Some(p) = carrier.into_iter().flatten().find(|p| p.is_file()) {
        return EngineCommand {
            program: p,
            lead: vec![tool.multicall_word.to_string()],
            user_data_dir: None,
            tool,
        };
    }
    EngineCommand::standalone_of(tool, tool.bin)
}

/// Whether the child gets THIS process's stdin, or nothing at all.
///
/// ⚠ **`Null` is the default and stays the default**, and the reason is the one this enum exists to
/// keep visible: a child holding an inherited stdin can READ from it, and every engine verb but one
/// reads nothing — so handing it the terminal buys nothing and costs the ability to say, in this
/// file, which verbs can consume a line the operator typed.
///
/// [`Stdin::Inherit`] has exactly one caller, `vike-cli data history rm`, and it is what lets the ENGINE own
/// the typed confirmation. The alternative — the CLI reading the line itself — needs the matched
/// COUNT the confirmation is bound to, which only the plan knows, which means a second spawn and a
/// machine document parsed on this side. Passing the terminal through costs one enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stdin {
    /// The child reads EOF. Every verb but `rm`'s confirmation.
    Null,
    /// The child gets this process's stdin — a terminal, when there is one.
    Inherit,
}

impl Stdin {
    fn stdio(self) -> Stdio {
        match self {
            Stdin::Null => Stdio::null(),
            Stdin::Inherit => Stdio::inherit(),
        }
    }
}

/// The one message a spawn failure gives, so [`run`] and [`run_capturing_stdout`] cannot describe
/// the same failure differently.
fn cannot_spawn(engine: &EngineCommand, e: std::io::Error) -> CliError {
    CliError::connect(
        FlagRefusal::CannotSpawn {
            title: engine.tool.title,
            engine: engine.display(),
            e: e.to_string(),
            missing_hint: engine.tool.missing_hint,
        }
        .rendered(),
    )
}

/// Spawn the engine with `args`, inherit its streams, and fold its exit status onto this crate's
/// ladder.
///
/// `verb` is the `vike-cli` subcommand that asked, so a failure message names the command the user
/// actually typed rather than the child.
pub(crate) fn run(
    engine: &EngineCommand,
    args: &[impl AsRef<OsStr>],
    verb: &str,
) -> CommandResult<()> {
    run_with_stdin(engine, args, verb, Stdin::Null)
}

/// [`run`], choosing what the child's stdin is. See [`Stdin`] for why that is a choice at all and
/// why exactly one caller makes the other one.
pub(crate) fn run_with_stdin(
    engine: &EngineCommand,
    args: &[impl AsRef<OsStr>],
    verb: &str,
    stdin: Stdin,
) -> CommandResult<()> {
    let status = engine
        .command()
        .args(args)
        // INHERIT, never capture. The engine's report — a human table or a `--json` document — is
        // the product of this command, and putting a pipe in front of it would mean buffering a
        // whole run's output and re-emitting it, changing both the timing and (on a failure) which
        // stream carried what.
        .stdin(stdin.stdio())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .map_err(|e| cannot_spawn(engine, e))?;
    fold_status(engine.tool, status.code(), verb)
}

/// The same spawn, with the child's STDOUT read back instead of inherited — and echoed to THIS
/// process's stderr as it arrives, so a person watching still sees the engine's report in order.
///
/// # ⚠ The one exception to "INHERIT, never capture", and what buys it
///
/// This module's doc says the engine's report is the product of the command and that putting a pipe
/// in front of it would change the timing and the stream a line lands on. That is right for every
/// caller but one: under `vike-cli data --json` the product is a JSON DOCUMENT, and this crate's
/// rule (`crate::cmd::secrets`, `crate::cmd::init`) is that stdout under `--json` is the document
/// and nothing else. An engine writing its human lines onto the same stdout would leave a caller
/// parsing a stream that is not JSON. So the lines are MOVED to stderr — where every diagnostic in
/// this crate already goes — and returned, so the document can carry them verbatim.
///
/// ⚠ Verbatim, and NOT parsed. `store fetch-starter` prints the bar and row counts
/// it knows as prose and has no `--json` mode of its own; reading numbers out of those
/// sentences would be a second implementation of the engine's output format, in a different crate,
/// that drifts the first time a word changes and reports a wrong count rather than failing. The
/// caller gets the lines; nothing here claims to understand them.
///
/// The child's stderr is still inherited: a failure diagnostic reaching the user unbuffered is the
/// behaviour [`run`] promises, and `--json` is no reason to hold it back.
pub(crate) fn run_capturing_stdout(
    engine: &EngineCommand,
    args: &[impl AsRef<OsStr>],
    verb: &str,
) -> CommandResult<Vec<String>> {
    run_capturing_stdout_with_stdin(engine, args, verb, Stdin::Null)
}

/// [`run_capturing_stdout`], choosing the child's stdin — the `--json` twin of
/// [`run_with_stdin`]. `data rm --json` without `--yes` needs both halves at once: the document
/// comes back on the pipe while the confirmation the child prompts for is typed on the terminal it
/// inherited.
pub(crate) fn run_capturing_stdout_with_stdin(
    engine: &EngineCommand,
    args: &[impl AsRef<OsStr>],
    verb: &str,
    stdin: Stdin,
) -> CommandResult<Vec<String>> {
    use std::io::{BufRead, BufReader};

    let mut child = engine
        .command()
        .args(args)
        .stdin(stdin.stdio())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| cannot_spawn(engine, e))?;

    let mut report = Vec::new();
    // `take()` so the pipe is CLOSED when this scope ends: holding the read end open while waiting
    // is how a child that fills the pipe buffer and a parent that has stopped reading deadlock.
    if let Some(out) = child.stdout.take() {
        for line in BufReader::new(out).lines() {
            let line = line.map_err(|e| {
                CliError::failed(format!("reading {}'s output failed: {e}", engine.tool.title))
            })?;
            eprintln!("{line}");
            report.push(line);
        }
    }
    let status = child
        .wait()
        .map_err(|e| CliError::failed(format!("waiting for {} failed: {e}", engine.tool.title)))?;
    fold_status(engine.tool, status.code(), verb)?;
    Ok(report)
}

/// The child's exit status, as one of ours: `0` is success and EVERY other outcome is
/// [`crate::exit::Exit::Failed`], the pre-existing catch-all this crate reserves for a failure it
/// has not classified.
///
/// # ⚠ Why the child's `2` is NOT folded onto the usage rung
///
/// It looks like it should be — both binaries call `2` "usage" — and that reading is wrong,
/// because the store tool's `2` is OVERLOADED. `crates/vike-datahub/src/datahub_cli/store_cmd.rs` returns
/// `ExitCode::from(2)` when `HistoryStoreDataFusion::open` cannot open the store and when the starter
/// download fails — neither of which is a command line.
/// [`crate::exit::Exit::Usage`] promises the opposite of both: "nothing was attempted, and
/// re-running unchanged cannot succeed". Folding a failed DOWNLOAD onto it would tell a wrapper to
/// STOP RETRYING a transient outage and tell an agent to report a typo to its user — the exact
/// retry-vs-fix inversion the ladder exists to remove.
///
/// ⚠ **The example this paragraph was written around is GONE, and it is kept because it is what
/// made the overload obvious.** `run_fetch` returned the same `2` when `fetch::fetch_into` failed,
/// and its comment beside that line read "a geoblock, a delisted symbol and a bad interval all
/// arrive here" — three causes, one status, and only one of them a typo. That verb and its module
/// were deleted when the backtest plane stopped reaching a venue. The overload itself is unchanged:
/// the three rungs above still share `2`, and a store that will not open is as retryable as a
/// geoblock was.
///
/// So an unclassifiable child status lands where every unclassified failure in this crate lands,
/// which is the rule [`crate::exit::CliError`]'s `From<String>` impl states: `Failed`. The RUNG is
/// honest (it ran and failed) and the child's own diagnostic — already on the stderr this process
/// inherited — is what says which of the two it was.
///
/// ⚠ Nothing is LOST by that, because this crate does not need the child to catch usage errors: the
/// argument shapes are checked HERE before a process is started (`crate::cmd::data`'s spec and
/// window checks, `crate::cmd::backtest`'s mode-exclusivity checks), and
/// the argv handed to the child is built by this crate rather than typed by the operator. What
/// would reopen this: a distinct pre-flight/runtime code on the engine side, at which point its
/// genuine argument errors could fold onto `Usage` without carrying a network failure with them.
///
/// ⚠ `None` — the child was SIGNALLED rather than exiting — gets its own sentence rather than the
/// numeric one, because there is no number to print. It cannot happen on Windows, where every
/// termination carries a code, which is exactly why it is stated here rather than left to a
/// platform's behaviour.
fn fold_status(tool: &Tool, code: Option<i32>, verb: &str) -> CommandResult<()> {
    match code {
        Some(0) => Ok(()),
        // The engine printed its own diagnostic on the stderr this process inherited, so a second
        // sentence here would only repeat it — and, as the doc above argues, this process cannot
        // tell WHICH failure a given code meant. What is added is the code itself and the RUNG.
        Some(other) => {
            Err(CliError::failed(format!("{} exited {other} (see its output above)", tool.title)))
        }
        None => Err(CliError::failed(format!(
            "{} was terminated by a signal during `vike-cli {verb}`",
            tool.title
        ))),
    }
}

/// `<project>/tmp`, where a REWRITTEN profile is staged before it is handed to the child.
///
/// ⚠ Not the system temp directory, and that is a rule rather than a preference:
/// `crates/vike-ops/tests/hygiene/system_temp_gate.rs` is a ratchet over exactly this class, and
/// `crates/vike-paths/src/state_path.rs`'s `PROJECT_TMP_DIR` argues why — inside the container this
/// project is moving to, the system temp directory is not the host's, does not survive a restart,
/// and cannot be mounted beside the project folder.
///
/// `None` when no project sits above the working directory. The caller REFUSES rather than falling
/// back, because the only remaining place to write is beside the user's own profile.
///
/// # ⚠ What this call also DOES, deliberately: it SWEEPS
///
/// `vike_paths::scratch`'s own contract is that "ownership bounds the ordinary path and the sweep
/// bounds the abort path, and neither substitutes for the other". `ScratchDir`'s `Drop` is the
/// first half and it does not run on a `SIGKILL`, an OOM kill, a power loss — or a Ctrl-C, which
/// terminates without unwinding and is the *ordinary* way somebody stops a long `--local` run. So
/// without the second half the leftover population under `<project>/tmp` is unbounded, which is
/// exactly how the 26,851 directories / 215 GB measured on the build box happened.
///
/// Folded into the RESOLVER rather than left as a second call, for the reason
/// `crates/vike-backfill/src/cli.rs`'s `scratch_root` folds it into its own: no consumer can
/// resolve the root and forget the retention. ⚠ Unlike that one there is no `tracing` line — this
/// crate links no logging, by design — so the sweep's result is dropped rather than reported.
pub(crate) fn scratch_root(project_root: Option<&Path>) -> Option<PathBuf> {
    let root = project_root.map(|r| r.join(vike_paths::state_path::PROJECT_TMP_DIR))?;
    // Best-effort and total: an absent or unreadable root is not an error (a project that has
    // staged nothing has no `tmp/`), and every removal is counted rather than propagated.
    vike_paths::scratch::sweep(&root, Some(vike_paths::scratch::DEFAULT_MAX_SCRATCH_ENTRIES));
    Some(root)
}

#[path = "engine_tests.rs"]
#[cfg(test)]
mod engine_tests;
