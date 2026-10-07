//! The entry point and the router: `run` boots once, then `dispatch` hands one verb to its module.

use std::process::ExitCode;

use crate::boot::{datahub_keyring, node_keyring, resolve_policy};
use crate::cmd;
use crate::exit::Exit;
use crate::{RETIRED_COMMANDS, Resolved, install_user_indicators, print_help, print_version};

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
/// [`Resolved::user_data_dir`] — that directory's SIBLING — reaches `init`, which scaffolds it,
/// `report`, which reads finished runs under it, and `backtest`, which does both halves: its
/// reading verbs join their runs root onto it and its `--local` arm hands the same directory to
/// the engine it spawns, so the run the engine SAVES is the run `backtest ls` lists. (This said
/// "`init` alone" for a while after `backtest` and `report` had started reading it.) The node KEYS
/// ([`node_keyring`]) reach the node-facing top-level surfaces only — the two ORDER-WRITE ones
/// (`trade`, `mcp`) plus the read-only `report` — and are resolved inside their arms so no other
/// subcommand opens the credential file at all. ⚠ This used to also name the read-only MEMBERS as
/// a fixed pair, `trade status` and `report`; [`node_keyring`]'s own doc is the authority on which
/// reads use just the observe half, and it no longer counts them either — the trade-CLI-plane's
/// group layer added `order`/`position`/`strategy ls` to that set.
/// The verbs that REFUSE to run while [`Resolved::seal_refusal`] is set, as against every other
/// verb in this binary, which runs.
///
/// ⚠ **The list is of ACTORS, and the test that matters is the one asserting the COMPLEMENT.** A
/// gate like this is satisfied trivially by naming nothing, so
/// `crates/vike-cli/tests/seal_enforcement.rs` drives BOTH directions: these two refuse, and
/// `config mirror` / `config show` / `secrets list` still run in the same state. The second half
/// is the JSON incident's lesson written as a test — a refusal must never take down the command
/// its own text names as the repair. ⚠ `config compare` and `config adopt` are RETIRED
/// (docs/decisions/0086) — there is no crossing to perform any more, so they are gone from this
/// comment rather than merely from the list.
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
             sealed with. `vike-cli config check` still runs and reports the fault. There are no \
             settings FILES any more (docs/decisions/0086), so there is no `config compare` or \
             `config adopt --undo` to step back to: the repair is restoring \
             `<project>/settings/db/vike.db` from this box's nightly backup — no command repairs \
             it."
        );
        return ExitCode::FAILURE;
    }
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
        // same keys `data` does. See [`datahub_keyring`] for where they come from.
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
                now_ms: vike_model::now_ms(),
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
        // `crates/vike-ops/tests/docs/unrun_command_gate.rs` RUNS the grep that answers who dials.
        "mcp" => cmd::mcp::run(
            args,
            resolved.policy_max_notional,
            &node_keyring(resolved),
            resolved.state_dir.as_deref(),
            datahub_keyring(resolved),
            resolved.backtest_addr.as_deref(),
            resolved.datahub_addr.as_deref(),
        ),
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
        // this arm joins the same runs/marks pair onto the same `Resolved::user_data_dir` the
        // `backtest` arm above hands down whole, for the same reason — a `src/cmd/` file may not
        // resolve a project for itself. The marks root is a SIBLING of the runs root and never a
        // child (`vike_model::paths::state_path::MARKS_SUBDIR` carries the argument), and it is here so
        // that `report @baseline` accepts the whole selector grammar rather than a truncated half
        // of it.
        "report" => cmd::report::run(
            args,
            &node_keyring(resolved),
            resolved
                .user_data_dir
                .as_ref()
                .map(|d| d.join(vike_model::paths::state_path::RUNS_SUBDIR)),
            resolved
                .user_data_dir
                .as_ref()
                .map(|d| d.join(vike_model::paths::state_path::MARKS_SUBDIR)),
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
                now_ms: vike_model::now_ms(),
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
                    now_ms: vike_model::now_ms(),
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
                now_ms: vike_model::now_ms(),
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
