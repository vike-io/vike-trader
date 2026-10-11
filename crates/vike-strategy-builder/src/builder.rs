//! The BUILDER SERVICE's own logic: its authenticated wire protocol, its retention pruner, and
//! its loopback-only bind rule. Task 6 (Track B).
//!
//! ⚠ **This module (and this whole crate) moved out of `vike-strategy-plugin` by a controller
//! ruling, after Track A and Track B independently found the same problem: `vike-strategy-plugin`
//! held both the loader a compiled plugin links against AND this service, so every compiled user
//! plugin inherited this service's whole dependency tree
//! (`vike-datahub-client`/`serde_json`, and `tracing` until it was dropped — see below) —
//! directly undercutting the design's own reason
//! for choosing DYNAMIC linkage (a plugin should be tens of kilobytes, not megabytes). Nothing in
//! this module's OWN logic changed across that move; only its crate changed, from
//! `vike_strategy_plugin::builder` to `vike_strategy_builder::builder`.
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
//! ⚠ **Split from `src/bin/vike-strategy-builder.rs` deliberately, not as Task 6's brief's file
//! list literally reads it.** Task 6's brief names one file to create
//! (`src/bin/vike-strategy-builder.rs`) plus a `tests/builder_auth.rs` that exercises it — but a
//! Cargo integration test can only link a crate's LIBRARY target, never one of its binary targets,
//! so `authenticate`/`prune`/`bind_addr` had to live somewhere `tests/builder_auth.rs` can
//! `use vike_strategy_builder::builder::…`. This module is that home; the actual
//! `src/bin/vike-strategy-builder.rs` stays a thin `main()` over it — the SAME split
//! `vike-backtest` uses (`compute_server.rs` in the library, `backtest_cli.rs`/`main.rs` wiring it
//! up), just discovered here rather than already being on the page.
//!
//! # Decision 1 — this service's OWN auth [`Domain`], never a sibling's
//!
//! [`DOMAIN`] is disjoint from BOTH `vike_tradehub_client::auth::DOMAIN` (the live order-signing
//! control channel) and `vike_node_proto::auth::DATAHUB_DOMAIN` (the data/compute
//! daemons) — a THIRD separator, not a reuse of either. `crates/vike-tradehub-client/src/auth.rs`
//! is explicit that `NodeKeys`/`Scope`/`Domain` are RE-EXPORTED from
//! [`vike_node_proto::auth`], generalized over the domain separator FOR EXACTLY THIS
//! REASON — so a third service names its own constant rather than copying the crypto.
//!
//! `crates/vike-backtest/src/compute_server.rs` reuses the DATAHUB pair for its own compute
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
//! `crates/vike-backtest/src/compute_server.rs`'s `required_scope`, whose doc argues
//! `WireSpec::Rhai` is `Scope::Write` because *"That is remote code execution BY DESIGN … the
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
use vike_node_proto::auth::{self, Domain, NodeKeys, Scope};

use crate::render;

/// This service's OWN domain separator — see this module's doc, Decision 1, for why it is
/// neither `vike_tradehub_client::auth::DOMAIN` nor
/// `vike_node_proto::auth::DATAHUB_DOMAIN`. NUL-terminated, following
/// [`Domain::new`]'s `b"vike-<service>-auth\0"` convention so no separator can be a prefix of
/// another.
pub const DOMAIN: Domain = Domain::new(b"vike-strategy-builder-auth\0");

/// This service's own wire-protocol version — independent of `vike_datahub_client::proto`'s or
/// `vike_tradehub_client::proto`'s, because this is a THIRD protocol, not a verb bolted onto
/// either existing one.
pub const PROTO_VERSION: u32 = 1;

/// The credential-store key name [`keys_from_vars`] reads. One name, not the usual
/// observe/control PAIR: this protocol has exactly one verb and it is [`Scope::Write`] (Decision
/// 2), so there is no read-only capability to hand a separate observe key for.
///
/// ⚠ **The service takes its key from EITHER this variable OR [`BUILDER_KEY_FILE_ENV`] — never
/// both** ([`key_source`]). This, the VALUE form, stays for the container image, whose operator
/// passes `-e` and has no service manager to load a file for it. The deployed systemd unit uses the
/// FILE form, and the reason is measured: a unit's `Environment=` is readable by any local user
/// over D-Bus (`busctl get-property … Service Environment`), and on the production box on
/// 2026-09-26 this key was the ONE secret-named variable exposed that way across all 243 loaded
/// units.
///
/// The CLIENT side (`vike-desktop`, the live smoke's own request) still reads this variable alone
/// through [`keys_from_vars`] — a GUI process has no service manager either.
pub const BUILDER_KEY_ENV: &str = "VIKE_STRATEGY_BUILDER_KEY";

