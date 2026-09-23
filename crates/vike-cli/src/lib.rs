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
//! `trade`, or an interactive `repl` later is: add the module, add one arm to
//! [`dispatch`], add one line to [`COMMANDS`]. The dispatcher itself never grows command logic —
//! it only routes. REMOVING one is the mirror image plus a row in [`RETIRED_COMMANDS`], so the
//! spelling that is going away fails naming its replacement rather than reading as a typo.
//!
//! # The exit ladder
//!
//! Every subcommand returns a rung of [`exit::Exit`] rather than the old success-or-failure pair:
//! `0` did it, `1` ran and failed, `2` the command line was wrong, `3` a service could not be
//! reached, `6` a declared threshold was breached, `7` nothing was evaluated. `0` and `1` mean
//! exactly what they always did, so nothing written against the old behaviour changes — the rest is
//! a subdivision of what used to be `1`, which is what lets a wrapper retry a timeout without
//! retrying a typo, and lets a CI step tell a failed gate from a gate that checked nothing.
//!
//! ⚠ [`exit::Exit`] also declares `4` (refused locally by a ceiling) and `5` (the far side
//! rejected the order), and **no code path in this crate produces either yet** — they are RESERVED
//! numbers, not behaviour, and a script may not branch on them today. [`exit`] argues every rung
//! and carries what has to exist before those two become live;
//! `crates/vike-cli/tests/exit_codes.rs` asserts the six that ARE live over the shipped binary,
//! which is the only place a rung is observable at all.
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
//! performs it for the node-facing arms (`trade` and `mcp`, plus the top-level read-only
//! `report`). `trade` covers the read-only `trade status` too, which uses just the observe half —
//! it was a top-level `strategy-status` with a dispatch arm of its own until ruling 17 folded it
//! into the `trade` family, which is why `report` is now the only top-level verb here whose whole
//! job is a node read.
//!
//! ⚠ **There are TWO node key pairs, and the DATAHUB pair had the identical defect** — resolved
//! from the process environment and nowhere else, so a pair sitting in the credential store, which
//! is where this binary's own refusal text sends an operator, did nothing at all.
//! [`datahub_keyring`] fixes it with the same precedence and carries the measurement.
//!
//! ⚠ **This widened which arms open the credential store, and the rule that used to sit here is
//! the thing that changed.** It read: "no other subcommand needs a credential, and a provenance or
//! backtest command has no business opening the file that holds every venue key on the box." The
//! first half is simply no longer true — `backtest`, `data` and `mcp` all
//! dial a datahub that may require authentication, and `research study` dials the COMPUTE daemon
//! under the
//! same node pair (ruling 7 puts every verb that RUNS an engine there, so it is a different daemon
//! reached with the same credential — the store read is what it has in common with the five, not
//! the peer). The second half was written when
//! nothing in this binary could authenticate to one, and a `backtest --addr` is a NODE-FACING
//! invocation, the category that sentence already excepted. So the rule now reads: **an arm that
//! can dial a node may open the store; one that cannot, may not.** `config`, `secrets`, `init`
//! and `indicators` still never touch it.
//!
//! ⚠ It remains genuinely true that a low-sensitivity credential now causes a high-sensitivity file
//! to be read, and the industrial answer to that is to SEPARATE the classes — a credential helper,
//! or an OS keyring — not to leave a resolution path that cannot work. That is a larger change than
//! a bug fix and belongs in `docs/decisions/`, against the "ONE store, no chain, no second
//! location" rule it would have to argue with. Recorded here so the next reader knows the shape was
//! considered rather than missed.
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
pub mod exit;
pub mod surface;

use crate::cmd::nodekeys::NodeKeyring;
use crate::exit::Exit;

