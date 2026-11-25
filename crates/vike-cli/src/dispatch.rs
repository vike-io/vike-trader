//! The entry point and the router: `run` picks the verb word, boots once, then `dispatch` hands the
//! verb its argv tail.

use std::process::ExitCode;

use vike_client::resolved::{
    Resolved, datahub_keyring, hub_keyring, install_user_indicators, seal_refusal_text,
};

use crate::boot::resolve_policy;
use crate::cmd;
use crate::cmd::args;
use crate::exit::Exit;
use crate::{COMMANDS, RETIRED_COMMANDS, Root};

/// Parse the command line (argv WITHOUT the binary name) and dispatch. Returns the process exit
/// code. The `src/main.rs` shim calls this with the process args after the program name, and the multicall
/// dispatcher (`vike-backend vike-cli …`, or `vike-cli` reached through a link) calls it with the
/// tail after the tool name — so the program name in every usage line is `vike-cli` in both cases.
///
/// ⚠ **"First token = verb" is the one place this binary splits argv by hand** (decision 0155 keeps
/// it, and says why: every other shape is `clap`'s). A word that is a verb goes straight to that
/// verb's module, which reads the rest of its line as its own `clap` struct; anything else — a bare
/// `vike-cli`, `--help`, `help`, `-V`, a mistyped verb — is the ROOT command's to answer
/// (`crate::Root`), which needs no settings, so none are loaded for it. A retired verb is
/// answered by name, with everything after it ignored: the old dispatcher never looked past it
/// either, and `vike-cli sweep --help` must still say where `sweep` went.
///
/// Before ANY verb runs, the environment is checked for variables that have been REMOVED (settings
/// unification, Phase 5) and the machine's policy ceiling is resolved — see [`resolve_policy`].
/// Both happen here rather than per-subcommand so a stale variable cannot be honoured by one
/// surface and refused by another, and so `vike-cli trade`'s guardrail reads the same
/// ceiling the trading binaries enforce.
///
/// ⚠ **A verb's help or version page runs nothing, so it is answered BEFORE that boot** (the root's
/// rule, above): the arms below resolve a hub keyring before the verb reads its own line, which put
/// "cannot read the hub-key store" on `--help` on an older store, and the boot opens the settings
/// database. A real run reads what it always read. `clap` judges what is a help request, over the
/// tree the verb then parses with, so a VALUE spelled `--help` (`config set -- KEY --help`) is a run.
pub fn run(mut argv: impl Iterator<Item = String>) -> ExitCode {
    let Some(command) = argv.next() else {
        // A BARE `vike-cli`: the help on stdout, the usage rung for the exit code (it always was).
        return match args::parse::<Root, _, _>(args::PROGRAM, ["--help".to_string()]) {
            Err(stop) => args::finish_help_as_usage(&stop),
            Ok(_) => args::refuse(args::PROGRAM, "no such command"),
        };
    };
    if let Some((_, why)) = RETIRED_COMMANDS.iter().find(|(name, _)| *name == command) {
        // ⚠ A RETIRED verb is answered with its replacement, never as an unknown one. See
        // [`RETIRED_COMMANDS`] for why "unknown command" is the wrong product on the right rung.
        eprintln!("vike-cli: {why}");
        return Exit::Usage.into();
    }
    if command == "help" {
        // `vike-cli help [anything]` is the root help, as it always was; `clap`'s own `help <verb>` would
        // answer a verb's page, which is `vike-cli <verb> --help`.
        return root_answer(vec!["--help".to_string()]);
    }
    if !COMMANDS.iter().any(|(name, _)| *name == command) {
        let mut all = vec![command];
        all.extend(argv);
        return root_answer(all);
    }
    let argv: Vec<String> = argv.collect();
    let tree = VERB_TREES.iter().find(|(verb, _)| *verb == command);
    if let Some(page) = tree.and_then(|(_, tree)| args::help_answer(tree(), &argv)) {
        return args::finish(&page);
    }
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
    install_user_indicators(args::PROGRAM, resolved.user_data_dir.as_deref());
    dispatch(&command, argv.into_iter(), &resolved)
}