/// The FILE form of the service's key: a path whose contents are the key. What
/// `deploy/vike-strategy-builder.service` sets, as `%d/builder-key` beside a `LoadCredential=` line,
/// so the key reaches the process as a file under systemd's credential directory and never as a
/// unit `Environment=` value.
///
/// The contents are read as the VALUE form reads its variable: UTF-8, surrounding whitespace
/// trimmed (so a trailing newline from `echo` or an editor is the same key as the bare string
/// `$(cat …)` produces), empty refused, and anything over [`MAX_KEY_FILE_LEN`] refused as not a key.
/// A refusal names the PATH and the reason, never a byte of the contents.
pub const BUILDER_KEY_FILE_ENV: &str = "VIKE_STRATEGY_BUILDER_KEY_FILE";

/// The largest key file [`resolve_keys`] accepts. A minted key is tens of bytes; a file of
/// kilobytes is the wrong file (a certificate, a log, a whole `.env`), and taking its bytes as an
/// HMAC key would start a service nobody can authenticate to while reporting that it has a key.
pub const MAX_KEY_FILE_LEN: usize = 4096;

/// Where [`resolve_keys`] will take the key from, decided from the variables alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeySource {
    /// [`BUILDER_KEY_ENV`] carries the key itself.
    Env,
    /// [`BUILDER_KEY_FILE_ENV`] names a file holding it.
    File(PathBuf),
}

/// Why [`resolve_keys`] produced no key. Its `Display` is the operator-facing reason — it names
/// variables and a path, never key material.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyRefusal {
    /// Neither form is set (a blank value counts as unset, as [`keys_from_vars`] has always
    /// treated it).
    Neither,
    /// Both forms are set. Refused rather than ranked: two live sources for one key is how an
    /// operator rotates the one the service is not reading, and on a systemd box it is exactly the
    /// state an old `Environment=` drop-in left beside the unit's `LoadCredential=` line produces.
    Both,
    /// The file form named a file that could not be read, or whose contents are not a key.
    File { path: PathBuf, reason: String },
}

impl fmt::Display for KeyRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KeyRefusal::Neither => write!(
                f,
                "neither {BUILDER_KEY_ENV} nor {BUILDER_KEY_FILE_ENV} is set. Set exactly one: the \
                 key itself, or the path of a file holding it (a systemd unit passes that file \
                 with `LoadCredential=` — see deploy/vike-strategy-builder.service)."
            ),
            KeyRefusal::Both => write!(
                f,
                "both {BUILDER_KEY_ENV} and {BUILDER_KEY_FILE_ENV} are set. Pick one: with two \
                 sources for one key, a rotation can change the one this service is not reading. \
                 On a systemd unit this is usually an old `Environment={BUILDER_KEY_ENV}=` drop-in \
                 left beside the unit's `LoadCredential=` line — delete the drop-in."
            ),
            KeyRefusal::File { path, reason } => {
                write!(f, "{BUILDER_KEY_FILE_ENV} names {}, which {reason}", path.display())
            }
        }
    }
}

/// Which form of the key `vars` carries — [`KeyRefusal::Neither`] or [`KeyRefusal::Both`] when it
/// is not exactly one. A pure map lookup, like [`keys_from_vars`].
pub fn key_source(vars: &HashMap<String, String>) -> Result<KeySource, KeyRefusal> {
    // Two literal `.get(CONST)` sites rather than one helper taking the name: the settings registry
    // proves a map lookup at a `.get(` site it can resolve, and a closure's parameter is not one.
    let value = vars.get(BUILDER_KEY_ENV).map(|v| v.trim()).filter(|v| !v.is_empty());
    let file = vars.get(BUILDER_KEY_FILE_ENV).map(|v| v.trim()).filter(|v| !v.is_empty());
    match (value, file) {
        (Some(_), Some(_)) => Err(KeyRefusal::Both),
        (Some(_), None) => Ok(KeySource::Env),
        (None, Some(file)) => Ok(KeySource::File(PathBuf::from(file))),
        (None, None) => Err(KeyRefusal::Neither),
    }
}