/// The registered subcommands, as `(name, one-line summary)` — the single source of truth for both
/// the help text and the "unknown command" hint, so a new command shows up in help for free.
const COMMANDS: &[(&str, &str)] = &[
    (
        "backtest",
        "compute a strategy over history and judge what came back: `backtest run` runs one on a \
         remote COMPUTE daemon — `vike-backend backtest --addr`, config.backtest_addr — or \
         --local, and the PROFILE says what — a [paramscan] grid \
         searches the parameter space, a [walkforward] table walks it forward, and declaring both \
         ships the profile whole to the walk-forward runner. Then `backtest ls|show|path` over the \
         run directory, `tag` to name a run, `diff` to see what moved between two, `gate` to turn \
         that into a CI exit code, and `params` and `strategies` for what can be tuned and what \
         can be run. Before a strategy exists: `templates` ships starters, `script-api` prints \
         everything a Rhai script may call, and `script-check` compiles one and answers in the \
         exit code",
    ),
    (
        "data",
        "the hist store: fetch real bars or seed the demo tape into it (write, local), list what \
         it holds and report coverage (read, over --addr)",
    ),
    (
        "config",
        "this box's settings: provenance (`show`), a validating pre-flight (`check`), and a \
         journalled one-key write (`set`)",
    ),
    ("mcp", "serve the create+backtest tools over stdio MCP (for Claude / an agent)"),
    (
        "trade",
        "observe + control a running vike-tradehub node: `trade status|halt|resume` run one \
         operation and exit, and a bare `trade` opens the interactive REPL (for a human)",
    ),
    // ⚠ **ONE summary states a REFUSAL now, and it used to be TWO.** That is not hedging — it is
    // the same rule the registry gates for a settings key: a line describing behaviour the build
    // does not have hands the reader positive confirmation of something false. `report` and
    // `research`'s one sub-verb `study` both shipped as client halves whose server arms were
    // follow-ups of ruling 16, so both refused on every box and both said so here.
    //
    // ⚠ **THE STUDY CLAUSE IS GONE BECAUSE THE ARM LANDED**, which is exactly what this comment
    // said would retire it — *the clause goes when the arm lands*. Stage 7 of
    // `docs/superpowers/specs/2026-09-12-backtest-cli-surface-design.md` added
    // `vike_datahub_client::proto`'s `Request::RunStudy` / `Response::StudyReport` and the compute
    // daemon's arm for it, under ruling 1 — a whole refusing PLANE being a different proposition
    // from a refusing verb. ⚠ The daemon serves it only when it MOUNTED a study runner
    // (`vike_backtest::compute_server`'s `StudyRunFactory`), so a refusal still exists and
    // `cmd::study`'s `no_capability_lines` still names `vike-backend study` — but *no backend
    // serves it yet* is false, and leaving it was the same false confirmation one paragraph up.
    // The withdrawn clause is recorded here rather than silently dropped.
    //
    // ⚠ The sentence that read *the compute daemon it dials is ruling 7's half and does not exist
    // yet* is likewise spent: `crates/vike-backtest/src/compute_server.rs` IS that daemon and
    // `deploy/vike-backtest.service` ships it. Which refusal an operator meets is now a property of
    // their DEPLOYMENT (a box that stood the daemon up reaches the capability negotiation; one that
    // did not ends at the dial), not of the build — `cmd::study`'s module doc carries both arms and
    // `crates/vike-cli/tests/study_report_refusal_cli.rs` drives them.
    //
    // ⚠ The sentence that read *`report`'s half is still open-ended: its wire verb shipped, its
    // RENDERER did not* is spent too, and it was the LAST of seven places in this tree asserting
    // that world. `crates/vike-tradehub/src/server.rs`'s `tearsheet_reply` renders the live
    // tearsheet from the journal that daemon is writing, and its `served_features` pushes the
    // capability unconditionally — pinned by that file's
    // `the_tearsheet_capability_is_advertised_now_that_the_arm_serves_it`. As with `study` one
    // paragraph up, the REFUSAL survives and is still exact for an OLDER daemon; what is false is
    // that every node meets it. The withdrawn clause is recorded here rather than silently
    // dropped, because this array is the thing three shipped pages are rendered from.
    //
    // ⚠ And `skills/*/SKILL.md`'s verb tables are RENDERED from this array by
    // `scripts/gen_skills.sh`, so a false line here is a false line in three shipped,
    // agent-consumed pages — and `--check` cannot catch it, because it proves the pages MATCH this
    // array, never that this array is TRUE.
    (
        "report",
        "ask a vike-tradehub node for a tearsheet over its live journal, or re-render a \
         FINISHED run from this machine's own run directory (read-only; a node built from this \
         tree serves the live verb, an older one refuses and names the command that does)",
    ),
    (
        "research",
        "investigate a signal and FIT a model — the plane BEFORE a strategy exists: \
         `research study` asks the backend to run a compiled study over the hist store it holds \
         (served by `vike-backend backtest --addr` when it mounts the study runner; a peer that \
         does not refuses by name and points at `vike-backend study`)",
    ),
    (
        "secrets",
        // ⚠ It names BOTH stores because this line is read by an operator deciding where to look,
        // and `docs/decisions/0054`'s credential half made `the store` a per-BOX answer rather than
        // a path: naming the file alone sent a migrated box's operator to edit something nothing
        // reads. `secrets path` is named because it is the only honest way to learn which one
        // answers here, and it is a real subcommand of this verb's own USAGE.
        //
        // ⚠ NO DOUBLE QUOTE may appear in a comment inside this array. `scripts/gen_skills.sh`
        // harvests every string LITERAL in this region and pairs them in order to render three
        // shipped skills' verb tables — it does not strip comments — so a quoted phrase in one
        // makes the count odd and the generator refuses to render at all. Measured twice while
        // writing the paragraph above, the second time inside the warning about the first.
        "inspect the credential store THIS box reads — <project>/settings/secrets.env, or the \
         settings database <project>/settings/db/vike.db once migrated, which `secrets path` \
         reports — set ONE key in it, and perform that migration \
         (list | path | template | set | migrate)",
    ),
    (
        "backend",
        "stand a vike-tradehub node — the running daemon — up, and attach this box to one \
         (setup on the daemon | connect | status | disconnect on the client)",
    ),
    (
        "datahub",
        "MINT the node key pair a vike-datahub server authenticates with (setup), on that \
         server's box",
    ),
    ("init", "create <project>/user_data — strategies, profiles, results — with examples"),
    ("indicators", "print the indicators a Rhai strategy can call, with their parameters"),
    (
        "surface",
        "write this binary's own command surface as JSON, for the documentation the docs site \
         generates rather than hand-writes. Reads nothing and dials nothing: the table is compiled \
         in, so the answer is a property of THIS build",
    ),
];