/// One row of [`VERB_TREES`], named so `clippy::type_complexity` has nothing to say.
type VerbTree = (&'static str, fn() -> clap::Command);

/// Every verb's `clap` tree, by its word: what answers a verb's help before the boot ([`run`]), and
/// what `crate::surface`'s `whole_command` folds into the published reference.
///
/// `surface_tests.rs`'s `every_verb_is_walked` holds every verb to carrying arguments or sub-verbs,
/// so a new verb left out of this list fails there.
pub(crate) const VERB_TREES: [VerbTree; 16] = [
    ("backtest", cmd::backtest::command),
    ("data", cmd::data::command),
    ("config", cmd::config::command),
    ("trade", cmd::trade::command),
    ("report", cmd::report::command),
    ("research", cmd::research::command),
    ("strategy", cmd::strategy::command),
    ("secrets", cmd::secrets::grammar::command),
    ("account", cmd::secrets::grammar::account_command),
    ("backend", cmd::backend::command),
    ("gatewayhub", cmd::gatewayhub::command),
    ("access", cmd::access::command),
    ("init", cmd::init::command),
    ("indicators", cmd::indicators::command),
    ("surface", cmd::surface::command),
    ("completions", cmd::completions::command),
];

/// What the ROOT command says to an argv whose first word is not a verb: the help, the version, or
/// a refusal with the closest verb named — never a run. `Root`'s values are never read; the only
/// outcome of reading it is a [`args::Stop`].
fn root_answer(argv: Vec<String>) -> ExitCode {
    match args::parse::<Root, _, _>(args::PROGRAM, argv) {
        Err(stop) => args::finish(&stop),
        // Unreachable: a first word that parses as a verb never gets here (the router above sends
        // it to its module). Refuse rather than guess if the two tables ever disagree.
        Ok(_) => args::refuse(args::PROGRAM, "no such command"),
    }
}

/// Route one VERB to its module. Help, version and an unknown verb never get here: [`run`] gives
/// them to the root command (`crate::Root`) before anything is booted.
///
/// [`Resolved::policy_max_notional`] reaches only `trade`, the ORDER-WRITE surface here, whose
/// mandatory preview shows a guardrail against it (and refuses an order over it); every other subcommand has no order to
/// size. [`Resolved::node_ack_wait`] reaches it too, for the same reason: it sends a command and then
/// waits for the node's verdict on it.
/// [`Resolved::settings_dir`] reaches `secrets`, which inspects the credential store inside it,
/// `trade`, which persists its REPL history under its `state/` sub-directory, and `config`, which
/// reports the whole directory — the files in it, what each setting resolved to, and which layer
/// set it — and, since `config set` landed, WRITES one key back into it. `config` takes it as a
/// PARAMETER rather than re-deriving it so the directory it prints and edits is provably the one
/// every other surface reads, and takes which rung answered for it alongside it for the same
/// reason: only this root can say. `secrets` takes [`Resolved::settings_dir_override`] as well — its fallback rung
/// resolves the store the way every credential reader on the box does, so that command can never
/// print a path nothing opens. (Since the boot honours a name with no walk, a `None` directory now
/// implies a `None` override too, so that rung is unreachable from here; it is kept as the belt
/// against a regression, and `cmd::secrets`'s `settings_dir_of` argues it at its site.)
/// [`Resolved::user_data_dir`] — that directory's SIBLING — reaches `init`, which scaffolds it,
/// `report`, which reads finished runs under it, and `backtest`, which does both halves: its
/// reading verbs join their runs root onto it and its `--local` arm hands the same directory to
/// the engine it spawns, so the run the engine SAVES is the run `backtest ls` lists. (This said
/// "`init` alone" for a while after `backtest` and `report` had started reading it.) This box's device
/// KEY ([`hub_keyring`]) reaches the node-facing top-level surfaces only — `trade`, and `report` until its
/// live-journal door was retired on 2026-11-18 — and are resolved inside their arms so no other
/// subcommand opens the credential file at all. ⚠ This used to also name the read-only MEMBERS as
/// a fixed pair, `trade status` and `report`; [`hub_keyring`]'s own doc is the authority on which
/// reads use just the observe half, and it no longer counts them either — the trade-CLI-plane's
/// group layer added `order`/`position`/`strategy ls` to that set.
/// The verbs that REFUSE to run while [`Resolved::seal_refusal`] is set, as against every other
/// verb in this binary, which runs.
///
/// ⚠ **The list is of ACTORS, and the test that matters is the one asserting the COMPLEMENT.** A
/// gate like this is satisfied trivially by naming nothing, so
/// `crates/vike-cli/tests/cli_verbs/seal_enforcement.rs` drives BOTH directions: `trade` refuses
/// (`crates/vike-mcp/src/main.rs` is the other actor, refusing before it reads a flag), and
/// `config set` / `config show` / `config unset` / `secrets ls` still run in the same state —
/// `config unset` above all, because it is the repair for a refused row. The second half
/// is the JSON incident's lesson written as a test — a refusal must never take down the command
/// its own text names as the repair. ⚠ `config compare` and `config adopt` are RETIRED
/// (docs/decisions/0086) — there is no crossing to perform any more, so they are gone from this
/// comment rather than merely from the list.
///
/// `config check` is deliberately ABSENT: it is the surface that REPORTS this state (`Level::Fail`,
/// so a deploy pre-flight and an `ExecStartPre=` stop a start), and a `config check` that refused
/// to run would leave the operator with no way to read what is wrong.
const SEAL_REFUSING_VERBS: [&str; 1] = ["trade"];

/// `config.saved_runs_per_strategy` as the backtesthub applies it on this box: over this project's runs root, the
/// marks beside it and the alert rules in the boot's state directory (the daemon's `run_serve` builds the same rule).
/// `None` without the row, or without a project. `backtest prune --dry-run` lists what it would remove.
fn saved_run_retention(resolved: &Resolved) -> Option<vike_runs::RunRetention> {
    let keep = resolved.saved_runs_per_strategy?;
    let user_data = resolved.user_data_dir.as_deref()?;
    Some(vike_runs::RunRetention {
        keep_per_strategy: usize::try_from(keep).unwrap_or(usize::MAX),
        marks_root: user_data.join(vike_paths::state_path::MARKS_SUBDIR),
        referencing_documents: resolved
            .state_dir
            .iter()
            .map(|state| state.join(vike_paths::state_path::ALERTS_FILE))
            .collect(),
    })
}

/// What `secrets` and `account` take from this root: the resolved directory and the override that
/// produced it, the ledger's home, the environment map, the store's scope, the instant, and the
/// gatewayhub tell. `cmd::secrets::Context` carries the argument for why they arrive as one struct.
fn secrets_context<'a>(
    resolved: &'a Resolved,
    tell_gatewayhub: &'a dyn Fn(i64) -> String,
) -> cmd::secrets::Context<'a> {
    cmd::secrets::Context {
        settings_dir: resolved.settings_dir.as_deref(),
        settings_dir_override: resolved.settings_dir_override.as_deref(),
        state_dir: resolved.state_dir.as_deref(),
        env: &resolved.env,
        scope: resolved.store.scope(),
        now_ms: vike_model::now_ms(),
        tell_gatewayhub: Some(tell_gatewayhub),
    }
}

