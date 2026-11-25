//! The BUILDER SERVICE's own logic: its authenticated wire protocol, its retention pruner, and
//! its loopback-only bind rule. Task 6 (Track B).
//!
//! ⚠ **This module (and this whole crate) moved out of `vike-rust-strategy` by a controller
//! ruling, after Track A and Track B independently found the same problem: `vike-rust-strategy`
//! held both the loader a compiled strategy file links against AND this service, so every compiled user
//! strategy file inherited this service's whole dependency tree
//! (`vike-datahub-client`/`serde_json`, and `tracing` until it was dropped — see below) —
//! directly undercutting the design's own reason
//! for choosing DYNAMIC linkage (a strategy file should be tens of kilobytes, not megabytes). Nothing in
//! this module's OWN logic changed across that move; only its crate changed, from
//! `vike_rust_strategy::builder` to `vike_strategy_builder::builder`.
//!
//! ⚠ **This module logged through `tracing` and emitted NOTHING, so the dependency is gone and
//! the two sites print to stderr.** `tracing` is a FACADE: an event reaches a destination only if
//! the process installed a subscriber, and `src/bin/vike-strategy-builder.rs` installs none — it
//! calls no `vike_log::init`, and deliberately does not run `vike_boot::boot` either (that
//! binary's own module doc argues the scope). So the crate's manifest claimed "connection-boundary
//! logging" for a dependency that produced no output at all: an accept failure and a failed
//! retention prune, the two events on this service worth an operator's eyes, were both silently
//! discarded.
//!
//! The alternative — install a subscriber — was weighed and REFUSED for this binary. It would make
//! this root a HALF-follower of the startup sequence `crates/vike-boot` owns: the log directory
//! and both log levels are themselves settings, this binary loads none, so a subscriber here could
//! only ever honour the environment and would put a rolling log file wherever a second,
//! settings-blind resolution happened to land. `crates/vike-boot/tests/one_owner.rs`'s own header
//! is about exactly that failure. A root that plainly does not boot is honest; one that performs
//! step 5 of six is the shape that gate exists to refuse. If this service is ever wired through
//! `vike_boot::boot` (the binary's scope note says what that costs), `tracing` comes back WITH a
//! subscriber and these two `eprintln!`s become events again.
//!
//! stderr is not a downgrade here: it is what this binary already uses for every other
//! operator-facing line (the two startup refusals, the listening banner, the serve exit), and
//! systemd captures it into the journal — so today it is strictly more output, not less.
//!
//! ⚠ **The service's KEY lives in the settings database and nowhere else** (the owner's ruling of
//! 2026-10-31, "all in the SQLite, not files"; `docs/decisions/0083-the-runtime-plugin-join-lands.md`'s
//! question 2). That does NOT make this binary a boot root: it opens the store through
//! `vike_secrets::open_credential_store` (the door a root that runs no boot uses) and reads that
//! ONE name, scoped — it loads no settings row, installs no subscriber and holds no other
//! credential. [`BUILDER_KEY_NAME`] carries the rest of the argument.
//!
//! ⚠ **Split from `src/bin/vike-strategy-builder.rs` deliberately, not as Task 6's brief's file
//! list literally reads it.** Task 6's brief names one file to create
//! (`src/bin/vike-strategy-builder.rs`) plus a `tests/builder_auth.rs` that exercises it — but a
//! Cargo integration test can only link a crate's LIBRARY target, never one of its binary targets,
//! so `authenticate`/`prune`/`bind_address` had to live somewhere `tests/builder_auth.rs` can
//! `use vike_strategy_builder::builder::…`. This module is that home; the actual
//! `src/bin/vike-strategy-builder.rs` stays a thin `main()` over it — the SAME split
//! `vike-backtest` uses (`backtesthub_server.rs` in the library, `backtest_cli.rs`/`main.rs` wiring it
//! up), just discovered here rather than already being on the page.
//!
//! # Decision 1 — this service's OWN auth [`Domain`], never a sibling's
//!
//! [`DOMAIN`] is disjoint from BOTH `vike_tradehub_client::auth::DOMAIN` (the live order-signing
//! control channel) and `vike_hub_protocol::auth::DATAHUB_DOMAIN` /
//! `vike_hub_protocol::auth::BACKTESTHUB_DOMAIN` (the data and compute daemons) — a separator of
//! its own, not a reuse of any. `crates/vike-tradehub-client/src/auth.rs`
//! is explicit that `HubKeys`/`Scope`/`Domain` are RE-EXPORTED from
//! [`vike_hub_protocol::auth`], generalized over the domain separator FOR EXACTLY THIS
//! REASON — so a third service names its own constant rather than copying the crypto.
//!
//! `crates/vike-backtesthub/src/backtesthub_server.rs` reuses the DATAHUB pair for its own compute
//! daemon, and it is worth being explicit about why that precedent does NOT apply here: that
//! reuse is argued as an internal REORGANISATION of one operator's own already-issued surface
//! ("the two daemons run on one box, over one tunnel, against one store, for one operator" —
//! ruling 7 split one served surface into two sockets, so keeping one key pair meant an existing
//! deployment did not have to mint new credentials for a boundary that used to be internal). This
//! service is not a split of anything that used to be one daemon — it is a NEW capability nothing
//! upstream had, and reusing a sibling's domain here would mean a key minted to observe or
//! control a live order-signing node, or to compile Rhai next to a hist store, would ALSO
//! authenticate a request to run arbitrary `cargo` on this box. That is a strictly wider grant
//! than whoever minted that key intended, and it is exactly the failure a domain separator exists
//! to make structurally impossible rather than merely undocumented. So: own domain, own key name,
//! and the two are provably disjoint below
//! ([`a_key_for_another_domain_is_refused`](../../tests/builder_auth.rs)).
//!
//! # Decision 2 — the one verb this service serves is [`Scope::Write`], and it is the CEILING
//!
//! This service compiles and RUNS code an authenticated caller supplies — that is its job, not a
//! flaw, and [`required_scope`] is the line arguing why that job demands the strongest scope this
//! domain will ever grant. Cross-check against
//! `crates/vike-backtesthub/src/backtesthub_server.rs`'s `required_scope`, whose doc argues
//! `StrategySpecWire::Rhai` is `Scope::Write` because *"That is remote code execution BY DESIGN … the
//! correct posture for it is the write scope."* — see [`required_scope`]'s own doc for why a
//! verb that hands source straight to `cargo` cannot rationally earn less than one that reaches a
//! compiler through a sandboxed interpreter.

use std::collections::HashMap;
use std::fmt;
use std::io;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::thread;

use serde::{Deserialize, Serialize};
use vike_hub_protocol::auth::{self, Domain, HubKeys, Scope};
use vike_hub_protocol::child_pipe::{read_message, write_message};
use vike_hub_protocol::frame::{read_frame_raw_capped, write_frame};
use vike_hub_protocol::limits::SERVER_HANDSHAKE_MAX_FRAME_LEN;
use vike_hub_protocol::refusal::HubRefusal;

use vike_rust_strategy::sdk::{Sdk, TemplateKind};

use crate::{jail, render};

/// This service's OWN domain separator — see this module's doc, Decision 1, for why it is
/// neither `vike_tradehub_client::auth::DOMAIN` nor
/// `vike_hub_protocol::auth::DATAHUB_DOMAIN`. NUL-terminated, following
/// [`Domain::new`]'s `b"vike-<service>-auth\0"` convention so no separator can be a prefix of
/// another.
pub const DOMAIN: Domain = Domain::new(b"vike-strategy-builder-auth\0");

/// This service's own wire-protocol version — independent of `vike_datahub_client::proto`'s or
/// `vike_tradehub_client::proto`'s, because this is a THIRD protocol, not a verb bolted onto
/// either existing one. 2 (plan 2b): a `Build` carries a strategy FILE, its path and its text. 3
/// (2026-11-15, the owner's rule that a research study is a FILE built like a strategy file): a
/// `BuildStudy` carries a study's name and its folder's `.rs` files. 4 (decision 0296, the owner's
/// approval of 2026-11-19): `BuildOk` names the SDK the file built against and its build folder, so the
/// caller hands the sandboxed child the artifact's whole path (`sdk-N/<folder>/<sha>.so`); and every
/// frame past the `Welcome` is positional msgpack ([`write_message`], the owner's "JSON only at the
/// edges" of 2026-11-19). The `Hello`/`Welcome` pair stays JSON, so a peer of another version still
/// reads the other's number and refuses in words, naming both, before a msgpack frame is sent.
/// Positional is sound here: both ends ship in one `vike-backend`, and this number refuses any other
/// build's peer. The frames are `vike_hub_protocol::child_pipe`'s codec. 5 (the refusal carriers, the
/// owner's approval of 2026-11-24): `BuildErr` is the refusal frame `{id, text}` ([`HubRefusal`]), so the
/// surface that shows a refused Build shows the builder's own id. A positional frame cannot gain a field an
/// older peer skips, so the bump is what refuses one, at the `Welcome` (decision 0112's lockstep fleet).
pub const PROTO_VERSION: u32 = 5;

