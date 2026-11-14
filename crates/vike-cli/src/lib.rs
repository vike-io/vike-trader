//! `vike-cli` — the unified vike command-line surface (headless two-layer plan, Layer 1).
//!
//! A **git/cargo-style subcommand dispatcher**: `vike-cli <command> [args…]`. [`COMMANDS`] is the
//! roster; the ones this paragraph was written around: `backtest`
//! (the compute-to-data offload that ships a profile's TOML to a remote backtesthub —
//! `vike-backend backtesthub --addr` — and returns a compact report, with **no DataFusion in this
//! side's build graph**), `config` (settings
//! provenance — `show` for every setting, its effective value and where that value came from;
//! `check` for the same tree JUDGED, with the exit code as the product, which is what the shipped
//! `deploy/*.service` units put in their `ExecStartPre=`) and `init` (scaffold
//! `<project>/user_data`, the user-content directory, with runnable examples).
//!
//! # Library + thin bin
//!
//! The dispatcher and subcommand modules live in this LIBRARY; `src/main.rs` is a one-line shim that
//! calls [`run`]. This matches the sibling bin crates (`vike-tradehub`; `vike-tradehub-core`, which carries
//! the `incident` bin) and — load-bearing for CI — gives the package a LIB TARGET, so
//! `cargo test --doc -p vike-cli` has something
//! to run (a bin-only crate errors "no library targets found in package vike-cli").
//!
//! # Growing the surface
//!
//! Each command is a sibling module under [`cmd`] exposing a `run(args) -> ExitCode` that reads its
//! own argv tail as a `clap` struct (decision 0155; `cmd::args` is the glue). Adding `trade`, or an
//! interactive `repl` later is: add the module, add one arm to [`dispatch`], add one line to
//! [`COMMANDS`] and one variant to `Root`, the command that backs `--help`. The dispatcher itself
//! never grows command logic — it only routes. REMOVING one is the mirror image plus a row in
//! [`RETIRED_COMMANDS`], so the spelling that is going away fails naming its replacement rather
//! than reading as a typo.
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
//! names it — and `crates/vike-cli/tests/shipped_cli/exit_codes.rs` is the one place a rung is asserted over
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
//! handed to `trade`, the order-write surface here, for its advisory preview guardrail.
//! The environment read lives here, in the entry point, per the settings-registry rule that only
//! binaries read env — and so does the obligation to SURFACE the loader's non-fatal resolutions,
//! which this dispatcher used to swallow (a preference clamped to a policy ceiling produced no
//! output at all). They print to **stderr**, never stdout, the stream a verb's answer owns.
//!
//! # The credential-store read is HERE too, and it is LAZY
//!
//! The order-write surface authenticates to a `vike-tradehub` node with HMAC keys the DAEMON
//! reads out of its hub-key store (the settings database's `hub_key` table). Both used to read `std::env::var` and nothing else, so a
//! correctly-configured box answered "nothing to do" and exited — see [`vike_client::hubkeys`],
//! which argues the single source (the store; the environment was a first rung until the owner's
//! ruling of 2026-10-31 and is none now). The store read belongs at this
//! composition root for exactly the reason the environment sweep does, and [`hub_keyring`]
//! performs it for the node-facing arms (`trade`, plus the top-level read-only
//! `report`). `trade` covers the read-only `trade status` too, which uses just the observe half —
//! it was a top-level `strategy-status` with a dispatch arm of its own until ruling 17 folded it
//! into the `trade` family, which is why `report` is now the only top-level verb here whose whole
//! job is a node read.
//!
//! ⚠ **There are TWO hub key pairs, and the DATAHUB pair had the identical defect** — resolved
//! from the process environment and nowhere else, so a pair sitting in the credential store, which
//! is where this binary's own refusal text sends an operator, did nothing at all.
//! [`datahub_keyring`] fixes it with the same single source and carries the measurement.
//!
//! ⚠ **This widened which arms open the credential store, and the rule that used to sit here is
//! the thing that changed.** It read: "no other subcommand needs a credential, and a provenance or
//! backtest command has no business opening the file that holds every venue key on the box." The
//! first half is simply no longer true — `backtest` and `data` both
//! dial a datahub that may require authentication, and `research study` dials the backtesthub
//! under the
//! same node pair (ruling 7 puts every verb that RUNS an engine there, so it is a different daemon
//! reached with the same credential — the store read is what it has in common with the five, not
//! the peer). The second half was written when
//! nothing in this binary could authenticate to one, and a `backtesthub --addr` is a NODE-FACING
//! invocation, the category that sentence already excepted. So the rule now reads: **an arm that
//! can dial a node may open the store; one that cannot, may not.** `config`, `secrets`, `init`
//! and `indicators` still never touch it.
//!
//! ⚠ **That paragraph's worry — a low-sensitivity key causing the file holding every venue
//! credential to be read — was answered by SEPARATING the classes**, which is what it said the
//! industrial answer was: `docs/decisions/0051-node-keys-live-in-their-own-store.md` gave the node
//! pairs their own store (the settings database's `hub_key` table), and these arms read THAT,
//! never the venue-credential table.
//!
//! It reaches the hub key store through `vike_secrets::CredentialStore::hub_keys`, on the handle the
//! boot opened over the settings DIRECTORY this dispatcher already resolved — the shape
//! `cmd::secrets` uses, and the one
//! `crates/vike-ops/tests/settings_secrets/settings_registry.rs`'s `CREDENTIAL_STORE_PIN` explicitly blesses ("the
//! composition root doing exactly what the rule asks for"), as opposed to the sweeping loaders that
//! open a location nothing in their signature mentions.

