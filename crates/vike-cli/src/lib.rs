//! `vike-cli` — the unified vike command-line surface (headless two-layer plan, Layer 1).
//!
//! A **git/cargo-style subcommand dispatcher**: `vike-cli <command> [args…]`. Commands: `backtest`
//! (the compute-to-data offload that ships a profile's TOML to a remote `vike-datahub` server and
//! prints the compact report, with **no DataFusion in this side's build graph**), `mcp` (a local
//! stdio MCP server exposing the create+backtest tools to an agent), `config` (settings
//! provenance — `show` for every setting, its effective value and where that value came from;
//! `check` for the same tree JUDGED, with the exit code as the product, which is what the shipped
//! `deploy/*.service` units put in their `ExecStartPre=`) and `init` (scaffold
//! `<project>/user_data`, the user-content directory, with runnable examples).
//!
//! # Library + thin bin
//!
//! The dispatcher and subcommand modules live in this LIBRARY; `src/main.rs` is a one-line shim that
//! calls [`run`]. This matches the sibling bin crates (`vike-tradehub`, `vike-run`) and — load-
//! bearing for CI — gives the package a LIB TARGET, so `cargo test --doc -p vike-cli` has something
//! to run (a bin-only crate errors "no library targets found in package vike-cli").
//!
//! # Growing the surface
//!
//! Each command is a sibling module under [`cmd`] exposing a `run(args) -> ExitCode`. Adding
//! `sweep`, `walkforward`, `trade`, or an interactive `repl` later is: add the module, add one arm
//! to [`dispatch`], add one line to [`COMMANDS`]. The dispatcher itself never grows command logic —
//! it only routes.
//!
//! # Settings, resolved once, before any subcommand
//!
//! [`run`] calls [`resolve_policy`] first, which runs the workspace's ONE startup sequence
//! (`vike_boot::boot`): it REFUSES a removed environment variable (today, the
//! `VIKE_MAX_ORDER_NOTIONAL` that Phase 5 of the settings-unification design deleted — a ceiling
//! any exported variable can raise is not a ceiling) and loads this machine's
//! `<project>/settings/policy.toml`. The resolved `max_notional_per_order` is handed to the two
//! order-write surfaces (`trade`, `mcp`) for their advisory preview guardrail. The environment read
//! lives here, in the entry point, per the settings-registry rule that only binaries read env — and
//! so does the obligation to SURFACE the loader's non-fatal resolutions, which this dispatcher used
//! to swallow (a preference clamped to a policy ceiling produced no output at all). They print to
//! **stderr**, never stdout, because `vike-cli mcp`'s stdout is a protocol.
//!
//! # The credential-store read is HERE too, and it is LAZY
//!
//! The two order-write surfaces authenticate to a `vike-tradehub` node with HMAC keys the DAEMON
//! reads out of `<project>/settings/secrets.env`. Both used to read `std::env::var` and nothing
//! else, so a correctly-configured box answered "nothing to do" and exited — see [`cmd::nodekeys`],
//! which argues the precedence (process env first, store second). The store read belongs at this
//! composition root for exactly the reason the environment sweep does, and [`node_keyring`]
//! performs it ONLY for the node-facing arms (`trade`/`mcp`/`strategy-status` — the last a READ
//! verb that uses just the observe half): no other subcommand needs a credential, and a
//! provenance or backtest command has no business opening the file that holds every venue key on
//! the box.
//!
//! It reaches the store through `vike_secrets::resolve`, which takes the PATH this dispatcher
//! already resolved — the shape `cmd::secrets` uses, and the one
//! `crates/vike-ops/tests/settings_registry.rs`'s `CREDENTIAL_STORE_PIN` explicitly blesses ("the
//! composition root doing exactly what the rule asks for"), as opposed to the sweeping loaders that
//! open a location nothing in their signature mentions.

use std::collections::HashMap;
use std::path::Path;
use std::process::ExitCode;

pub mod cmd;

use crate::cmd::nodekeys::NodeKeyring;

