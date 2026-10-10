//! `vike-cli` — the unified vike command-line surface (headless two-layer plan, Layer 1).
//!
//! A **git/cargo-style subcommand dispatcher**: `vike-cli <command> [args…]`. [`COMMANDS`] is the
//! roster; the ones this paragraph was written around: `backtest`
//! (the compute-to-data offload that ships a profile's TOML to a remote COMPUTE daemon —
//! `vike-backend backtest --addr` — and returns a compact report, with **no DataFusion in this
//! side's build graph**), `mcp` (a local
//! stdio MCP server exposing the create+backtest tools to an agent), `config` (settings
//! provenance — `show` for every setting, its effective value and where that value came from;
//! `check` for the same tree JUDGED, with the exit code as the product, which is what the shipped
//! `deploy/*.service` units put in their `ExecStartPre=`) and `init` (scaffold
//! `<project>/user_data`, the user-content directory, with runnable examples).
//!
//! # Library + thin bin
//!
//! The dispatcher and subcommand modules live in this LIBRARY; `src/main.rs` is a one-line shim that
//! calls [`run`]. This matches the sibling bin crates (`vike-tradehub`; `vike-mount`, which carries
//! the `incident` bin) and — load-bearing for CI — gives the package a LIB TARGET, so
//! `cargo test --doc -p vike-cli` has something
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
//! [`exit::Exit::Ok`] did it, [`exit::Exit::Failed`] ran and failed, [`exit::Exit::Usage`] the
//! command line was wrong, [`exit::Exit::Connect`] a service could not be reached,
//! [`exit::Exit::Refused`] refused LOCALLY before anything was sent, [`exit::Exit::Venue`] the far
//! side accepted the connection and said no, [`exit::Exit::Breach`] a declared threshold was
//! breached, and [`exit::Exit::Empty`] nothing was evaluated. `Ok` and `Failed` mean exactly what
//! they always did, so nothing written against the old two-value behaviour changes — the rest is a
//! subdivision of what used to be `Failed`, which is what lets a wrapper retry a timeout without
//! retrying a typo, and lets a CI step tell a failed gate from a gate that checked nothing.
//!
//! Every rung [`exit::Exit`] declares is LIVE and has a producer — [`exit`] argues each one and
//! names it — and `crates/vike-cli/tests/exit_codes.rs` is the one place a rung is asserted over
//! the shipped binary and therefore the authority for which ones are covered; this file names no
//! count of its own, because a count is exactly the kind of claim that rots the moment a new rung
//! or a new case lands and this paragraph is not the one that gets read next.
//!
//! # Settings, resolved once, before any subcommand
//!
//! [`run`] calls [`resolve_policy`] first, which runs the workspace's ONE startup sequence
//! (`vike_boot::boot`): it REFUSES a removed environment variable (today, the
//! `VIKE_MAX_ORDER_NOTIONAL` that Phase 5 of the settings-unification design deleted — a ceiling
//! any exported variable can raise is not a ceiling) and loads this machine's settings (the
//! `policy` rows of `<project>/settings/db/vike.db`). The resolved `max_notional_per_order` is
//! handed to the two order-write surfaces (`trade`, `mcp`) for their advisory preview guardrail.
//! The environment read lives here, in the entry point, per the settings-registry rule that only
//! binaries read env — and so does the obligation to SURFACE the loader's non-fatal resolutions,
//! which this dispatcher used to swallow (a preference clamped to a policy ceiling produced no
//! output at all). They print to **stderr**, never stdout, because `vike-cli mcp`'s stdout is a
//! protocol.
//!
//! # The credential-store read is HERE too, and it is LAZY
//!
//! The two order-write surfaces authenticate to a `vike-tradehub` node with HMAC keys the DAEMON
//! reads out of its node-key store (the settings database's `node_key` table). Both used to read `std::env::var` and nothing else, so a
//! correctly-configured box answered "nothing to do" and exited — see [`cmd::nodekeys`],
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
//! ⚠ **That paragraph's worry — a low-sensitivity key causing the file holding every venue
//! credential to be read — was answered by SEPARATING the classes**, which is what it said the
//! industrial answer was: `docs/decisions/0051-node-keys-live-in-their-own-store.md` gave the node
//! pairs their own store (the settings database's `node_key` table), and these arms read THAT,
//! never the venue-credential table.
//!
//! It reaches the node store through `vike_secrets::resolve_node_keys`, which takes the settings
//! DIRECTORY this dispatcher already resolved — the shape `cmd::secrets` uses, and the one
//! `crates/vike-ops/tests/settings_secrets/settings_registry.rs`'s `CREDENTIAL_STORE_PIN` explicitly blesses ("the
//! composition root doing exactly what the rule asks for"), as opposed to the sweeping loaders that
//! open a location nothing in their signature mentions.

