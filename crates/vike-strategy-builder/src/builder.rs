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
//! control channel) and `vike_hub_protocol::auth::DATAHUB_DOMAIN` (the data/compute
//! daemons) — a THIRD separator, not a reuse of either. `crates/vike-tradehub-client/src/auth.rs`
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
use vike_datahub_client::proto::{MAX_FRAME_LEN, read_frame_raw_capped, write_frame};
use vike_hub_protocol::auth::{self, Domain, HubKeys, Scope};
use vike_hub_protocol::limits::SERVER_HANDSHAKE_MAX_FRAME_LEN;

use crate::{jail, render};

/// This service's OWN domain separator — see this module's doc, Decision 1, for why it is
/// neither `vike_tradehub_client::auth::DOMAIN` nor
/// `vike_hub_protocol::auth::DATAHUB_DOMAIN`. NUL-terminated, following
/// [`Domain::new`]'s `b"vike-<service>-auth\0"` convention so no separator can be a prefix of
/// another.
pub const DOMAIN: Domain = Domain::new(b"vike-strategy-builder-auth\0");

/// This service's own wire-protocol version — independent of `vike_datahub_client::proto`'s or
/// `vike_tradehub_client::proto`'s, because this is a THIRD protocol, not a verb bolted onto
/// either existing one. 2 (plan 2b): a `Build` carries a strategy FILE, its path and its text.
pub const PROTO_VERSION: u32 = 2;

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
/// Two readers, one row: the SERVICE reads it from its own box's store ([`resolve_keys`]) and the
/// desktop's Studio from the PC's own ([`keys_from_credentials`], over the credential map the
/// desktop already loads). `vike-cli secrets set` writes it on either box ([`set_key_advice`]).
pub const BUILDER_KEY_NAME: &str = "VIKE_STRATEGY_BUILDER_KEY";

/// The ONE command that sets the key, and how its value travels — the sentence both the service's
/// refusal and Studio's say, so an operator is told the same thing wherever the key is missing.
/// The value goes in on stdin (or from a variable named by `--from-env`, for that one command),
/// NEVER on the command line (`docs/decisions/0036-credentials-are-read-only-from-the-cli-and-the-mcp-surface.md`).
#[must_use]
pub fn set_key_advice() -> String {
    format!(
        "Set it once on this machine with `vike-cli secrets set {BUILDER_KEY_NAME}` (the value on \
         stdin, or add `--from-env VAR` to take it from a variable you export for that one \
         command; never on the command line). The key lives in the settings database only: there \
         is no key file and no environment variable for it."
    )
}

/// Why [`resolve_keys`] produced no key. Its `Display` is the operator-facing reason — it names
/// the row, the store and the command that sets it, never key material.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyRefusal {
    /// The store holds no (non-blank) row of that name — or there is no store at all, which is the
    /// same answer an unconfigured box always gives. `store` says where the service looked.
    Missing { store: String },
    /// The store EXISTS and could not be read (or `VIKE_NO_LIVE_KEYS` is malformed). Not "no key":
    /// a permissions fault wearing the not-configured answer is the failure this variant exists to
    /// keep apart from [`KeyRefusal::Missing`].
    Unreadable(String),
}

impl fmt::Display for KeyRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KeyRefusal::Missing { store } => write!(
                f,
                "no strategy-builder key: {store} holds no `{BUILDER_KEY_NAME}` row. {}",
                set_key_advice()
            ),
            KeyRefusal::Unreadable(why) => write!(
                f,
                "the strategy-builder key could not be read — the credential store is there but \
                 would not open ({why}). That is a fault to fix, not an absent key; once the \
                 store opens, `{BUILDER_KEY_NAME}` is read from it."
            ),
        }
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
    let unreadable = |e: &dyn fmt::Display| KeyRefusal::Unreadable(e.to_string());
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

/// `--retain N` — how many artifacts [`prune`] keeps in the current build folder
/// ([`DEFAULT_RETAIN`] when absent).
pub const RETAIN_FLAG: &str = "--retain";