/// The credential-store NAME of the service's key — a DEPLOYMENT row of the settings database
/// (`<project>/settings/db/vike.db`), and the ONLY place the key lives. One name, not the usual
/// observe/control PAIR: this protocol has exactly one verb and it is [`Scope::Write`] (Decision
/// 2), so there is no read-only capability to hand a separate observe key for.
///
/// ⚠ **The owner's ruling of 2026-10-31 — "all in the SQLite, not files" — retired two earlier
/// homes, and neither is read any more.** Until then the service took its key from a process
/// variable of this very name, or from a key FILE a `LoadCredential=` line copied
/// (`settings/strategy-builder.key`), and the desktop took it from the PC's environment, where the
/// Studio launcher put it. Both variables and the file are read by NOTHING now, and there is no
/// fallback: a retired name just stops being read (decision 0111, no migration code). A key left
/// behind in an old file or variable is not found, and the refusal below says where to put it.
/// ⚠ The shipped unit still SETS the file variable (and loads the old key file) for ONE release,
/// so the previous binary can be rolled back under it: this binary ignores the variable and
/// deliberately does NOT refuse it — a refusal would make the unit that serves both binaries
/// unstartable by the new one. `crates/vike-strategy-builder/tests/child_env.rs` starts the daemon
/// under exactly that environment.
///
/// ⚠ **A DAEMON key, of decision 0164's class, since decision 0288's amendment of 2026-11-16** (the
/// owner's option A): one box, one row, read by this SERVICE ([`resolve_keys`]) and by the daemons
/// beside it that dial it (`crate::client::build_file`: the backtesthub, which forwards a Studio's
/// Build and builds a run's file, and the tradehub's mount). No person and no PC holds it: Studio
/// builds THROUGH the backtesthub, which checks the person against the one access list first.
/// `vike-cli backend mint-missing` mints it at every server start when the box lacks it;
/// `vike-cli secrets set` still writes it by hand ([`KeyRefusal::Missing`] names the command).
pub const BUILDER_KEY_NAME: &str = "VIKE_STRATEGY_BUILDER_KEY";

vike_utility::refusal_enum! {
    /// Why [`resolve_keys`] produced no key: the operator-facing reason, which names the row, the store and the
    /// command that sets it, never key material. Each has its id (`vike_utility::refusal_enum!`, decision 0301).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum KeyRefusal with ids KeyRefusalId {
        /// The store holds no (non-blank) row of that name — or there is no store at all, which is the
        /// same answer an unconfigured box always gives. `store` says where the service looked. The text names
        /// the ONE command that sets the key and how its value travels: on stdin (or from a variable named by
        /// `--from-env`, for that one command), NEVER on the command line
        /// (`docs/decisions/0036-credentials-are-read-only-from-the-cli-and-the-mcp-surface.md`).
        Missing { store: String } => "strategy.builder.key.missing":
            "no strategy-builder key: {store} holds no `{BUILDER_KEY_NAME}` row. Set it once on this \
             machine with `vike-cli secrets set {BUILDER_KEY_NAME}` (the value on stdin, or add \
             `--from-env VAR` to take it from a variable you export for that one command; never on the \
             command line). The key lives in the settings database only: there is no key file and no \
             environment variable for it.",
        /// The store EXISTS and could not be read (or `VIKE_NO_LIVE_KEYS` is malformed). Not "no key":
        /// a permissions fault wearing the not-configured answer is the failure this variant exists to
        /// keep apart from [`KeyRefusal::Missing`].
        Unreadable { why: String } => "strategy.builder.key.unreadable":
            "the strategy-builder key could not be read — the credential store is there but \
             would not open ({why}). That is a fault to fix, not an absent key; once the \
             store opens, `{BUILDER_KEY_NAME}` is read from it.",
    }
}

vike_utility::refusal_enum! {
    /// Why this service refuses to START over its command line (the operator reads it in the unit's journal,
    /// after `vike-strategy-builder: refusing to start — `): each has its id (`vike_utility::refusal_enum!`,
    /// decision 0301) and is said in its shown form (`rendered()`): that journal line IS the surface, no parent
    /// wraps it. A missing key is [`KeyRefusal`]'s.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum StartRefusal with ids StartRefusalId {
        /// Both source flags: a builder serves ONE tree or the installed SDKs.
        BothSourceRoots => "strategy.builder.start.both_source_roots":
            "{WORKSPACE_ROOT_FLAG} and {SDK_ROOT_FLAG} are both given. A builder compiles against ONE source \
             tree ({WORKSPACE_ROOT_FLAG}: a git checkout) or against the installed SDKs ({SDK_ROOT_FLAG}: the \
             deployed layout); name one.",
        /// Neither source flag: there is no compile-time fallback (`run`'s comment says why).
        NoSourceRoot => "strategy.builder.start.no_source_root":
            "neither {SDK_ROOT_FLAG} nor {WORKSPACE_ROOT_FLAG} is given.\n\
             A rendered strategy file's Cargo.toml resolves its `path` dependencies against a \
             source tree: the installed SDKs ({SDK_ROOT_FLAG} <project>/vendor/vike-strategy-source, \
             one sdk-N/ each) or one tree ({WORKSPACE_ROOT_FLAG}, a git checkout). There is no \
             compile-time fallback: a binary built in one checkout and deployed elsewhere would \
             silently point cargo at a tree that does not exist there, and a compiler has no \
             data-path ladder to fall through to instead — it either finds real crates to build \
             against or the compile fails.",
        /// The source flag `flag` names a tree `root` (as `Path::display` shows it) holding its own `settings/`,
        /// which the jail would bind into every compile.
        SourceRootHoldsSettings { flag: &'static str, root: String }
            => "strategy.builder.start.source_root_holds_settings":
            "{flag} at\n\
             {root} names a tree that contains its own `settings/` directory.\n\
             That directory is where this project's venue credentials live, and the builder's \
             jail binds this variable's target back into every compile READ-ONLY — so pointing \
             it at a project root (rather than a strategy-source-only tree) would hand every \
             `build.rs` a path to the credential store. Point this at the release asset \
             `fetch_release_tools.sh strategy-source` extracts, or at a subtree of a \
             hand-maintained checkout that holds no `settings/` of its own.",
    }
}

/// The key a stored value carries: surrounding whitespace trimmed (so a trailing newline from a
/// pipe is the same key as the bare string), blank refused.
fn key_from_value(raw: &str) -> Option<HubKeys> {
    let key = raw.trim();
    if key.is_empty() {
        return None;
    }
    Some(HubKeys::new(Vec::new(), key.as_bytes().to_vec()))
}

/// The service's key, read from the credential store `vars` locates — `$VIKE_SETTINGS_DIR` when the
/// caller's one environment sweep carries it, else the working directory's project walk: the door
/// every root that runs no boot uses (`vike_secrets::open_credential_store`).
///
/// The read is SCOPED to [`BUILDER_KEY_NAME`] alone, so this process holds no other credential.
/// `vars` contributes the store's LOCATION and the live-key fence (`VIKE_NO_LIVE_KEYS`) — never the
/// key.
///
/// # Errors
/// [`KeyRefusal::Missing`] when there is no store or it holds no such row;
/// [`KeyRefusal::Unreadable`] when the store is there and will not open.
pub fn resolve_keys(vars: &HashMap<String, String>) -> Result<HubKeys, KeyRefusal> {
    let unreadable = |e: &dyn fmt::Display| KeyRefusal::Unreadable { why: e.to_string() };
    let store = vike_secrets::open_credential_store(vars).map_err(|e| unreadable(&e))?;
    let read = store
        .read(&vike_secrets::CredentialKeyScope::of([BUILDER_KEY_NAME]))
        .map_err(|e| unreadable(&e))?;
    let held = read.get(BUILDER_KEY_NAME).declared().map_err(|e| unreadable(&e))?;
    held.and_then(key_from_value)
        .ok_or_else(|| KeyRefusal::Missing { store: describe_store(&store) })
}