#![warn(unreachable_pub)]

use std::collections::HashMap;

pub mod cmd;
pub mod exit;
pub mod surface;

mod boot;
mod dispatch;

#[cfg(doc)]
use boot::{datahub_keyring, node_key_store, node_keyring, resolve_policy};
pub use dispatch::run;

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
        "observe + control a running vike-tradehub node: every verb but three lives under a \
         REQUIRED group (`order`, `position`, `strategy` built; `account`, `watch` named in the \
         roster and refused as designed-not-built) — `trade status|halt|resume` are the three \
         NODE-WIDE, risk-REDUCING words that take no book and so take no group, and a bare \
         `trade` opens the interactive REPL (for a human)",
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
    // that world. `crates/vike-tradehub/src/server/tearsheet.rs`'s `tearsheet_reply` renders the live
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
        "inspect the credential store THIS box reads — the settings database \
         <project>/settings/db/vike.db, the only store, which `secrets path` reports — set ONE \
         key in it, and create it EMPTY (list | path | set | init)",
    ),
    (
        "backend",
        "stand a vike-tradehub node — the running daemon — up, and attach this box to one \
         (setup on the daemon | connect | status | disconnect on the client)",
    ),
    (
        "datahub",
        "MINT the node key pair a vike-datahub server authenticates with (setup), or hand its \
         CONTROL key to a pipe for `just studio` (control-key), on that server's box",
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
/// `crates/vike-backtest/src/backtest_cli/search_flags.rs`'s `parse_search_flags` gives the retired
/// `--search` flag. The rung is unchanged — [`Exit::Usage`], because re-running unchanged cannot
/// succeed — which is deliberately the SAME rung the catch-all uses: what changes is the message,
/// not the classification, and a test asserting only the code would not see the difference.
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

/// Loads `<project>/user_data/indicators/*.rhai` and installs them process-wide, so a script run by
/// ANY subcommand can call them.
///
/// This is the composition root doing the I/O on the library's behalf — `vike_script`'s
/// `install_user_indicators` takes already-compiled prototypes precisely so that no library resolves
/// this directory for itself (its doc is the authority on why, and names `vike_log::init` as the
/// precedent). The load-plus-install pair is `vike_script::load_and_install_user_indicators`, which
/// is what a root wires WHEN it is wired — `git grep -l load_and_install_user_indicators -- crates` finds
/// every file that NAMES the pair — which is not the same as the set of callers: `vike-desktop`
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
    /// `max_notional_per_order` (the `policy.max_notional_per_order` row), or `None` when unset.
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
    /// resolver takes the override as a parameter and returns a bare path. `cmd::config::check`
    /// needs the distinction and nothing else does — a NAMED directory that is not on disk is a
    /// set-but-unhonoured value and a refusal, while a WALKED one that is not there is an
    /// unconfigured checkout and merely a warning.
    settings_dir_origin: cmd::config::check::DirOrigin,
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
    /// writes the AGENT TRANSCRIPT into a sibling directory (`crate::cmd::mcp::trace`). `None` (no
    /// project above the working directory) means the first one still writes the store and records
    /// NOTHING — an append-only ledger in a guessed directory is worse than a counted absence, the
    /// same rule `vike_boot::journal_boot_settings` follows — while the second REFUSES to start,
    /// because there the operator ASKED for a record and a silent absence is what they would
    /// discover after the incident. `crate::cmd::mcp::config::resolve_trace` argues the asymmetry.
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
    /// and `cmd::secrets`'s `settings_dir_of` states its own reachability at its site.
    settings_dir_override: Option<String>,
    /// `<project>/user_data` — THE user-content directory (strategies, run profiles, results,
    /// notebooks), or `None` when no project sits above the working directory.
    ///
    /// A SIBLING of [`Resolved::settings_dir`], never a child, and resolved from the SAME walk so
    /// the two can never answer with different projects — `crates/vike-model/src/paths/state_path.rs`'s
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
    /// `vike_model::paths::state_path::user_data_dir_beside` applies to the sibling beside it. ⚠ Neither
    /// of those two directories has an environment variable of its own, deliberately
    /// (`crates/vike-model/src/paths/state_path.rs`'s `PROJECT_TMP_DIR` argues both halves), so unlike
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
    use crate::boot::settings_warning_lines;

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
        let settings = vike_config::load(None).unwrap();
        let lines = settings_warning_lines(&settings);
        assert_eq!(lines.len(), 1, "no project ⇒ exactly one line: {lines:?}");
        assert!(lines[0].contains("no settings directory resolved"), "{}", lines[0]);
    }
}
