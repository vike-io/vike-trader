//! `vike-datahub` — the data service command line, as a LIBRARY function.
//!
//! This was the binary's body until the multicall merge. `main` became [`run`], taking the
//! environment, the working directory and argv as parameters. Both cfg arms travelled — the
//! serve-datafusion server and the feature-off stub — so a build that cannot serve still answers
//! `--help` identically, which is the property `short_circuit` exists to hold.
//!
//! The working directory is a parameter for the same reason the environment is, and the settings
//! registry cannot see it: a `current_dir()` inside a library is ambient state its caller cannot
//! override. This body read it TWICE before the move and now receives one answer.
//!
//! Everything below is the binary's own documentation, unchanged.
//! The real server (a `DataFusionHist`-backed [`serve`](crate::serve) loop) is compiled
//! ONLY under the `serve-datafusion` feature, which is what makes the concrete `DataFusionHist`
//! backend nameable here. A default build compiles the inert stub `main` below instead, so
//! `cargo build -p vike-datahub` always produces a runnable (if non-serving) binary that compiles
//! cleanly. (NB since vike-backtest's `hist-replay` no longer forces DataFusion — the harness now
//! compiles against the `HistStore` trait — `serve-datafusion` is what pulls the DataFusion/Arrow
//! tree; a default build is DataFusion-free.)
//!
//! Environment:
//! - `VIKE_DATAHUB_ADDR` — listen address (default `127.0.0.1:7878`). ⚠ A NON-LOOPBACK address is
//!   REFUSED at startup unless `VIKE_DATAHUB_ALLOW_PUBLIC_BIND` is ALSO set to the exact string
//!   `"1"` **AND at least one node key is configured** — the tradehub `bind_decision` treatment,
//!   mirrored and then composed with this server's own key presence (see `crate::server`'s
//!   module doc, and the guard's own comment at the match below). The opt-in alone used to be
//!   enough, behind a `warn!`; that is the state `docs/decisions/0025-datahub-remote-posture.md`
//!   names verbatim as the one it exists to prevent. The guard did NOT soften when authentication
//!   landed: the handshake is plaintext and authenticates the CONNECTION, not each frame, so the
//!   tunnel is still what supplies confidentiality and integrity.
//!
//! Credential store (`<project>/settings/secrets.env`) — `docs/decisions/0025-datahub-remote-posture.md`:
//! - `VIKE_DATAHUB_OBSERVE_KEY` / `VIKE_DATAHUB_CONTROL_KEY` — the scoped HMAC node keys. ⚠ **Their
//!   ABSENCE is the switch**: with neither set (the default) this server authenticates NOTHING and
//!   serves every verb that predates the keys exactly as every build before them did. With either
//!   set, EVERY connection must complete the `Hello`/`Auth` handshake, Observe reads history and
//!   the catalog, and Control additionally admits the `Backfill` WRITE and every `Run*` verb —
//!   those COMPILE CLIENT-SUPPLIED RHAI server-side, so they are not reads whatever they return.
//!
//!   ⚠ **The absence is no longer ONLY an authentication switch, and this bullet said it was until
//!   2026-09-07** ("behaves exactly as every build before them did"). It also decides one VERB: the
//!   destructive `DeleteSeries` is served by a KEYED server alone.
//!   `crates/vike-datahub/src/server.rs`'s `delete_series_verb` refuses it outright without keys
//!   and the `Welcome` does not advertise it, because `Scope::Control` is a word nothing enforces
//!   where nothing authenticates. `Backfill` is Control-scoped too and is NOT withheld for
//!   key-lessness — only `delete_series_verb` gates on `keyed` — so in a `backfill-serve` build
//!   with a table mounted it is served here. That asymmetry is the argument rather than an
//!   oversight, since a backfill writes rows a re-fetch restores and a removal takes the only copy.
//!   `docs/decisions/0050-a-key-less-datahub-serves-no-delete-verb.md` is the record.
//! - `VIKE_DATAHUB_STORE` — hist-store root dir. Unset, it falls through the SHARED precedence that
//!   `resolve_store_root_from` documents at its call site below, which is the authority: the
//!   CWD-relative `market_data/hist` this line used to name as the last resort is gone, and it was the
//!   whole bug (launching from anywhere but the repo root created an empty store beside the shell).
//! - `VIKE_USER_DATA_DIR` — user-content root, whose `indicators/` this server installs; falls back
//!   to the project walk from the working directory. ⚠ A client script resolves against the set
//!   THIS box installs — see `install_user_indicators`.
//!
//! ⚠ **`--help`/`--version` are answered in BOTH builds, identically, before anything else.** This
//! binary parsed NO argv at all: under the feature it went straight to opening a store and BINDING
//! A LISTENER, so `vike-datahub --help` STARTED A SERVER; without it, every invocation printed the
//! missing-feature message and exited 2, so `--help` looked like a first-class feature error when it
//! was simply not implemented. Help is a question about the COMMAND LINE, which both builds have;
//! the missing backend is a question about running, which is why only the RUN path still fails.

use std::process::ExitCode;

const USAGE: &str = "\
usage: vike-datahub