/// Verbs this binary USED to have, and the sentence each one's replacement is named in.
///
/// ⚠ **A retired verb is not an unknown verb, and answering it as one is the defect this closes.**
/// [`dispatch`]'s catch-all prints `unknown command 'sweep'` plus the help — which tells a scripted
/// caller the verb never existed, for a verb that shipped for months. The operator's own words are
/// the fastest route to the replacement, so they are matched BEFORE the catch-all and answered with
/// it.
///
/// It is `vike_config::REMOVED_ENV`'s shape applied to a VERB, and the same shape
/// `crates/vike-backtest/src/backtest_cli.rs`'s `parse_search_flags` gives the retired `--search`
/// flag. The rung is unchanged — [`Exit::Usage`], because re-running unchanged cannot succeed —
/// which is deliberately the SAME rung the catch-all uses: what changes is the message, not the
/// classification, and a test asserting only the code would not see the difference.
///
/// ⚠ These names are NOT in [`COMMANDS`]: help must not advertise a verb that refuses, and
/// `skills/*/SKILL.md`'s verb tables are RENDERED from that array by `scripts/gen_skills.sh`.
const RETIRED_COMMANDS: &[(&str, &str)] = &[
    (
        "sweep",
        "`vike-cli sweep` is retired — a parameter search is a BACKTEST, run many times and ranked, so \
     it is `vike-cli backtest run` with the same profile. A profile carrying a [paramscan] table \
     searches it; `--rank-by` picks the metric and `--optimizer grid|euler|tpe|genetic` picks the \
     method, on either route.\n\
     \n\
     was:  vike-cli sweep        --profile sweep.toml --rank-by sharpe\n\
     now:  vike-cli backtest run --profile sweep.toml --rank-by sharpe",
    ),
    (
        "walkforward",
        "`vike-cli walkforward` is retired — walking forward is how a run is VALIDATED, not a \
         different kind of run, so it is `vike-cli backtest run` with the same profile. The \
         profile's own [walkforward] table selects it: `n_splits` is the window count and \
         `mode`/`rank_by` shape it. There is no --local: the standalone engine has no walk-forward \
         driver to spawn.\n\
         \n\
         was:  vike-cli walkforward     --profile wf.toml\n\
         now:  vike-cli backtest run    --profile wf.toml",
    ),
    (
        "study",
        "`vike-cli study` is retired from the top level — a study fits a MODEL rather than \
         computing a strategy's PnL, so it belongs to the research plane and lives inside it: \
         `vike-cli research study`, with the same flags. ⚠ A backend serves the wire verb since \
         stage 7, but only when it MOUNTED a study runner: `vike-backend backtest --addr` does, a \
         bare `vike-backtest` bin cannot, and either refusal names `vike-backend study` — the one \
         thing that runs a study on the box that holds the store.\n\
         \n\
         was:  vike-cli study          --study NAME --recipe FILE --from WHEN --to WHEN\n\
         now:  vike-cli research study --study NAME --recipe FILE --from WHEN --to WHEN",
    ),
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
        // No subcommand: print help and exit on the USAGE rung. It is the same class as an unknown
        // verb — the command line was wrong and re-running it unchanged cannot succeed — and it
        // used to be indistinguishable from a run that tried and failed.
        print_help();
        return Exit::Usage.into();
    };
    // A REMOVED environment variable, or a settings tree that will not load. Neither is a usage
    // error (the command line was fine) and neither is a connect failure, so this stays on the
    // pre-existing catch-all rung: the box is misconfigured, and the message says how.
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
    /// **EITHER settings mark** — [`vike_config::Settings::seal_refusal`] (the store was read and
    /// says something illegal) or [`vike_config::Settings::store_refusal`] (it could not be read at
    /// all) — carried so [`dispatch`] can gate the verbs that ACT on a ceiling without the loader
    /// having refused every verb that REPAIRS one.
    ///
    /// One field for two marks because the two differ in what the OPERATOR must do and not in what
    /// this binary must do: in both states the values this process resolved are not reliably the
    /// ones a daemon on this box would resolve, and in both the repair is a verb that has to keep
    /// running. The message names which it was.
    ///
    /// ⚠ This is the field that makes the sentence in `resolve_policy`'s `settings:` arm TRUE. That
    /// comment has claimed "`trade` and `mcp` refuse on it where a repair verb does not" since the
    /// mark was introduced, and so has [`vike_config::Settings::store_refusal`]'s own doc — and
    /// until this field existed NOTHING in this crate read either one: both marks were printed as
    /// warnings and every verb ran. A claim two comments make about a gate that is not there is
    /// worse than no gate, because it is what a reader checks instead of the code.
    seal_refusal: Option<String>,
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
    /// `<project>/settings/state` — the PROGRAM-WRITTEN state root, off the SAME walk (it is
    /// `vike_boot::Booted::state_dir`, never re-derived: the `_from`-less resolvers are
    /// `$VIKE_SETTINGS_DIR`-BLIND, so a second walk could answer with a different project than the
    /// one the settings and credentials came from).
    ///
    /// Reaches the APPENDING arms — deliberately not counted here, since `config set` made the
    /// count wrong the day it landed and a count is the shape of claim this repository has watched
    /// rot. The arms that take it are visible in [`dispatch`] below, each one carrying its own
    /// reason. `secrets set` writes one
    /// `vike_model::change_journal` `credential_write` record beside the store write; `mcp --trace`
    /// writes the AGENT TRANSCRIPT into a sibling directory (`crate::cmd::mcp_trace`). `None` (no
    /// project above the working directory) means the first one still writes the store and records
    /// NOTHING — an append-only ledger in a guessed directory is worse than a counted absence, the
    /// same rule `vike_boot::journal_boot_settings` follows — while the second REFUSES to start,
    /// because there the operator ASKED for a record and a silent absence is what they would
    /// discover after the incident. `crate::cmd::mcp::resolve_trace` argues the asymmetry.
    state_dir: Option<std::path::PathBuf>,
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
    /// `<project>` itself — the PARENT of [`Resolved::settings_dir`], and the root the two
    /// machine-owned siblings hang off: `bin/` (where a project's tools are installed) and `tmp/`
    /// (where a rewritten profile is staged before it is handed to a child process).
    ///
    /// Resolved from the SAME walk as everything else, for the reason that walk exists — "which
    /// project am I in" must have ONE answer — and by the same `parent()` rule
    /// `vike_model::state_path::user_data_dir_beside` applies to the sibling beside it. ⚠ Neither
    /// of those two directories has an environment variable of its own, deliberately
    /// (`crates/vike-model/src/state_path.rs`'s `PROJECT_TMP_DIR` argues both halves), so unlike
    /// `user_data_dir` there is nothing to honour here but the parent.
    ///
    /// `None` when no project sits above the working directory: the `--local` engine then falls
    /// back to `PATH`, and a run that would need scratch REFUSES rather than reaching for the
    /// system temp directory.
    project_root: Option<std::path::PathBuf>,
    /// `config.node_addr` — THIS box's dial address for a running `vike-tradehub` node, and the
    /// default that makes `--node` optional. `None` = the key is unset, which is every box that has
    /// never attached to one.
    ///
    /// ⚠ It is the CLIENT's address, and its daemon-side twin `config.tradehub_addr` is deliberately
    /// NOT read here: that key is where a daemon BINDS, on the daemon's own box, and with a tunnel
    /// in front the two agree only by coincidence of the tunnel.
    /// `crates/vike-config/src/config.rs`'s `Config::node_addr` sorts every one of that file's
    /// addresses by whose box holds the value and whether that box listens or dials.
    ///
    /// Reaches the `backend` arm alone today. The verbs that still REQUIRE `--node`
    /// (`report`, `trade` — REPL and one-shot alike — and `mcp`) are a deliberately separate
    /// change: each parses the flag as mandatory, and making it optional is a per-verb migration
    /// rather than a dispatcher edit.
    node_addr: Option<String>,
    /// `config.backtest_addr` — THIS box's dial address for the COMPUTE daemon
    /// (`vike-backend backtest --addr`), and the middle rung of the ladder both planes fold:
    /// `--addr` → this
    /// → `vike_config::DEFAULT_BACKTEST_ADDR`. `None` = the key is unset, which is every box that
    /// has not been pointed at one.
    ///
    /// ⚠ Unlike [`Resolved::node_addr`] it is NOT a client-only key: the compute daemon BINDS the
    /// same value on its own box, because it runs beside the store it computes over and no tunnel
    /// separates the two. `crates/vike-config/src/config.rs`'s `Config::backtest_addr` argues why
    /// one key serves both ends where every other plane here needs a pair.
    ///
    /// Reaches the `backtest` arm and the `research` one (whose `study` sub-verb dials the same
    /// daemon), and it is resolved HERE rather than in either of
    /// them for the rule this whole struct exists for: only the composition root reads settings,
    /// and a `src/cmd/` file takes what it needs as a parameter. ⚠ Both fold it through ONE
    /// function, `crate::cmd::backtest::resolve_addr` — the two planes dial the same compute
    /// daemon and read the same setting, so a second copy of the fold would be the defect that
    /// function was written to remove: two copies could disagree about a blank rung and aim one at
    /// `7878` or `7879`.
    ///
    /// ⚠ It reached the `study` arm ALONE until 2026-09-13, and that arm RETIRED — a field written
    /// and never read is `dead_code` on a lane that runs `-D warnings`, so ruling 1's `research`
    /// plane supplying a successor reader is load-bearing rather than incidental.
    backtest_addr: Option<String>,
    /// `config.datahub_addr` — THIS box's dial address for the DATA server, and the middle rung of
    /// the ladder the datahub dialers fold: `--addr` → this → the verb's own compiled-in default.
    /// `None` = the key is unset, which is every box that has not been pointed at one.
    ///
    /// ⚠ Unlike [`Resolved::backtest_addr`] this one IS client-only, and the distinction is the
    /// whole reason there are two keys: the data server binds `config.datahub_advertise_addr` on
    /// its own box, while this names where a client DIALS — across a tunnel, if that is how the
    /// operator reaches it. `crates/vike-config/src/config.rs`'s `Config::datahub_addr` argues the
    /// split.
    ///
    /// Reaches the `data` arm and the `mcp` one (whose `list_series` and `delete_series` are the
    /// two tools on the data plane), for the rule this whole struct exists for: only the
    /// composition root reads settings, and a `src/cmd/` file takes what it needs as a parameter.
    ///
    /// ⚠ It reached NEITHER until this field existed, and the shape of that hole is worth keeping:
    /// `crates/vike-desktop/src/app_methods.rs` read the key and was the ONLY reader, so
    /// `vike_config::CONSUMPTION` was satisfied by one consumer while every CLI dialer sat on its
    /// own compiled-in `127.0.0.1:7878`. On a box whose datahub is anywhere else, the GUI reached
    /// it and `vike-cli data` did not — two clients, one setting, two answers, and the failure
    /// reads as "the datahub is down" rather than "I dialled the wrong place". It is the exact
    /// defect [`Resolved::backtest_addr`] was threaded here to close on the compute plane, one
    /// plane over.
    datahub_addr: Option<String>,
    /// The process environment, kept because [`node_keyring`] and [`datahub_keyring`] need it and it
    /// is already collected. The dispatcher owns this map; nothing below it reads `std::env` for a
    /// node key.
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
        // ⚠ REPORT, and this is the arm the whole design turns on. `resolve_policy` runs for EVERY
        // subcommand, so a refusal here would take down `config mirror`, `config adopt --undo`,
        // `config check` and every `secrets` verb — the exact commands whose names the refusal
        // prints. That is the JSON incident of 2026-09-18 in one line: its first implementation
        // refused a row it could not read, a stale Windows path took every binary down on upgrade,
        // and `vike-cli config mirror` went with them. The mark rides `Settings::store_refusal`,
        // and `trade`/`mcp` refuse on it where a repair verb does not.
        settings: vike_boot::SettingsLoad::Load,

        credentials: vike_boot::Credentials::Deferred(
            "the credential store is opened ONLY for the node-facing surfaces (trade — the REPL \
             and the one-shot status/halt/resume alike — mcp, and the top-level read-only \
             report), inside their own dispatch arms — see `node_keyring`. A provenance \
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
    // `<project>` — see `Resolved::project_root`. The empty-parent filter is the same one
    // `user_data_dir_beside` applies: a relative `settings` has `""` as its parent, and joining a
    // sibling onto that would silently name a directory in the working directory.
    let project_root = booted
        .settings_dir
        .as_deref()
        .and_then(Path::parent)
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf);
    Ok(Resolved {
        policy_max_notional: booted.settings.policy.max_notional_per_order,
        // BOTH marks, and the `or` is what closes the one hazard the store-unreadable arm's own
        // degrade argument does not cover. That argument is *an unreadable settings store ALWAYS
        // co-occurs with an all-paper mount*, because `vike_secrets::Backend` decides both on one
        // file probe — true for a whole-store failure (a rollback journal, a schema out of range, a
        // corrupt header). It is NOT true for a failure isolated to the `setting` TABLE:
        // `read_settings` issues per-table queries, so `SELECT … FROM setting` can return
        // `SQLITE_CORRUPT` while the `credential` table reads perfectly — venues arm LIVE off
        // credentials that loaded, and the ceilings fall back to the files with `adoption` `None`,
        // which also skips the drift block, so not even a per-key warning is printed. Refusing the
        // two ACTING verbs costs a healthy box nothing and removes that corner.
        seal_refusal: booted
            .settings
            .seal_refusal
            .clone()
            .or_else(|| booted.settings.store_refusal.clone()),
        settings_dir: booted.settings_dir,
        settings_dir_origin,
        // The boot's OWN answer, not a second derivation — see `Resolved::state_dir`.
        state_dir: booted.state_dir,
        // Carried, not re-read: `cmd::secrets`' fallback rung needs the VALUE and not just the
        // origin verdict above — see `Resolved::settings_dir_override`.
        settings_dir_override: booted.settings_dir_override,
        user_data_dir,
        project_root,
        // This box's dial default for a node — read off the SAME loaded `Settings` the policy
        // ceiling above comes from, so `vike-cli config show` and the `backend` verbs can never
        // disagree about which node this box is pointed at.
        node_addr: booted.settings.config.node_addr.clone(),
        // …and this box's dial default for the COMPUTE daemon, off that same loaded `Settings` for
        // the same reason. `study` is its only reader today; `config show` reports it either way,
        // which is precisely why it must be READ somewhere — see `vike_config::CONSUMPTION`.
        backtest_addr: booted.settings.config.backtest_addr.clone(),
        // …and this box's dial address for the DATA server, off that same loaded `Settings`. It is
        // the CLIENT half of the datahub pair — see the field for why the server's own bind address
        // is a different key, and for what a box whose datahub is not on the compiled-in default
        // used to get from the CLI while the GUI reached it.
        datahub_addr: booted.settings.config.datahub_addr.clone(),
        // ⚠ The datahub KEY PAIR is NOT resolved here any more. It was, off this same sweep, and that
        // read the PROCESS ENVIRONMENT AND NOTHING ELSE — so keys sitting in the credential store,
        // which is where this binary's own refusal text tells an operator to put them, did nothing.
        // It is now [`datahub_keyring`], lazily, env first and store second: the precedence
        // `cmd::nodekeys::resolve` already applies to the TRADEHUB pair.
        env: vars,
    })
}