/// The registered subcommands, as `(name, one-line summary)` — the single source of truth for both
/// the help text and the "unknown command" hint, so a new command shows up in help for free.
const COMMANDS: &[(&str, &str)] = &[
    ("backtest", "run a backtest on a remote vike-datahub server and print the report"),
    (
        "sweep",
        "run a parameter-grid search on a remote vike-datahub server and print the ranked grid",
    ),
    ("walkforward", "run an anchored walk-forward validation on a remote vike-datahub server"),
    ("config", "settings provenance (`show`) and a validating pre-flight (`check`) for this box"),
    ("mcp", "serve the create+backtest tools over stdio MCP (for Claude / an agent)"),
    ("trade", "interactive REPL to observe + control a running vike-tradehub node (for a human)"),
    (
        "strategy-status",
        "ask a running vike-tradehub node what it is running (read-only; --json for machines)",
    ),
    ("secrets", "inspect the credential store: <project>/settings/secrets.env (list | path)"),
    ("init", "create <project>/user_data — strategies, profiles, results — with examples"),
    ("indicators", "print the indicators a Rhai strategy can call, with their parameters"),
];

/// Parse the command line (argv WITHOUT the binary name) and dispatch. Returns the process exit
/// code. The `src/main.rs` shim calls this with `std::env::args().skip(1)`.
///
/// Before ANY subcommand runs, the environment is checked for variables that have been REMOVED
/// (settings unification, Phase 5) and the machine's policy ceiling is resolved — see
/// [`resolve_policy`]. Both happen here rather than per-subcommand so a stale variable cannot be
/// honoured by one surface and refused by another, and so `vike-cli trade`'s advisory guardrail
/// reads the same ceiling the trading binaries enforce.
pub fn run(mut args: impl Iterator<Item = String>) -> ExitCode {
    let Some(command) = args.next() else {
        // No subcommand: print help and exit non-zero (there was nothing to do).
        print_help();
        return ExitCode::FAILURE;
    };
    let resolved = match resolve_policy() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("vike-cli: {e}");
            return ExitCode::FAILURE;
        }
    };
    install_user_indicators(resolved.user_data_dir.as_deref());
    dispatch(&command, args, &resolved)
}

/// Loads `<project>/user_data/indicators/*.rhai` and installs them process-wide, so a script run by
/// ANY subcommand can call them.
///
/// This is the composition root doing the I/O on the library's behalf — `vike_script`'s
/// `install_user_indicators` takes already-compiled prototypes precisely so that no library resolves
/// this directory for itself (its doc is the authority on why, and names `vike_log::init` as the
/// precedent). The load-plus-install pair is `vike_script::load_and_install_user_indicators`, which
/// is what a root wires WHEN it is wired — `git grep -l load_and_install_user_indicators -- crates` finds
/// every file that NAMES the pair — which is not the same as the set of callers: `vike-app`
/// appears there because it documents why it does NOT use it (it has TWO consumers of one load and
/// needs the report, not just the messages), and it is deliberately not written down here. ⚠ It is NOT yet every root that
/// compiles Rhai: `crates/vike-backtest/src/backtest_cli.rs` and its `cheap_np_*` siblings reach
/// the `"rhai"` arm of `crates/vike-backtest/src/harness/registry.rs`'s `strategy_by_name` and call
/// none of this, so a user indicator does not resolve there. That is a known gap, not a claim.
/// This function used to be a hand-written copy of the pair, and a copy per root is exactly how
/// they would come to disagree about which directory a user's indicators live in.
///
/// ⚠ **Every rejected file is REPORTED on stderr, and a bad one never fails the command.** From
/// inside a strategy, an indicator that failed to load is indistinguishable from a typo in the call
/// — both are function-not-found — so this is the only place that can say which. Aborting instead
/// would be worse: one unrelated half-edited indicator file would block every `vike-cli` invocation,
/// including the `init` that scaffolds the examples. **stderr, never stdout** — `vike-cli mcp`'s
/// stdout is a protocol, which is also why the messages come back as data rather than being printed
/// by the library that produced them.
fn install_user_indicators(user_data_dir: Option<&std::path::Path>) {
    let Some(dir) = user_data_dir else {
        return;
    };
    for message in vike_script::load_and_install_user_indicators(dir) {
        eprintln!("vike-cli: {message}");
    }
}