/// The key a key FILE's bytes carry — see [`BUILDER_KEY_FILE_ENV`] for the rules. `Err` is the
/// reason as a clause ("is empty"), for [`KeyRefusal::File`] to finish.
pub fn key_from_file_bytes(bytes: &[u8]) -> Result<NodeKeys, String> {
    if bytes.len() > MAX_KEY_FILE_LEN {
        return Err(format!(
            "is {} bytes — larger than any key ({MAX_KEY_FILE_LEN}-byte limit), so it is not the \
             key file",
            bytes.len()
        ));
    }
    let text = std::str::from_utf8(bytes).map_err(|_| "is not UTF-8 text".to_string())?;
    let key = text.trim();
    if key.is_empty() {
        return Err("is empty".to_string());
    }
    Ok(NodeKeys::new(Vec::new(), key.as_bytes().to_vec()))
}

/// The service's key, from whichever ONE form `vars` carries. `read_key_file` is how the file form
/// is opened — both composition roots pass `std::fs::read`, so this library module performs no
/// credential-file read of its own (the workspace rule that only a binary reads a credential
/// store), and a test can hand it any bytes or any error.
pub fn resolve_keys(
    vars: &HashMap<String, String>,
    read_key_file: &dyn Fn(&Path) -> io::Result<Vec<u8>>,
) -> Result<NodeKeys, KeyRefusal> {
    match key_source(vars)? {
        // `key_source` answered Env only because the value is non-blank, so this cannot be `None`;
        // the fallback keeps the two readers from ever disagreeing about what "set" means.
        KeySource::Env => keys_from_vars(vars).ok_or(KeyRefusal::Neither),
        KeySource::File(path) => {
            let bytes = read_key_file(&path).map_err(|e| KeyRefusal::File {
                path: path.clone(),
                reason: format!("could not be read: {e}"),
            })?;
            key_from_file_bytes(&bytes).map_err(|reason| KeyRefusal::File { path, reason })
        }
    }
}

/// `--port N` — the loopback port this service listens on ([`DEFAULT_PORT`] when absent; `0` asks
/// the OS for a free one, which the listening banner then names).
pub const PORT_FLAG: &str = "--port";

/// `--out-dir DIR` — where finished artifacts land ([`DEFAULT_OUT_DIR`] when absent).
pub const OUT_DIR_FLAG: &str = "--out-dir";

/// `--retain N` — how many artifacts [`prune`] keeps per strategy name
/// ([`DEFAULT_RETAIN_PER_STRATEGY`] when absent).
pub const RETAIN_FLAG: &str = "--retain";

/// `--workspace-root DIR` — the checkout root a rendered plugin's `Cargo.toml` resolves its `path`
/// dependencies against. REQUIRED: [`run`] refuses to start without it.
pub const WORKSPACE_ROOT_FLAG: &str = "--workspace-root";

/// The artifact directory when [`OUT_DIR_FLAG`] is absent — relative to the working directory, as
/// it always was.
pub const DEFAULT_OUT_DIR: &str = "user_data/plugins";

/// What `--help` prints, and what a usage error is followed by.
pub const USAGE: &str = "\
usage: vike-strategy-builder --workspace-root DIR [--port N] [--out-dir DIR] [--retain N]

  --workspace-root DIR  the strategy-source tree a plugin's Cargo.toml builds against. REQUIRED,
                        and refused if it holds a settings/ directory of its own
  --port N              loopback port to listen on (default 7881; 0 = a free port)
  --out-dir DIR         where finished artifacts land (default user_data/plugins)
  --retain N            artifacts kept per strategy name (default 5)
  -h, --help            print this and exit 0

The key is VIKE_STRATEGY_BUILDER_KEY or the file VIKE_STRATEGY_BUILDER_KEY_FILE names — never
both, never on this command line.";

/// This service's command line, parsed — the four plain settings decision 0111 made arguments
/// (they were `VIKE_STRATEGY_BUILDER_*` variables): how ONE builder process is started, which is
/// what its unit's `ExecStart=` states.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuilderArgs {
    /// [`PORT_FLAG`].
    pub port: u16,
    /// [`OUT_DIR_FLAG`].
    pub out_dir: PathBuf,
    /// [`RETAIN_FLAG`].
    pub retain: usize,
    /// [`WORKSPACE_ROOT_FLAG`] — `None` when absent, which [`run`] refuses by name.
    pub workspace_root: Option<PathBuf>,
}

/// What a successful parse asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Parsed {
    /// Serve with these arguments.
    Run(BuilderArgs),
    /// `-h` / `--help`.
    Help,
}