/// The NODE-key store — `<project>/settings/node.env`, falling back to the credential store for a
/// pair that has not been moved yet, and SAYING SO when it does.
///
/// A store that EXISTS but cannot be read is reported on **stderr** (never stdout — `mcp`'s stdout
/// is a protocol) and treated as EMPTY rather than fatal. Deliberate: an exported key must still
/// work when the file is broken, and the "no key anywhere" message this then produces NAMES the
/// store — so the operator is pointed at the same file either way, twice. There is no
/// silent-wrong-credential hazard to weigh against that: a key that does not resolve cannot open a
/// connection at all.
///
/// ⚠ The warning is emitted here rather than inside `vike-secrets` because that crate carries no
/// logging dependency and returns findings as DATA — the same division `permission_warning` already
/// has. It goes to stderr, never stdout, because `vike-cli mcp`'s stdout is a protocol.
///
/// ⚠ It is printed ONCE PER RESOLUTION, not once per key. Five arms resolve keys and two pairs
/// exist; a per-key notice would put four identical lines in front of an operator who has one thing
/// to do.
///
/// ⚠ **`is_node_key` is the CALLER's own family, never the four-name `is_platform_key`, and this
/// binary is the one that HAS to get that right because it is the one that WRITES both files.**
/// `resolve_node_keys` answers *which file*, and the caller then reads its own pair out of it — so
/// with the wide predicate, a box where `vike-cli datahub setup` had written `node.env` made that
/// file the answer for the TRADEHUB pair too, and a working tradehub pair still in `secrets.env`
/// resolved to nothing: `trade`/`report`/`mcp` then signed with no key and the node answered
/// `bad mac`, with no migration notice, because the source was `NodeFile` rather than the legacy
/// one. Both of this function's callers name their own family
/// (`vike_model::credential_keys::is_tradehub_node_key` / `is_datahub_node_key`), which is decision
/// 0051's "answers wholly" scoped to the pair it is actually about.
fn node_key_store(
    resolved: &Resolved,
    is_node_key: impl Fn(&str) -> bool,
) -> HashMap<String, String> {
    let settings = resolved.settings_dir.as_deref().and_then(|p| p.to_str());
    match vike_secrets::resolve_node_keys(settings, is_node_key) {
        Ok((r, source)) => {
            if let Some(w) = &r.warning {
                eprintln!("vike-cli: ⚠ {w}");
            }
            if source == vike_secrets::NodeKeySource::LegacyCredentialStore {
                let dir = resolved
                    .settings_dir
                    .as_ref()
                    .map_or_else(|| "<project>/settings".to_string(), |d| d.display().to_string());
                eprintln!("vike-cli: ⚠ {}", vike_secrets::legacy_node_key_notice(&dir));
            }
            r.secrets.into_map()
        }
        Err(e) => {
            eprintln!("vike-cli: cannot read the node-key store: {e}");
            HashMap::new()
        }
    }
}