#![warn(unreachable_pub)]

pub mod cmd;
pub mod exit;
pub mod surface;
// The planted-engine plant and its `ETXTBSY` retry, shared by the `shipped_cli` group and
// `tests/planted_binary_retry.rs`: compiled once here. Only the self dev-dependency turns it on.
#[cfg(feature = "test-support")]
pub mod test_support;

mod boot;
mod dispatch;

#[cfg(doc)]
use boot::resolve_policy;
pub use dispatch::run;
#[cfg(doc)]
use vike_client::resolved::{datahub_keyring, hub_keyring};

/// The registered subcommands, as `(name, one-line summary)` — the single source of truth for both
/// the help text and the "unknown command" hint, so a new command shows up in help for free.
const COMMANDS: &[(&str, &str)] = &[
    (
        "backtest",
        "compute a strategy over history and judge what came back: `backtest run` runs one on a \
         remote backtesthub — `vike-backend backtesthub --addr`, config.backtest_addr — or \
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
        "the hist store: fetch, import or load the published real bars into it (no demo or \
         synthetic tape ships), list what it holds and report coverage (read, over --addr)",
    ),
    (
        "config",
        "this box's settings: provenance (`show`), a validating pre-flight (`check`), and a \
         journalled one-key write (`set`)",
    ),
    (
        "trade",
        "observe + control a running vike-tradehub: every verb but three lives under a \
         REQUIRED group (`account`, `order`, `position`, `strategy` built; `watch` named in the \
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
    // `vike_backtesthub_client::proto`'s `Request::RunStudy` / `Response::StudyReport` and the compute
    // daemon's arm for it, under ruling 1 — a whole refusing PLANE being a different proposition
    // from a refusing verb. ⚠ A daemon built before the runner became a direct call served it only when it MOUNTED a study runner
    // (an injected `StudyRunFactory`, since deleted), so a refusal from an OLDER peer still exists and
    // `cmd::study`'s `no_capability_lines` still names `vike-backend study` — but *no backend
    // serves it yet* is false, and leaving it was the same false confirmation one paragraph up.
    // The withdrawn clause is recorded here rather than silently dropped.
    //
    // ⚠ The sentence that read *the backtesthub it dials is ruling 7's half and does not exist
    // yet* is likewise spent: `crates/vike-backtesthub/src/backtesthub_server.rs` IS that daemon and
    // `deploy/vike-backtesthub.service` ships it. Which refusal an operator meets is now a property of
    // their DEPLOYMENT (a box that stood the daemon up reaches the capability negotiation; one that
    // did not ends at the dial), not of the build — `cmd::study`'s module doc carries both arms and
    // `crates/vike-cli/tests/shipped_cli/study_report_refusal_cli.rs` drives them.
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
        "ask a vike-tradehub for a tearsheet over its live journal, or re-render a \
         FINISHED run from this machine's own run directory (read-only; a node built from this \
         tree serves the live verb, an older one refuses and names the command that does)",
    ),
    (
        "research",
        "investigate a signal and FIT a model — the plane BEFORE a strategy exists: \
         `research study` asks the backend to run a compiled study over the hist store it holds \
         (served by `vike-backend backtesthub --addr` when it mounts the study runner; a peer that \
         does not refuses by name and points at `vike-backend study`)",
    ),
    (
        "secrets",
        // ⚠ It names BOTH stores because this line is read by an operator deciding where to look,
        // and `docs/decisions/0054`'s credential half made `the store` a per-BOX answer rather than
        // a path: naming the file alone sent a migrated box's operator to edit something nothing
        // reads. `secrets path` is named because it is the only honest way to learn which one
        // answers here, and it is a real subcommand of this verb's own USAGE.
        "inspect the credential store THIS box reads — the settings database \
         <project>/settings/db/vike.db, the only store, which `secrets path` reports — set ONE \
         key in it, and create it EMPTY (list | path | set | init)",
    ),
    (
        "backend",
        "stand a vike-tradehub — the running daemon — up, and attach this box to one \
         (setup on the daemon | connect | status | disconnect on the client)",
    ),
    (
        "datahub",
        "MINT the hub key pair a vike-datahub server authenticates with (setup), or hand its \
         CONTROL key to a pipe for `just studio` (control-key), on that server's box",
    ),
    (
        "gatewayhub",
        "MINT the hub key pair a vike-gatewayhub server authenticates with (setup), on that \
         server's box",
    ),
    (
        "init",
        "create <project>/user_data — starter strategies, presets, notebooks; no run profile and no \
         data ship — and print how to load real bars and run a strategy file",
    ),
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
/// `crates/vike-backtesthub/src/backtest_cli/search_flags.rs`'s `parse_search_flags` gives the retired
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
         stage 7: every backtesthub (`vike-backend backtesthub --addr`, the `backtesthub` bin) \
         serves it, and a refusal from the data server or an older backend names \
         `vike-backend study` — the one thing that runs a study on the box that holds the \
         store.\n\
         \n\
         was:  vike-cli study          --study NAME --recipe FILE --from WHEN --to WHEN\n\
         now:  vike-cli research study --study NAME --recipe FILE --from WHEN --to WHEN",
    ),
];