/// Parse the command line (the arguments AFTER the program or verb name). Each value flag takes the
/// next argument or an `=VALUE`; an unknown argument, a missing value or a number that does not
/// parse is an ERROR naming it — never a silently-defaulted knob, which is what a malformed
/// variable used to be here.
///
/// # Errors
/// One line naming the offending argument.
pub fn parse_args(args: &[String]) -> Result<Parsed, String> {
    let mut parsed = BuilderArgs {
        port: DEFAULT_PORT,
        out_dir: PathBuf::from(DEFAULT_OUT_DIR),
        retain: DEFAULT_RETAIN_PER_STRATEGY,
        workspace_root: None,
    };
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        if arg == "-h" || arg == "--help" {
            return Ok(Parsed::Help);
        }
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v.to_string())),
            _ => (arg.as_str(), None),
        };
        if ![PORT_FLAG, OUT_DIR_FLAG, RETAIN_FLAG, WORKSPACE_ROOT_FLAG].contains(&flag) {
            return Err(format!("unknown argument: {arg}"));
        }
        // A following flag is not a value: `--out-dir --port 0` is a missing directory, not a
        // directory named `--port`.
        let next = match inline {
            Some(v) => Some(v),
            None => it.next().filter(|v| !v.starts_with("--")).cloned(),
        };
        let value = match next {
            Some(v) if !v.trim().is_empty() => v,
            _ => return Err(format!("{flag} needs a value")),
        };
        match flag {
            PORT_FLAG => {
                parsed.port = value
                    .trim()
                    .parse()
                    .map_err(|_| format!("{PORT_FLAG}: not a port number: {value}"))?;
            }
            OUT_DIR_FLAG => parsed.out_dir = PathBuf::from(value),
            RETAIN_FLAG => {
                parsed.retain = value
                    .trim()
                    .parse()
                    .map_err(|_| format!("{RETAIN_FLAG}: not a count: {value}"))?;
            }
            _ => parsed.workspace_root = Some(PathBuf::from(value.trim())),
        }
    }
    Ok(Parsed::Run(parsed))
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

/// How many artifacts [`prune`] keeps per strategy name when nothing else is configured — the
/// same SHAPE as `vike_log::DEFAULT_MAX_LOG_FILES` (a named, documented retention constant rather
/// than an unbounded default), chosen independently: a strategy iteration session realistically
/// wants "the last few attempts" for compare/rollback, and anything older is an iteration nobody
/// is about to go back to.
///
/// ⚠ **The retention DECISION, not just the number — open question 2 of
/// `docs/superpowers/specs/2026-09-21-runtime-loaded-rust-strategies-design.md`, closed here rather
/// than left for the knob to answer implicitly.** That design doc measured the CAPACITY question
/// (a plugin runs 674 KiB–2.5 MiB depending how much of the widened dependency surface it reaches)
/// and left the POLICY question open: what N, and per-strategy or global. [`prune`] already
/// answers the second half BY CONSTRUCTION — it groups artifacts by
/// [`artifact_strategy_name`] and keeps `keep` newest PER GROUP, never a flat cap over the whole
/// directory — and this constant is that shape's default, kept rather than replaced with a global
/// one.
///
/// **Why per-strategy over global.** A global cap makes one strategy's iteration churn able to
/// evict ANOTHER strategy's only artifact — an operator heads-down on strategy A for an afternoon
/// (each edit is a new sha, a new file) can silently age a rarely-touched strategy B's sole build
/// out of a shared quota, so pressing Run on B gets a cold rebuild (or, worse in a future world
/// where the cache trusted a stale entry, nothing at all) for a reason that has nothing to do with
/// B. A per-strategy cap makes iteration on A cost only A's own budget: B's artifact is untouched
/// by A's churn no matter how long the session runs. Given the single-operator premise this whole
/// design rests on (`docs/decisions/0014-audience-solo-plus-agents.md`) — a handful of strategies
/// actively iterated, not hundreds — isolation between them is worth more here than squeezing the
/// LAST byte out of a shared ceiling would be.
///
/// **What it costs.** At `N = 5` and the heaviest artifact measured to date (2,549,832 bytes,
/// ≈2.49 MiB — a strategy naming the whole indicator catalog), one strategy's worst case is
/// ≈12.45 MiB; the FLOOR artifact (≈944 KiB) five times over is ≈4.7 MiB. Total disk use scales
/// with the number of DISTINCT strategy NAMES an operator has ever pressed Build on, not with edit
/// count per strategy: each Build call writes ONE more artifact into the SAME name's group,
/// pruned back to `N` immediately after (`prune` runs at the end of every `Build` in
/// `handle_connection`), so a long iteration session on one strategy never grows the total beyond
/// `N` for that name. A few dozen distinct strategy names — already more than one operator
/// typically has open at once — stays a rounding error beside a hist store, the same conclusion
/// the design doc reached for a GLOBAL count at a larger N. `5` is deliberately tighter than that
/// doc's own suggested "twenty per strategy" — raising it is one argument ([`RETAIN_FLAG`], parsed
/// by [`parse_args`]), so starting conservative costs nothing an operator cannot undo the moment it
/// pinches.
///
/// **What would reopen this.** Not disk pressure — the design doc's own words apply unchanged:
/// "choosing a small N for [disk pressure] would be optimising against a number this design
/// measured" in the tens of megabytes. What WOULD reopen it is the strategy-NAME count itself
/// growing large enough that `N * distinct_names` stops being a rounding error (hundreds of named
/// strategies, not a handful) — at that point a GLOBAL backstop alongside this per-strategy floor
/// would be the next knob, not a replacement for it. [`RETAIN_FLAG`] is the one
/// knob either way: it already expresses "how many per strategy", and nothing here invents a
/// second one for "how many total" before that condition is actually observed.
pub const DEFAULT_RETAIN_PER_STRATEGY: usize = 5;