/// `--workspace-root DIR` — the checkout root a rendered strategy file's `Cargo.toml` resolves its `path`
/// dependencies against. REQUIRED: [`run`] refuses to start without it.
pub const WORKSPACE_ROOT_FLAG: &str = "--workspace-root";

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
    /// Artifacts kept in the current build folder.
    #[arg(long = "retain", value_name = "N", default_value_t = DEFAULT_RETAIN, value_parser = |raw: &str| number_value::<usize>(raw, RETAIN_FLAG, "count"))]
    pub retain: usize,
    /// The strategy-source tree a strategy file's Cargo.toml builds against. REQUIRED (`run` refuses to
    /// start without it), and refused if it holds a settings/ directory of its own.
    #[arg(long = "workspace-root", value_name = "DIR", value_parser = path_value)]
    pub workspace_root: Option<PathBuf>,
    /// Answer whether the build jail can be applied on this box, then exit (0 yes, 3 no). Hidden: the
    /// daemon re-enters itself with it (`vike_sandbox::probe_in_child`), so a unit's syscall filter
    /// that kills on a Landlock call kills that child, never the daemon.
    #[arg(long = "jail-probe", hide = true)]
    pub jail_probe: bool,
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
/// every booting root, and the deploy helper's retired-variable check reads that one.
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
/// uses. So the count is over the folder, newest built first, and a previous release's folder goes
/// whole. An artifact this count evicts costs its next request a
/// build of its own crate in the warm shared target, not a cold one. Every deployed unit passes
/// [`RETAIN_FLAG`] itself; a box that runs many Rust strategy files raises it there.
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

/// Build this service's [`HubKeys`] from a CREDENTIAL MAP — [`BUILDER_KEY_NAME`] alone. The CLIENT's
/// reader: the desktop's Studio hands it the map it already loads from the PC's own settings
/// database (`workspace_credentials`), and a pure map lookup (`Layer::Injected`) is all it does —
/// it opens no store and reads no variable. (The SERVICE reads the same row through
/// [`resolve_keys`].)
///
/// Only the WRITE slot is ever filled — there is no observe-scope key for this protocol at all
/// (Decision 2), so [`HubKeys::has`]`(Scope::Read)` is always `false` for a key built here, by
/// construction rather than by a check anyone has to remember.
pub fn keys_from_credentials(creds: &HashMap<String, String>) -> Option<HubKeys> {
    key_from_value(creds.get(BUILDER_KEY_NAME)?)
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
        toolchain_fp: String,
    },
}

/// The response half — `pub` for [`Request`]'s reason exactly.
#[derive(Debug, Serialize, Deserialize)]
pub enum Response {
    Welcome { proto_version: u32, nonce: [u8; 32] },
    AuthDenied { reason: String },
    AuthOk,
    BuildOk { sha: String },
    BuildErr { diagnostics: String },
}