The headless compute-to-data backtest server. It takes no options: the listen address and the
hist-store root come from the environment.

  -h, --help      print this and exit 0
  -V, --version   print the version and exit 0

environment:
  VIKE_DATAHUB_ADDR    listen address (default 127.0.0.1:7878). A non-loopback address is
                       refused at startup unless VIKE_DATAHUB_ALLOW_PUBLIC_BIND=1 is also set
                       AND a node key is configured (see the credential store below).
                       The handshake is plaintext even when keys are set, so it is meant to be
                       reached over an SSH tunnel (ssh -L 7878:localhost:7878 <box>).
  VIKE_DATAHUB_ALLOW_PUBLIC_BIND
                       the explicit opt-in for a non-loopback bind (the exact string 1). It
                       consents to being REACHABLE, and is not sufficient on its own: with no
                       node key configured the bind is still REFUSED, because the opt-in is not
                       consent to serve a store write and a Rhai compiler unauthenticated. With
                       a key it proceeds behind a logged warning naming what is exposed.

credential store (<project>/settings/secrets.env — `vike-cli secrets path` prints it):
  VIKE_DATAHUB_OBSERVE_KEY   read scope: history + catalog. Absent = no authentication at all.
  VIKE_DATAHUB_CONTROL_KEY   write scope: the above, plus the Backfill store write, the
                             DeleteSeries store REMOVAL, and every Run* verb (those COMPILE
                             CLIENT-SUPPLIED RHAI on this server).
                       With NEITHER set this server authenticates nothing and serves every verb
                       EXCEPT DeleteSeries to whoever can reach the socket — which is why that
                       build is CONFINED to a loopback address (see VIKE_DATAHUB_ADDR).
                       A key-less server neither advertises the delete verb nor answers one:
                       with nothing authenticating, its Control scope would be a word nothing
                       enforces, and unlike a backfill (whose rows a re-fetch restores) a
                       removal takes the only copy. Delete on the box instead, with
                       `vike-cli data rm --store DIR`.
                       Setting either key makes the handshake mandatory on every connection, and
                       is what makes the delete verb exist at all; reaching it additionally needs
                       the CONTROL key, since a server holding none refuses that scope outright.
  VIKE_DATAHUB_STORE   hist-store root; unset, falls through VIKE_HIST_STORE, a repo checkout if
                       this machine has one, this project's own market_data/hist, then a per-user dir
  VIKE_USER_DATA_DIR   user-content root, whose indicators/ this server installs; falls back to
                       the project walk. A client script resolves against THIS box's set.

This binary serves only when built with the `serve-datafusion` feature, which is what makes the
concrete DataFusion backend nameable:
  cargo run -p vike-datahub --features serve-datafusion";

/// What a successful parse produced. Named variants rather than a bool pair, the shape
/// `vike-tradehub`'s and `vike-recorder`'s `Parsed` use — nothing here can be confused for a run.
///
/// `Debug` so a parse that was supposed to FAIL can report what it actually produced instead
/// (`Result::expect_err` requires it) — the whole value of the unknown-argument test below.
#[derive(Debug)]
enum Parsed {
    Run,
    Help,
    Version,
}

/// This binary takes no options, so the parser's whole job is the three things every binary owes a
/// caller: answer `-h`/`--help`, answer `-V`/`--version`, and REJECT anything else instead of
/// ignoring it (a typo in a systemd `ExecStart=` used to start the server anyway).
///
/// There is nothing to accumulate, so there is no loop: with no options to collect, the FIRST
/// argument already decides all three outcomes, and anything after it is unreachable by
/// construction (an argument this binary rejects is rejected on sight).
fn parse_args(mut argv: impl Iterator<Item = String>) -> Result<Parsed, String> {
    match argv.next().as_deref() {
        None => Ok(Parsed::Run),
        Some("-h" | "--help") => Ok(Parsed::Help),
        Some("-V" | "--version") => Ok(Parsed::Version),
        Some(other) => Err(format!("unknown argument: {other}")),
    }
}