/// Frame ceiling for the pre-auth `Hello`/`Auth` exchange — mirrors
/// `vike_backtest::compute_server::HANDSHAKE_MAX_FRAME_LEN` and for the identical reason: the
/// full [`MAX_FRAME_LEN`] is sized for a legitimate ANSWER (a `source` string can run to
/// kilobytes), and accepting that much before a peer has proven anything means four bytes make
/// this process allocate megabytes.
pub const HANDSHAKE_MAX_FRAME_LEN: u32 = 64 * 1024;

/// The scope [`Request::Build`] requires — and, because this protocol has exactly one verb, the
/// CEILING this domain will ever grant a key for.
///
/// **Why [`Scope::Write`] and not something weaker.** Compare
/// `crates/vike-backtest/src/compute_server.rs`'s `required_scope`, which classifies
/// `WireSpec::Rhai`-carrying verbs as `Scope::Write` because compiling client-supplied source is
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

/// Build this service's [`NodeKeys`] from an already-loaded var map — [`BUILDER_KEY_ENV`] alone.
/// A pure map lookup (`Layer::Injected`), never `std::env::var`: the CALLER (the binary) owns the
/// one `std::env::vars()` sweep, per this workspace's "libraries take configuration as
/// parameters" rule.
///
/// ⚠ The VALUE form only. The SERVICE resolves its key through [`resolve_keys`], which also takes
/// the file form and refuses both at once; this stays the reader for a CLIENT, which holds the
/// key in its own environment and has no unit to load a file for it.
///
/// Only the WRITE slot is ever filled — there is no observe-scope key for this protocol at all
/// (Decision 2), so [`NodeKeys::has`]`(Scope::Read)` is always `false` for a key built here, by
/// construction rather than by a check anyone has to remember.
pub fn keys_from_vars(vars: &HashMap<String, String>) -> Option<NodeKeys> {
    let key = vars.get(BUILDER_KEY_ENV)?.trim();
    if key.is_empty() {
        return None;
    }
    Some(NodeKeys::new(Vec::new(), key.as_bytes().to_vec()))
}

/// Verify a presented `Auth` frame: the scope must be [`required_scope`] (the only scope this
/// service ever grants a key for), the mac must verify under THIS service's own [`DOMAIN`] and
/// the connection's nonce, and — since [`keys_from_vars`] never fills the read slot — a
/// `Scope::Read` or `Scope::Account` presentation is refused before `key_for` is even consulted.
pub fn verify_auth(
    keys: &NodeKeys,
    nonce: &[u8; 32],
    proto_version: u32,
    scope: Scope,
    mac: &[u8],
) -> bool {
    scope == required_scope()
        && keys.has(scope)
        && auth::verify(DOMAIN, keys.key_for(scope), nonce, proto_version, scope, mac)
}