/// The ROOT command, which backs everything `vike-cli` answers BEFORE a verb is chosen: `--help`,
/// `help`, `-V`/`--version`, a bare `vike-cli`, and a verb word that is not one (clap's
/// "unrecognized subcommand" with its suggestion). It is NOT how a verb's arguments are read: each
/// verb's module reads its own tail as its own `clap` struct, under `vike-cli <verb>`, and
/// [`dispatch::run`] hands a known verb word straight to it — "first token = verb" is the one split
/// decision 0155 leaves in the router, and this enum is its description. A verb added here without
/// a [`COMMANDS`] row (or the reverse) fails `the_root_command_and_the_verb_table_name_the_same_verbs`.
///
/// ⚠ The three RETIRED verbs are hidden rows, so `vike-cli --help` does not advertise a verb that
/// refuses (the rule [`RETIRED_COMMANDS`] states); [`dispatch::run`] answers them by name.
#[derive(clap::Parser)]
#[command(name = "vike-cli", about = "the unified vike command-line surface", long_about = None)]
enum Root {
    /// Compute a strategy over history and judge what came back: run, ls, show, path, tag, diff,
    /// gate, params, strategies, templates, script-api, script-check.
    Backtest,
    /// The hist store: fetch or import real bars, list what it holds, report coverage.
    Data,
    /// This box's settings: provenance (show), a pre-flight (check), a one-key write (set).
    Config,
    /// Observe and control a running vike-tradehub; a bare `trade` opens the REPL.
    Trade,
    /// A tearsheet over a node's live journal, or a finished run from this machine.
    Report,
    /// Investigate a signal and fit a model — the plane BEFORE a strategy exists.
    Research,
    /// Inspect the credential store this box reads, set one key in it, create it empty.
    Secrets,
    /// Stand a vike-tradehub up and attach this box to one.
    Backend,
    /// Mint the hub key pair a vike-datahub server authenticates with.
    Datahub,
    /// Mint the hub key pair a vike-gatewayhub server authenticates with.
    Gatewayhub,
    /// Create <project>/user_data — strategies, profiles, results — with examples.
    Init,
    /// Print the indicators a Rhai strategy can call, with their parameters.
    Indicators,
    /// Write this binary's own command surface as JSON.
    Surface,
    #[command(hide = true)]
    Sweep,
    #[command(hide = true)]
    Walkforward,
    #[command(hide = true)]
    Study,
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use clap::CommandFactory;