/// Answer `--help`, `--version` and a usage error IDENTICALLY in both builds — `Some(code)` means
/// the process is done. Shared by the two `main`s below so the feature can never change what a
/// caller's `--help` does.
fn short_circuit(args: &[String]) -> Option<ExitCode> {
    match parse_args(args.iter().cloned()) {
        // Help is normal output a user pipes into a pager, so STDOUT and exit 0 — a non-zero
        // `--help` breaks `set -e`, packaging smoke tests and any wrapper that checks a status.
        Ok(Parsed::Help) => {
            println!("{USAGE}");
            Some(ExitCode::SUCCESS)
        }
        // `<name> <version> (<build identity>)` — the shape every `--version` on the box prints
        // (`git version 2.x`), with the COMMIT this binary was built from appended. This server is
        // deployed on the CI box and reached over an SSH tunnel, so "is the running binary the code I
        // pushed?" is asked of it from a terminal; `crates/vike-buildinfo/src/lib.rs` carries the
        // near-miss that made a bare version number insufficient.
        Ok(Parsed::Version) => {
            println!(
                "{}",
                vike_buildinfo::version_line(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"))
            );
            Some(ExitCode::SUCCESS)
        }
        Ok(Parsed::Run) => None,
        Err(e) => {
            eprintln!("vike-datahub: {e}\n\n{USAGE}");
            Some(ExitCode::from(2))
        }
    }
}

/// Load `<project>/user_data/indicators/*.rhai` and install them process-wide, so a client-supplied
/// Rhai strategy can call the user's OWN indicators — the same set `vike-cli` installs, out of the
/// same directory, through the same `vike_script::load_and_install_user_indicators`.
///
/// `VIKE_USER_DATA_DIR` names the directory outright and beats the walk, exactly as it does for
/// `vike-cli`: the two must not disagree about which strategy library this box has. The read is a
/// `.get` on the sweep `main` owns — ONE per composition root, the shape `vike-app`'s `PROCESS_ENV`
/// and `vike-cli`'s `resolve_policy` both use. Taking it as a parameter is also what keeps this
/// function honest about reading nothing else.
///
/// ⚠ Every rejected file is REPORTED and none of them is fatal. A half-edited indicator must not
/// stop a server that other clients are using, and a diagnostic is the only thing that can tell a
/// script author why their call resolved to nothing — but the server has no other channel to them,
/// so the message lands in the OPERATOR's log, not the client's error.
///
/// ⚠⚠ **THE SERVER'S SET WINS, AND THE CLIENT CANNOT SEE IT.** This is a real semantic change and
/// the reason the success case logs rather than staying silent. Before this existed, a client script
/// calling `my_ind()` failed HERE with an unknown-function compile error — loud, and unambiguous.
/// Now it binds to whatever `<server project>/user_data/indicators/my_ind.rhai` contains. `vike-cli
/// backtest` ships the SCRIPT to a remote server (see `crates/vike-cli/src/lib.rs`'s `COMMANDS`) and
/// never the indicator files, and no hash or version is exchanged, so the same script on two servers
/// with different indicator directories returns different numbers with nothing in the response
/// saying so.
///
/// That trade is deliberate — the alternative is that a user's own indicators simply do not work on
/// the path they run backtests through — but it is only defensible if a divergence is DIAGNOSABLE
/// afterwards. Hence the startup line naming the directory and the installed set: given two runs
/// that disagree, an operator can compare two logs and see it immediately. Shipping the set with the
/// request is the real fix and is a protocol change, not a startup one.
///
/// ⚠ `settings_dir` is `vike_boot::Booted::settings_dir` — the directory THIS process's ONE walk
/// resolved — and `user_data/` is its sibling. It used to be a second walk here
/// (`project_user_data_dir_from`, whose fallback does not honour `$VIKE_SETTINGS_DIR`), and
/// `deploy/vike-datahub.service` sets that variable: the moment its value and `WorkingDirectory=`
/// disagree, the indicator set this server installs and the project it otherwise reads stop being
/// the same project — which is undiagnosable from the client, per the ⚠⚠ above.
#[cfg(feature = "serve-datafusion")]
fn install_user_indicators(
    vars: &std::collections::HashMap<String, String>,
    settings_dir: Option<&std::path::Path>,
) {
    // `None` = no project above the working directory — the ordinary state of a server started from
    // somewhere with no user content under it. Nothing to read, nothing to say.
    let Some(user_data) = vike_model::state_path::user_data_dir_beside(
        vars.get("VIKE_USER_DATA_DIR").map(String::as_str),
        settings_dir,
    ) else {
        return;
    };
    for message in vike_script::load_and_install_user_indicators(&user_data) {
        tracing::warn!("{message}");
    }
    // Logged on SUCCESS too — see the ⚠⚠ above. A silent install is what makes a server-vs-server
    // divergence undiagnosable, and this line is the only record that this box answered with THIS
    // set. Empty is worth printing for the same reason: it distinguishes "no indicators" from "the
    // directory was never looked at".
    tracing::info!(
        user_data = %user_data.display(),
        installed = ?vike_script::installed_user_indicators(),
        "user indicators installed; a client script resolves against THIS set, not its own"
    );
}

#[cfg(feature = "serve-datafusion")]
pub fn run(
    vars: &std::collections::HashMap<String, String>,
    cwd: Option<&std::path::Path>,
    args: &[String],
) -> std::process::ExitCode {
    use std::net::{TcpListener, ToSocketAddrs};
    use std::sync::Arc;

    use vike_data::DataFusionHist;

    // BEFORE the log subscriber, the store and the listener: `--help` must not open a store, and it
    // certainly must not bind a port. (It did both — this binary read no argv at all.)
    if let Some(code) = short_circuit(args) {
        return code;
    }

    // ONE environment sweep for this whole root, threaded to every reader below — the shape
    // `vike-app`'s `PROCESS_ENV` and `vike-cli`'s `resolve_policy` use. Two sweeps could disagree if
    // anything mutated the environment between them, and a composition root is the one place that
    // question should have a single answer.

    // The workspace's ONE startup sequence. This root uses the narrowest slice of it there is — the
    // identity line and the log home — and every way it departs is a named arm below, which is the
    // point: those departures used to be prose in this comment block and nothing could see them.
    let booted = match vike_boot::boot(&vike_boot::BootSpec {
        env: vars,
        cwd,
        identity: vike_boot::Identity {
            name: env!("CARGO_PKG_NAME"),
            version: env!("CARGO_PKG_VERSION"),
        },
        removed_env: vike_boot::RemovedEnv::Ignore(
            "this server reads none of the ceilings whose variables were removed, and refusing to \
             start would make a data server fail on a stale variable that could not have affected \
             a single answer it gives.",
        ),
        settings: vike_boot::SettingsLoad::Skip(
            "this crate loads no `vike_config::Settings` at all — its store root and address come \
             from `VIKE_DATAHUB_STORE`/`VIKE_HIST_STORE`/`VIKE_DATAHUB_ADDR`. It joins when it \
             reads them.",
        ),
        // ⚠ The REASON here changed when `docs/decisions/0025-datahub-remote-posture.md` was
        // adopted; the ARM did not, and that is deliberate. It used to read "this server
        // authenticates nothing and mounts no venue", which is now false in its first clause — this
        // server CAN authenticate, and its node keys live in the credential store.
        //
        // It stays `Deferred` because the departure is about TIMING, not need: the keys are read a
        // few lines below, from `booted.settings_dir_override` — the answer THIS BOOT's one project
        // walk produced — rather than from a walk of this binary's own. That is the whole point of
        // the consolidation `vike_ops::settings`'s `VIKE_SETTINGS_DIR` block describes (four
        // composition-root reads became one under `vike-boot`), and taking `LoadWith` would mean
        // this root reading `$VIKE_SETTINGS_DIR` for itself to feed a loader — a fifth walk, and a
        // fifth chance for the log home, the store root and the credentials to answer with three
        // different projects.
        //
        // What is genuinely skipped with `Deferred` is `refuse_credential_file_arming`. That is
        // correct HERE and nowhere near a general licence: it refuses a store that arms REAL MONEY,
        // and this root mounts no venue, holds no `ExecutionClient` and can place no order, so it
        // has nothing to arm. The roots that CAN — `vike-app`, `vike-tradehub` — take `LoadWith` and
        // run it against the same file on the same box.
        credentials: vike_boot::Credentials::Deferred(
            "this server mounts no venue and can place no order, so it has no real-money arming to \
             refuse. It DOES read the credential store — for its own node keys — but a few lines \
             later, off this boot's own `settings_dir_override`, so the project walk stays the one \
             this crate performed rather than a second one of the binary's.",
        ),
        log_home: vike_boot::LogHome::UnderSettings,
        disclosure: vike_boot::Disclosure::Skip(
            "there are no settings to disclose — see the `SettingsLoad::Skip` reason. Describing a \
             tree this server does not read would make it the only consumer of settings it has no \
             consumer for.",
        ),
    }) {
        Ok(b) => b,
        // Unreachable with the arms above (nothing here can fail), but a refusal must never become
        // an unwrap in a server's `main`.
        Err(e) => {
            eprintln!("vike-datahub: {e}");
            return ExitCode::from(2);
        }
    };

    // Hold the appender guards for the process lifetime (dropping flushes the non-blocking writer).
    // `project_dir`: default the rolling trace file to `<project>/settings/state/logs` rather than
    // vike-log's `<exe_dir>/logs` last resort — this server runs from its project root under systemd,
    // where "beside the binary" is a directory nobody opens. `$VIKE_LOG_DIR` still wins.
    //
    // ⚠ The home now comes off the boot's OWN walk, which honours `$VIKE_SETTINGS_DIR`; the
    // `project_log_dir(&cwd)` call that stood here did not. `deploy/vike-datahub.service` sets BOTH
    // `WorkingDirectory=/srv/vike-<unit>` and `Environment=VIKE_SETTINGS_DIR=/srv/vike-<unit>/settings`,
    // so on the shipped deployment the two resolve to the same directory and nothing moves. Where
    // they DIFFER the old answer was the wrong one — the same defect `vike-recorder` had, where the
    // variable was set by every unit, claimed by both runbooks, and read by nothing.
    let _log_guards = vike_log::init(vike_log::LogConfig {
        file_prefix: "vike-datahub".to_string(),
        project_dir: booted.log_home.clone(),
        ..Default::default()
    });

    // WHICH BINARY IS THIS — the first line in the log, and the same string `--version` prints, so
    // the answer is readable from the log alone and from the binary alone and the two cannot
    // disagree. This server is deployed and long-lived; `crates/vike-buildinfo/src/lib.rs` carries
    // the release binary built four commits behind `main` that motivated it.
    tracing::info!("{}", booted.identity_line);

    // The datahub NODE KEYS (`docs/decisions/0025-datahub-remote-posture.md`), from the credential
    // store — read HERE, off `booted.settings_dir_override`, which is what THIS process's ONE
    // project walk answered. Using the boot's answer rather than a walk of this binary's own is
    // what keeps the log home, the store root and the credentials all pointing at one project;
    // `vike-recorder`'s `webhook_targets` threads the same value into its own store read for
    // exactly that reason, after the same defect bit it.
    //
    // ⚠ `vike_secrets::resolve_project`, not `vike_bridge_core::credentials::…`, and that is a
    // WEIGHT decision: the canonical wrapper drags the ureq/tungstenite/rustls transport stack,
    // which is absent from a DEFAULT vike-datahub graph and must not join it to read a `.env`.
    // vike-secrets is a zero-vike-dependency leaf already in this crate's closure.
    //
    // ⚠ A store that EXISTS and cannot be READ is NOT "no credentials". The first is a permissions
    // bug that silently drops this server to unauthenticated; the second is the ordinary
    // unconfigured state. They must never look the same to an operator, so the unreadable case is
    // an `error!` naming the consequence, not a shrug.
    // ⚠ WHY THIS FLAG EXISTS: an UNREADABLE store and an ABSENT one both arrive downstream as an
    // empty credential map — deliberately, and documented (`CLAUDE.md`: a store that is present and
    // unopenable logs `error!` and returns an EMPTY map). That is fine while the consequence is
    // "stay unauthenticated"; it stopped being fine when the bind guard turned the same state into
    // a REFUSAL, because the refusal then tells an operator whose keys are merely unreadable that
    // they have "NO node keys" — and `Restart=on-failure` repeats that wrong diagnosis forever.
    // The loader's behaviour is untouched (that is `0013`'s degrade case, and other consumers rely
    // on it); only this binary's ability to SAY WHICH case it is changes.
    let mut store_unreadable = false;
    // ⚠ THE NODE STORE, NOT THE CREDENTIAL STORE — and for this binary the distinction is the whole
    // point. This server needs a node key pair and NOTHING else: it does not trade, holds no venue
    // account and signs no order. Until 2026-09-08 it called `resolve_project`, which opens
    // `<project>/settings/secrets.env` — the file holding every venue key on the box, 168 names of
    // them — to find two. `resolve_node_keys` reads `node.env`, falls back to the old file only
    // while a box has not migrated, and says which it used.
    //
    // A deployment can now give this service a settings directory whose `node.env` holds its two
    // keys and whose `secrets.env` does not exist at all, and it starts correctly authenticated —
    // which is what "compute-to-data" should have meant from the start.
    let (credentials, key_source): (std::collections::HashMap<String, String>, _) =
        match vike_secrets::resolve_node_keys(
            booted.settings_dir_override.as_deref(),
            vike_model::credential_keys::is_platform_key,
        ) {
            Ok((resolved, source)) => {
                // The store's own findings. `vike-secrets` returns them as DATA (it carries no
                // logging dependency) and `vike_bridge_core::credentials::
                // try_load_workspace_secrets_at` is what normally logs them — this root does not
                // go through that wrapper (see the ⚠ above), so it owes the same surfacing rather
                // than being the one binary where a 0644 credential file is read in silence.
                if let Some(w) = &resolved.warning {
                    tracing::warn!("{w}");
                }
                if let Some(w) = &resolved.legacy {
                    tracing::warn!("{w}");
                }
                (resolved.secrets.into_map(), source)
            }
            Err(e) => {
                tracing::error!(
                    error = %e,
                    "vike-datahub: credential store PRESENT but UNREADABLE — any configured \
                     datahub node keys were NOT loaded. On a LOOPBACK bind this server is about \
                     to serve UNAUTHENTICATED behind the bind guard alone; on a NON-LOOPBACK one \
                     it will now REFUSE TO START a few lines below, because keyless plus \
                     off-box is the combination the guard exists for. Either way the cause is \
                     this file: fix the store's permissions and restart"
                );
                store_unreadable = true;
                (Default::default(), vike_secrets::NodeKeySource::Absent)
            }
        };
    // Said ONCE, at the root that read it, naming the file and the move. A daemon that keeps its
    // node keys in the venue-key file still works and is told, every start, what to do about it.
    if key_source == vike_secrets::NodeKeySource::LegacyCredentialStore {
        // `settings_dir` is an OPTION here — a box with no project above its working directory is a
        // legitimate state — so the notice names the directory the store resolver actually used
        // rather than unwrapping, and falls back to the placeholder the message reads well with.
        let dir = vike_secrets::workspace_node_path_from(booted.settings_dir_override.as_deref())
            .parent()
            .map_or_else(|| "<project>/settings".to_string(), |d| d.display().to_string());
        tracing::warn!("{}", vike_secrets::legacy_node_key_notice(&dir));
    }
    let keys = vike_datahub_client::node_auth::node_keys_from_vars(&credentials);

    // The user's OWN indicators, installed process-wide BEFORE the listener accepts anything: this
    // server COMPILES client-supplied Rhai. Two verbs reach it — `server.rs`'s `run_backtest`, whose
    // profile carries the SOURCE in `[strategy.params].src` (`vike-cli backtest --script <s.rhai>`
    // injects it there) and resolves through `harness::registry`'s `"rhai"` arm, and `RunSlice` via
    // `wire_run::to_strategy_spec` → `vike_studio_core::build_strategy`. Without this, the identical
    // script that runs under `vike-cli` on the operator's own box fails HERE with an
    // unknown-function compile error — and `vike-cli` is the very surface that installs them
    // locally, so the asymmetry lands on the one user who cannot see the server.
    //
    // ⚠ After `vike_log::init`, before the bind: every rejected file must be REPORTED (from inside a
    // strategy a failed load is indistinguishable from a typo in the call — both are
    // function-not-found), so it needs a subscriber; and an indicator set that changed after a
    // connection was accepted would make two runs of one script disagree. `install` is
    // once-per-process, so this is also the only moment it CAN happen.
    install_user_indicators(vars, booted.settings_dir.as_deref());

    // `"market_data/hist"` used to be the last resort here — a CWD-RELATIVE literal, so launching the
    // server from anywhere but the repo root silently created an empty store beside the shell and
    // answered every query with zero rows. Resolved through the shared precedence instead: an
    // explicit `VIKE_DATAHUB_STORE`, then `VIKE_HIST_STORE`, then the repo checkout if this machine
    // has one, then this PROJECT's own `<project>/market_data/hist`, then the per-user `…/vike-data`.
    // The project rung matters most for exactly this binary: it runs from its project root under
    // systemd, where there is no checkout, and before that rung existed the store resolved under
    // the service user's `$HOME` — outside the directory the operator installed and backs up.
    // ⚠ ...and the repo checkout is a DEBUG-BUILD rung only. `env!("CARGO_MANIFEST_DIR")` is a
    // string literal in the binary, and `release.yml` builds the `vike-datahub` asset (and the
    // `vike` multicall that links it) `--release` on a runner whose checkout path names the runner
    // account — a path `--remap-path-prefix` cannot touch, because that flag rewrites what the
    // COMPILER emits and not what a crate embeds itself. The release's box-path guard
    // (`scripts/refuse_box_paths.sh`) refused both assets by name. A release build passes no rung
    // and resolves from the project walk down, which is the rung this binary takes under systemd
    // anyway; `vike_model::store_path`'s module doc (rung 4) carries the argument, and the
    // attribute rather than `cfg!()` is what keeps the literal out of the bytes.
    #[cfg(debug_assertions)]
    let repo_default = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .map(|r| r.join("market_data").join("hist"));
    #[cfg(not(debug_assertions))]
    let repo_default: Option<std::path::PathBuf> = None;
    //
    // ⚠ `resolve_store_root_from`, never the bare `resolve_store_root` ladder: that one's project
    // and per-user defaults are two adjacent `Option<PathBuf>`s, so transposing them here would
    // COMPILE SILENTLY and put the tape somewhere else. This form has no two arguments of the same
    // type, and it resolves the project WALK (and `$VIKE_SETTINGS_DIR`, and the
    // `XDG_DATA_HOME`/`HOME`/`LOCALAPPDATA` trio) inside `vike_model` out of the environment map
    // this binary collects — the same shape as the `project_log_dir` call above.

    let resolved = vike_model::store_path::resolve_store_root_from(
        vars.get("VIKE_DATAHUB_STORE").cloned().map(std::path::PathBuf::from),
        vars.get("VIKE_HIST_STORE").cloned(),
        repo_default.as_deref(),
        cwd,
        vars,
    );
    // Say WHICH root, and by which rung, BEFORE anything opens it. A store does not merge: if this
    // answer ever moves, the old store is simply no longer read and every query returns zero rows —
    // which looks exactly like an empty date range. This line is what makes that visible, and it
    // comes before the open so a failure to open is preceded by the path that failed.
    tracing::info!(
        store = %resolved.root.display(),
        rung = resolved.rung.as_str(),
        "hist store root resolved: {}",
        resolved.rung.why()
    );
    let store_dir = resolved.root.display().to_string();
    let addr = vars
        .get("VIKE_DATAHUB_ADDR")
        .cloned()
        .unwrap_or_else(|| crate::server::DEFAULT_ADDR.to_string());

    // Classify the bind target BEFORE the store opens or the listener binds, and REFUSE a
    // non-loopback bind without the named opt-in — the tradehub treatment
    // (`crates/vike-tradehub/src/server.rs`'s `bind_decision`), mirrored into this crate's own
    // `server.rs`. It matters MORE here than there whenever this server is KEY-LESS — which is
    // still the default: that build authenticates NOTHING (the `Hello` informs, it does not gate),
    // so loopback plus the SSH tunnel is the ONLY barrier in front of history reads,
    // client-supplied Rhai compute and — in a `backfill-serve` build — WRITES into the store, and
    // a key-less server is therefore refused a non-loopback bind even WITH the opt-in (see the ⚠
    // below). On a KEYED server the guard still applies, for a different reason: the handshake is
    // plaintext, so the tunnel is what supplies confidentiality and integrity either way. And the
    // shipped unit cannot hold the posture by itself: its
    // `EnvironmentFile=` (the project `.env`) beats its own `Environment=` bind default, so before
    // this guard one `.env` line exposed the server with no unit edit and no warning.
    //
    // The opt-in is the EXACT string "1" (the `VIKE_RECONCILE` master-gate idiom, off the one
    // `vars` sweep this root owns), and it is a SEPARATE knob rather than an inference from the
    // address because the mistake this catches is typing an address — no address can be its own
    // consent. ⚠ A refusal EXITS, where the tradehub daemon keeps trading headless: serving is
    // this process's only job, so "do not bind" and "do not run" are the same decision. Exit 2 —
    // the refused-configuration family this binary already uses for a rejected argument — not
    // FAILURE: nothing was tried and failed; the configuration was refused before anything opened.
    //
    // ⚠ The KEYS are the guard's THIRD input, not a decoration on its message. `ServerAuth::of`
    // classifies the very value handed to `serve_authed` below, so the guard and the server cannot
    // disagree about whether this process authenticates — and a non-loopback bind on a KEY-LESS
    // server is now REFUSED rather than warned about, opt-in or no opt-in. That combination used
    // to be a `ProceedExposed(_) if keys.is_none()` arm right here that logged a `warn!` and FELL
    // THROUGH to bind and serve, which is the state
    // `docs/decisions/0025-datahub-remote-posture.md` names in its own reopening conditions: "an
    // opt-in warning in front of an unauthenticated write verb is the exact state this record
    // exists to prevent". The decision lives in `server.rs` rather than as an `if keys.is_none()`
    // guard here for two reasons: it is CHEAPER to test there, beside the rest of the policy — ⚠ not
    // because a match arm in this `main` is unreachable from a test, which the first draft of this
    // comment claimed and `crates/vike-datahub/tests/help_cli.rs` refutes by spawning this very
    // binary and asserting its status and streams (and which now covers this refusal end to end) —
    // and a NEW ENUM VARIANT makes every match on
    // `BindDecision` — this one and any future caller's — fail to compile until its author
    // confronts the case, where a dropped match guard would silently restore the old behaviour.
    let allow_public = vars.get("VIKE_DATAHUB_ALLOW_PUBLIC_BIND").map(String::as_str) == Some("1");
    let resolved_addrs: Vec<std::net::SocketAddr> =
        addr.to_socket_addrs().map(|it| it.collect()).unwrap_or_default();
    let server_auth = crate::server::ServerAuth::of(keys.as_ref());
    match crate::server::bind_decision(&resolved_addrs, allow_public, server_auth) {
        crate::server::BindDecision::Proceed => {}
        // Reachable only on a KEYED server now — `bind_decision` answers the key-less half with
        // `RefuseUnauthenticated` instead — so this text may say "node keys ARE configured"
        // without a match guard to check it.
        crate::server::BindDecision::ProceedExposed(exposed) => {
            tracing::warn!(
                %addr, %exposed,
                "vike-datahub binding a NON-LOOPBACK address (VIKE_DATAHUB_ALLOW_PUBLIC_BIND=1). \
                 Node keys ARE configured, so every connection must authenticate — but the \
                 handshake is PLAINTEXT and authenticates the CONNECTION, not each frame, so keep \
                 an SSH tunnel or a VPN in front of it for confidentiality and integrity"
            );
        }
        // ⚠ The combination this guard gained: reachable off-box, consented to, and authenticating
        // NOTHING. The message names both variables and both fixes — and names no VALUE: `keys` is
        // a redacting-`Debug` `NodeKeys` and nothing here reads a credential, so no secret can
        // reach a log line from this arm.
        crate::server::BindDecision::RefuseUnauthenticated(exposed) if store_unreadable => {
            // ⚠ THE SAME REFUSAL, THE OTHER CAUSE. Reaching the arm below instead would tell an
            // operator who HAS keys that they have none, and `Restart=on-failure` would repeat it
            // until somebody read the earlier `error!` line and connected the two. The disposition
            // is unchanged — still fail closed, still exit 2 — only the diagnosis is true.
            tracing::error!(
                %addr, %exposed,
                "VIKE_DATAHUB_ADDR is NOT loopback and this server could not READ its credential \
                 store, so it has no node keys to authenticate with — refusing to start. This is \
                 NOT a missing configuration: the store is present and its keys may well be \
                 correct. Fix the file's permissions (`vike-cli secrets path` prints it; the \
                 earlier `credential store PRESENT but UNREADABLE` line names the OS error) and \
                 restart. Until then this daemon will keep exiting 2 and being restarted"
            );
            return ExitCode::from(2);
        }
        crate::server::BindDecision::RefuseUnauthenticated(exposed) => {
            tracing::error!(
                %addr, %exposed,
                "VIKE_DATAHUB_ADDR is NOT loopback and this server has NO node keys — refusing to \
                 start. VIKE_DATAHUB_ALLOW_PUBLIC_BIND=1 consents to being REACHABLE; it is not \
                 consent to serve history, backtest compute over Rhai THE CLIENT SUPPLIES and (in \
                 a backfill-serve build) WRITES into the store, to anyone who can open a socket. \
                 Either set VIKE_DATAHUB_OBSERVE_KEY (reads) and VIKE_DATAHUB_CONTROL_KEY (the \
                 Backfill write and every Run* verb) in the credential store — `vike-cli secrets \
                 path` prints the file — or put the address back on 127.0.0.1 and reach it with \
                 `ssh -L 7878:localhost:7878 <host>`. Keep the tunnel or a VPN either way: the \
                 handshake is plaintext even when the keys ARE set"
            );
            return ExitCode::from(2);
        }
        crate::server::BindDecision::Refuse(exposed) => {
            tracing::error!(
                %addr, %exposed,
                keyed = keys.is_some(),
                "VIKE_DATAHUB_ADDR is NOT loopback — refusing to start. This server is meant to be \
                 reached over an SSH tunnel (its handshake is plaintext even when node keys ARE \
                 set): keep the address on 127.0.0.1 and run `ssh -L 7878:localhost:7878 <host>`. \
                 If this box genuinely must listen on a trusted network, set \
                 VIKE_DATAHUB_ALLOW_PUBLIC_BIND=1 — and set VIKE_DATAHUB_OBSERVE_KEY / \
                 VIKE_DATAHUB_CONTROL_KEY in the credential store first, so what is exposed is \
                 authenticated"
            );
            return ExitCode::from(2);
        }
    }

    let store = match DataFusionHist::open(&store_dir) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(store = %store_dir, error = %e, "vike-datahub: failed to open hist store");
            return ExitCode::FAILURE;
        }
    };
    let listener = match TcpListener::bind(&addr) {
        Ok(l) => l,
        Err(e) => {
            tracing::error!(addr = %addr, error = %e, "vike-datahub: failed to bind listener");
            return ExitCode::FAILURE;
        }
    };
    tracing::info!(addr = %addr, store = %store_dir, "vike-datahub: listening");

    let store = Arc::new(store);
    // Backfill-on-demand (split-plane REQ-9): a `backfill-serve` build mounts the REAL collector
    // table over the SAME store handle the server serves, which is what makes `serve` advertise
    // and answer `Request::Backfill`; any other build mounts none and the verb is a clean refusal.
    #[cfg(feature = "backfill-serve")]
    let backfill = Some(crate::backfill::real_backfill_table(Arc::clone(&store)));
    #[cfg(not(feature = "backfill-serve"))]
    let backfill: Option<crate::backfill::BackfillTable> = None;

    match crate::server::serve_authed(listener, store, backfill, keys) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!(error = %e, "vike-datahub: serve loop ended with an error");
            ExitCode::FAILURE
        }
    }
}