/// Resolve the two vike-tradehub NODE KEYS for the surface about to run: the process environment
/// first, then `<project>/settings/node.env` — the store `vike-tradehub` itself loads them from.
///
/// Called ONLY from the node-facing arms of [`dispatch`] — `trade` (the REPL, and the one-shot
/// `status`/`halt`/`resume`) and `mcp`, plus the top-level read-only `report`. The two READS among
/// those — `trade status` and `report` — use just the keyring's observe half. Every other
/// subcommand keeps the file unopened: the least credential exposure that still fixes the defect.
fn node_keyring(resolved: &Resolved) -> NodeKeyring {
    // ⚠ The PATH handed on is the NODE store's, and which file it names is the whole question this
    // branch exists to answer. `cmd::nodekeys::resolve` puts it in the "where would I have found
    // this" message an operator with NO key reads — and that operator has nothing to migrate, so
    // naming `secrets.env` would send them to write a key into the deprecated file and then be told
    // by the notice below to move it. `backend setup` writes `node.env`; the refusal names
    // `node.env`; the two mouths of this binary say one thing. A box that HAS a legacy pair never
    // sees this message at all — its keys resolve, and the migration notice is what it gets
    // instead.
    let store = node_key_store(resolved, vike_model::credential_keys::is_tradehub_node_key);
    let store_path = resolved.settings_dir.as_ref().map(|d| d.join(vike_secrets::NODE_FILE));
    cmd::nodekeys::resolve(&resolved.env, &store, store_path.as_deref().map(Path::new))
}