/// What the dispatcher resolves ONCE, before any subcommand runs, out of the single
/// `std::env::vars()` sweep this crate performs — see [`resolve_policy`].
struct Resolved {
    /// `max_notional_per_order` from `<project>/settings/policy.toml`, or `None` when unset.
    policy_max_notional: Option<f64>,
    /// `<project>/settings` — THE settings directory, or `None` when no project sits above the
    /// working directory. Resolved here rather than inside a subcommand because the rule is that
    /// only the composition root reads the environment, and this one already had the map in hand:
    /// `cmd::secrets` inspects the credential store inside it, and `cmd::trade` persists its REPL
    /// history under its `state/` sub-directory.
    settings_dir: Option<std::path::PathBuf>,
    /// WHICH rung answered for [`Resolved::settings_dir`] — `VIKE_SETTINGS_DIR` or the walk.
    ///
    /// Only this composition root can know: below it the two are indistinguishable, because the
    /// resolver takes the override as a parameter and returns a bare path. `cmd::config_check`
    /// needs the distinction and nothing else does — a NAMED directory that is not on disk is a
    /// set-but-unhonoured value and a refusal, while a WALKED one that is not there is an
    /// unconfigured checkout and merely a warning.
    settings_dir_origin: cmd::config_check::DirOrigin,
    /// The `$VIKE_SETTINGS_DIR` value the boot HONOURED — trimmed, and `None` when blank or unset.
    ///
    /// ⚠ **It is the RUNG, and it is carried because only this root can know it.** Below here the
    /// two are indistinguishable: the resolver takes the override as a parameter and returns a bare
    /// path. [`Resolved::settings_dir_origin`] answers the same question as a two-state verdict for
    /// `config check`'s refusal; `cmd::secrets` needs the VALUE.
    ///
    /// ⚠ **This used to be "not derivable from [`Resolved::settings_dir`]", and that has CHANGED.**
    /// `vike_boot::boot` resolved the directory as `spec.cwd.and_then(..)`, so a process whose
    /// working directory had been removed, unmounted or made unsearchable got `settings_dir: None`
    /// while this stayed `Some(..)` — the pairing #1514 taught `cmd::secrets` to survive. The boot
    /// now honours a name with no walk (`vike_secrets::project_settings_dir_for`), so `Some` here
    /// implies `Some` there, holding the same path, and that fallback rung is unreachable through
    /// this dispatcher. It is KEPT rather than deleted: it is the belt against the boot regressing,
    /// and `cmd::secrets`'s `store_path` states its own reachability at its site.
    settings_dir_override: Option<String>,
    /// `<project>/user_data` — THE user-content directory (strategies, run profiles, results,
    /// notebooks), or `None` when no project sits above the working directory.
    ///
    /// A SIBLING of [`Resolved::settings_dir`], never a child, and resolved from the SAME walk so
    /// the two can never answer with different projects — `crates/vike-model/src/state_path.rs`'s
    /// `PROJECT_USER_DATA_DIR` argues why the ownership split matters. `$VIKE_USER_DATA_DIR` names
    /// it outright; it is a separate variable from `$VIKE_SETTINGS_DIR` because pointing the app at
    /// a strategy library on another disk is a different question from relocating a deployment's
    /// settings, and one variable for both would force them to move together.
    ///
    /// ⚠ Unlike `settings_dir`, this is where the directory BELONGS rather than one that was found:
    /// a tree with no `user_data/` is the ordinary state of a fresh install, which is precisely the
    /// case `init` exists to fix.
    user_data_dir: Option<std::path::PathBuf>,
    /// The process environment, kept because [`node_keyring`] needs it and it is already collected.
    /// The dispatcher owns this map; nothing below it reads `std::env` for a node key.
    env: HashMap<String, String>,
}

