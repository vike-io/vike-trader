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
//! breached, [`exit::Exit::Empty`] nothing was evaluated, and [`exit::Exit::OutcomeUnknown`] a write
//! was sent and never answered (read the book before resending it). `Ok` and `Failed` mean exactly
//! what they always did, so nothing written against the old two-value behaviour changes — the rest
//! is a subdivision of what used to be `Failed`, which is what lets a wrapper retry a timeout without
//! retrying a typo, lets a CI step tell a failed gate from a gate that checked nothing, and lets a
//! script tell "unknown, do not resend blindly" from "refused" and "failed".
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
//! handed to `trade`, the order-write surface here, for its preview guardrail, which refuses an order over it.
//! The environment read lives here, in the entry point, per the settings-registry rule that only
//! binaries read env — and so does the obligation to SURFACE the loader's non-fatal resolutions,
//! which this dispatcher used to swallow (a preference clamped to a policy ceiling produced no
//! output at all). They print to **stderr**, never stdout, the stream a verb's answer owns.
//!
//! # The device-key read is HERE too, and it is LAZY
//!
//! Every hub dial signs with THIS box's device key (decision 0288 item 8: the `device_key` row of the
//! settings database; no shared key, no variable), and the hub's access list decides what that key may
//! do — see [`vike_client::hubkeys`]. The read belongs at this composition root for exactly the reason
//! the environment sweep does: [`hub_keyring`] performs it for the tradehub-facing arms (`trade`, plus
//! the top-level read-only `report`; `trade` covers the read-only `trade status` too, which uses just
//! the observe half — it was a top-level `strategy-status` until ruling 17 folded it into the `trade`
//! family) and [`datahub_keyring`] for the datahub- and backtesthub-facing ones.
//!
//! ⚠ **This widened which arms open the credential store, and the rule that used to sit here is
//! the thing that changed.** It read: "no other subcommand needs a credential, and a provenance or
//! backtest command has no business opening the file that holds every venue key on the box." The
//! first half is simply no longer true — `backtest` and `data` both
//! dial a datahub that may require authentication, and `research study` dials the backtesthub
//! under the same device key (ruling 7 puts every verb that RUNS an engine there, so it is a
//! different daemon reached with the same credential — the store read is what it has in common with
//! the five, not the peer). The second half was written when
//! nothing in this binary could authenticate to one, and a `backtesthub --addr` is a NODE-FACING
//! invocation, the category that sentence already excepted. So the rule now reads: **an arm that
//! can dial a node may open the store; one that cannot, may not.** `config`, `secrets`, `init`
//! and `indicators` still never touch it.
//!
//! ⚠ **That paragraph's worry — a low-sensitivity key causing the file holding every venue
//! credential to be read — was answered by SEPARATING the classes**, which is what it said the
//! industrial answer was: `docs/decisions/0051-node-keys-live-in-their-own-store.md` gave the hub
//! keys their own table, and a device key has its own (`device_key`); these arms read THAT, never
//! the venue-credential table.
//!
//! It reaches the store on the handle the boot opened over the settings DIRECTORY this dispatcher
//! already resolved (`vike_secrets::access::device_key_load` for the key, and
//! `vike_secrets::CredentialStore::hub_keys` probed for the store's own findings) — the shape
//! `cmd::secrets` uses, and the one
//! `crates/vike-ops/tests/settings_secrets/settings_registry.rs`'s `CREDENTIAL_STORE_PIN` explicitly blesses ("the
//! composition root doing exactly what the rule asks for"), as opposed to the sweeping loaders that
//! open a location nothing in their signature mentions.

#![warn(unreachable_pub)]