/// Where [`resolve_keys`] looked, for [`KeyRefusal::Missing`]: the database's path (not a secret),
/// and the verb that creates it when the box has none.
fn describe_store(store: &vike_secrets::CredentialStore) -> String {
    let Some(location) = store.location() else {
        return "the credential store (none located)".to_string();
    };
    let db = vike_secrets::db_path_in(location.settings_dir());
    match store.backend() {
        vike_secrets::StoreBackend::Database(_) => {
            format!("the settings database {}", db.display())
        }
        vike_secrets::StoreBackend::Absent => format!(
            "the settings database {} (it does not exist: {})",
            db.display(),
            vike_secrets::CREATE_STORE_REMEDY
        ),
    }
}

/// `--port N` — the loopback port this service listens on ([`DEFAULT_PORT`] when absent; `0` asks
/// the OS for a free one, which the listening banner then names).
pub const PORT_FLAG: &str = "--port";

/// `--out-dir DIR` — where finished artifacts land ([`DEFAULT_OUT_DIR`] when absent).
pub const OUT_DIR_FLAG: &str = "--out-dir";

/// `--retain N` — how many artifacts [`prune`] keeps in the current build folder, the most recently
/// USED ([`DEFAULT_RETAIN`] when absent; the deployed units pass 2000, and
/// `deploy/vike-strategy-builder.service`'s `--retain` block carries the measurement behind it).
pub const RETAIN_FLAG: &str = "--retain";

/// `--workspace-root DIR` — ONE source tree every build compiles against: a git checkout (SDK dev; a dev
/// box's, a lane's, a test's) or one installed SDK snapshot (`render::source_tree_refusal`). [`run`]
/// takes it or [`SDK_ROOT_FLAG`], exactly one.
pub const WORKSPACE_ROOT_FLAG: &str = "--workspace-root";

/// `--sdk-root DIR` — the installed SDKs (`<project>/vendor/vike-strategy-source`, one `sdk-N/` each,
/// decision 0296), each checked against its marker at the start (`render::InstalledSdks::open`); a build
/// compiles against the one it resolves (`render::InstalledSdks::resolve`). What a deployed builder runs on.
pub const SDK_ROOT_FLAG: &str = "--sdk-root";

/// The builds directory when [`OUT_DIR_FLAG`] is absent — relative to the working directory, as
/// it always was. It holds one folder per build contract, each artifact `<folder>/<sha256>.so`
/// (`vike_rust_strategy::artifact`'s module doc).
pub const DEFAULT_OUT_DIR: &str = "user_data/strategy-builds";

/// The program name every message of this program carries: `vike-strategy-builder: <message>`, and
/// the `Usage:` line.
const BIN: &str = "vike-strategy-builder";

/// This service's command line, as `clap` reads it (decision 0155) — the four plain settings
/// decision 0111 made arguments (they were `VIKE_STRATEGY_BUILDER_*` variables): how ONE builder
/// process is started, which is what its unit's `ExecStart=` states.
///
/// Each value flag takes the next argument or an `=VALUE`; an unknown argument, a missing or blank
/// value or a number that does not parse is an ERROR naming it — never a silently-defaulted knob,
/// which is what a malformed variable used to be here. Every value is TRIMMED.
#[derive(clap::Parser, Debug, Clone, PartialEq, Eq)]
#[command(
    about = "Compile an authenticated caller's Rust strategy so a backtest can load it.",
    long_about = None,
    after_help = "\
The key is the VIKE_STRATEGY_BUILDER_KEY row of the settings database -- set it with
`vike-cli secrets set VIKE_STRATEGY_BUILDER_KEY` (value on stdin). It is not a flag, a file or a
variable, and never on this command line.",
    disable_help_subcommand = true
)]
pub struct BuilderArgs {
    /// Loopback port to listen on (0 = a free port; the listening banner names it).
    #[arg(long = "port", value_name = "N", default_value_t = DEFAULT_PORT, value_parser = |raw: &str| number_value::<u16>(raw, PORT_FLAG, "port number"))]
    pub port: u16,
    /// Where finished artifacts land.
    #[arg(long = "out-dir", value_name = "DIR", default_value = DEFAULT_OUT_DIR, value_parser = path_value)]
    pub out_dir: PathBuf,
    /// Artifacts kept in the current build folder, the most recently used.
    #[arg(long = "retain", value_name = "N", default_value_t = DEFAULT_RETAIN, value_parser = |raw: &str| number_value::<usize>(raw, RETAIN_FLAG, "count"))]
    pub retain: usize,
    /// One source tree every build compiles against: a git checkout (SDK dev) or one installed SDK.
    /// Exactly one of this and --sdk-root, and refused if it holds a settings/ directory of its own.
    #[arg(long = "workspace-root", value_name = "DIR", value_parser = path_value)]
    pub workspace_root: Option<PathBuf>,
    /// The installed SDKs (`<project>/vendor/vike-strategy-source`, one sdk-N/ each): a build compiles
    /// against its file's pin, else the newest usable one. Exactly one of this and --workspace-root.
    #[arg(long = "sdk-root", value_name = "DIR", value_parser = path_value)]
    pub sdk_root: Option<PathBuf>,
    /// Answer whether the build jail can be applied on this box, then exit (0 yes, 3 no). Hidden: the
    /// daemon re-enters itself with it (`vike_sandbox::probe_in_child`), so a unit's syscall filter
    /// that kills on a Landlock call kills that child, never the daemon.
    #[arg(long = "jail-probe", hide = true)]
    pub jail_probe: bool,
    /// The deploy's prebuild (`crate::prebuild`): build every `.rs` file the active daemon profile mounts,
    /// read from this folder (the project's strategies/), say one line each and exit 0. Serves nothing and
    /// needs no key. Hidden: `deploy/docker/vikectl` runs it on the new build before the tradehub restarts.
    #[arg(long = "prebuild-mounts", value_name = "STRATEGIES", value_parser = path_value, hide = true)]
    pub prebuild_mounts: Option<PathBuf>,
}

/// A numeric flag's value (`--port`, `--retain`), trimmed: a `T`, or `<flag>: not a <what>: <raw>`.
fn number_value<T: std::str::FromStr>(raw: &str, flag: &str, what: &str) -> Result<T, String> {
    vike_utility::cli::non_empty(raw)?.parse().map_err(|_| format!("{flag}: not a {what}: {raw}"))
}

/// A directory flag's value: non-blank, trimmed.
fn path_value(raw: &str) -> Result<PathBuf, String> {
    vike_utility::cli::non_empty(raw).map(PathBuf::from)
}