/// Refuse a stale environment, then resolve this machine's per-order notional ceiling
/// (`max_notional_per_order` in `<project>/settings/policy.toml`).
///
/// `VIKE_MAX_ORDER_NOTIONAL` is no longer read by anything: it named the same per-order ceiling the
/// trading binaries enforce, and a ceiling any exported variable can raise is not a ceiling. An
/// operator who still has it set believes a cap is in force, so this REFUSES rather than ignoring
/// it — `vike_config::refuse_removed_env` writes the message, naming the file and key that replace
/// it. The check runs for every subcommand, including read-only ones: a variable that is stale is
/// stale, and the error is exactly the diagnostic that explains it.
///
/// A missing `policy.toml` is not an error (`None` — no advisory cap, today's behaviour); a file
/// that exists and is broken is.
///
/// The read of `std::env::vars()` happens HERE, at the dispatcher, per the settings-registry rule;
/// neither `vike_config` nor `vike_boot` ever touches `std::env`. The ORDER of the steps below is
/// `vike_boot::boot`'s — four other roots run the same one — and each way this dispatcher departs
/// from it is a named arm of [`vike_boot::BootSpec`] carrying its own reason.
///
/// It also SURFACES the loader's non-fatal resolutions, which this dispatcher used to drop on the
/// floor. `vike_config` returns them as DATA rather than logging them, deliberately: it does not
/// depend on `tracing`, because a library that writes to a caller's stderr on its own initiative
/// cannot be used by a binary whose STDOUT is a protocol — which `vike-cli mcp`'s is. The obligation
/// to emit them is therefore the binary's, and a preference clamped to a policy ceiling used to take
/// effect here with no output at all: a limit the operator believes they set and does not have. They
/// go to **stderr**, never stdout, for the MCP reason above.
///
/// The same sweep also yields THE settings directory, which `cmd::secrets` and `cmd::trade` both
/// need, and its SIBLING `<project>/user_data` for `cmd::init`. Several consumers, one read — which
/// is also what keeps `cmd::init` free of any `std::env` call of its own.
fn resolve_policy() -> Result<Resolved, String> {
    let vars: HashMap<String, String> = std::env::vars().collect();
    let cwd = std::env::current_dir().ok();
    // ⚠ The ORDER — refuse, resolve the directory ONCE, load — belongs to `vike-boot`, not to this
    // file. Four other composition roots run the same sequence, each used to carry its own copy of
    // it, and the walk happening in five places is what made the CI box's "no policy, no credentials,
    // every venue silently paper" expensive to fix. The three arms below are the ways THIS root
    // genuinely departs, each stating its reason where a diff can see it.
    let booted = vike_boot::boot(&vike_boot::BootSpec {
        env: &vars,
        cwd: cwd.as_deref(),
        identity: vike_boot::Identity {
            name: env!("CARGO_PKG_NAME"),
            version: env!("CARGO_PKG_VERSION"),
        },
        removed_env: vike_boot::RemovedEnv::Refuse,
        settings: vike_boot::SettingsLoad::Load,
        credentials: vike_boot::Credentials::Deferred(
            "the credential store is opened ONLY for the node-facing surfaces (trade, mcp, \
             strategy-status), inside their own dispatch arms — see `node_keyring`. A provenance \
             or backtest command has no business opening the file that holds every venue key on \
             the box, and this crate must not link `vike_bridge_core`'s transport stack to read \
             it.",
        ),
        log_home: vike_boot::LogHome::Elsewhere(
            "this CLI builds no log subscriber at all: it is a short-lived command whose stdout is \
             a protocol under `mcp`, and everything it has to say goes to stderr.",
        ),
        disclosure: vike_boot::Disclosure::Skip(
            "`boot_lines` re-reads every settings file to recover each row's ORIGIN, which is a \
             daemon's one-off cost and a command-line tool's per-invocation one. `vike-cli config \
             show` is the surface that prints all of it, on request.",
        ),
    })?;
    // ⚠ **NO BOOT ANCHOR is written here, and the exclusion is deliberate.** The two roots that
    // ENFORCE the ceilings — `vike-app` and `vike-tradehub` — each call
    // `vike_boot::journal_boot_settings(..)` once, appending one `boot_settings` line per start to
    // `vike_model::change_journal`. This one does not, for two reasons, and
    // `crates/vike-boot/tests/boot_journal_wiring.rs` is where the row carrying them lives (it
    // fails if this file quietly starts writing one).
    //
    // First, RATE: this function runs for EVERY subcommand, `secrets path` and `config show`
    // included, so the ledger's growth would track how often a human or an MCP client types a
    // command — unbounded, and uncorrelated with anything changing. That is the shape the change
    // journal's own module doc refuses for connectivity events: a per-invocation stream mixed into
    // a per-change ledger buries the ledger.
    //
    // Second, and worse, TRUTH: a `boot_settings` record claims the EFFECTIVE ceilings, and in this
    // process none of them is. `max_notional_per_order` is an advisory guardrail on two surfaces
    // (`trade`, `mcp`); the other two govern a venue mount this binary never performs.
    //
    // What is LOST is real and worth naming: on a box where `vike-cli` is the only vike binary that
    // ever runs, no anchor is ever written, so a hand edit of `policy.toml` there is bracketed by
    // nothing. `vike-cli config show` still PRINTS the effective values and their origin on
    // demand — it just does not durably record them.
    //
    // WHICH rung answered — see `Resolved::settings_dir_origin`. Derived from the same override
    // value the resolver was handed (`vike-boot` returns it already trimmed and blank-filtered), so
    // the two cannot disagree about a blank one.
    let settings_dir_origin = cmd::config_check::dir_origin(
        booted.settings_dir_override.as_deref(),
        booted.settings_dir.as_deref(),
    );
    // `<project>/user_data` — the SIBLING of the settings directory, off the SAME walk (see
    // `Resolved::user_data_dir`). Resolved here, in the one place this crate reads the environment,
    // so `cmd::init` takes it as a parameter and names no variable of its own.
    //
    // ⚠ Literally the same walk now, not merely the same rules: it is `user_data_dir_beside` over
    // the directory `vike_boot::boot` ALREADY resolved. `project_user_data_dir_from`, which stood
    // here, falls back to a walk of its own that does not honour `$VIKE_SETTINGS_DIR` — so
    // `vike-cli init` scaffolded into one project while `config show` reported another, on exactly
    // the deployments the override exists for.
    let user_data_dir = vike_model::state_path::user_data_dir_beside(
        vars.get("VIKE_USER_DATA_DIR").map(String::as_str),
        booted.settings_dir.as_deref(),
    );
    for line in settings_warning_lines(&booted.settings) {
        eprintln!("{line}");
    }
    Ok(Resolved {
        policy_max_notional: booted.settings.policy.max_notional_per_order,
        settings_dir: booted.settings_dir,
        settings_dir_origin,
        // Carried, not re-read: `cmd::secrets`' fallback rung needs the VALUE and not just the
        // origin verdict above — see `Resolved::settings_dir_override`.
        settings_dir_override: booted.settings_dir_override,
        user_data_dir,
        env: vars,
    })
}