fn dispatch(command: &str, args: impl Iterator<Item = String>, resolved: &Resolved) -> ExitCode {
    if let Some(why) = resolved.seal_refusal.as_deref()
        && SEAL_REFUSING_VERBS.contains(&command)
    {
        eprintln!("{}", seal_refusal_text(&format!("{} {command}", args::PROGRAM), why));
        return ExitCode::FAILURE;
    }
    // A write that landed on an account is told to the running gatewayhub through the datahub (decision
    // 0287); a closure because `datahub_keyring` opens the store, so only a write pays for it.
    let tell_gatewayhub = |account: i64| {
        cmd::secrets::running::tell(
            resolved.datahub_addr.as_deref(),
            datahub_keyring(resolved, args::PROGRAM).as_ref(),
            account,
        )
    };
    match command {
        // ⚠ The ONE remote run verb, and it is a PLANE rather than a verb: `backtest run`
        // computes, and WHAT it computes is the PROFILE's answer on two independent axes
        // (`cmd::backtest`'s `route_of` — a non-empty `[paramscan]` grid searches the parameter space,
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
        // same device key `data` does. See [`datahub_keyring`] for where it comes from.
        // `backtest_addr` is handed in because the middle rung of its address ladder is a SETTING,
        // which only this composition root may read.
        "backtest" => cmd::backtest::run(
            args,
            resolved.project_root.as_deref(),
            // ⚠ The `user_data` directory WHOLE, not a pre-joined runs root, and that is the fix for
            // a measured defect rather than a tidy-up. The READING verbs join their runs and marks
            // roots onto it, and the `--local` arm hands the SAME value to the engine it spawns —
            // so the reader and the writer answer with one directory by construction.
            //
            // ⚠ This comment used to say they could not disagree because "the producer honours
            // `$VIKE_USER_DATA_DIR` too", and that covered only half the resolution. The engine's
            // fallback is a walk from ITS working directory that does not read `$VIKE_SETTINGS_DIR`,
            // while `Resolved::user_data_dir` is the sibling of the settings directory the boot
            // resolved with that override applied. So with `$VIKE_SETTINGS_DIR` naming one project
            // and the working directory inside another, `run --local` saved where `ls` did not look
            // (#2179's review). `cmd::engine`'s `Engine::with_user_data_dir` carries the rest.
            //
            // Resolved HERE, from the walk this dispatcher already owns, because a `src/cmd/` file
            // may not resolve a project for itself.
            resolved.user_data_dir.as_deref(),
            // The clock, read ONCE here and passed down, because `vike-model` contains no ambient
            // clock read and a `src/cmd/` file reads no global state — the same rule that makes the
            // directory above a parameter. `tag` is the only sub-verb that uses it.
            vike_model::now_ms() / 1_000,
            resolved.backtest_addr.as_deref(),
            datahub_keyring(resolved, args::PROGRAM).as_ref(),
            saved_run_retention(resolved).as_ref(),
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
            datahub_keyring(resolved, args::PROGRAM).as_ref(),
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
            cmd::config::Context {
                settings_dir: resolved.settings_dir.as_deref(),
                // WHICH rung answered for the directory: `VIKE_SETTINGS_DIR` or the walk. Only this
                // root can know (below it the two are indistinguishable: the resolver takes the
                // override as a parameter and returns a bare path), `cmd::config::check` needs the
                // distinction and nothing else does: a NAMED directory that is not on disk is a
                // set-but-unhonoured value and a refusal, while a WALKED one that is not there is
                // an unconfigured checkout and merely a warning. Derived from the same override
                // value the resolver was handed (`vike-boot` returns it already trimmed and
                // blank-filtered), so the two cannot disagree about a blank one.
                origin: cmd::config::check::dir_origin(
                    resolved.settings_dir_override.as_deref(),
                    resolved.settings_dir.as_deref(),
                ),
                state_dir: resolved.state_dir.as_deref(),
                scope: resolved.store.scope(),
                now_ms: vike_model::now_ms(),
                project_root: resolved.project_root.as_deref(),
                max_mount_leverage: resolved.policy_max_mount_leverage,
            },
        ),
        // The ORDER-WRITE surface. The device key means the settings database is opened for it (see
        // [`hub_keyring`]) — including for `trade status`, the READ that authenticates to a node and
        // now rides this arm.
        // ⚠ ONE arm, and it has grown twice since it first collapsed four operations into it.
        // Ruling 17 of `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md` is
        // the first collapse: `trade` used to be the REPL alone, with the read-only node question
        // promoted to a top-level `strategy-status` beside it — the only member of that family
        // (`orders`, `positions`, the old `state`) to be. It became `trade status`, and `trade
        // halt` / `trade resume` joined it as the two writes, so the same three words work at the
        // prompt and on the command line. The trade-CLI-plane design
        // (`docs/superpowers/specs/2026-09-21-trade-cli-surface-design.md`) is the second: a
        // REQUIRED group layer on top, so `trade order|position|strategy <verb>` are one-shot too
        // now — routed inside `cmd::trade::run` rather than given their own dispatch arms, which is
        // why this stays ONE arm for the whole plane rather than growing a new match arm per verb.
        // `cmd::trade::run` claims a leading verb and falls through to the REPL when there is none;
        // the keyring is resolved here either way, since every path authenticates to a node.
        "trade" => cmd::trade::run(
            args,
            resolved.policy_max_notional,
            resolved.node_ack_wait,
            resolved.settings_dir.clone(),
            &hub_keyring(resolved, args::PROGRAM),
            // For `trade health` and `trade links`, and a closure because `datahub_keyring` opens the store.
            resolved.datahub_addr.as_deref(),
            &|| datahub_keyring(resolved, args::PROGRAM),
        ),
        // `report` takes its keyrings as CLOSURES: only a live source (`report account:`, a forward
        // run) dials, the datahub for the fills and the tradehub (`config.node_addr`, the observe
        // key) for the tier and the ledgers, so `report run:<backtest>` opens no credential store.
        // Its live-journal door (`--node`) was retired on 2026-11-18 and is refused at the parse.
        //
        // ⚠ `report run:<run>` re-renders a FINISHED run out of the run directory, so this arm joins
        // the same runs/marks pair onto the same `Resolved::user_data_dir` the `backtest` arm above
        // hands down whole, for the same reason — a `src/cmd/` file may not resolve a project for
        // itself. The marks root is a SIBLING of the runs root and never a child
        // (`vike_paths::state_path::MARKS_SUBDIR` carries the argument), and it is here so that
        // `report run:@baseline` accepts the whole selector grammar rather than a truncated half of it.
        "report" => cmd::report::run(
            args,
            &cmd::report::Context {
                runs_root: resolved
                    .user_data_dir
                    .as_ref()
                    .map(|d| d.join(vike_paths::state_path::RUNS_SUBDIR)),
                marks_root: resolved
                    .user_data_dir
                    .as_ref()
                    .map(|d| d.join(vike_paths::state_path::MARKS_SUBDIR)),
                datahub_addr: resolved.datahub_addr.as_deref(),
                datahub_keys: &|| datahub_keyring(resolved, args::PROGRAM),
                node_addr: resolved.node_addr.as_deref(),
                hub_keys: &|| hub_keyring(resolved, args::PROGRAM),
                now_ms: vike_model::now_ms(),
            },
        ),
        // `research` is the plane BEFORE a strategy exists — investigate a signal, fit a model —
        // and `research study` asks the BACKEND, because a study opens the hist store and drives a
        // trainer next to it: the same compute-to-data argument as `backtest` directly above. ⚠ It
        // is aimed at the backtesthub (`vike-backend backtesthub --addr`), not the data server:
        // ruling 7 puts every verb that RUNS an engine there, and that daemon admits this box's
        // device key by the same access list the datahub keeps, so this is the same keyring.
        // Each module doc argues its own half; neither sends a request today (the server arms are
        // follow-ups) and both say so where an operator will read it.
        //
        // ⚠ `backtest_addr` is handed in rather than looked up below: the sub-verb's address
        // ladder is `--addr` → this key → `vike_config::DEFAULT_BACKTEST_ADDRESS`, and the middle
        // rung is a SETTING — which only this composition root may read. The plane folds
        // `vike_client::resolved`'s `backtesthub_dial_address` rather than carrying a second copy; a ladder forks when
        // a SETTING forks, not when a plane does.
        //
        // ⚠ **This arm is what keeps `Resolved::backtest_addr` alive.** It was the `study` arm's
        // only reader, and `study` retired off the top level here (ruling 1). A retirement to
        // NOTHING would have left the field written and never read — `dead_code` on a lane that
        // runs `-D warnings` — so the successor arm is load-bearing rather than cosmetic.
        "research" => cmd::research::run(
            args,
            resolved.backtest_addr.as_deref(),
            datahub_keyring(resolved, args::PROGRAM).as_ref(),
        ),
        // `strategy sdk-diff` runs backtests on the backtesthub and `strategy migrate` builds each
        // step there, so both dial as `backtest run` does: the same address ladder and the same
        // keyring (a run and a Build are Write scope). `migrate` also reads the installed SDKs from
        // the project.
        "strategy" => {
            let keys = datahub_keyring(resolved, args::PROGRAM);
            cmd::strategy::run(
                args,
                &cmd::strategy::Context {
                    project_root: resolved.project_root.as_deref(),
                    configured_address: resolved.backtest_addr.as_deref(),
                    keys: keys.as_ref(),
                },
            )
        }
        // BOTH the resolved directory and the override that produced it — see
        // `Resolved::settings_dir_override` for why the second is not redundant — plus the three
        // things its `set` arm needs and no reading subcommand does: the ledger's home, the
        // environment map (`--from-env NAME`, so nothing under `src/cmd/` reads the environment
        // itself) and the instant to stamp the record with. `cmd::secrets::Context` carries the
        // argument for why they arrive as one struct.
        //
        // ⚠ The CLOCK is read HERE rather than in `cmd/`, and it is read for every `secrets`
        // invocation including the read-only ones. `vike_change_journal` deliberately reads
        // no clock, so the instant is a parameter all the way down — the same shape
        // `vike_boot::journal_boot_settings`' `timestamp_ms` takes. One wall-clock read costs a
        // `secrets path` nothing and keeps the value a caller can see and a test can pin.
        "secrets" => cmd::secrets::run(args, secrets_context(resolved, &tell_gatewayhub)),
        // The ACCOUNT TABLE (the owner's ruling 2 of the stage-5 naming plan moved it out of
        // `secrets`): the same store, the same flag set and the same executors, so the same context.
        "account" => {
            cmd::secrets::run_account_plane(args, secrets_context(resolved, &tell_gatewayhub))
        }
        // The ONBOARDING surface for a vike-tradehub, and the SECOND credential writer in this
        // binary. It takes the same five facts `secrets` does — the resolved directory, the
        // override that produced it, the ledger's home, and the instant to stamp a record with —
        // plus two this dispatcher already holds and no other arm needs: `config.node_addr` (this
        // box's dial default, which `connect` REWRITES and `status` reports) and the KEYRING, whose
        // observe half signs the round trip that verifies a fresh attachment.
        //
        // ⚠ It takes NO environment map, unlike `secrets`. That is not an oversight — it is the
        // property that makes this writer narrower than the one 0036 reopened: `secrets set` has a
        // `--from-env NAME` form because an operator supplies its value, and here there is no
        // operator-supplied value to source. `setup` MINTS both keys; `connect` registers this box's own
        // device key. Neither path can be pointed at a variable, so no map is needed.
        //
        // This arm resolves the keyring too, and it is the only one that also WRITES it. (That
        // used to read "the FOURTH arm"; `report` made it the fifth on the day it landed, so the
        // count is gone rather than re-stated — an ordinal over a growing list of arms is the
        // shape of claim this repository has watched rot.)
        // ⚠ It resolves BOTH keyrings since `backend ping` landed, and the two are not
        // interchangeable: the tradehub verbs take the tradehub keyring, while `ping` dials a
        // vike-datahub-PROTOCOL daemon (the data server or the compute server) and takes the
        // other, signed under a different domain separator. Both hold this box's device key.
        //
        // ⚠ `datahub_keyring` OPENS THE SETTINGS DATABASE, and that cost now falls on every `backend`
        // invocation rather than only on a compute dialer. It reads the `device_key` row, never the
        // 168-venue `credential` table, and this arm already opens the same store for the tradehub
        // keyring — so the marginal act is one extra row read. The narrower
        // alternative (resolve it only on the `ping` arm) was refused for the reason
        // `datahub_keyring`'s own doc gives about `--addr`: a `Context` is built before a sub-verb is
        // claimed, so "will this invocation dial" cannot be answered here.
        "backend" => {
            let datahub_keys = datahub_keyring(resolved, args::PROGRAM);
            cmd::backend::run(
                args,
                cmd::backend::Context {
                    settings_dir: resolved.settings_dir.as_deref(),
                    settings_dir_override: resolved.settings_dir_override.as_deref(),
                    state_dir: resolved.state_dir.as_deref(),
                    node_addr: resolved.node_addr.as_deref(),
                    keys: &hub_keyring(resolved, args::PROGRAM),
                    backtest_addr: resolved.backtest_addr.as_deref(),
                    datahub_addr: resolved.datahub_addr.as_deref(),
                    datahub_keys: datahub_keys.as_ref(),
                    scope: resolved.store.scope(),
                    now_ms: vike_model::now_ms(),
                },
            )
        }
        // The gatewayhub's own key minting (decision 0164, verdict 6): a separate VERB because no key
        // is shared across services, sharing `cmd::backend`'s `Context` and its store/mint/journal
        // helpers. ⚠ It resolves NO keyring and no address: `Context` is shared with `backend`, whose
        // `keys` this arm never reads, so the empty default is the honest input.
        "gatewayhub" => cmd::gatewayhub::run(
            args,
            &cmd::backend::Context {
                settings_dir: resolved.settings_dir.as_deref(),
                settings_dir_override: resolved.settings_dir_override.as_deref(),
                state_dir: resolved.state_dir.as_deref(),
                node_addr: resolved.node_addr.as_deref(),
                keys: &vike_client::hubkeys::HubKeyring::default(),
                backtest_addr: None,
                datahub_addr: None,
                datahub_keys: None,
                scope: resolved.store.scope(),
                now_ms: vike_model::now_ms(),
            },
        ),
        // The access list (decision 0288): `gatewayhub`'s `Context` for the same reasons — it reads the settings
        // directory and the instant, and no keyring and no address.
        "access" => cmd::access::run(
            args,
            &cmd::backend::Context {
                settings_dir: resolved.settings_dir.as_deref(),
                settings_dir_override: resolved.settings_dir_override.as_deref(),
                state_dir: resolved.state_dir.as_deref(),
                node_addr: None,
                keys: &vike_client::hubkeys::HubKeyring::default(),
                backtest_addr: None,
                datahub_addr: None,
                datahub_keys: None,
                scope: resolved.store.scope(),
                now_ms: vike_model::now_ms(),
            },
        ),
        // The ONE subcommand that writes to the project's USER-CONTENT trees, and it writes ONLY
        // under `user_data/` and `strategies/` — never into `settings/`, and it opens no credential.
        "init" => cmd::init::run(
            args,
            resolved.user_data_dir.as_deref(),
            resolved.project_root.as_deref(),
        ),
        // Takes NOTHING from the dispatcher: the callable indicator roster is a property of the
        // BUILD (`vike_rhai::RHAI_INDICATORS` joined onto `vike_indicators::registry()`), never
        // of the project, the environment or a credential.
        "indicators" => cmd::indicators::run(args),
        // Takes NOTHING from the dispatcher either, and for a stronger reason than `indicators`:
        // the surface table is COMPILED IN, so this verb's answer cannot vary with a project, a
        // settings file, a credential or a network. That is what makes it safe to run in a release
        // workflow, and what lets the gate call the same function in-process with no subprocess.
        "surface" => cmd::surface::run(args),
        // Takes nothing either, for `surface`'s reason: the script is rendered from the compiled-in tree.
        "completions" => cmd::completions::run(args),
        // ⚠ Unreachable for a word that is not a verb: [`run`] answers those (help, version, a
        // retired or mistyped verb) before anything is booted, and the table it checks is
        // [`COMMANDS`]; `crate::tests::the_root_command_and_the_verb_table_name_the_same_verbs`
        // holds that table to the root command, and a verb in the table with no arm here lands in
        // this one, loudly.
        other => {
            eprintln!(
                "vike-cli: no route for the verb `{other}` — `COMMANDS` names it and this `match` \
                 does not"
            );
            Exit::Usage.into()
        }
    }
}