pub mod cmd;
pub mod exit;
// Public for the workspace's refusal catalog alone (`crates/vike-backend/tests/refusal_catalog.rs` walks `FlagRefusalId::ALL`).
pub mod refusals;
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
         exit code. `prune --dry-run` lists the saved runs config.saved_runs_per_strategy would \
         remove",
    ),
    (
        "data",
        "market data, every verb under a REQUIRED group: `data history` is a past window in the \
         hist store — fetch, import or load the published real bars into it (no demo or \
         synthetic tape ships), read and export what it holds, report coverage and gaps (over \
         --addr); `realtime` is now (watch, status, record), `catalog` what is addressable, \
         `source` where rows may come from, and `instruments` and `splits` the operator's own rows",
    ),
    (
        "config",
        "this box's settings: provenance (`show`), a validating pre-flight (`check`) and \
         journalled one-key changes (`set`, `unset`), plus the stored rows a deploy builds and an \
         operator edits — the profiles this box runs from, a symbol rename, the instruments the \
         operator added, the venue address rows (`url`) and the IBKR cash-settled futures roots; \
         `config --help` lists every verb",
    ),
    (
        "trade",
        "observe + control a running vike-tradehub: the NODE-WIDE words take no group — \
         `trade status|health|links` read the node and `trade halt|resume` set its trading mode — \
         and the rest live under a REQUIRED group: one account, its orders, positions and \
         strategies, its venue's own reads, and the reconcile divergences held for an operator \
         (`trade --help` lists them; `watch` is named and refused as designed-not-built); a bare \
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
    // `cmd::research::study`'s `no_capability_lines` still names `vike-backend study` — but *no backend
    // serves it yet* is false, and leaving it was the same false confirmation one paragraph up.
    // The withdrawn clause is recorded here rather than silently dropped.
    //
    // ⚠ The sentence that read *the backtesthub it dials is ruling 7's half and does not exist
    // yet* is likewise spent: `crates/vike-backtesthub/src/backtesthub_server.rs` IS that daemon and
    // `deploy/vike-backtesthub.service` ships it. Which refusal an operator meets is now a property of
    // their DEPLOYMENT (a box that stood the daemon up reaches the capability negotiation; one that
    // did not ends at the dial), not of the build — `cmd::research::study`'s module doc carries both arms and
    // `crates/vike-cli/tests/shipped_cli/study_report_refusal_cli.rs` drives them.
    //
    // ⚠ `report`'s live-journal half is RETIRED (owner, 2026-11-18), so its line no longer offers a
    // tearsheet over a node's journal: `--node` and a bare run selector are refused naming the
    // spelling that replaced each (`crate::cmd::report`'s module doc). A live account's results are
    // its `account:` source, read from the datahub, and `--compare` holds two sources side by side.
    //
    // ⚠ And `skills/*/SKILL.md`'s verb tables are RENDERED from this array by
    // `scripts/gen_skills.sh`, so a false line here is a false line in three shipped,
    // agent-consumed pages — and `--check` cannot catch it, because it proves the pages MATCH this
    // array, never that this array is TRUE. (Its `cell` wraps a bare lower-case `<word>` in a code
    // span, so the example account is spelled out rather than as `<venue>`.)
    (
        "report",
        "the results of a run on this machine (`report run:@last`) or of a live account read from \
         the datahub (`report account:binance/MAIN`), two side by side with `--compare`; read-only \
         — the live-journal `--node` and a bare run selector are retired and refused, naming the \
         spelling that replaced each",
    ),
    (
        "research",
        "investigate a signal and FIT a model — the plane BEFORE a strategy exists: \
         `research study` asks the backtesthub (`vike-backend backtesthub --addr`, \
         config.backtest_addr) to run a study FILE over the hist store it holds; a peer that does \
         not advertise `study` (the data server, an older backtesthub) is refused by name and \
         pointed at `vike-backend study`",
    ),
    (
        "strategy",
        "what a strategy FILE does across the numbered SDKs it can be built against: \
         `strategy sdk-diff` runs one backtest with the file built against SDK N and against SDK M \
         on the backtesthub and compares every metric, the trades and every recorded field (exit 0 \
         identical, 6 they differ, 7 neither traded so nothing was compared, 1 a run or a build \
         failed); `--all` reports a folder, a line a file. `strategy migrate` first rewrites a Rust \
         file's JSON `save_state`/`load_state` onto `StrategyState` (ABI 15), pinned or not, and a \
         bar's or a tick's `String` symbol onto `vike_marketdata::Symbol` unless its pinned SDK \
         already lends `Symbol` rows (a hook or read of another shape gets a hint and is left as it \
         is), then moves a file pinned to `sdk = N` to \
         a newer SDK one SDK at a time, each step built before it is kept, or applies the renamed \
         names to a Rhai script; the original is kept as a numbered backup, and `--dry-run` writes \
         nothing",
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
         key in it, remove one, and create it EMPTY (ls | path | set | rm | init | copy-hub-keys \
         | ibc-start | ibkr-cp-login)",
    ),
    (
        "account",
        "the ACCOUNT TABLE of the settings database: list it (`account ls`), add a row with the \
         venue and the tier it trades at, rename, re-tier, cap, deactivate, activate or remove one \
         (`account rm`), copy one row's credentials onto another, write the venue's own account id \
         (`set-book`) and fold what live venues confirmed (`confirm`)",
    ),
    (
        "backend",
        "this box and its hubs, and a client's link to a vike-tradehub: on the server's box \
         `mint-missing` mints every hub key the project lacks and `probe-copy` copies the settings \
         database with every key replaced, for a deploy's pre-flight; on the client's box \
         `connect` attaches this box to a tradehub, `status` says what it reaches and \
         `disconnect` closes the tunnel; `ping` asks any datahub or backtesthub address what it \
         serves",
    ),
    (
        "gatewayhub",
        "MINT the hub keys a vike-gatewayhub server authenticates with (setup): the observe and \
         control pair and the trade key, on that server's box",
    ),
    (
        "access",
        "who may reach the hubs, on the server's box: every device signs with its own key and this \
         box keeps the public halves (add | ls | role | revoke | invite | swap)",
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
    (
        "completions",
        "print the script that makes bash, elvish, fish, powershell or zsh tab-complete this \
         binary's verbs, sub-verbs and flags, rendered from the same compiled-in tree; turn it on \
         once, e.g. `vike-cli completions powershell >> $PROFILE`",
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
    /// gate, params, strategies, templates, script-api, script-check, prune.
    Backtest,
    /// Market data, by group: history (a past window), realtime, catalog, source, instruments,
    /// splits.
    Data,
    /// This box's settings: provenance (show), a pre-flight (check), one-key changes (set, unset),
    /// the stored profiles and the operator's rows.
    Config,
    /// Observe and control a running vike-tradehub; a bare `trade` opens the REPL.
    Trade,
    /// The results of a finished run on this machine, or of a live account read from the datahub.
    Report,
    /// Investigate a signal and fit a model — the plane BEFORE a strategy exists.
    Research,
    /// What a strategy file does across the SDKs it can be built against: sdk-diff, migrate.
    Strategy,
    /// Inspect the credential store this box reads, set one key in it, create it empty.
    Secrets,
    /// The account table: list it, add, change or remove a row, fold what the venues confirmed.
    Account,
    /// This box and its hubs (mint-missing, probe-copy), a client's link to a vike-tradehub
    /// (connect, status, disconnect), and ping.
    Backend,
    /// Mint the three hub keys a vike-gatewayhub server authenticates with.
    Gatewayhub,
    /// Who may reach the hubs: grant a device, list (ls), change a role, revoke, invite, swap.
    Access,
    /// Create <project>/user_data — starter strategies, presets, notebooks; no run profile and no
    /// data — and print how to load real bars and run a strategy file.
    Init,
    /// Print the indicators a Rhai strategy can call, with their parameters.
    Indicators,
    /// Write this binary's own command surface as JSON.
    Surface,
    /// Print the script that makes a shell tab-complete this binary's verbs and flags.
    Completions,
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