/// Resolve the two vike-tradehub NODE KEYS for the surface about to run: the process environment
/// first, then `<project>/settings/secrets.env` — the store `vike-tradehub` itself loads them from.
///
/// Called ONLY from the node-facing arms of [`dispatch`] — `trade`, `mcp` and the read-only
/// `strategy-status` (which uses just the keyring's observe half). Every other subcommand keeps
/// the file unopened: the least credential exposure that still fixes the defect.
///
/// A store that EXISTS but cannot be read is reported on **stderr** (never stdout — `mcp`'s stdout
/// is a protocol) and treated as EMPTY rather than fatal. Deliberate: the node keys are two entries
/// in a file that mostly holds venue credentials, an exported key must still work when that file is
/// broken, and the "no key anywhere" message this then produces NAMES the store — so the operator
/// is pointed at the same file either way, twice. There is no silent-wrong-credential hazard to
/// weigh against that: a key that does not resolve cannot open a connection at all.
fn node_keyring(resolved: &Resolved) -> NodeKeyring {
    let store_path = resolved.settings_dir.as_ref().map(|d| d.join(vike_secrets::SECRETS_FILE));
    let store = match &store_path {
        // `vike_secrets::resolve` takes the path the CALLER names — an absent file is an empty
        // answer, not an error (absent credentials ARE the live gate).
        Some(path) => match vike_secrets::resolve(path) {
            Ok(r) => {
                if let Some(w) = &r.warning {
                    // A permissions finding — a path and an octal mode, never a credential.
                    eprintln!("vike-cli: ⚠ {w}");
                }
                r.secrets.into_map()
            }
            Err(e) => {
                eprintln!("vike-cli: cannot read the credential store: {e}");
                HashMap::new()
            }
        },
        None => HashMap::new(),
    };
    cmd::nodekeys::resolve(&resolved.env, &store, store_path.as_deref().map(Path::new))
}