/// The port this service listens on when [`PORT_FLAG`] is absent — arbitrarily the
/// next free number after `vike-backtest`'s compute port (7880) and `vike-tradehub`'s control port
/// (7879), on the same "one small integer per localhost service" convention neither of those two
/// units' own comments treat as anything more than a default to restate.
///
/// ⚠ Declared HERE rather than in `src/bin/vike-strategy-builder.rs`, where it used to live,
/// because [`crate::client`] needs the same number to build its own default address: two spellings
/// of one port is exactly the shape that leaves a client dialling a service that moved. The binary
/// reads it from this constant now, and `default_addr` derives the whole address from
/// [`bind_addr`], so there is one authority for the address on both ends of the socket.
pub const DEFAULT_PORT: u16 = 7881;

/// Always `127.0.0.1:port` — the IP half is not a parameter and cannot be made one from outside
/// this function. This service compiles and runs whatever an authenticated caller sends it, so a
/// public bind is not a configuration this process can be talked into; only the PORT varies (a
/// real deployment knob, e.g. to avoid colliding with `vike-tradehub`'s control port or
/// `vike-backtest`'s compute port on one box).
pub fn bind_addr(port: u16) -> SocketAddr {
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
    /// toolchain-fingerprint scheme is Task 4's (`vike_strategy_plugin::fingerprint`, landed on
    /// this branch, and this crate DOES now depend on that sibling crate — see `src/render.rs`'s
    /// module doc for why: it reaches `vike-strategy-plugin`'s `TEMPLATE_CARGO_TOML`/
    /// `TEMPLATE_LIB_RS` constants over an ordinary Cargo dependency edge). Wiring an actual
    /// comparison against `fingerprint::FINGERPRINT` here is still real additional surface (a new
    /// refusal branch, its own test) that the crate SPLIT this module rode in on does not ask for;
    /// inventing it now would be the same "do not invent another track's contract" mistake the
    /// cdylib template avoids for `guest::HostBroker`. The field is accepted and threaded through
    /// so the wire shape does not have to change again once that check lands.
    Build {
        name: String,
        source: String,
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
/// `workspace_root` is the checkout root `render::build_plugin`'s rendered scratch crate resolves
/// its `vike-model`/`vike-strategy-plugin` `path` dependencies against, and `cargo_bin` is the
/// `cargo` executable it invokes — see `render.rs`'s module doc for why BOTH are caller-supplied
/// parameters ([`run`] takes [`WORKSPACE_ROOT_FLAG`] and the process's own `CARGO` variable,
/// refusing to start without the former) rather than a path
/// either resolves for itself. `child_env` is the whole environment every `cargo` it spawns gets —
/// `render::child_env` over the same map, computed once in [`run`].
pub fn serve(
    listener: TcpListener,
    keys: NodeKeys,
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
/// ([`parse_args`]) and `CARGO`, in ONE place.
///
/// ⚠ **This used to be split across the two composition roots that need it, and that was the
/// defect this function exists to close.** `src/bin/vike-strategy-builder.rs`'s `main` read its
/// settings and `CARGO` directly
/// (`Layer::Binary`, five rows in `vike_ops::settings::SETTINGS`) — correct while that binary was
/// the only composition root. The multicall join (`vike-backend strategy-builder`) gave this
/// service a SECOND one, and the alternative to this function was a second copy of every refusal
/// message and every default in `crates/vike/src/main.rs` — exactly the "two implementations of
/// one exchange" shape this workspace's conventions refuse on sight. So the whole sequence moved
/// here instead: a pure parser over a caller-supplied map (`Layer::Injected`, the same shape
/// [`keys_from_vars`] already had), with the two composition roots reduced to "sweep the real
/// process environment once, hand the map to this function, return what it returns" — the binary's
/// own `main` and `vike-backend`'s `strategy_builder_main` row are that call, nothing more. The
/// five `SETTINGS` rows moved to `Layer::Injected` with this function as their one reader.
///
/// Binding the real socket and creating `out_dir` still happen HERE rather than being handed back
/// as more parsed config: both are the same kind of one-shot process action `serve`'s own call
/// already is, and splitting them out would just move the duplication one line up instead of
/// removing it.
///
/// `read_key_file` opens the key FILE when the unit supplies one ([`BUILDER_KEY_FILE_ENV`]); both
/// composition roots pass `std::fs::read`. See [`resolve_keys`] for why the read is injected.
pub fn run(
    vars: &HashMap<String, String>,
    args: &[String],
    read_key_file: &dyn Fn(&Path) -> io::Result<Vec<u8>>,
) -> ExitCode {
    let args = match parse_args(args) {
        Ok(Parsed::Run(a)) => a,
        Ok(Parsed::Help) => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(e) => {
            eprintln!("vike-strategy-builder: {e}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    if let Err(refusal) = refuse_retired_variables(vars) {
        eprintln!("vike-strategy-builder: refusing to start — {refusal}");
        return ExitCode::FAILURE;
    }
    let keys = match resolve_keys(vars, read_key_file) {
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

    let BuilderArgs { port, out_dir, retain, workspace_root } = args;

    // Required, refused the same way as a missing key: `render::build_plugin`'s module doc argues
    // why a compile-time-baked fallback here would be exactly the defect
    // `crates/vike-ops/tests/hygiene/compile_time_path_gate.rs` exists to catch, and why — unlike a data
    // path — there is no ladder to fall through to instead.
    let workspace_root: PathBuf = match workspace_root {
        Some(root) => root,
        None => {
            eprintln!(
                "vike-strategy-builder: refusing to start — {WORKSPACE_ROOT_FLAG} is not given.\n\
                 This is the checkout root holding crates/vike-model and \
                 crates/vike-strategy-plugin that a rendered plugin's Cargo.toml resolves its \
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

    // The `cargo` binary `render::build_plugin` invokes. Cargo's own `CARGO` variable when this
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

    let addr = bind_addr(port);
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
        "vike-strategy-builder: listening on {bound}, artifacts under {}, retaining {retain} per strategy",
        out_dir.display()
    );

    if let Err(e) =
        serve(listener, keys, out_dir, retain, workspace_root, cargo_bin, child_env, cargo_home_pin)
    {
        eprintln!("vike-strategy-builder: serve exited: {e}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

/// One connection's whole lifetime: `Hello` -> `Welcome{nonce}` -> `Auth` -> `AuthOk`, then a
/// loop of `Build` requests until the peer disconnects or sends anything else.
fn handle_connection(
    mut stream: TcpStream,
    keys: &NodeKeys,
    out_dir: &Path,
    retain: usize,
    workspace_root: &Path,
    cargo_bin: &str,
    child_env: &[(String, String)],
    cargo_home_pin: &render::CargoHomePin,
) {
    // Nagle OFF before the first frame, as on every node socket
    // (`vike_node_proto::frame::configure_node_stream`). A refusal is DROPPED rather than printed:
    // it costs a build's reply some latency and nothing else, and this module's stderr lines are
    // the few its own doc names — an operator does not need one per connection for this.
    let _ = vike_node_proto::frame::configure_node_stream(&stream);
    let body = match read_frame_raw_capped(&mut stream, HANDSHAKE_MAX_FRAME_LEN) {
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

    let body = match read_frame_raw_capped(&mut stream, HANDSHAKE_MAX_FRAME_LEN) {
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
        let Request::Build { name, source, toolchain_fp: _ } = req else {
            // Anything but `Build` after a completed handshake: this protocol has one verb, so
            // there is nothing else a peer could legitimately send.
            return;
        };
        let source_sha = render::sha256_hex(source.as_bytes());
        let response = match render::build_plugin(
            &source,
            &name,
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
            Ok(_path) => Response::BuildOk { sha: source_sha },
            Err(e) => {
                // ⚠ SAID ON STDERR AS WELL AS ANSWERED, because the answer reaches ONE peer and the
                // journal reaches the operator. MEASURED on a deployed box: after a release moved
                // the binary and not its source tree, `render::verify_source_version` refused every
                // Build for two days while `journalctl -u vike-strategy-builder` showed nothing but
                // the listening banners — a daemon refusing every request read exactly like an
                // idle one. See [`build_refusal_line`] for what the line may and may not carry.
                eprintln!("{}", build_refusal_line(&name, &source_sha, &e));
                Response::BuildErr { diagnostics: e.to_string() }
            }
        };
        if write_frame(&mut stream, &response).is_err() {
            return;
        }
        if let Err(e) = prune(out_dir, retain) {
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
/// grep for), the strategy NAME (escaped and capped — it is the caller's string, and a newline in
/// it must not forge a second journal line), the SOURCE SHA (the content address the artifact is
/// named by, which says which request this was without saying what it contained), and for every
/// other variant the reason as `render` composed it — for the version-skew refusal that is both
/// stamps and the tree's path, which is the whole of what an operator needs to act.
///
/// One LINE, by construction: any newline in a reason is flattened, because a journal reader
/// scanning for `REFUSED` must see the whole refusal on the line that matches.
fn build_refusal_line(name: &str, source_sha: &str, err: &render::BuildError) -> String {
    use render::BuildError;
    const NAME_CAP: usize = 64;
    let (kind, reason) = match err {
        BuildError::SourceVersionMismatch(msg) => ("source-version-mismatch", msg.clone()),
        BuildError::UnwiredHook(hooks) => (
            "unwired-hook",
            format!(
                "the source overrides {} — a Strategy hook the plugin mechanism does not deliver",
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
        BuildError::InvalidName(_) => (
            "invalid-name",
            "the strategy name must not contain `/` or `\\`, and must not be empty, `.`, or `..` \
             — it becomes a path component"
                .to_string(),
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
    let shown_name: String = name.chars().take(NAME_CAP).collect();
    let cut = if name.chars().count() > NAME_CAP { "…" } else { "" };
    let reason = reason.replace(['\r', '\n'], " ");
    format!(
        "vike-strategy-builder: Build REFUSED [{kind}] strategy {shown_name:?}{cut} (source sha256 \
         {sha}): {reason}",
        sha = &source_sha[..source_sha.len().min(16)],
    )
}

/// The name a `<name>-<sha256>.so` artifact belongs to, parsed from the FIXED shape — never a
/// `filename.starts_with(candidate)` test.
///
/// ⚠ **Why not `starts_with`, stated because it is this function's whole reason to exist as its
/// own parser rather than a loop over known names.** `vike_log::LogConfig::file_prefix`'s own doc
/// carries the incident this mirrors: that crate's retention used to prune by
/// `filename.starts_with(prefix)` against a SHARED default prefix (`"vike"`), so
/// `vike-app.2026-08-06` and `vike-tradehub.2026-08-06` — two different binaries' logs — were
/// both "prefixed by vike" and a one-shot backfill would have deleted both. The fix there was
/// making the default prefix distinct per binary; the fix here is structural instead: extracting
/// the exact name from the ONE fixed suffix shape every artifact has (`-` + 64 lowercase hex + `.so`)
/// rather than testing candidate names against the filename at all, so two strategies whose names
/// happen to share a prefix (`strat` / `strat_ab`, or the exact pair this module's own test
/// drives) can never merge into one prune group by construction — there is no `starts_with` call
/// anywhere in this file for a rename to silently widen.
pub fn artifact_strategy_name(path: &Path) -> Option<String> {
    let file_name = path.file_name()?.to_str()?;
    let stem = file_name.strip_suffix(".so")?;
    // `-` + 64 lowercase-hex sha256 = 65 trailing bytes; anything shorter cannot carry the shape.
    const SHA_LEN: usize = 64;
    if stem.len() <= SHA_LEN + 1 {
        return None;
    }
    let split_at = stem.len() - (SHA_LEN + 1);
    let (name, rest) = stem.split_at(split_at);
    let hash_part = rest.strip_prefix('-')?;
    if hash_part.len() != SHA_LEN || !hash_part.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    if name.is_empty() {
        return None;
    }
    Some(name.to_string())
}

/// Prune `dir` to the newest `keep` artifacts PER STRATEGY NAME (grouped by
/// [`artifact_strategy_name`], never by a filename prefix test — see that function's doc).
/// Ties in modification time (possible on a coarse filesystem clock, or in a test that plants
/// several files inside one clock tick) break on the FILENAME, descending, so the ordering is
/// always total and deterministic rather than depending on `read_dir`'s unspecified order.
pub fn prune(dir: &Path, keep: usize) -> io::Result<()> {
    let mut groups: HashMap<String, Vec<(PathBuf, std::time::SystemTime)>> = HashMap::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let Some(name) = artifact_strategy_name(&path) else { continue };
        let modified = entry.metadata()?.modified()?;
        groups.entry(name).or_default().push((path, modified));
    }
    for files in groups.values_mut() {
        files.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| b.0.cmp(&a.0)));
        for (path, _) in files.iter().skip(keep) {
            std::fs::remove_file(path)?;
        }
    }
    Ok(())
}

/// How many artifacts `dir` currently holds for strategy `name` — the retention test's own
/// counter, exported so the test and [`prune`] agree on what "belongs to `name`" means via the
/// SAME parser rather than two independent guesses at the naming rule.
pub fn count_for(dir: &Path, name: &str) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else { return 0 };
    entries.flatten().filter(|e| artifact_strategy_name(&e.path()).as_deref() == Some(name)).count()
}

#[path = "builder_tests.rs"]
#[cfg(test)]
mod builder_tests;