/// Read the command line (the arguments AFTER the program or verb name).
///
/// # Errors
/// The [`vike_utility::cli::CliExit`] to print and exit with: `-h`/`--help` and `-V`/`--version`
/// (code 0, stdout), every refusal (code 2, stderr).
pub fn read_args(args: &[String]) -> Result<BuilderArgs, vike_utility::cli::CliExit> {
    let version = vike_buildinfo::version_line(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
    vike_utility::cli::Program::new(BIN, &version).try_parse_from(args.iter().cloned())
}

/// The process variables this service read until decision 0111, each with the argument that
/// replaced it. The `vike-config` table (`vike_config::REMOVED_ENV`) carries the same four for
/// every booting root.
pub const RETIRED_VARIABLES: [(&str, &str); 4] = [
    ("VIKE_STRATEGY_BUILDER_PORT", "pass --port N instead"),
    ("VIKE_STRATEGY_BUILDER_OUT_DIR", "pass --out-dir DIR instead"),
    ("VIKE_STRATEGY_BUILDER_RETAIN", "pass --retain N instead"),
    ("VIKE_STRATEGY_BUILDER_WORKSPACE_ROOT", "pass --workspace-root DIR instead"),
];

/// Refuse while a [`RETIRED_VARIABLES`] name is set — by NAME. ⚠ Refused HERE, not merely
/// ignored: this service boots nothing, so `vike_config::REMOVED_ENV`'s startup refusal never
/// reaches it, and a leftover `VIKE_STRATEGY_BUILDER_WORKSPACE_ROOT` beside a missing
/// `--workspace-root` would otherwise fail with a message about the flag while the operator stares
/// at the variable. A blank value is unset.
///
/// # Errors
/// The refusal text, naming every set variable and its replacement.
pub fn refuse_retired_variables(vars: &HashMap<String, String>) -> Result<(), String> {
    let set: Vec<String> = RETIRED_VARIABLES
        .iter()
        .filter(|(var, _)| vars.get(*var).is_some_and(|v| !v.trim().is_empty()))
        .map(|(var, instead)| {
            format!("  {var} is set, but this service no longer reads it: {instead}")
        })
        .collect();
    if set.is_empty() {
        return Ok(());
    }
    Err(format!(
        "decision 0111 retired these process variables, and a set one would be ignored in \
         silence:\n{}\nMove each to the unit's ExecStart= line and unset it.",
        set.join("\n")
    ))
}

/// How many artifacts (and, apart, how many recorded failures) [`prune`] keeps in the current build
/// folder when nothing else is configured — the same SHAPE as `vike_log::DEFAULT_MAX_LOG_FILES`, a
/// named retention constant rather than an unbounded default.
///
/// ⚠ **Its unit changed with plan 2b (every strategy is a file).** It was the count kept PER
/// STRATEGY NAME, when an artifact was `<name>-<sha>.so`. An artifact is `<folder>/<sha>.so` now:
/// two identical files share one, so a per-name count would delete an artifact another file still
/// uses. So the count is over the folder, most recently used first (2026-11-17: it was newest BUILT
/// first, which evicted an artifact in daily use for one built after it), and a folder of other
/// sources goes whole. An artifact this count evicts costs its next request a
/// build of its own crate in the warm shared target, not a cold one. Every deployed unit passes
/// [`RETAIN_FLAG`] itself, sized for every file a box runs.
pub const DEFAULT_RETAIN: usize = 5;

/// The scope [`Request::Build`] requires — and, because this protocol has exactly one verb, the
/// CEILING this domain will ever grant a key for.
///
/// **Why [`Scope::Write`] and not something weaker.** Compare
/// `crates/vike-backtesthub/src/backtesthub_server.rs`'s `required_scope`, which classifies
/// `StrategySpecWire::Rhai`-carrying verbs as `Scope::Write` because compiling client-supplied source is
/// "remote code execution BY DESIGN". This service's one verb does not merely REACH a compiler
/// through Rhai's sandboxed, API-bounded interpreter — it hands `cargo` the source file directly.
/// `cargo build` runs arbitrary `build.rs` and proc-macro code as an ordinary part of compiling,
/// with none of Rhai's boundary, and the artifact it produces is native code the backtest server
/// later `dlopen`s and calls into unsandboxed
/// (docs/superpowers/specs/2026-09-21-runtime-loaded-rust-strategies-design.md: *"It does not
/// sandbox the user's Rust… it can open a socket"*). A verb that reaches an interpreter earns
/// `Scope::Write`; a verb that reaches an unrestricted native compiler and hands back
/// dlopen-able machine code cannot rationally earn less. There is deliberately no
/// [`Scope::Read`] verb in this protocol at all — a connection either proves `Write` or is
/// refused before the handshake completes, because there is no read-only action this service
/// could perform that would be worth a separate, weaker key.
///
/// [`Scope::Account`] is not a stronger grant than `Write` and is not a candidate either way — it
/// names KEY-MATERIAL verbs specifically (`docs/decisions/0065`), and this service holds no
/// credentials for another domain to leak, so the concept does not apply to it.
pub fn required_scope() -> Scope {
    Scope::Write
}

/// Verify a presented `Auth` frame: the scope must be [`required_scope`] (the only scope this
/// service ever grants a key for), the mac must verify under THIS service's own [`DOMAIN`] and
/// the connection's nonce, and — since the key readers never fill the read slot — a
/// `Scope::Read` or `Scope::Account` presentation is refused before `key_for` is even consulted.
pub fn verify_auth(
    keys: &HubKeys,
    nonce: &[u8; 32],
    proto_version: u32,
    scope: Scope,
    mac: &[u8],
) -> bool {
    scope == required_scope() && keys.verify(DOMAIN, scope, nonce, proto_version, mac)
}

/// The port this service listens on when [`PORT_FLAG`] is absent — arbitrarily the
/// next free number after `vike-backtest`'s compute port (7880) and `vike-tradehub`'s control port
/// (7879), on the same "one small integer per localhost service" convention neither of those two
/// units' own comments treat as anything more than a default to restate.
///
/// ⚠ Declared HERE rather than in `src/bin/vike-strategy-builder.rs`, where it used to live,
/// because [`crate::client`] needs the same number to build its own default address: two spellings
/// of one port is exactly the shape that leaves a client dialling a service that moved. The binary
/// reads it from this constant now, and `default_address` derives the whole address from
/// [`bind_address`], so there is one authority for the address on both ends of the socket.
pub const DEFAULT_PORT: u16 = 7881;

/// Always `127.0.0.1:port` — the IP half is not a parameter and cannot be made one from outside
/// this function. This service compiles and runs whatever an authenticated caller sends it, so a
/// public bind is not a configuration this process can be talked into; only the PORT varies (a
/// real deployment knob, e.g. to avoid colliding with `vike-tradehub`'s control port or
/// `vike-backtest`'s compute port on one box).
pub fn bind_address(port: u16) -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], port))
}

/// The request half of this service's wire protocol.
///
/// ⚠ **`pub`, and that is a decision rather than a relaxation.** These two enums used to be
/// private, which meant nothing outside this file could ASK this service for a build — the
/// mechanism was reachable only by a peer that re-declared the schema, and re-declaring a wire
/// schema is how two halves of one protocol drift. [`crate::client`] is the one consumer, it lives
/// in this crate deliberately (see its module doc), and it speaks THESE types rather than a copy.
#[derive(Debug, Serialize, Deserialize)]
pub enum Request {
    Hello {
        proto_version: u32,
    },
    Auth {
        scope: Scope,
        mac: Vec<u8>,
    },
    /// `toolchain_fp` rides the wire today but is not yet CHECKED against anything. The
    /// toolchain-fingerprint scheme is Task 4's (`vike_rust_strategy::fingerprint`, landed on
    /// this branch, and this crate DOES now depend on that sibling crate — see `src/render.rs`'s
    /// module doc for why: it reaches `vike-rust-strategy`'s `TEMPLATE_CARGO_TOML`/
    /// `TEMPLATE_LIB_RS` constants over an ordinary Cargo dependency edge). Wiring an actual
    /// comparison against `fingerprint::FINGERPRINT` here is still real additional surface (a new
    /// refusal branch, its own test) that the crate SPLIT this module rode in on does not ask for;
    /// inventing it now would be the same "do not invent another track's contract" mistake the
    /// cdylib template avoids for `guest::HostBroker`. The field is accepted and threaded through
    /// so the wire shape does not have to change again once that check lands.
    Build {
        /// The file's path under the project's `strategies/` folder (`trend/momentum.rs`): its
        /// identity in refusals. The builder never reads that folder; the caller sends the text.
        path: String,
        /// The file's text, as the caller read it.
        text: String,
        /// The SDK to build against, over the text's own `[strategy] sdk` pin and the newest SDK
        /// (decision 0296): a backtest profile's `[strategy] sdk`, a Studio's pick. `None` builds as the
        /// text says. A field of `PROTO_VERSION` 4, positional like every frame past the `Welcome`.
        sdk: Option<u32>,
        toolchain_fp: String,
    },
    /// Build a research STUDY file (`render::build_study_file`): its folder's name and its `.rs`
    /// files as `(file name, text)`, read by the caller. Answered with `BuildOk`, its sha the one the
    /// artifact is named by (`render::study_sha`).
    BuildStudy {
        name: String,
        files: Vec<(String, String)>,
        toolchain_fp: String,
    },
}

/// The response half — `pub` for [`Request`]'s reason exactly.
#[derive(Debug, Serialize, Deserialize)]
pub enum Response {
    Welcome {
        proto_version: u32,
        nonce: [u8; 32],
    },
    AuthDenied {
        reason: String,
    },
    AuthOk,
    /// The artifact, `<builds>/sdk-N/<folder>/<sha>.so` (`vike_rust_strategy::artifact::artifact_path`):
    /// the sha256 of the text (a study's, `render::study_sha`), the numbered SDK it was built against
    /// (`None`: SDK dev, a checkout's) and its build folder.
    BuildOk {
        sha: String,
        sdk: Option<u32>,
        folder: String,
    },
    /// The refused Build: its id (`render::BuildError`'s, a compile's `strategy.build.compile_failed`) and its
    /// bare words, rustc's diagnostics verbatim for a compile (`PROTO_VERSION` 5).
    BuildErr(HubRefusal),
}

/// A refused Build's frame: the refusal's id and its bare words.
impl From<render::BuildError> for HubRefusal {
    fn from(refusal: render::BuildError) -> Self {
        HubRefusal::new(refusal.id().word(), &refusal)
    }
}

/// What this service builds against: ONE source tree (`--workspace-root`: a checkout, SDK dev, or one
/// installed SDK), or the installed SDKs (`--sdk-root`), among which each build resolves its own
/// (`render::InstalledSdks::resolve`).
#[derive(Debug, Clone)]
pub enum BuildSources {
    Tree(PathBuf),
    Installed(render::InstalledSdks),
}