/// Every non-fatal resolution the loader made, formatted for stderr — the PURE half of
/// [`resolve_policy`]'s surfacing step.
///
/// A function rather than an inline loop because "the binary that loaded the settings SURFACES the
/// warnings, never swallows them" is a real property with a real failure mode, and a property worth
/// stating is worth gating — see `a_clamp_warning_is_surfaced_not_swallowed`.
fn settings_warning_lines(settings: &vike_config::Settings) -> Vec<String> {
    settings.warnings.iter().map(|w| format!("vike-cli: settings: {w}")).collect()
}

/// Route one subcommand to its module. A top-level `--help`/`-h`/`help` prints the command list and
/// succeeds; `--version`/`-V` prints the version and succeeds; an unknown command prints help to
/// stderr and fails.
///
/// [`Resolved::policy_max_notional`] reaches only the two ORDER-WRITE surfaces, whose mandatory
/// preview shows an advisory guardrail against it; every other subcommand has no order to size.
/// [`Resolved::settings_dir`] reaches `secrets`, which inspects the credential store inside it,
/// `trade`, which persists its REPL history under its `state/` sub-directory, and `config`, which
/// reports the whole directory — the files in it, what each setting resolved to, and which layer
/// set it. `config` takes it as a PARAMETER rather than re-deriving it so the directory it prints
/// is provably the one every other surface reads, and takes
/// [`Resolved::settings_dir_origin`] alongside it for the same reason: only this root can say which
/// rung answered. `secrets` takes [`Resolved::settings_dir_override`] as well — its fallback rung
/// resolves the store the way every credential reader on the box does, so that command can never
/// print a file nothing opens. (Since the boot honours a name with no walk, a `None` directory now
/// implies a `None` override too, so that rung is unreachable from here; it is kept as the belt
/// against a regression, and `cmd::secrets`'s `store_path` argues it at its site.)
/// [`Resolved::user_data_dir`] — that directory's
/// SIBLING — reaches `init` alone, the only subcommand that writes to the project. The node KEYS
/// ([`node_keyring`]) reach the node-facing surfaces only — the two ORDER-WRITE ones (`trade`,
/// `mcp`) plus the read-only `strategy-status`, which uses just the observe half — and are
/// resolved inside their arms so no other subcommand opens the credential file at all.
fn dispatch(command: &str, args: impl Iterator<Item = String>, resolved: &Resolved) -> ExitCode {
    match command {
        "backtest" => cmd::backtest::run(args),
        "sweep" => cmd::sweep::run(args),
        "walkforward" => cmd::walkforward::run(args),
        "config" => {
            cmd::config::run(args, resolved.settings_dir.as_deref(), resolved.settings_dir_origin)
        }
        // The two ORDER-WRITE surfaces. A node key means the credential store is opened for them
        // (see [`node_keyring`]) — as it is for `strategy-status` below, the one READ verb that
        // also authenticates to a node.
        "mcp" => cmd::mcp::run(args, resolved.policy_max_notional, &node_keyring(resolved)),
        "trade" => cmd::trade::run(
            args,
            resolved.policy_max_notional,
            resolved.settings_dir.clone(),
            &node_keyring(resolved),
        ),
        // The read-only node question (split-plane B4). It authenticates to a node, so it is the
        // THIRD arm that resolves the keyring — and the only one of the three that uses just the
        // OBSERVE half (a control key cannot open a read; the node verifies per scope).
        "strategy-status" => cmd::strategy_status::run(args, &node_keyring(resolved)),
        // BOTH the resolved directory and the override that produced it — see
        // `Resolved::settings_dir_override` for why the second is not redundant.
        "secrets" => cmd::secrets::run(
            args,
            resolved.settings_dir.as_deref(),
            resolved.settings_dir_override.as_deref(),
        ),
        // The ONE subcommand that writes to the project, and it writes ONLY under `user_data/` —
        // never into `settings/`, and it opens no credential.
        "init" => cmd::init::run(args, resolved.user_data_dir.as_deref()),
        // Takes NOTHING from the dispatcher: the callable indicator roster is a property of the
        // BUILD (`vike_script::RHAI_INDICATORS` joined onto `vike_indicators::registry()`), never
        // of the project, the environment or a credential.
        "indicators" => cmd::indicators::run(args),
        "-h" | "--help" | "help" => {
            print_help();
            ExitCode::SUCCESS
        }
        "-V" | "--version" => {
            print_version();
            ExitCode::SUCCESS
        }
        other => {
            eprintln!("vike-cli: unknown command '{other}'");
            print_help();
            ExitCode::FAILURE
        }
    }
}