/// The DATAHUB node pair — `VIKE_DATAHUB_OBSERVE_KEY` / `VIKE_DATAHUB_CONTROL_KEY` — resolved
/// PROCESS ENVIRONMENT FIRST, CREDENTIAL STORE SECOND.
///
/// ⚠ **This used to be a field resolved off the environment sweep alone, and that was the same
/// defect this file had already found and fixed for the TRADEHUB pair.** The module doc above
/// records that one: both node keys "used to read `std::env::var` and nothing else, so a
/// correctly-configured box answered 'nothing to do' and exited". The datahub pair was written to
/// the same shape afterwards, its doc comment claiming it worked "exactly as `node_keyring` does" —
/// which it did not. MEASURED 2026-09-08 against a keyed datahub: the pair in
/// `<project>/settings/secrets.env` gave "no node keys were supplied", and the identical pair
/// exported into the environment served 6.8 billion rows.
///
/// The worst part was the refusal text, not the resolution: `vike-datahub-client`'s message names
/// the credential store as the remedy, and following it exactly left an operator broken. A refusal
/// that names the wrong remedy is more expensive than one that names none.
///
/// ⚠ The store read is why this is a FUNCTION rather than a field: it opens the file holding every
/// venue key on the box, and only the arms that can dial a datahub call it. That is a
/// deliberate widening of the rule the module doc states — "a provenance or backtest command has no
/// business opening" that file — and the argument for it is that a `backtest --addr` IS a
/// node-facing invocation, the category the rule already excepts for `trade`/`mcp`. The narrower
/// alternative (read the store only when the invocation names a remote) was measured and REFUSED:
/// `cmd::data`'s address resolves to `DEFAULT_ADDR` whether or not `--addr` was passed, and
/// `vike_config::Config::datahub_addr` is a second way to be remote with no flag at all, so "did
/// this command go remote" cannot be answered from the argv the dispatcher holds.
///
/// The environment still WINS, so a box that exports the pair opens no file at all.
fn datahub_keyring(resolved: &Resolved) -> Option<vike_node_proto::auth::NodeKeys> {
    if let Some(keys) = vike_node_proto::auth::node_keys_from_vars(&resolved.env) {
        return Some(keys);
    }
    // ⚠ The NODE store, not the credential store. Before 2026-09-08 this read `secrets.env`, which
    // meant an arm needing a low-sensitivity key opened the file holding 168 venue secrets. It now
    // reads `node.env` and falls back to the old file only while a box has not migrated, saying so.
    let store = node_key_store(resolved, vike_model::credential_keys::is_datahub_node_key);
    vike_node_proto::auth::node_keys_from_vars(&store)
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
/// set it — and, since `config set` landed, WRITES one key back into it. `config` takes it as a
/// PARAMETER rather than re-deriving it so the directory it prints and edits is provably the one
/// every other surface reads, and takes
/// [`Resolved::settings_dir_origin`] alongside it for the same reason: only this root can say which
/// rung answered. `secrets` takes [`Resolved::settings_dir_override`] as well — its fallback rung
/// resolves the store the way every credential reader on the box does, so that command can never
/// print a file nothing opens. (Since the boot honours a name with no walk, a `None` directory now
/// implies a `None` override too, so that rung is unreachable from here; it is kept as the belt
/// against a regression, and `cmd::secrets`'s `store_path` argues it at its site.)
/// [`Resolved::user_data_dir`] — that directory's
/// SIBLING — reaches `init` alone, the only subcommand that writes to the project. The node KEYS
/// ([`node_keyring`]) reach the node-facing surfaces only — the two ORDER-WRITE ones (`trade`,
/// `mcp`) plus the two read-only ones, `trade status` and `report`, which use just the observe
/// half — and are resolved inside their arms so no other subcommand opens the credential file at
/// all.
/// The verbs that REFUSE to run while [`Resolved::seal_refusal`] is set, as against every other
/// verb in this binary, which runs.
///
/// ⚠ **The list is of ACTORS, and the test that matters is the one asserting the COMPLEMENT.** A
/// gate like this is satisfied trivially by naming nothing, so
/// `crates/vike-cli/tests/seal_enforcement.rs` drives BOTH directions: these two refuse, and
/// `config mirror` / `config adopt --undo` / `config show` / `config compare` / `secrets list`
/// still run in the same state. The second half is the JSON incident's lesson written as a test —
/// a refusal must never take down the command its own text names as the repair.
///
/// `config check` is deliberately ABSENT: it is the surface that REPORTS this state (`Level::Fail`,
/// so a deploy pre-flight and an `ExecStartPre=` stop a start), and a `config check` that refused
/// to run would leave the operator with no way to read what is wrong.
const SEAL_REFUSING_VERBS: [&str; 2] = ["trade", "mcp"];

fn dispatch(command: &str, args: impl Iterator<Item = String>, resolved: &Resolved) -> ExitCode {
    if let Some(why) = resolved.seal_refusal.as_deref()
        && SEAL_REFUSING_VERBS.contains(&command)
    {
        eprintln!(
            "REFUSED: `vike-cli {command}` will not run while this box's settings seal is \
             unsound.\n\n{why}\n\nThis verb is refused because it ACTS on the ceilings — it places \
             or gates orders — and the values this process resolved are not the ones the box was \
             sealed with. Every REPAIR and disclosure verb still runs: `vike-cli config check` \
             reports the fault, `vike-cli config compare` shows what the two sources disagree \
             about, `vike-cli config mirror` re-derives every row from the settings files, and \
             `vike-cli config adopt --undo` steps back to the files."
        );
        return ExitCode::FAILURE;
    }
    match command {
        // ⚠ The ONE remote run verb, and it is a PLANE rather than a verb: `backtest run`
        // computes, and WHAT it computes is the PROFILE's answer on two independent axes
        // (`cmd::backtest`'s `route_of` — a non-empty `[sweep]` grid searches the parameter space,
        // a `[walkforward]` table walks the run forward over out-of-sample windows, and the two
        // compose). Its `--local` arm SPAWNS the standalone engine rather than linking it
        // (`cmd::engine` argues why at length), and `Resolved::project_root` is what tells it
        // where a project's `bin/` and `tmp/` are — a parameter, because a `src/cmd/` file may not
        // read the environment for itself.
        //
        // ⚠ It was THREE verbs until ruling 13 deleted `sweep`, and TWO until decision 3 of the
        // backtest-CLI-surface design folded `walkforward` inward. Both retirements are answered
        // by name in [`RETIRED_COMMANDS`] rather than by the unknown-command catch-all.
        //
        // It dials a datahub, and `required_scope` puts every one of its wire verbs under
        // `Scope::Write` — they compile client-supplied Rhai on the server — so it carries the
        // same keys `data` does. See [`datahub_keyring`] for where they come from.
        // `backtest_addr` is handed in because the middle rung of its address ladder is a SETTING,
        // which only this composition root may read.
        "backtest" => cmd::backtest::run(
            args,
            resolved.project_root.as_deref(),
            // ⚠ The RUNS root, and it is the override-AWARE one. `Resolved::user_data_dir` honours
            // `$VIKE_USER_DATA_DIR`, and since the producer does too
            // (`vike_model::state_path::user_runs_dir_from`) the reader and the writer cannot answer
            // with different directories. Resolved HERE, from the walk this dispatcher already owns,
            // because a `src/cmd/` file may not resolve a project for itself.
            resolved.user_data_dir.as_ref().map(|d| d.join(vike_model::state_path::RUNS_SUBDIR)),
            // ⚠ The MARKS root, a SIBLING of the runs root and never a child: a `marks/` directory
            // under `runs/` is, to every scan of that tree, a run holding no manifest — i.e. a
            // permanent "this run never finished writing" row in every listing.
            // `vike_model::state_path::MARKS_SUBDIR` carries the argument.
            resolved.user_data_dir.as_ref().map(|d| d.join(vike_model::state_path::MARKS_SUBDIR)),
            // The clock, read ONCE here and passed down, because `vike-model` contains no ambient
            // clock read and a `src/cmd/` file reads no global state — the same rule that makes the
            // two roots above parameters. `tag` is the only sub-verb that uses it.
            vike_model::clock::now_ms() / 1_000,
            resolved.backtest_addr.as_deref(),
            datahub_keyring(resolved).as_ref(),
        ),
        // The store-filling verb. It computes nothing itself — every subcommand is a route to the
        // same standalone engine, for the same reason `--local` is (this crate may not link
        // DataFusion), so it takes the same project root and nothing else.
        // ⚠ `datahub_addr` is handed in since the ladder gained its middle rung, and its absence
        // was the same recorded shape as `backtest_addr`'s: every READ verb here dials a datahub
        // and read the setting from nowhere, so a box that set the key pointed its GUI at one
        // address and its CLI at `127.0.0.1:7878`.
        // ⚠ The SETTINGS directory is a second root here since `data realtime record` shipped, and
        // it is NOT `project_root.join("settings")`: `Resolved::project_root` is derived as
        // `booted.settings_dir.parent()`, so under a `$VIKE_SETTINGS_DIR` that does not end in
        // `settings` the two name different directories — and this verb group WRITES the profile
        // rows the recording daemon mounts. The boot's own answer is the only one, for
        // `Resolved::state_dir`'s reason: a `src/cmd/` file re-deriving it would be a second walk,
        // blind to the override, answering with a different project than the credentials came from.
        "data" => cmd::data::run(
            args,
            resolved.project_root.as_deref(),
            resolved.settings_dir.as_deref(),
            datahub_keyring(resolved).as_ref(),
            resolved.datahub_addr.as_deref(),
        ),
        // ⚠ TWO facts more than the reading verbs need, and they arrive for the same reason
        // `secrets` takes them: `config set` WRITES a settings file and records the write in the
        // change journal, so it needs the ledger's home and the instant to stamp a record with.
        // Both come off the boot's own walk and the dispatcher's own clock read — a `src/cmd/` file
        // may resolve neither for itself. A `None` state directory journals nothing and still
        // writes, which is `vike_boot::journal_boot_settings`' declared disposition.
        "config" => cmd::config::run(
            args,
            cmd::config::Ctx {
                settings_dir: resolved.settings_dir.as_deref(),
                origin: resolved.settings_dir_origin,
                state_dir: resolved.state_dir.as_deref(),
                now_ms: vike_model::clock::now_ms(),
            },
        ),
        // The two ORDER-WRITE surfaces. A node key means the credential store is opened for them
        // (see [`node_keyring`]) — including for `trade status`, the READ that authenticates to a
        // node and now rides the `trade` arm.
        // ⚠ The STATE directory is a fourth parameter here and nowhere else in this arm's history:
        // `mcp --trace` writes the agent transcript beside the change journal, and the rule
        // `Resolved::state_dir` states is that the BOOT's answer is the only one — a `src/cmd/` file
        // re-deriving it would be a second walk, blind to `$VIKE_SETTINGS_DIR`, answering with a
        // different project than the settings and credentials came from.
        // ⚠ `backtest_addr` is handed in here TOO since stage 7, and its absence was a recorded
        // residual rather than a design: this server's compute tools (`run_backtest`, `run_sweep`,
        // `run_walk_forward`, `list_strategies`) dial the same daemon `backtest` and `research` do,
        // and they read the setting from nowhere — so a box that set the key moved some of its
        // compute dialers and silently left this one on `vike_config::DEFAULT_BACKTEST_ADDR`.
        // `crates/vike-ops/tests/unrun_command_gate.rs` RUNS the grep that answers who dials.
        "mcp" => cmd::mcp::run(
            args,
            resolved.policy_max_notional,
            &node_keyring(resolved),
            resolved.state_dir.as_deref(),
            datahub_keyring(resolved),
            resolved.backtest_addr.as_deref(),
            resolved.datahub_addr.as_deref(),
        ),
        // ⚠ ONE arm for FOUR operations, and the collapse is ruling 17 of
        // `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md`. `trade` used to be
        // the REPL alone, with the read-only node question promoted to a top-level
        // `strategy-status` beside it — the only member of that family (`orders`, `positions`, the
        // old `state`) to be. It is now `trade status`, and `trade halt` / `trade resume` join it as
        // the two writes, so the same three words work at the prompt and on the command line.
        // `cmd::trade::run` claims a leading verb and falls through to the REPL when there is none;
        // the keyring is resolved here either way, since every path authenticates to a node.
        "trade" => cmd::trade::run(
            args,
            resolved.policy_max_notional,
            resolved.settings_dir.clone(),
            &node_keyring(resolved),
        ),
        // Ruling 16's two OWED client halves — `vike-cli <X>` asks the backend to do X — and they
        // reach DIFFERENT daemons, which is the whole of their scope decision.
        //
        // `report` asks a vike-tradehub NODE, because the live journal a tearsheet is computed
        // from is written by the trading daemon and exists nowhere else. So it resolves the
        // tradehub keyring, and like the `trade status` read above it uses only the OBSERVE half:
        // a tearsheet changes nothing, and a control key cannot authenticate a read. ⚠ That
        // sibling was the top-level `strategy-status` when this arm was written; ruling 17 folded
        // it into `trade`, so `report` is now the ONE top-level verb whose whole job is a
        // read against a node.
        //
        // ⚠ It takes TWO PATHS as well as the keyring, and they are what make the verb useful on a
        // box with no node: `report <run>` re-renders a FINISHED run out of the run directory, so
        // this arm resolves the same runs/marks pair the `backtest` arm above does, from the same
        // walk, for the same reason — a `src/cmd/` file may not resolve a project for itself. The
        // marks root is a SIBLING of the runs root and never a child (`vike_model::state_path::
        // MARKS_SUBDIR` carries the argument), and it is here so that `report @baseline` accepts the
        // whole selector grammar rather than a truncated half of it.
        "report" => cmd::report::run(
            args,
            &node_keyring(resolved),
            resolved.user_data_dir.as_ref().map(|d| d.join(vike_model::state_path::RUNS_SUBDIR)),
            resolved.user_data_dir.as_ref().map(|d| d.join(vike_model::state_path::MARKS_SUBDIR)),
        ),
        // `research` is the plane BEFORE a strategy exists — investigate a signal, fit a model —
        // and `research study` asks the BACKEND, because a study opens the hist store and drives a
        // trainer next to it: the same compute-to-data argument as `backtest` directly above. ⚠ It
        // is aimed at the COMPUTE daemon (`vike-backend backtest --addr`), not the data server:
        // ruling 7 puts every verb that RUNS an engine there, and that daemon authenticates with
        // the same datahub node pair under the same domain separator, so this is the same keyring.
        // Each module doc argues its own half; neither sends a request today (the server arms are
        // follow-ups) and both say so where an operator will read it.
        //
        // ⚠ `backtest_addr` is handed in rather than looked up below: the sub-verb's address
        // ladder is `--addr` → this key → `vike_config::DEFAULT_BACKTEST_ADDR`, and the middle
        // rung is a SETTING — which only this composition root may read. The plane folds
        // `cmd::backtest`'s `resolve_addr` rather than carrying a second copy; a ladder forks when
        // a SETTING forks, not when a plane does.
        //
        // ⚠ **This arm is what keeps `Resolved::backtest_addr` alive.** It was the `study` arm's
        // only reader, and `study` retired off the top level here (ruling 1). A retirement to
        // NOTHING would have left the field written and never read — `dead_code` on a lane that
        // runs `-D warnings` — so the successor arm is load-bearing rather than cosmetic.
        "research" => cmd::research::run(
            args,
            resolved.backtest_addr.as_deref(),
            datahub_keyring(resolved).as_ref(),
        ),
        // BOTH the resolved directory and the override that produced it — see
        // `Resolved::settings_dir_override` for why the second is not redundant — plus the three
        // things its `set` arm needs and no reading subcommand does: the ledger's home, the
        // environment map (`--from-env NAME`, so nothing under `src/cmd/` reads the environment
        // itself) and the instant to stamp the record with. `cmd::secrets::Ctx` carries the
        // argument for why they arrive as one struct.
        //
        // ⚠ The CLOCK is read HERE rather than in `cmd/`, and it is read for every `secrets`
        // invocation including the read-only ones. `vike_model::change_journal` deliberately reads
        // no clock, so the instant is a parameter all the way down — the same shape
        // `vike_boot::journal_boot_settings`' `ts_ms` takes. One wall-clock read costs a
        // `secrets path` nothing and keeps the value a caller can see and a test can pin.
        "secrets" => cmd::secrets::run(
            args,
            cmd::secrets::Ctx {
                settings_dir: resolved.settings_dir.as_deref(),
                settings_dir_override: resolved.settings_dir_override.as_deref(),
                state_dir: resolved.state_dir.as_deref(),
                env: &resolved.env,
                now_ms: vike_model::clock::now_ms(),
            },
        ),
        // The ONBOARDING surface for a vike-tradehub node, and the SECOND credential writer in this
        // binary. It takes the same five facts `secrets` does — the resolved directory, the
        // override that produced it, the ledger's home, and the instant to stamp a record with —
        // plus two this dispatcher already holds and no other arm needs: `config.node_addr` (this
        // box's dial default, which `connect` REWRITES and `status` reports) and the KEYRING, whose
        // observe half signs the round trip that verifies a fresh attachment.
        //
        // ⚠ It takes NO environment map, unlike `secrets`. That is not an oversight — it is the
        // property that makes this writer narrower than the one 0036 reopened: `secrets set` has a
        // `--from-env NAME` form because an operator supplies its value, and here there is no
        // operator-supplied value to source. `setup` MINTS both keys; `connect --manual` reads them
        // from stdin. Neither path can be pointed at a variable, so no map is needed.
        //
        // This arm resolves the keyring too, and it is the only one that also WRITES it. (That
        // used to read "the FOURTH arm"; `report` made it the fifth on the day it landed, so the
        // count is gone rather than re-stated — an ordinal over a growing list of arms is the
        // shape of claim this repository has watched rot.)
        // ⚠ It resolves BOTH keyrings since `backend ping` landed, and the two are not
        // interchangeable: the four onboarding verbs are about a vike-tradehub node and take that
        // pair, while `ping` dials a vike-datahub-PROTOCOL daemon (the data server or the compute
        // server) and takes the other, under a different domain separator. Threading one for the
        // other fails as an opaque `AuthDenied` — the confusion `cmd::node::ping`'s module doc
        // opens with. (The `mcp` arm above resolves both too, for its own two tool families; no
        // ordinal is claimed here, because every ordinal written over these arms has rotted.)
        //
        // ⚠ `datahub_keyring` OPENS THE NODE STORE, and that cost now falls on every `backend`
        // invocation rather than only on a compute dialer. It is the narrow store (`node.env` /
        // the database's `node_key` table, never the 168-venue credential file), the environment
        // still wins outright, and this arm already opens the same store for the tradehub pair —
        // so the marginal act is one extra lookup in a map that was read anyway. The narrower
        // alternative (resolve it only on the `ping` arm) was refused for the reason
        // `datahub_keyring`'s own doc gives about `--addr`: a `Ctx` is built before a sub-verb is
        // claimed, so "will this invocation dial" cannot be answered here.
        "backend" => {
            let datahub_keys = datahub_keyring(resolved);
            cmd::node::run(
                args,
                cmd::node::Ctx {
                    settings_dir: resolved.settings_dir.as_deref(),
                    settings_dir_override: resolved.settings_dir_override.as_deref(),
                    state_dir: resolved.state_dir.as_deref(),
                    node_addr: resolved.node_addr.as_deref(),
                    keys: &node_keyring(resolved),
                    backtest_addr: resolved.backtest_addr.as_deref(),
                    datahub_keys: datahub_keys.as_ref(),
                    now_ms: vike_model::clock::now_ms(),
                },
            )
        }
        // The datahub's own key minting. It shares `cmd::node`'s `Ctx` and its store/mint/journal
        // helpers deliberately — one shape for "mint a node pair", two services — while staying a
        // separate VERB, because `backend` means a vike-tradehub node and a datahub is not one.
        // ⚠ `keys` is the TRADEHUB keyring here and this arm uses none of it; it is threaded because
        // `Ctx` is shared and building a second context type to omit one unread field would be more
        // to keep in step than it saves.
        "datahub" => cmd::datahub::run(
            args,
            &cmd::node::Ctx {
                settings_dir: resolved.settings_dir.as_deref(),
                settings_dir_override: resolved.settings_dir_override.as_deref(),
                state_dir: resolved.state_dir.as_deref(),
                node_addr: resolved.node_addr.as_deref(),
                keys: &node_keyring(resolved),
                // ⚠ Both unread here, and NOT resolved for that reason: this verb MINTS a pair, so
                // handing it the pair this box currently holds would be handing `setup` the value
                // its `--rotate` refusal exists to protect. `None` is the honest input, and it is
                // cheaper too — `datahub_keyring` opens the node store.
                backtest_addr: None,
                datahub_keys: None,
                now_ms: vike_model::clock::now_ms(),
            },
        ),
        // The ONE subcommand that writes to the project's USER-CONTENT tree, and it writes ONLY
        // under `user_data/` — never into `settings/`, and it opens no credential.
        "init" => cmd::init::run(args, resolved.user_data_dir.as_deref()),
        // Takes NOTHING from the dispatcher: the callable indicator roster is a property of the
        // BUILD (`vike_script::RHAI_INDICATORS` joined onto `vike_indicators::registry()`), never
        // of the project, the environment or a credential.
        "indicators" => cmd::indicators::run(args),
        // Takes NOTHING from the dispatcher either, and for a stronger reason than `indicators`:
        // the surface table is COMPILED IN, so this verb's answer cannot vary with a project, a
        // settings file, a credential or a network. That is what makes it safe to run in a release
        // workflow, and what lets the gate call the same function in-process with no subprocess.
        "surface" => cmd::surface::run(args),
        "-h" | "--help" | "help" => {
            print_help();
            ExitCode::SUCCESS
        }
        "-V" | "--version" => {
            print_version();
            ExitCode::SUCCESS
        }
        // ⚠ A RETIRED verb is answered with its replacement, before the catch-all can call it
        // unknown. See [`RETIRED_COMMANDS`] for why an "unknown command" answer to a verb that
        // shipped for months is the wrong product on the right rung.
        other if RETIRED_COMMANDS.iter().any(|(name, _)| *name == other) => {
            let (_, why) = RETIRED_COMMANDS
                .iter()
                .find(|(name, _)| *name == other)
                .expect("the guard just matched it");
            eprintln!("vike-cli: {why}");
            Exit::Usage.into()
        }
        other => {
            eprintln!("vike-cli: unknown command '{other}'");
            print_help();
            // The USAGE rung — see [`crate::exit`]. An unknown verb is the one failure a caller can
            // always fix and can never usefully retry, which is exactly what separates it from the
            // catch-all it used to share a number with.
            Exit::Usage.into()
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
    /// ceiling that bounded a value nothing read. The loader HAS a producer again
    /// (`vike_config::NO_SETTINGS_DIRECTORY_WARNING`), and this test deliberately does not use it:
    /// driving the real one would test the producer, where what this gates is the half that lives in
    /// THIS crate and was the actual Phase-6c defect — a warning that exists reaches the operator,
    /// verbatim.
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
    ///
    /// ⚠ The second half used to drive `load(None, …)` and assert silence, and that is no longer a
    /// clean load — it is the "no project resolved" case, which now says so
    /// (`vike_config::NO_SETTINGS_DIRECTORY_WARNING`). Both halves are kept and the second is
    /// INVERTED rather than deleted: a `vike-cli` run from outside any project must still print
    /// exactly one line, on stderr, and never on the stdout `mcp` speaks a protocol over.
    #[test]
    fn a_clean_load_prints_nothing_and_a_projectless_one_prints_exactly_one_line() {
        assert!(settings_warning_lines(&vike_config::Settings::default()).is_empty());
        let settings = vike_config::load(None, &HashMap::new()).unwrap();
        let lines = settings_warning_lines(&settings);
        assert_eq!(lines.len(), 1, "no project ⇒ exactly one line: {lines:?}");
        assert!(lines[0].contains("no settings directory resolved"), "{}", lines[0]);
    }
}