impl BuildSources {
    /// The directory every build may read (the jail's grant): the tree, or the root of every SDK.
    pub fn root(&self) -> &Path {
        match self {
            BuildSources::Tree(root) => root,
            BuildSources::Installed(sdks) => sdks.root(),
        }
    }

    /// The trees a build may compile against now, what the start prefetches and prunes: the one tree,
    /// or every usable SDK's snapshot, newest first.
    pub fn trees(&self) -> Vec<PathBuf> {
        match self {
            BuildSources::Tree(root) => vec![root.clone()],
            BuildSources::Installed(sdks) => {
                sdks.usable_snapshots().into_iter().map(|(_, dir)| dir).collect()
            }
        }
    }

    /// The tree a build compiles against: `chosen`, an explicit override (`Request::Build`'s `sdk`: a
    /// backtest profile's `[strategy] sdk`, a Studio's pick), else for a strategy file its `text`'s
    /// `[strategy] sdk` pin (`render::pin_of`), else the newest usable SDK (a study names none).
    ///
    /// One tree (`--workspace-root`) builds an SDK the file or the run names only when it IS that SDK:
    /// another SDK's snapshot is refused in words naming both, never built as though it were the one
    /// asked for, and so is a checkout for an override. A checkout ("SDK dev") ignores a file's pin, said
    /// on stderr (the render module's doc): a development builder builds its own tree.
    ///
    /// # Errors
    /// The resolution's refusal (`render::BuildError::Sdk`).
    pub fn tree_for(
        &self,
        text: Option<&str>,
        chosen: Option<u32>,
    ) -> Result<PathBuf, render::BuildError> {
        match self {
            BuildSources::Tree(root) => {
                let named = match (chosen, text.and_then(render::pin_of)) {
                    (Some(number), _) => Some((number, "this run asks for")),
                    (None, Some(number)) => Some((number, "this file pins (`[strategy] sdk`)")),
                    (None, None) => None,
                };
                if let Some((number, who)) = named {
                    let sdk = render::sdk_of(root)?;
                    let pin_on_a_checkout = sdk == Sdk::Dev && chosen.is_none();
                    if sdk != Sdk::Number(number) && !pin_on_a_checkout {
                        return Err(render::BuildError::Sdk {
                            why: format!(
                                "{who} SDK {number}, and this strategy builder builds one tree, {sdk} at \
                             {}: start it on the installed SDKs (`--sdk-root \
                             <project>/vendor/vike-strategy-source`) to build another",
                                root.display()
                            ),
                        });
                    }
                }
                Ok(root.clone())
            }
            BuildSources::Installed(sdks) => sdks.resolve(chosen, text.and_then(render::pin_of)),
        }
    }
}

/// The SDK and the build folder an artifact path names (`<builds>/sdk-N/<folder>/<sha>.so`): what
/// `Response::BuildOk` says beside the sha. `None` for a path of another shape.
#[must_use]
pub fn sdk_and_folder(artifact: &Path) -> Option<(Option<u32>, String)> {
    let folder_dir = artifact.parent()?;
    let folder = folder_dir.file_name()?.to_str()?.to_string();
    let sdk = Sdk::of_directory_name(folder_dir.parent()?.file_name()?.to_str()?)?;
    Some((
        match sdk {
            Sdk::Number(number) => Some(number),
            Sdk::Dev => None,
        },
        folder,
    ))
}

/// Accept connections forever on `listener`, each on its own thread, authenticating every one
/// under `keys` before answering a single [`Request::Build`]. `out_dir` is where finished
/// artifacts land (and where [`prune`] runs after each build); `retain` is [`prune`]'s `keep`.
/// `sources` is what a build compiles against ([`BuildSources`]: the source tree the rendered crate's
/// `path` dependencies resolve against, one per build), and `cargo_bin` is the `cargo` executable it
/// invokes — see `render.rs`'s module doc for why BOTH are caller-supplied parameters ([`run`] takes
/// [`WORKSPACE_ROOT_FLAG`] or [`SDK_ROOT_FLAG`] and the process's own `CARGO` variable, refusing to
/// start without the former) rather than a path either resolves for itself. `child_env` is the whole
/// environment every `cargo` it spawns gets — `render::child_env` over the same map, computed once in
/// [`run`].
pub fn serve(
    listener: TcpListener,
    keys: HubKeys,
    out_dir: PathBuf,
    retain: usize,
    sources: BuildSources,
    cargo_bin: String,
    child_env: Vec<(String, String)>,
    cargo_home_pin: render::CargoHomePin,
) -> io::Result<()> {
    let keys = Arc::new(keys);
    let out_dir = Arc::new(out_dir);
    let sources = Arc::new(sources);
    let cargo_bin = Arc::new(cargo_bin);
    let child_env = Arc::new(child_env);
    let cargo_home_pin = Arc::new(cargo_home_pin);
    for incoming in listener.incoming() {
        match incoming {
            Ok(stream) => {
                let keys = Arc::clone(&keys);
                let out_dir = Arc::clone(&out_dir);
                let sources = Arc::clone(&sources);
                let cargo_bin = Arc::clone(&cargo_bin);
                let child_env = Arc::clone(&child_env);
                let cargo_home_pin = Arc::clone(&cargo_home_pin);
                thread::Builder::new().name("stb-conn".into()).spawn(move || {
                    handle_connection(
                        stream,
                        &keys,
                        &out_dir,
                        retain,
                        &sources,
                        &cargo_bin,
                        &child_env,
                        &cargo_home_pin,
                    )
                })?;
            }
            Err(e) => {
                // ⚠ `eprintln!`, NOT `tracing::warn!`, and the change is a correction rather than
                // a style preference — see this module's header for the whole argument. The short
                // version: `src/bin/vike-strategy-builder.rs` installs no subscriber, so every
                // `tracing` event this crate emitted went NOWHERE. stderr is what this binary
                // already uses for every other operator-facing line it prints (the refusals, the
                // listening banner, the serve exit), and systemd captures it into the journal.
                eprintln!("vike-strategy-builder: accept failed, continuing: {e}");
            }
        }
    }
    Ok(())
}