/// Inert stub for a default (feature-off) build: there is no DataFusion backend to serve, so print
/// a rebuild hint and exit non-zero. Keeps `cargo build -p vike-datahub` producing a runnable bin.
///
/// ⚠ The missing-feature message is a RUN failure, and only that. `--help` and `--version` are
/// answered first, exactly as under the feature: a build that cannot serve can still describe its
/// own command line, and answering `vike-datahub --help` with "rebuild with --features …" told a
/// caller the wrong thing about the wrong question.
#[cfg(not(feature = "serve-datafusion"))]
// ⚠ The two parameters are UNUSED in this arm and keep their names underscored rather than being
// dropped: the signature must match the `serve-datafusion` `run` exactly, or the dispatcher and
// the bin would fail to compile in whichever configuration they were not written against.
pub fn run(
    _vars: &std::collections::HashMap<String, String>,
    _cwd: Option<&std::path::Path>,
    args: &[String],
) -> std::process::ExitCode {
    if let Some(code) = short_circuit(args) {
        return code;
    }
    eprintln!(
        "vike-datahub was built without the `serve-datafusion` feature, so it has no data backend \
         to serve. Rebuild with: cargo run -p vike-datahub --features serve-datafusion"
    );
    ExitCode::from(2)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(v: &[&str]) -> std::vec::IntoIter<String> {
        v.iter().map(|s| s.to_string()).collect::<Vec<_>>().into_iter()
    }

    /// The three outcomes, as PARSE results. The exit status and the stream each produces are
    /// asserted over the shipped binary in `tests/help_cli.rs` — only a real run shows those.
    #[test]
    fn help_and_version_are_outcomes_not_errors_and_a_bare_invocation_runs() {
        for flag in ["-h", "--help"] {
            assert!(matches!(parse_args(argv(&[flag])), Ok(Parsed::Help)), "{flag}");
        }
        for flag in ["-V", "--version"] {
            assert!(matches!(parse_args(argv(&[flag])), Ok(Parsed::Version)), "{flag}");
        }
        assert!(matches!(parse_args(argv(&[])), Ok(Parsed::Run)), "no args is the server");
    }

    /// An unrecognised flag is REJECTED rather than ignored. This binary used to parse no argv at
    /// all, so a typo in a systemd `ExecStart=` line started the server as if nothing were wrong.
    #[test]
    fn an_unknown_argument_is_rejected_rather_than_ignored() {
        let err = parse_args(argv(&["--adr=1.2.3.4:9"])).expect_err("a typo must not be ignored");
        assert!(err.contains("--adr"), "the error names the offending argument: {err}");
        // `-v` is not a version request — lowercase is verbosity everywhere else on the box.
        assert!(parse_args(argv(&["-v"])).is_err(), "-v must stay unknown");
    }
}