    use super::{COMMANDS, RETIRED_COMMANDS, Root};
    use crate::cmd::args;

    /// The root command and the verb table agree on WHICH verbs exist, in both directions, and the
    /// hidden rows are exactly the retired ones. `COMMANDS` is the roster `scripts/gen_skills.sh`
    /// renders three shipped skills' verb tables from, so it stays a table of its own; this is what
    /// stops it and `--help` from naming different verbs.
    #[test]
    fn the_root_command_and_the_verb_table_name_the_same_verbs() {
        let root = Root::command();
        let shown: BTreeSet<&str> =
            root.get_subcommands().filter(|c| !c.is_hide_set()).map(|c| c.get_name()).collect();
        let hidden: BTreeSet<&str> =
            root.get_subcommands().filter(|c| c.is_hide_set()).map(|c| c.get_name()).collect();
        let table: BTreeSet<&str> = COMMANDS.iter().map(|(name, _)| *name).collect();
        let retired: BTreeSet<&str> = RETIRED_COMMANDS.iter().map(|(name, _)| *name).collect();
        assert_eq!(shown, table, "`Root` and `COMMANDS` must name the same verbs");
        assert_eq!(hidden, retired, "the hidden rows of `Root` are the retired verbs, no more");
    }

    /// What the root command answers BEFORE a verb is chosen. Each of these used to be a hand arm
    /// of the dispatcher's `match`; now `clap` says it, and the exit code is the point.
    #[test]
    fn the_root_command_answers_help_version_and_a_mistyped_verb() {
        let root = |argv: &[&str]| args::parse::<Root, _, _>(args::PROGRAM, argv.iter().copied());
        for help in [&["--help"][..], &["-h"][..], &["help"][..]] {
            let stop = root(help).err().expect("help is a stop");
            assert!(stop.is_help(), "{help:?} is a success: {stop}");
            assert!(stop.contains("backtest"), "{help:?} lists the verbs: {stop}");
            assert!(!stop.contains("sweep"), "…and not the retired ones: {stop}");
        }
        for version in [&["--version"][..], &["-V"][..]] {
            let stop = root(version).err().expect("version is a stop");
            assert!(stop.is_help(), "{version:?} is a success: {stop}");
            assert!(stop.starts_with("vike-cli "), "name first, then the version: {stop}");
        }
        let none = root(&[]).err().expect("no verb is a refusal");
        assert!(!none.is_help(), "a bare `vike-cli` is a usage error: {none}");
        let typo = root(&["bakctest"]).err().expect("a mistyped verb is a refusal");
        assert!(!typo.is_help());
        assert!(typo.contains("backtest"), "the suggestion names the verb it meant: {typo}");
    }
}

#[path = "skill_facts_tests.rs"]
#[cfg(test)]
mod skill_facts_tests;