/// Accept connections forever on `listener`, each on its own thread, authenticating every one
/// under `keys` before answering a single [`Request::Build`]. `out_dir` is where finished
/// artifacts land (and where [`prune`] runs after each build); `retain` is [`prune`]'s `keep`.
/// `workspace_root` is the checkout root `render::build_strategy_file`'s rendered scratch crate resolves
/// its `vike-model`/`vike-rust-strategy` `path` dependencies against, and `cargo_bin` is the
/// `cargo` executable it invokes — see `render.rs`'s module doc for why BOTH are caller-supplied
/// parameters ([`run`] takes [`WORKSPACE_ROOT_FLAG`] and the process's own `CARGO` variable,
/// refusing to start without the former) rather than a path
/// either resolves for itself. `child_env` is the whole environment every `cargo` it spawns gets —
/// `render::child_env` over the same map, computed once in [`run`].
pub fn serve(
    listener: TcpListener,
    keys: HubKeys,
    out_dir: PathBuf,
    retain: usize,
    workspace_root: PathBuf,
    cargo_bin: String,
    child_env: Vec<(String, String)>,
    cargo_home_pin: render::CargoHomePin,
) -> io::Result<()> {
    let keys = Arc::new(keys);
    let out_dir = Arc::new(out_dir);
    let workspace_root = Arc::new(workspace_root);
    let cargo_bin = Arc::new(cargo_bin);
    let child_env = Arc::new(child_env);
    let cargo_home_pin = Arc::new(cargo_home_pin);
    for incoming in listener.incoming() {
        match incoming {
            Ok(stream) => {
                let keys = Arc::clone(&keys);
                let out_dir = Arc::clone(&out_dir);
                let workspace_root = Arc::clone(&workspace_root);
                let cargo_bin = Arc::clone(&cargo_bin);
                let child_env = Arc::clone(&child_env);
                let cargo_home_pin = Arc::clone(&cargo_home_pin);
                thread::Builder::new().name("stb-conn".into()).spawn(move || {
                    handle_connection(
                        stream,
                        &keys,
                        &out_dir,
                        retain,
                        &workspace_root,
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
/// message and every default in `crates/vike/src/main.rs` — exactly the "two implementations of
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
    let keys = match resolve_keys(vars) {
        Ok(k) => k,
        Err(refusal) => {
            eprintln!(
                "vike-strategy-builder: refusing to start — {refusal}\n\
                 This service compiles and RUNS whatever source an authenticated caller sends it \
                 (see this module's doc, Decision 2). Starting with no key configured would mean \
                 serving that to anyone who can reach the socket, which is a materially worse \
                 failure mode than refusing to start."
            );
            return ExitCode::FAILURE;
        }
    };

    let BuilderArgs { port, out_dir, retain, workspace_root, jail_probe: _ } = args;

    // Required, refused the same way as a missing key: `render::build_strategy_file`'s module doc argues
    // why a compile-time-baked fallback here would be exactly the defect
    // `crates/vike-ops/tests/hygiene/compile_time_path_gate.rs` exists to catch, and why — unlike a data
    // path — there is no ladder to fall through to instead.
    let workspace_root: PathBuf = match workspace_root {
        Some(root) => root,
        None => {
            eprintln!(
                "vike-strategy-builder: refusing to start — {WORKSPACE_ROOT_FLAG} is not given.\n\
                 This is the checkout root holding crates/vike-model and \
                 crates/vike-rust-strategy that a rendered strategy file's Cargo.toml resolves its \
                 `path` dependencies against. There is no compile-time fallback: a binary built \
                 in one checkout and deployed elsewhere would silently point cargo at a tree that \
                 does not exist there, and a compiler has no data-path ladder to fall through to \
                 instead — it either finds real crates to build against or the compile fails."
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
    if workspace_root.join("settings").is_dir() {
        eprintln!(
            "vike-strategy-builder: refusing to start — {WORKSPACE_ROOT_FLAG} at\n\
             {workspace_root} names a tree that contains its own `settings/` directory.\n\
             That directory is where this project's venue credentials live, and the builder's \
             jail binds this variable's target back into every compile READ-ONLY — so pointing \
             it at a project root (rather than a strategy-source-only tree) would hand every \
             `build.rs` a path to the credential store. Point this at the release asset \
             `fetch_release_tools.sh strategy-source` extracts, or at a subtree of a \
             hand-maintained checkout that holds no `settings/` of its own.",
            workspace_root = workspace_root.display()
        );
        return ExitCode::FAILURE;
    }

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
    // A previous release's build folder goes at start, not only after the next Build.
    if let Err(e) = prune(&out_dir, &build_folder(), retain) {
        eprintln!(
            "vike-strategy-builder: prune failed, continuing: {e} (dir {})",
            out_dir.display()
        );
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
    // port is `0` — which is what `deploy/sbin/vike-trader-ci-deploy`'s pre-flight probe passes,
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
    match enter_jail(vars, &workspace_root, &out_dir, &cargo_bin, &child_env, reentry) {
        Ok(line) => eprintln!("vike-strategy-builder: {line}"),
        Err(refusal) => {
            eprintln!("vike-strategy-builder: refusing to start — {refusal}");
            return ExitCode::FAILURE;
        }
    }

    if let Err(e) =
        serve(listener, keys, out_dir, retain, workspace_root, cargo_bin, child_env, cargo_home_pin)
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
/// serves — a build, or an artifact's initializers during the cache check's `dlopen` — can read the
/// store again, and nothing can send the in-memory key anywhere: the jail opens no socket and
/// refuses every TCP bind and connect (the listener was bound above; `accept` is not a new socket).
///
/// In order: [`vike_sandbox::probe_in_child`] — this program re-entered (`reentry`) as a child, so
/// a unit whose syscall filter KILLS on a Landlock call (one written before `@sandbox`) kills only the
/// child; where the probe fails the builder STILL serves (a deploy's pre-flight and health check see
/// it listening) and `render::build_strategy_file` refuses every Build with the same reason, never an
/// unjailed build, and this process then makes no Landlock call at all; the grants (artifact
/// directory, scratch root, cargo home to write); a check that no grant shows the settings directory
/// (a misconfiguration: refuse to start, as for a `--workspace-root` holding `settings/`);
/// [`render::prefetch`], the one moment this process uses the network; then the jail.
fn enter_jail(
    vars: &HashMap<String, String>,
    workspace_root: &Path,
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
             artifact directory, the cargo home, the toolchain and {WORKSPACE_ROOT_FLAG} must each \
             lie outside it and above none of it",
            settings.display(),
            grant.display()
        ));
    }
    let fetched = match render::prefetch(workspace_root, cargo_bin, child_env) {
        Ok(()) => "the template's crates are in the cargo home".to_string(),
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
    workspace_root: &Path,
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

    let body = match read_frame_raw_capped(&mut stream, SERVER_HANDSHAKE_MAX_FRAME_LEN) {
        Ok(b) => b,
        Err(_) => return,
    };
    match serde_json::from_slice::<Request>(&body) {
        Ok(Request::Auth { scope, mac }) => {
            if !verify_auth(keys, &nonce, PROTO_VERSION, scope, &mac) {
                let _ =
                    write_frame(&mut stream, &Response::AuthDenied { reason: "bad mac".into() });
                return;
            }
        }
        _ => {
            let _ = write_frame(
                &mut stream,
                &Response::AuthDenied { reason: "expected Auth (undecodable request)".into() },
            );
            return;
        }
    }
    if write_frame(&mut stream, &Response::AuthOk).is_err() {
        return;
    }

    loop {
        let body = match read_frame_raw_capped(&mut stream, MAX_FRAME_LEN) {
            Ok(b) => b,
            Err(_) => return,
        };
        let req: Request = match serde_json::from_slice(&body) {
            Ok(r) => r,
            Err(_) => return,
        };
        let Request::Build { path, text, toolchain_fp: _ } = req else {
            // Anything but `Build` after a completed handshake: this protocol has one verb, so
            // there is nothing else a peer could legitimately send.
            return;
        };
        let source_sha = vike_hub_protocol::hash::sha256_hex(text.as_bytes());
        let response = match render::build_strategy_file(
            &text,
            &path,
            out_dir,
            workspace_root,
            cargo_bin,
            child_env,
            // The deployed backtest server is a RELEASE build, so a release artifact is the
            // only one whose toolchain fingerprint that host accepts. See `render::Profile`
            // for why this is a correctness parameter rather than a tuning one.
            render::Profile::Release,
            cargo_home_pin,
        ) {
            Ok(_artifact) => Response::BuildOk { sha: source_sha },
            Err(e) => {
                // ⚠ SAID ON STDERR AS WELL AS ANSWERED, because the answer reaches ONE peer and the
                // journal reaches the operator. MEASURED on a deployed box: after a release moved
                // the binary and not its source tree, `render::verify_source_version` refused every
                // Build for two days while `journalctl -u vike-strategy-builder` showed nothing but
                // the listening banners — a daemon refusing every request read exactly like an
                // idle one. See [`build_refusal_line`] for what the line may and may not carry.
                eprintln!("{}", build_refusal_line(&path, &source_sha, &e));
                Response::BuildErr { diagnostics: e.to_string() }
            }
        };
        if write_frame(&mut stream, &response).is_err() {
            return;
        }
        if let Err(e) = prune(out_dir, &build_folder(), retain) {
            // stderr, for the reason given at the accept-failure site above.
            eprintln!(
                "vike-strategy-builder: prune failed, continuing: {e} (dir {})",
                out_dir.display()
            );
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
        BuildError::SourceVersionMismatch(msg) => ("source-version-mismatch", msg.clone()),
        BuildError::UnwiredHook(hooks) => (
            "unwired-hook",
            format!(
                "the source overrides {} — a Strategy hook a Rust strategy built in Studio is \
                 never sent",
                hooks.join(", ")
            ),
        ),
        BuildError::Compile(_) => (
            "compile-error",
            "rustc reported errors; its diagnostics went back to the caller and are NOT logged \
             here, because they quote the caller's source"
                .to_string(),
        ),
        BuildError::Io(msg) => ("io", msg.clone()),
        BuildError::Toolchain(msg) => ("toolchain", msg.clone()),
        BuildError::Unsandboxed(why) => ("unsandboxed", why.clone()),
        BuildError::InvalidPath(_) => (
            "invalid-path",
            "the strategy file path must be relative, end in `.rs` and hold no `..`, `\\` or `:`"
                .to_string(),
        ),
        BuildError::TwoEntries => {
            ("two-entries", "the file exports both `build` and `rule`".to_string())
        }
        // Like a compile failure: a header parse error quotes the file's own text.
        BuildError::Header(_) => (
            "header",
            "the rule file's header does not hold; the reason went back to the caller".to_string(),
        ),
        BuildError::CargoHomeTampered { was_present, now_present } => (
            "cargo-home-tampered",
            format!(
                "$CARGO_HOME/config.toml changed since this daemon started (was {}, is now {}) — \
                 refusing every Build until a restart re-pins it",
                if *was_present { "present" } else { "absent" },
                if *now_present { "present" } else { "absent" }
            ),
        ),
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

/// The build folder this service builds into: release artifacts, this binary's commit
/// (`render::build_folder`). The deployed hosts are RELEASE builds, so a release artifact is the
/// only one whose toolchain fingerprint they accept (`render::Profile`).
fn build_folder() -> String {
    render::build_folder(render::Profile::Release)
}

/// Retention over `dir`, the builds directory: every build FOLDER other than `current` goes (a
/// release starts a new folder; a host restarted on the new release asks for, and is served from,
/// the new one), and inside `current` the newest `keep` artifacts (`.so`) and the newest `keep`
/// recorded failures (`.err`) stay, newest built first (a cache hit rewrites nothing, so an artifact
/// in use but built long ago can be evicted: its next request rebuilds only its own crate). Ties break on the file name,
/// descending, so the order never depends on `read_dir`'s.
///
/// A folder is an eight-lowercase-hex-digit directory name, the shape
/// `vike_rust_strategy::artifact::build_folder` writes; anything else in `dir` is left alone. A
/// running strategy process keeps the artifact it opened even when its file goes (an unlinked file
/// stays mapped); its next start asks the builder again.
pub fn prune(dir: &Path, current: &str, keep: usize) -> io::Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    let is_folder = |name: &str| {
        name.len() == 8 && name.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    };
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if is_folder(name) && name != current && entry.file_type()?.is_dir() {
            std::fs::remove_dir_all(entry.path())?;
        }
    }
    let folder = dir.join(current);
    for ext in ["so", "err"] {
        let mut files: Vec<(PathBuf, std::time::SystemTime)> = Vec::new();
        let listing = match std::fs::read_dir(&folder) {
            Ok(l) => l,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        for entry in listing {
            let path = entry?.path();
            if path.extension().is_some_and(|e| e == ext) {
                let modified = std::fs::metadata(&path)?.modified()?;
                files.push((path, modified));
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