/// The whole startup sequence over a caller-supplied environment map and command line, from
/// refusal through the (never-returning, on success) serve loop — the key, the four arguments
/// ([`read_args`]) and `CARGO`, in ONE place.
///
/// ⚠ **This used to be split across the two composition roots that need it, and that was the
/// defect this function exists to close.** `src/bin/vike-strategy-builder.rs`'s `main` read its
/// settings and `CARGO` directly
/// (`Layer::Binary`, five rows in `vike_config::settings::SETTINGS`) — correct while that binary was
/// the only composition root. The multicall join (`vike-backend strategy-builder`) gave this
/// service a SECOND one, and the alternative to this function was a second copy of every refusal
/// message and every default in `crates/vike-backend/src/main.rs` — exactly the "two implementations of
/// one exchange" shape this workspace's conventions refuse on sight. So the whole sequence moved
/// here instead: a pure parser over a caller-supplied map (`Layer::Injected`), with the two
/// composition roots reduced to "sweep the real
/// process environment once, hand the map to this function, return what it returns" — the binary's
/// own `main` and `vike-backend`'s `strategy_builder_main` row are that call, nothing more. The
/// five `SETTINGS` rows moved to `Layer::Injected` with this function as their one reader.
///
/// Binding the real socket and creating `out_dir` still happen HERE rather than being handed back
/// as more parsed config: both are the same kind of one-shot process action `serve`'s own call
/// already is, and splitting them out would just move the duplication one line up instead of
/// removing it.
///
/// The key is read from the settings database `vars` locates ([`resolve_keys`]) — the only place it
/// lives. A box whose store holds no such row refuses to start NAMING the row and the one command
/// that sets it ([`KeyRefusal`]'s `Display`).
///
/// `reentry` is what re-enters THIS service on this program's command line, before its own
/// arguments: nothing for the standalone binary, `["strategy-builder"]` for the multicall
/// dispatcher. The jail probe (`vike_sandbox::probe_in_child`) runs this program again with it.
pub fn run(vars: &HashMap<String, String>, args: &[String], reentry: &[&str]) -> ExitCode {
    // ⚠ A command line that does not parse NEVER starts the service: `--help`, `--version` and every
    // refusal are printed and the process exits (a refusal is status 2).
    let args = match read_args(args) {
        Ok(a) => a,
        Err(exit) => exit.emit(),
    };
    // The probe child (`vike_sandbox::probe_in_child`): answer and exit, before the key or anything
    // else.
    if args.jail_probe {
        return match vike_sandbox::probe() {
            Ok(()) => ExitCode::SUCCESS,
            Err(why) => {
                eprintln!("{why}");
                ExitCode::from(3)
            }
        };
    }
    if let Err(refusal) = refuse_retired_variables(vars) {
        eprintln!("vike-strategy-builder: refusing to start — {refusal}");
        return ExitCode::FAILURE;
    }
    // The deploy's prebuild serves nothing, so it reads no key (`crate::prebuild`).
    let prebuilding = args.prebuild_mounts.is_some();
    let keys = match if prebuilding { Ok(None) } else { resolve_keys(vars).map(Some) } {
        Ok(k) => k,
        Err(refusal) => {
            eprintln!(
                "vike-strategy-builder: refusing to start — {}\n\
                 This service compiles and RUNS whatever source an authenticated caller sends it \
                 (see this module's doc, Decision 2). Starting with no key configured would mean \
                 serving that to anyone who can reach the socket, which is a materially worse \
                 failure mode than refusing to start.",
                refusal.rendered()
            );
            return ExitCode::FAILURE;
        }
    };

    let BuilderArgs {
        port,
        out_dir,
        retain,
        workspace_root,
        sdk_root,
        jail_probe: _,
        prebuild_mounts,
    } = args;

    // Required, refused the same way as a missing key: `render::build_strategy_file`'s module doc argues
    // why a compile-time-baked fallback here would be exactly the defect
    // `crates/vike-ops/tests/hygiene/compile_time_path_gate.rs` exists to catch, and why — unlike a data
    // path — there is no ladder to fall through to instead. Exactly one of the two: a builder serves one
    // tree, or the installed SDKs.
    let (root, root_flag) = match (workspace_root, sdk_root) {
        (Some(root), None) => (root, WORKSPACE_ROOT_FLAG),
        (None, Some(root)) => (root, SDK_ROOT_FLAG),
        (Some(_), Some(_)) => {
            eprintln!(
                "vike-strategy-builder: refusing to start — {}",
                StartRefusal::BothSourceRoots.rendered()
            );
            return ExitCode::FAILURE;
        }
        (None, None) => {
            eprintln!(
                "vike-strategy-builder: refusing to start — {}",
                StartRefusal::NoSourceRoot.rendered()
            );
            return ExitCode::FAILURE;
        }
    };

    // ⚠ Refuse loud rather than build silently against the venue credential store. The unit's own
    // header names a hand-maintained checkout as "a reasonable choice on a box that already keeps
    // one for other reasons" — and an operator who points this variable at THAT root (or at any
    // other tree carrying a real `settings/`) hands every `build.rs` the jail was built to hide
    // read-only access to `settings/db/vike.db` and everything else under it, because the jail
    // binds back exactly what `WORKSPACE_ROOT_FLAG` names. A legitimate strategy-source tree — the
    // release asset `fetch_release_tools.sh strategy-source` extracts, or the derived subtree a
    // hand-maintained checkout can point at instead — never has a `settings/` of its own, so this
    // can only fire on a misconfiguration, never on ordinary use.
    if root.join("settings").is_dir() {
        let refusal = StartRefusal::SourceRootHoldsSettings {
            flag: root_flag,
            root: root.display().to_string(),
        };
        eprintln!("vike-strategy-builder: refusing to start — {}", refusal.rendered());
        return ExitCode::FAILURE;
    }

    // Before the listener binds (decision 0296; it replaced the `SOURCE_GIT_SHA` stamp's two checks): one
    // tree must be a git checkout or an installed SDK its marker describes (`render::source_tree_refusal`),
    // and every installed SDK is checked against its marker once (`render::InstalledSdks::open`) — one that
    // fails is never built against, said here, and the rest are served.
    let sources = if root_flag == WORKSPACE_ROOT_FLAG {
        if let Some(why) = render::source_tree_refusal(&root) {
            let refusal = render::BuildError::Sdk { why };
            eprintln!("vike-strategy-builder: refusing to start — {}", refusal.rendered());
            return ExitCode::FAILURE;
        }
        BuildSources::Tree(root)
    } else {
        match render::InstalledSdks::open(&root) {
            Ok(sdks) => {
                for line in sdks.report() {
                    eprintln!("vike-strategy-builder: {line}");
                }
                BuildSources::Installed(sdks)
            }
            Err(why) => {
                eprintln!("vike-strategy-builder: refusing to start — {why}");
                return ExitCode::FAILURE;
            }
        }
    };

    // The `cargo` binary `render::build_strategy_file` invokes. Cargo's own `CARGO` variable when this
    // process was itself launched by cargo (a lane, `cargo run`); `"cargo"` off `PATH` otherwise,
    // the ordinary case for a deployed unit — or for the multicall dispatcher, which reads no
    // `CARGO` of its own either.
    let cargo_bin: String = vars.get("CARGO").cloned().unwrap_or_else(|| "cargo".to_string());

    // Everything `cargo` — and every `build.rs` it runs — will see, decided ONCE, from this map
    // alone. `render::CHILD_ENV_ALLOWLIST` carries why each name is on it.
    let child_env = render::child_env(vars);

    // Captured HERE, before the listener even binds — so only the operator's own pre-existing
    // `config.toml` (or its deliberate absence) can ever become the pin. See `CargoHomePin`'s doc
    // for what it closes and why an absent file is a legitimate baseline, not a special case.
    let cargo_home_pin = child_env
        .iter()
        .find(|(k, _)| k == "CARGO_HOME")
        .map(|(_, v)| render::CargoHomePin::capture(Path::new(v)))
        .unwrap_or_else(render::CargoHomePin::disabled);

    if let Err(e) = std::fs::create_dir_all(&out_dir) {
        eprintln!("vike-strategy-builder: cannot create {}: {e}", out_dir.display());
        return ExitCode::FAILURE;
    }
    // The deploy's prebuild (`crate::prebuild`): the mounted files read BEFORE the jail, which hides the
    // settings database and the strategies folder, then built inside it. No prune and no listener: the old
    // daemons still run on what they mapped, and this process serves nothing. It exits 0 whatever each file did.
    if let Some(strategies) = prebuild_mounts {
        let files = vike_secrets::open_credential_store(vars)
            .map_err(|e| e.to_string())
            .and_then(|store| crate::prebuild::mounted_files(&store, &strategies));
        let files = match files {
            Ok(files) if files.is_empty() => {
                eprintln!("vike-strategy-builder: prebuild: no mounted .rs strategy file to build");
                return ExitCode::SUCCESS;
            }
            Ok(files) => files,
            Err(why) => {
                eprintln!(
                    "vike-strategy-builder: prebuild: nothing built, the mounts could not be read: {why}"
                );
                return ExitCode::SUCCESS;
            }
        };
        match enter_jail(vars, &sources, &out_dir, &cargo_bin, &child_env, reentry) {
            Ok(line) => eprintln!("vike-strategy-builder: {line}"),
            Err(refusal) => {
                eprintln!("vike-strategy-builder: prebuild: nothing built — {refusal}");
                return ExitCode::SUCCESS;
            }
        }
        crate::prebuild::build_all(
            &files,
            &sources,
            &out_dir,
            &cargo_bin,
            &child_env,
            &cargo_home_pin,
        );
        return ExitCode::SUCCESS;
    }
    let Some(keys) = keys else { return ExitCode::FAILURE };
    // A build folder of other sources goes at start, not only after the next Build: in every SDK served.
    for tree in sources.trees() {
        prune_tree(&out_dir, &tree, retain);
    }

    let addr = bind_address(port);
    let listener = match TcpListener::bind(addr) {
        Ok(l) => l,
        Err(e) => {
            eprintln!(
                "vike-strategy-builder: cannot bind {addr} (loopback only, by construction): {e}"
            );
            return ExitCode::FAILURE;
        }
    };
    // ⚠ THE ADDRESS THE SOCKET ACTUALLY HOLDS, not the one requested. They differ exactly when the
    // port is `0` — which is what `deploy/docker/vikectl`'s pre-flight probe passes,
    // and what `crates/vike-strategy-builder/tests/refusal_reaches_stderr.rs` passes so it can
    // find this daemon without releasing a probed port first (the bind-then-release race sibling
    // daemon tests in this tree have paid for). The requested address is the fallback only if the
    // OS cannot say.
    let bound = listener.local_addr().unwrap_or(addr);
    eprintln!(
        "vike-strategy-builder: listening on {bound}, artifacts under {}, retaining {retain} in the build folder",
        out_dir.display()
    );

    // ⚠ THE JAIL (decision 0273), entered ONCE, after the key was read (above, the one read of the
    // settings database) and the port bound, before the first connection is served — every thread
    // and child this process starts from here inherits it. See `enter_jail`.
    match enter_jail(vars, &sources, &out_dir, &cargo_bin, &child_env, reentry) {
        Ok(line) => eprintln!("vike-strategy-builder: {line}"),
        Err(refusal) => {
            eprintln!("vike-strategy-builder: refusing to start — {refusal}");
            return ExitCode::FAILURE;
        }
    }

    if let Err(e) =
        serve(listener, keys, out_dir, retain, sources, cargo_bin, child_env, cargo_home_pin)
    {
        eprintln!("vike-strategy-builder: serve exited: {e}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

/// Jail THIS process's serving thread (decision 0273, step 1), and say how: `Ok` carries the line
/// [`run`] prints, `Err` a refusal to start.
///
/// **How the key survives the jail.** [`resolve_keys`] has already read it into memory, before
/// this runs; the jail then hides the settings database from this process for good, so nothing it
/// serves — a build (the cache check reads an artifact's record and runs none of its code:
/// `crate::jail`'s module doc) — can read the store again, and nothing can send the in-memory key
/// anywhere: the jail opens no socket and
/// refuses every TCP bind and connect (the listener was bound above; `accept` is not a new socket).
///
/// In order: [`vike_sandbox::probe_in_child`] — this program re-entered (`reentry`) as a child, so
/// a unit whose syscall filter KILLS on a Landlock call (one written before `@sandbox`) kills only the
/// child; where the probe fails the builder STILL serves (a deploy's pre-flight and health check see
/// it listening) and `render::build_strategy_file` refuses every Build with the same reason, never an
/// unjailed build, and this process then makes no Landlock call at all; the grants (artifact
/// directory, scratch root, cargo home to write); a check that no grant shows the settings directory
/// (a misconfiguration: refuse to start, as for a `--workspace-root` holding `settings/`);
/// [`render::prefetch`] of every tree `sources` serves, the one moment this process uses the network;
/// then the jail.
fn enter_jail(
    vars: &HashMap<String, String>,
    sources: &BuildSources,
    out_dir: &Path,
    cargo_bin: &str,
    child_env: &[(String, String)],
    reentry: &[&str],
) -> Result<String, String> {
    let refused = |why: String| {
        format!(
            "every Build is REFUSED on this box: {why}. Rust strategies are built only inside the \
             build jail, and there is no override (decision 0273); Rhai and the built-in \
             strategies still run."
        )
    };
    if let Err(why) = vike_sandbox::probe_in_child(reentry) {
        return Ok(refused(why));
    }
    let scratch = render::scratch_root();
    std::fs::create_dir_all(&scratch)
        .map_err(|e| format!("cannot create the scratch root {}: {e}", scratch.display()))?;
    let workspace_root = sources.root();
    let grants = match jail::grants(workspace_root, &[out_dir, &scratch], cargo_bin, child_env) {
        Ok(g) => g,
        Err(why) => return Ok(refused(why)),
    };
    // The same location `resolve_keys` read the key from (opening performs no I/O).
    let settings = vike_secrets::open_credential_store(vars)
        .ok()
        .and_then(|s| s.location().map(|l| l.settings_dir().to_path_buf()));
    if let Some((settings, grant)) =
        settings.as_deref().and_then(|s| jail::grant_covering(&grants, s).map(|g| (s, g)))
    {
        return Err(format!(
            "the build jail would show the settings directory {} through its grant of {} — the \
             artifact directory, the cargo home, the toolchain and the source root must each \
             lie outside it and above none of it",
            settings.display(),
            grant.display()
        ));
    }
    let fetched =
        sources.trees().iter().try_for_each(|tree| render::prefetch(tree, cargo_bin, child_env));
    let fetched = match fetched {
        Ok(()) => "the templates' crates are in the cargo home".to_string(),
        Err(e) => format!(
            "fetching the template's crates FAILED, so a build finds only what the cargo home \
             already holds: {}",
            e.replace(['\r', '\n'], " ")
        ),
    };
    vike_sandbox::restrict_current_thread(&grants)
        .map_err(|e| format!("the build jail failed: {e}"))?;
    Ok(format!(
        "jailed (Landlock and a socket filter): reads only the toolchain, {} and system files; \
         writes only {}, {} and the cargo home; no network. {fetched}.",
        workspace_root.display(),
        out_dir.display(),
        scratch.display()
    ))
}

/// One connection's whole lifetime: `Hello` -> `Welcome{nonce}` -> `Auth` -> `AuthOk`, then a
/// loop of `Build` requests until the peer disconnects or sends anything else.
fn handle_connection(
    mut stream: TcpStream,
    keys: &HubKeys,
    out_dir: &Path,
    retain: usize,
    sources: &BuildSources,
    cargo_bin: &str,
    child_env: &[(String, String)],
    cargo_home_pin: &render::CargoHomePin,
) {
    // Nagle OFF before the first frame, as on every node socket
    // (`vike_hub_protocol::frame::configure_hub_stream`). A refusal is DROPPED rather than printed:
    // it costs a build's reply some latency and nothing else, and this module's stderr lines are
    // the few its own doc names — an operator does not need one per connection for this.
    let _ = vike_hub_protocol::frame::configure_hub_stream(&stream);
    let body = match read_frame_raw_capped(&mut stream, SERVER_HANDSHAKE_MAX_FRAME_LEN) {
        Ok(b) => b,
        Err(_) => return,
    };
    match serde_json::from_slice::<Request>(&body) {
        Ok(Request::Hello { .. }) => {}
        _ => {
            let _ = write_frame(
                &mut stream,
                &Response::AuthDenied { reason: "not authenticated: expected Hello first".into() },
            );
            return;
        }
    }

    let nonce = auth::fresh_nonce();
    if write_frame(&mut stream, &Response::Welcome { proto_version: PROTO_VERSION, nonce }).is_err()
    {
        return;
    }

    // Past the `Welcome` both ends speak this version, so every frame from here is msgpack
    // ([`write_message`]); a peer of another version refused itself on the `Welcome`.
    let body = match read_frame_raw_capped(&mut stream, SERVER_HANDSHAKE_MAX_FRAME_LEN) {
        Ok(b) => b,
        Err(_) => return,
    };
    match rmp_serde::from_slice::<Request>(&body) {
        Ok(Request::Auth { scope, mac }) => {
            if !verify_auth(keys, &nonce, PROTO_VERSION, scope, &mac) {
                let _ =
                    write_message(&mut stream, &Response::AuthDenied { reason: "bad mac".into() });
                return;
            }
        }
        _ => {
            let _ = write_message(
                &mut stream,
                &Response::AuthDenied { reason: "expected Auth (undecodable request)".into() },
            );
            return;
        }
    }
    if write_message(&mut stream, &Response::AuthOk).is_err() {
        return;
    }

    loop {
        let req: Request = match read_message(&mut stream) {
            Ok(r) => r,
            Err(_) => return,
        };
        // The deployed backtest server is a RELEASE build, so a release artifact is the only one whose
        // toolchain fingerprint that host accepts. See `render::Profile` for why this is a
        // correctness parameter rather than a tuning one.
        let profile = render::Profile::Release;
        // What a refusal names in the journal: the strategy file's path, or the study's name. Each
        // build compiles against the tree its SDK resolves to (`BuildSources::tree_for`).
        let (path, built) = match req {
            Request::Build { path, text, sdk, toolchain_fp: _ } => {
                let sha = vike_hub_protocol::hash::sha256_hex(text.as_bytes());
                let built = sources.tree_for(Some(&text), sdk).and_then(|tree| {
                    render::build_strategy_file(
                        &text,
                        &path,
                        out_dir,
                        &tree,
                        cargo_bin,
                        child_env,
                        profile,
                        cargo_home_pin,
                    )
                    .map(|artifact| (tree, artifact))
                });
                (path, built.map(|built| (sha.clone(), built)).map_err(|e| (sha, e)))
            }
            Request::BuildStudy { name, files, toolchain_fp: _ } => {
                let built = sources.tree_for(None, None).and_then(|tree| {
                    render::build_study_file(
                        &name,
                        &files,
                        out_dir,
                        &tree,
                        cargo_bin,
                        child_env,
                        profile,
                        cargo_home_pin,
                    )
                    .map(|(artifact, sha)| (sha, (tree, artifact)))
                });
                let sha = render::study_sha(&name, &files);
                (format!("study {name}"), built.map_err(|e| (sha, e)))
            }
            // Anything else after a completed handshake: these two verbs are all a peer could
            // legitimately send.
            _ => return,
        };
        let mut built_tree = None;
        let response = match built {
            Ok((sha, (tree, artifact))) => match sdk_and_folder(&artifact) {
                Some((sdk, folder)) => {
                    built_tree = Some(tree);
                    Response::BuildOk { sha, sdk, folder }
                }
                None => Response::BuildErr(
                    render::BuildError::ArtifactUnnamed {
                        artifact: artifact.display().to_string(),
                    }
                    .into(),
                ),
            },
            Err((source_sha, e)) => {
                // ⚠ SAID ON STDERR AS WELL AS ANSWERED, because the answer reaches ONE peer and the
                // journal reaches the operator. MEASURED on a deployed box: after a release moved
                // the binary and not its source tree, the builder's version check (retired by decision
                // 0296) refused every Build for two days while `journalctl -u vike-strategy-builder` showed nothing but
                // the listening banners — a daemon refusing every request read exactly like an
                // idle one. See [`build_refusal_line`] for what the line may and may not carry.
                eprintln!("{}", build_refusal_line(&path, &source_sha, &e));
                Response::BuildErr(e.into())
            }
        };
        if write_message(&mut stream, &response).is_err() {
            return;
        }
        // Retention inside the SDK this build compiled against, the one whose folders it can grow.
        if let Some(tree) = built_tree {
            prune_tree(out_dir, &tree, retain);
        }
    }
}

/// The ONE stderr line a refused `Build` produces — the journal's record of the refusal, beside
/// the `BuildErr` the peer receives.
///
/// ⚠ **WHAT IT MAY NOT CARRY, and why each variant is spelled out rather than `Display`ed.** A
/// user's strategy is their code, so the SOURCE never reaches this line — and
/// [`render::BuildError::Compile`] is exactly the variant whose text would smuggle it in: it is
/// rustc's diagnostics VERBATIM, and rustc quotes the offending source lines. So that variant logs
/// that a compile failed and says where its diagnostics went, never the diagnostics themselves. The
/// key never reaches this function at all. What IS here: the variant (a fixed word an operator can
/// grep for), the strategy file's PATH (escaped and capped — it is the caller's string, and a newline in
/// it must not forge a second journal line), the SOURCE SHA (the content address the artifact is
/// named by, which says which request this was without saying what it contained), and for every
/// other variant the reason as `render` composed it — for the version-skew refusal that is both
/// stamps and the tree's path, which is the whole of what an operator needs to act.
///
/// One LINE, by construction: any newline in a reason is flattened, because a journal reader
/// scanning for `REFUSED` must see the whole refusal on the line that matches.
fn build_refusal_line(path: &str, source_sha: &str, err: &render::BuildError) -> String {
    use render::BuildError;
    const NAME_CAP: usize = 64;
    let (kind, reason) = match err {
        BuildError::Sdk { why } => ("sdk", why.clone()),
        BuildError::UnwiredHook { hooks } => (
            "unwired-hook",
            format!(
                "the source overrides {} — a Strategy hook a Rust strategy built in Studio is \
                 never sent",
                hooks.0.join(", ")
            ),
        ),
        BuildError::Compile { .. } => (
            "compile-error",
            "rustc reported errors; its diagnostics went back to the caller and are NOT logged \
             here, because they quote the caller's source"
                .to_string(),
        ),
        BuildError::Io { why } => ("io", why.clone()),
        BuildError::Toolchain { why } => ("toolchain", why.clone()),
        BuildError::Unsandboxed { why } => ("unsandboxed", why.clone()),
        BuildError::InvalidPath { .. } => (
            "invalid-path",
            "the strategy file path must be relative, end in `.rs` and hold no `..`, `\\` or `:`"
                .to_string(),
        ),
        BuildError::InvalidStudy { why } => ("invalid-study", why.clone()),
        BuildError::TwoEntries => {
            ("two-entries", "the file exports both `build` and `rule`".to_string())
        }
        // Like a compile failure: a header parse error quotes the file's own text.
        BuildError::Header { .. } => (
            "header",
            "the rule file's header does not hold; the reason went back to the caller".to_string(),
        ),
        BuildError::CargoHomeTampered { was_present, now_present } => (
            "cargo-home-tampered",
            format!(
                "$CARGO_HOME/config.toml changed since this daemon started (was {was_present}, is now \
                 {now_present}) — refusing every Build until a restart re-pins it"
            ),
        ),
        BuildError::ArtifactUnnamed { .. } => ("artifact-unnamed", err.to_string()),
    };
    let shown_path: String = path.chars().take(NAME_CAP).collect();
    let cut = if path.chars().count() > NAME_CAP { "…" } else { "" };
    let reason = reason.replace(['\r', '\n'], " ");
    format!(
        "vike-strategy-builder: Build REFUSED [{kind}] strategy file {shown_path:?}{cut} (source sha256 \
         {sha}): {reason}",
        sha = &source_sha[..source_sha.len().min(16)],
    )
}

/// [`prune`] inside the SDK `tree` is, its current folders those of every template kind this service
/// builds into (release artifacts: the deployed hosts are RELEASE builds, `render::Profile`). A failure
/// is said on stderr and costs nothing else: a sweep must not fail a build.
fn prune_tree(out_dir: &Path, tree: &Path, retain: usize) {
    let folders: Result<Vec<(Sdk, String)>, render::BuildError> = TemplateKind::ALL
        .into_iter()
        .map(|kind| render::build_folder(tree, kind, render::Profile::Release))
        .collect();
    let pruned = folders.map_err(|e| e.to_string()).and_then(|folders| {
        let Some((sdk, _)) = folders.first() else { return Ok(()) };
        let current: Vec<String> = folders.iter().map(|(_, folder)| folder.clone()).collect();
        prune(out_dir, *sdk, &current, retain).map_err(|e| e.to_string())
    });
    if let Err(e) = pruned {
        // stderr, for the reason given at `serve`'s accept-failure site.
        eprintln!(
            "vike-strategy-builder: prune failed, continuing: {e} (dir {})",
            out_dir.display()
        );
    }
}

/// Retention over `dir`, the builds directory, PER SDK (decision 0296): inside `dir/sdk-N/` (`sdk`'s
/// folder) every build FOLDER other than the `current` ones goes (a change of the recipe or the
/// toolchain starts a new one; a host restarted on it asks for, and is served from, the new one), and
/// inside each `current` folder the `keep` artifacts (`.so`) and, apart, the `keep` recorded failures
/// (`.err`) USED most recently stay. A file's last use is the later of its access time, which every
/// cache hit stamps (`render`'s `stamp_use`), and its modification time, when it was built: so an
/// artifact built long ago and asked for every day stays, and one built last week and never asked for
/// again goes first. Ties break on the file name, descending, so the order never depends on `read_dir`'s.
///
/// A folder is an eight-lowercase-hex-digit directory name, the shape
/// `vike_rust_strategy::artifact::build_folder` writes; anything else (a `work-…` directory, the age
/// sweep's: `render::prune_stale_scratch`) is left alone, and so is every OTHER SDK's folder: an SDK this
/// builder no longer serves goes whole with its source, by the deploy's retention. A folder directly in
/// `dir` is the layout before SDKs, never served again, and goes. A running strategy process keeps the
/// artifact it opened even when its file goes (an unlinked file stays mapped); its next start asks the
/// builder again.
pub fn prune(dir: &Path, sdk: Sdk, current: &[String], keep: usize) -> io::Result<()> {
    let is_folder = |name: &str| {
        name.len() == 8 && name.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    };
    let remove_folders = |dir: &Path, kept: &[String]| -> io::Result<()> {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        for entry in entries {
            let entry = entry?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if is_folder(name) && !kept.iter().any(|k| k == name) && entry.file_type()?.is_dir() {
                std::fs::remove_dir_all(entry.path())?;
            }
        }
        Ok(())
    };
    remove_folders(dir, &[])?;
    let sdk_dir = dir.join(sdk.directory_name());
    remove_folders(&sdk_dir, current)?;
    for folder in current {
        prune_folder(&sdk_dir.join(folder), keep)?;
    }
    Ok(())
}

/// [`prune`] inside one current folder: the `keep` artifacts and, apart, the `keep` recorded failures
/// used most recently stay.
fn prune_folder(folder: &Path, keep: usize) -> io::Result<()> {
    for ext in ["so", "err"] {
        let mut files: Vec<(PathBuf, std::time::SystemTime)> = Vec::new();
        let listing = match std::fs::read_dir(folder) {
            Ok(l) => l,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        for entry in listing {
            let path = entry?.path();
            if path.extension().is_some_and(|e| e == ext) {
                let meta = std::fs::metadata(&path)?;
                let built = meta.modified()?;
                let last_use = meta.accessed().map_or(built, |used| used.max(built));
                files.push((path, last_use));
            }
        }
        files.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| b.0.cmp(&a.0)));
        for (path, _) in files.iter().skip(keep) {
            std::fs::remove_file(path)?;
        }
    }
    Ok(())
}

#[path = "builder_tests.rs"]
#[cfg(test)]
mod builder_tests;