/// List the binary's usage and its registered subcommands (to stdout — it is normal output for the
/// `help` path; the unknown-command path prints its own error to stderr first).
fn print_help() {
    println!("vike-cli — the unified vike command-line surface");
    println!();
    println!("usage: vike-cli <command> [args…]");
    println!();
    println!("commands:");
    for (name, summary) in COMMANDS {
        println!("  {name:<12} {summary}");
    }
    println!();
    println!("run `vike-cli <command> --help` for a command's own options");
    println!("run `vike-cli --version` to print this build's version");
}

/// `<name> <version> (<build identity>)`, on stdout — the shape every `--version` on the box
/// already prints (`git version 2.x`, `cargo 1.x`) with this build's PROVENANCE appended, so a
/// packaging script or a bug report can read it without knowing anything about this binary.
///
/// Both spellings are accepted (`--version` and the conventional short `-V`, never `-v`, which is
/// verbosity everywhere else). Until they were added neither was recognised: they fell into the
/// unknown-command arm and exited 1 with `unknown command '--version'` on stderr — the same class
/// of defect as a `--help` that fails, and the first thing an installer probes.
///
/// The parenthesised half — `vike_buildinfo::version_line` — answers the question a bare version
/// number cannot: WHICH COMMIT is this? A release binary was once built on a test clone from a bare
/// repo four commits behind `main` and nearly installed on the live recorder;
/// `crates/vike-buildinfo/src/lib.rs` carries that incident, and
/// `crates/vike-buildinfo/tests/identity_adoption.rs` is the gate that keeps every `--version` in
/// the tree answering it.
fn print_version() {
    println!("{}", vike_buildinfo::version_line(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION")));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A loader warning is SURFACED, never swallowed. [`resolve_policy`] emits exactly what
    /// [`settings_warning_lines`] returns, so proving a warning survives that step proves the
    /// dispatcher prints it — the failure this guards is the loader resolving something the
    /// operator did not write while they believe their own file is in force. (Warnings were
    /// swallowed here until Phase 6c.)
    ///
    /// ⚠ The warning is HAND-STUFFED, which it deliberately was not before. This drove
    /// `Settings::clamp_to_policy` — `policy.rate.max_utilization` over
    /// `preferences.rate_utilization` — and that clamp was removed with both of its fields: a
    /// ceiling that bounded a value nothing read. `Settings::warnings` therefore has no producer
    /// today (its doc argues why the CHANNEL is kept regardless), so there is nothing left to build
    /// one through. What this still gates is the half that lives in THIS crate and was the actual
    /// Phase-6c defect: a warning that exists reaches the operator, verbatim.
    #[test]
    fn a_loader_warning_is_surfaced_not_swallowed() {
        let mut settings = vike_config::Settings::default();
        settings.warnings.push("preferences.something was resolved to 0.5".to_string());

        let lines = settings_warning_lines(&settings);
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].contains("preferences.something"), "names the key: {}", lines[0]);
        assert!(lines[0].contains("0.5"), "carried verbatim, not summarised: {}", lines[0]);
    }

    /// The quiet path stays quiet: nothing to resolve ⇒ nothing printed, so a line on stderr always
    /// means something actually happened. Load-bearing for `vike-cli mcp`, whose stdout is a
    /// protocol and whose stderr an agent may still read.
    #[test]
    fn a_clean_load_prints_nothing() {
        assert!(settings_warning_lines(&vike_config::Settings::default()).is_empty());
        let settings = vike_config::load(None, &HashMap::new()).unwrap();
        assert!(settings_warning_lines(&settings).is_empty(), "no files ⇒ nothing to resolve");
    }
}
