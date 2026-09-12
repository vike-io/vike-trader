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
//!   the catalog, and Control additionally admits the `Backfill` WRITE and the `DeleteSeries`
//!   REMOVAL. ⚠ It used to say "and every `Run*` verb" — those verbs are the compute daemon's now
//!   (ruling 7), and the scope table that governs them moved with them to
//!   `vike_datahub_client::proto`'s `required_scope`, which BOTH daemons consult.
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
//!
//! ⚠ `VIKE_USER_DATA_DIR` is NOT read here any more. It named the `indicators/` directory this
//! server installed process-wide so a CLIENT-SUPPLIED Rhai strategy could call the operator's own
//! indicators; ruling 7 moved every verb that compiles a strategy to `vike-backend backtest
//! --addr`, so this daemon compiles none and has nothing to install. The read went with the verbs
//! — `crates/vike-backtest/src/compute_server.rs`'s `install_user_indicators`.
//!
//! ⚠ **Since ruling 10 this binary also RECORDS**, and `--record PATH` is the whole of the new
//! surface. `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md` §0.5 merged
//! `vike-recorder`'s daemon into this one — one process owning venue connections, the store and
//! serving — so `crate::recorder` is the mount and this file is where its flags are parsed and its
//! thread layout is decided. Read that module's doc before changing the order of anything below:
//! the recording keeps the MAIN thread and `serve_authed` moves to a spawned one, deliberately.
//!
//! ⚠ **No new environment variable and no new settings key.** The profile arrives as a FLAG, the
//! way it did on the retired `vike-recorder --profile`, so a deployment moves one `ExecStart=` line
//! and nothing else — and `crates/vike-ops/tests/settings_registry.rs` and
//! `vike_config::CONSUMPTION` gain no row for a knob nobody asked for.
//!
//! ⚠ **`--help`/`--version` are answered in BOTH builds, identically, before anything else.** This
//! binary parsed NO argv at all: under the feature it went straight to opening a store and BINDING
//! A LISTENER, so `vike-datahub --help` STARTED A SERVER; without it, every invocation printed the
//! missing-feature message and exited 2, so `--help` looked like a first-class feature error when it
//! was simply not implemented. Help is a question about the COMMAND LINE, which both builds have;
//! the missing backend is a question about running, which is why only the RUN path still fails.

use std::process::ExitCode;

const USAGE: &str = "\
usage: vike-datahub [--record PATH] [--tick-secs N] [--silent-secs N] [--exit-on-silence] [--once]

The headless data daemon: it serves the hist store over the node protocol and, with --record, ALSO
owns the venue subscriptions that fill it. The listen address and the store root come from the
environment; the recording profile is the one flag. (The COMPUTE verbs are NOT here — they are
served by `vike-backend backtest --addr`, and this daemon refuses them by name.)

  --record PATH     record venue market data into this server's OWN store, from the recorder
                    profile at PATH (store root + subscriptions + maintenance + alerting).
                    Omitted, this daemon serves and records nothing, exactly as it always did.
                    ⚠ The profile's `store` key must name the SAME directory this server
                    resolved (see VIKE_DATAHUB_STORE below); a disagreement is refused at
                    startup rather than silently resolved in either direction.
  --tick-secs N     how often to re-resolve the desired symbol set (default 30). Needs --record.
  --silent-secs N   warn when a subscribed series has received no rows for N seconds
                    (default 300; 0 disables). Catches a feed that is subscribed and
                    connected but receiving nothing — which raises no error anywhere.
                    Needs --record.
  --exit-on-silence stop with status 3 when a series is silent, so Restart=on-failure
                    can act. OFF by default: a venue can be legitimately quiet, and a
                    quiet market must not kill a daemon recording five healthy ones — and
                    since the merge it would take the data wire down with them. Alerting
                    (the [alerting] profile table) is the always-on reaction. Needs --record.
  --once            run ONE recording tick and exit — the dry run. Binds NO listener, so it is
                    safe to run beside the daemon it is checking. Exits 0 only if EVERY feed
                    ended that tick with at least one live subscription and no refused
                    subscribe; otherwise status 4, naming each feed that would record
                    nothing. Stricter than the daemon on purpose: this is a commissioning
                    check of your own profile, with you watching. Needs --record.
  -h, --help        print this and exit 0
  -V, --version     print the version and exit 0

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
                       consent to serve a store write and a store REMOVAL unauthenticated. With
                       a key it proceeds behind a logged warning naming what is exposed.

credential store (<project>/settings/secrets.env — `vike-cli secrets path` prints it):
  VIKE_DATAHUB_OBSERVE_KEY   read scope: history + catalog. Absent = no authentication at all.
  VIKE_DATAHUB_CONTROL_KEY   write scope: the above, plus the Backfill store write, the
                             DeleteSeries store REMOVAL. (The Run* verbs, which COMPILE
                             CLIENT-SUPPLIED RHAI, moved to `vike-backend backtest --addr`.)
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
                       (VIKE_USER_DATA_DIR is no longer read: this daemon compiles no strategy.
                       The Run* verbs, and the indicator install they needed, moved to
                       `vike-backend backtest --addr`.)
  VIKE_DATAHUB_LIVE    the exact string \"1\" ARMS the LIVE MARKET-DATA plane: this daemon becomes
                       the single subscriber to each venue's book/depth/tape and pushes them to
                       clients over the MdSubscribe stream. UNSET = off, and off is
                       byte-identical to a build without the plane — no venue socket, no
                       `market_data` capability in Welcome.features, MdSubscribe refused by name.
                       ⚠ It spends VENUE-API budget from THIS BOX'S IP, shared with the
                       order-signing daemon, which is why it is an explicit operator act and why
                       the per-venue key cap and the 60 s linger exist.
                       Needs a build carrying `--features live-feeds` plus the `live-<venue>`
                       features for the venues you want; a build without them arms a hub that
                       refuses every venue BY NAME rather than serving nothing quietly.
  VIKE_DATAHUB_LIVE_RESIDENT
                       comma-separated `venue:symbol:lane` rows PINNED at startup (lane is one of
                       depth|book|trades), e.g. `binance:BTCUSDT.P:depth,binance:BTCUSDT.P:trades`.
                       A resident key has a refcount FLOOR of 1 and is never released, so a hot
                       symbol's ladder paints on the first DOM open instead of waiting on a REST
                       re-seed, and the daemon's traded pair stays observable with no desktop
                       attached. Unset = an EMPTY resident set, which is a real configuration:
                       the hub still serves on-demand keys.

This binary serves only when built with the `serve-datafusion` feature, which is what makes the
concrete DataFusion backend nameable:
  cargo run -p vike-datahub --features serve-datafusion";

/// The recording half of the command line, as a plain value.
///
/// ⚠ **Parsed in BOTH builds, feature-free, and that is the point.** `--record` and its four
/// companions are a question about the COMMAND LINE, which every build has; whether this binary can
/// ACT on them is a question about the backend, which only a `record` build answers — the same
/// split `short_circuit` already drew for `--help`, and for the same reason (a build that cannot
/// serve still described its own command line correctly, and a build that cannot record must
/// describe its own too, then say so at RUN time rather than reject a flag it understands).
///
/// Named fields rather than `vike_datahub::recorder::RecordArgs` because that type lives behind the
/// feature and this parser must not.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RecordRequest {
    profile: std::path::PathBuf,
    tick_secs: u64,
    silent_secs: u64,
    exit_on_silence: bool,
    once: bool,
}

/// How often the desired symbol set is re-resolved when no `--tick-secs` is given.
///
/// ⚠ Spelled here as well as in `crate::recorder` because this parser is feature-free and that
/// module is not — the two are held equal by [`the_parser_defaults_match_the_recorders`], which is
/// compiled only in a `record` build, i.e. exactly where both spellings exist.
const DEFAULT_TICK_SECS: u64 = 30;

/// The silence-watchdog grace when no `--silent-secs` is given. Same duplication, same gate.
const DEFAULT_SILENT_SECS: u64 = 300;

/// What a successful parse produced. Named variants rather than a bool pair, the shape
/// `vike-tradehub`'s `Parsed` uses — nothing here can be confused for a run.
///
/// `Debug` so a parse that was supposed to FAIL can report what it actually produced instead
/// (`Result::expect_err` requires it) — the whole value of the unknown-argument test below.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Parsed {
    /// Serve, and record if a profile was named.
    Run(Option<RecordRequest>),
    Help,
    Version,
}

/// The parser's job is the three things every binary owes a caller — answer `-h`/`--help`, answer
/// `-V`/`--version`, and REJECT anything else instead of ignoring it (a typo in a systemd
/// `ExecStart=` used to start the server anyway) — plus, since ruling 10 merged the recorder in,
/// the five flags that name a recording.
///
/// ⚠ **It used to be loop-free**, because there was nothing to accumulate: "the FIRST argument
/// already decides all three outcomes, and anything after it is unreachable by construction". That
/// stopped being true the moment a flag took a VALUE, so the loop is back — and with it the rule
/// the recorder's own parser carried: a flag that needs an argument and does not get one is an
/// error, never a silently-defaulted knob.
///
/// ⚠ A companion flag without `--record` is REFUSED rather than ignored. `--once` alone would
/// otherwise start a server, and `--silent-secs 0` alone would leave an operator believing they had
/// turned a watchdog off on a daemon that was never watching anything.
fn parse_args(argv: impl Iterator<Item = String>) -> Result<Parsed, String> {
    let mut profile: Option<std::path::PathBuf> = None;
    let mut tick_secs = DEFAULT_TICK_SECS;
    let mut silent_secs = DEFAULT_SILENT_SECS;
    let mut exit_on_silence = false;
    let mut once = false;
    let mut companion: Option<&'static str> = None;
    let mut it = argv;
    while let Some(a) = it.next() {
        match a.as_str() {
            "-h" | "--help" => return Ok(Parsed::Help),
            "-V" | "--version" => return Ok(Parsed::Version),
            "--record" => {
                profile = Some(std::path::PathBuf::from(
                    it.next().ok_or("--record needs a path".to_string())?,
                ));
            }
            "--tick-secs" => {
                let v = it.next().ok_or("--tick-secs needs a number".to_string())?;
                let n: u64 = v.parse().map_err(|_| format!("--tick-secs: not a number: {v}"))?;
                if n == 0 {
                    // A zero tick would spin the resolver flat out against the venue's directory API.
                    return Err("--tick-secs must be > 0".into());
                }
                tick_secs = n;
                companion = Some("--tick-secs");
            }
            "--silent-secs" => {
                let v = it.next().ok_or("--silent-secs needs a number".to_string())?;
                silent_secs = v.parse().map_err(|_| format!("--silent-secs: not a number: {v}"))?;
                companion = Some("--silent-secs");
            }
            "--exit-on-silence" => {
                exit_on_silence = true;
                companion = Some("--exit-on-silence");
            }
            "--once" => {
                once = true;
                companion = Some("--once");
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    let Some(profile) = profile else {
        return match companion {
            Some(flag) => Err(format!(
                "{flag} configures a recording, so it needs --record PATH. Without it this daemon \
                 only serves the store"
            )),
            None => Ok(Parsed::Run(None)),
        };
    };
    if exit_on_silence && silent_secs == 0 {
        // `--silent-secs 0` disables detection outright, so the exit could never fire — an
        // operator who wrote both believes something is armed that is not.
        return Err("--exit-on-silence needs silence detection on (--silent-secs > 0)".into());
    }
    Ok(Parsed::Run(Some(RecordRequest { profile, tick_secs, silent_secs, exit_on_silence, once })))
}

/// Answer `--help`, `--version` and a usage error IDENTICALLY in both builds — `Err(code)` means
/// the process is done. Shared by the two `main`s below so the feature can never change what a
/// caller's `--help` does; `Ok` carries the recording the caller asked for, if any.
fn short_circuit(args: &[String]) -> Result<Option<RecordRequest>, ExitCode> {
    match parse_args(args.iter().cloned()) {
        // Help is normal output a user pipes into a pager, so STDOUT and exit 0 — a non-zero
        // `--help` breaks `set -e`, packaging smoke tests and any wrapper that checks a status.
        Ok(Parsed::Help) => {
            println!("{USAGE}");
            Err(ExitCode::SUCCESS)
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
            Err(ExitCode::SUCCESS)
        }
        Ok(Parsed::Run(record)) => Ok(record),
        Err(e) => {
            eprintln!("vike-datahub: {e}\n\n{USAGE}");
            Err(ExitCode::from(2))
        }
    }
}

// ⚠ `install_user_indicators` is GONE, and its absence is a POSTURE change worth naming rather than
// a deletion. This binary used to load `<project>/user_data/indicators/*.rhai` and install them
// process-wide, because it COMPILED CLIENT-SUPPLIED RHAI: `RunBacktest` resolved a profile's
// `[strategy.params].src` through `harness::registry`, and `RunSlice` went through
// `wire_run::to_strategy_spec`. Ruling 7 moved every one of those verbs to
// `vike-backend backtest --addr`, so this daemon compiles no strategy at all — and a server that
// compiles nothing has no reason to hold a script compiler, a `vike-script` edge or an indicator
// set whose contents a client cannot see. The whole apparatus (and the ⚠⚠ "the server's set wins
// and the client cannot see it" hazard it carried) moved with the verbs to
// `crates/vike-backtest/src/compute_server.rs`, which is where it now belongs.

#[cfg(feature = "serve-datafusion")]
pub fn run(
    vars: &std::collections::HashMap<String, String>,
    cwd: Option<&std::path::Path>,
    args: &[String],
) -> std::process::ExitCode {
    use std::net::{TcpListener, ToSocketAddrs};
    use std::sync::Arc;

    // BEFORE the log subscriber, the store and the listener: `--help` must not open a store, and it
    // certainly must not bind a port. (It did both — this binary read no argv at all.)
    let record = match short_circuit(args) {
        Ok(r) => r,
        Err(code) => return code,
    };
    // ⚠ A `--record` on a build without the `record` feature is a RUN failure naming the feature,
    // never a silent serve-only start. This is `vike_recorder::venues::build_feed`'s rule one level
    // up: "a startup ERROR naming the missing feature, never a silent no-record", and it matters
    // more here — the daemon would otherwise come up healthy, answer every query, and accumulate
    // nothing, which is indistinguishable from a venue that is merely quiet.
    #[cfg(not(feature = "record"))]
    if record.is_some() {
        eprintln!(
            "vike-datahub: --record was given but this binary was built without the `record` \
             feature, so it carries no venue feeds and would serve a store nothing fills. Rebuild \
             with: cargo build -p vike-datahub --features record-polymarket,record-binance (or \
             `record` alone for the mount with no venue compiled in)"
        );
        return ExitCode::from(2);
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

    // ...AND WHICH MODE IT IS IN, in the same breath. See `startup_mode_line` for the silent
    // no-op this exists to make impossible to ship again.
    tracing::info!("{}", startup_mode_line(record.as_ref()));

    // ⚠ **THE STOP HANDLERS, AND ONLY WHEN A RECORDING WAS ASKED FOR.** `crate::recorder::arm`'s
    // own doc carries why this is as early as it can be (the subscriber has to exist first, because
    // the outcome is LOGGED) and why it must be before the store opens, the port binds and the
    // first feed thread starts: until it runs, SIGTERM has the OS default disposition and the
    // process dies where it stands with no flush.
    //
    // ⚠ And why it is CONDITIONAL. A serve-only data server has no teardown to run, so installing a
    // handler that raises a flag nothing reads would take SIGTERM's default kill away from it — a
    // daemon that stops answering `systemctl stop` is a worse regression than anything this merge
    // fixes. Without `--record` this line does nothing and the process keeps the behaviour it has
    // always had.
    #[cfg(feature = "record")]
    let stop = record.as_ref().map(|_| crate::recorder::arm());

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

    // ⚠⚠ **EVERY RECORDING REFUSAL HAPPENS HERE — BEFORE THE BIND GUARD, THE STORE OPEN AND THE
    // LISTENER.** `crate::recorder::load_and_check_profile` carries the four questions and the
    // argument; the ORDER is this file's to keep, and getting it wrong has a name. These checks
    // used to live inside `crate::recorder::record`, which this file reaches only after the port is
    // bound and the serve thread is spawned — so a profile with a typo'd `store` key did not
    // REFUSE TO START, it bound `VIKE_DATAHUB_ADDR`, brought the wire up, refused, exited, and let
    // `Restart=on-failure` do it again every five seconds. A crash-loop is strictly worse than a
    // refusal: it takes the data wire up and down, it buries the one line that says why under a
    // repeating boot sequence, and `systemctl status` shows "activating" rather than "failed".
    //
    // Everything below this point can still fail (a port in use, a store that will not open) — but
    // those are failures of an ACTION, and this is a refusal of a CONFIGURATION. Exit 2 is the
    // family this binary already uses for the second kind, and it is deliberately distinct from the
    // `1` a recording that RAN and then broke returns through `finish_recording`.
    #[cfg(feature = "record")]
    let profile = match record.as_ref() {
        Some(req) => {
            match crate::recorder::load_and_check_profile(&req.profile, &resolved.root, cwd) {
                Ok(p) => Some(p),
                Err(e) => {
                    tracing::error!(
                        error = %e,
                        "vike-datahub: refusing to start — the recording was refused before \
                         anything was opened or bound, so nothing on this box changed"
                    );
                    eprintln!("vike-datahub: {e}");
                    return ExitCode::from(2);
                }
            }
        }
        None => None,
    };

    let addr = vars
        .get("VIKE_DATAHUB_ADDR")
        .cloned()
        .unwrap_or_else(|| crate::server::DEFAULT_ADDR.to_string());

    // ⚠ **THE DRY RUN BINDS NOTHING**, so it is answered here — before the bind guard, before the
    // listener, and before anything can fail on a port. `--once` is a commissioning check an
    // operator runs BY HAND, usually while the daemon it is checking is already running and already
    // holding `VIKE_DATAHUB_ADDR`; a dry run that died on `EADDRINUSE` would be answering a
    // question nobody asked. It still opens the store, because "the store opens" is one of the four
    // things `docs/ops/recorder-deploy.md` says `--once` proves.
    //
    // ⚠ Its EXIT STATUS is the verdict, not its log: `0` only if every feed ended that tick with a
    // live subscription, `4` otherwise. That distinction is why `crate::recorder::EXIT_DRY_RUN`
    // exists at all — see its doc for the the CI box run that exited 0 having resolved nothing.
    // (`profile.clone()` rather than a move: this arm is CONDITIONAL and the daemon arm below needs
    // the same value. It is a handful of paths and subscriptions, cloned once at startup, on the
    // path that exits after one tick.)
    #[cfg(feature = "record")]
    if let (Some(req), Some(profile), Some(stop)) =
        (record.as_ref().filter(|r| r.once), profile.clone(), stop.as_ref())
    {
        let store = match open_hist_store(&store_dir) {
            Ok(s) => Arc::new(s),
            Err(code) => return code,
        };
        return finish_recording(crate::recorder::record(
            &to_record_args(req),
            profile,
            store,
            stop,
            booted.settings_dir_override.as_deref(),
            cwd,
            vars,
        ));
    }

    // Classify the bind target BEFORE the store opens or the listener binds, and REFUSE a
    // non-loopback bind without the named opt-in — the tradehub treatment
    // (`crates/vike-tradehub/src/server.rs`'s `bind_decision`), mirrored into this crate's own
    // `server.rs`. It matters MORE here than there whenever this server is KEY-LESS — which is
    // still the default: that build authenticates NOTHING (the `Hello` informs, it does not gate),
    // so loopback plus the SSH tunnel is the ONLY barrier in front of history reads,
    // and — in a `backfill-serve` build — WRITES into the store plus the irreversible DeleteSeries, and
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
    let server_auth = vike_datahub_client::bind::ServerAuth::of(keys.as_ref());
    match vike_datahub_client::bind::bind_decision(&resolved_addrs, allow_public, server_auth) {
        vike_datahub_client::bind::BindDecision::Proceed => {}
        // Reachable only on a KEYED server now — `bind_decision` answers the key-less half with
        // `RefuseUnauthenticated` instead — so this text may say "node keys ARE configured"
        // without a match guard to check it.
        vike_datahub_client::bind::BindDecision::ProceedExposed(exposed) => {
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
        vike_datahub_client::bind::BindDecision::RefuseUnauthenticated(exposed)
            if store_unreadable =>
        {
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
        vike_datahub_client::bind::BindDecision::RefuseUnauthenticated(exposed) => {
            tracing::error!(
                %addr, %exposed,
                "VIKE_DATAHUB_ADDR is NOT loopback and this server has NO node keys — refusing to \
                 start. VIKE_DATAHUB_ALLOW_PUBLIC_BIND=1 consents to being REACHABLE; it is not \
                 consent to serve history, and (in a backfill-serve build) WRITES into the store \
                 plus the IRREVERSIBLE DeleteSeries, to anyone who can open a socket. \
                 Either set VIKE_DATAHUB_OBSERVE_KEY (reads) and VIKE_DATAHUB_CONTROL_KEY (the \
                 Backfill write and the DeleteSeries removal) in the credential store — \
                 `vike-cli secrets path` prints the file — or put the address back on 127.0.0.1 \
                 and reach it with `ssh -L 7878:localhost:7878 <host>`. Keep the tunnel or a VPN \
                 either way: the handshake is plaintext even when the keys ARE set"
            );
            return ExitCode::from(2);
        }
        vike_datahub_client::bind::BindDecision::Refuse(exposed) => {
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

    let store = match open_hist_store(&store_dir) {
        Ok(s) => s,
        Err(code) => return code,
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
    // ⚠ **THE LIVE MARKET-DATA PLANE, ARMED BY AN OPERATOR AND NOT BY A BUILD.**
    //
    // `VIKE_DATAHUB_LIVE` is read as the EXACT string "1" — this workspace's idiom for a switch
    // whose safe state is off — out of the ONE `std::env::vars()` sweep the bin already owns, so
    // both rows stay `Layer::Injected`/`Naming::MapLookup` in `vike_ops::settings::SETTINGS` and
    // `LIBRARY_PIN` does not grow (`library_rows_do_not_grow` refuses an addition).
    //
    // ⚠ The `.get(` calls below are DELIBERATELY literal rather than routed through a local
    // closure: `crates/vike-ops/tests/settings_registry.rs`'s `MAP_LOOKUP_PROVEN` is a SHAPE-keyed
    // measurement of which `MapLookup` rows the scanner can positively prove at a real `.get(`
    // site, pinned in BOTH directions. A `let get = |k| vars.get(k)` helper would drop both keys
    // into the measured blind spot and redden that pin from the removal direction.
    //
    // ⚠ Unset is BYTE-IDENTICAL to a build without the plane: no hub is mounted, `served_features`
    // advertises neither `market_data` nor any `md_venue=` entry, and `MdSubscribe` is answered
    // with `crate::server::NO_MARKET_DATA_PLANE` on a connection that stays positional. That is the
    // credential-is-the-gate idiom pointed at a CAPABILITY: the build carries it, the operator arms
    // it, and the two cannot answer differently because the advertisement keys on the MOUNT.
    let md_hub = if vars.get("VIKE_DATAHUB_LIVE").map(String::as_str) == Some("1") {
        // ⚠ NO `#[cfg]` HERE, and that is the whole point of the feature-free hub. The venue TABLE
        // is feature-free code with cfg'd ARMS, so a build with no venue feature mounts a hub that
        // refuses every venue BY NAME — naming the feature to rebuild with — rather than serving
        // nothing quietly. That is a real configuration, exactly as `record` alone is, and it is
        // `vike_recorder::venues::build_feed`'s rule one level up.
        let hub = crate::md::MdHub::new(
            crate::md::venues::real_market_venue_table(),
            crate::md::venues::supported().into_iter().map(str::to_string).collect(),
        );
        // Tier R — the RESIDENT set. Parsed from the same sweep; an unparseable row is a WARNING
        // that names the row rather than a startup refusal, because a typo in one resident key must
        // not take the whole data wire down (`docs/decisions/0013-degrade-vs-refuse.md`).
        let resident_raw = vars.get("VIKE_DATAHUB_LIVE_RESIDENT").cloned().unwrap_or_default();
        let (resident, bad) = crate::md::parse_resident_set(&resident_raw);
        for row in &bad {
            tracing::warn!(
                row = %row,
                "vike-datahub md: ignoring an unparseable VIKE_DATAHUB_LIVE_RESIDENT row — the \
                 format is venue:symbol:lane with lane one of depth|book|trades"
            );
        }
        // ⚠ **A ROW THAT PARSES IS NOT A ROW THAT CAN BE SERVED**, and `parse_resident_set` checks
        // only the three-field shape and the lane word — `notavenue:X:depth` and a real venue on a
        // lane its declared caps do not serve both get through it. `add_resident` runs `acquire`'s
        // capability checks and the two key caps, and each refusal is named HERE for the same
        // reason the unparseable rows are: a resident key the daemon silently could not subscribe
        // is an endless 5-second reconcile-failure loop with no line saying which row caused it.
        let mut pinned = 0usize;
        for spec in &resident {
            match hub.add_resident(spec) {
                Ok(()) => pinned += 1,
                Err(why) => tracing::warn!(
                    venue = %spec.venue,
                    symbol = %spec.symbol,
                    lane = ?spec.lane,
                    %why,
                    "vike-datahub md: REFUSING a VIKE_DATAHUB_LIVE_RESIDENT row — it is not pinned \
                     and nothing will retry it. Every other row is unaffected"
                ),
            }
        }
        hub.spawn();
        tracing::info!(
            venues = ?hub.served_venues(),
            resident = pinned,
            "vike-datahub: LIVE MARKET-DATA plane armed (VIKE_DATAHUB_LIVE=1) — this process is now \
             the single subscriber to each venue it serves, and spends that venue's API budget from \
             this box's IP"
        );
        Some(hub)
    } else {
        None
    };

    // Backfill-on-demand (split-plane REQ-9): a `backfill-serve` build mounts the REAL collector
    // table over the SAME store handle the server serves, which is what makes `serve` advertise
    // and answer `Request::Backfill`; any other build mounts none and the verb is a clean refusal.
    #[cfg(feature = "backfill-serve")]
    let backfill = Some(crate::backfill::real_backfill_table(Arc::clone(&store)));
    #[cfg(not(feature = "backfill-serve"))]
    let backfill: Option<crate::backfill::BackfillTable> = None;

    // ⚠ **THE MERGED PROCESS: RECORD ON THE MAIN THREAD, SERVE ON A SPAWNED ONE** (ruling 10). The
    // order matters and it is not arbitrary — see `crate::recorder`'s module doc. The serve loop has
    // no teardown at ALL (`listener.incoming()` for the process lifetime; SIGKILL has always been
    // its stop), while the recorder's teardown is where the buffered tape is either flushed or
    // lost. So the flush keeps the main thread, and abandoning the serve thread when `main` returns
    // is byte-identical to what a `systemctl stop` did to this daemon before the merge.
    //
    // The feeds could not take the spawned thread anyway: `RecorderRuntime` owns
    // `Box<dyn VenueFeed>`, which is not `Send` (`crate::recorder::FEED_STOP_BUDGET_SECS`' doc
    // carries what that costs the teardown), so it is built and dropped where it is used.
    #[cfg(feature = "record")]
    if let (Some(req), Some(profile), Some(stop)) = (record.as_ref(), profile, stop.as_ref()) {
        let serve_store = Arc::clone(&store) as Arc<dyn vike_data::HistStore + Send + Sync>;
        let serve_stop = stop.flag();
        // ⚠ **A `record` + `live-feeds` DAEMON OPENS TWO SUBSCRIPTIONS TO A SHARED VENUE, and the
        // spec does not notice because it was written before ruling 10.** The recorder builds its
        // own venue clients through `vike_recorder::venues::build_feed`; the hub builds a second
        // set through `crate::md::venues::build_market_client`. If binance BTCUSDT.P is in both the
        // recorder profile and the hub's resident set, that is two depth sockets and two folded
        // books inside the daemon whose whole justification (ruling 10, §0.5) is that ONE process
        // should own venue connections — and it spends budget the `MD_LINGER` arithmetic reserved
        // for the ORDER-SIGNING daemon.
        //
        // The real fix is one client per venue with `vike_data::TeeSink` feeding both planes, which
        // needs the recorder's and the hub's subscription drivers merged: real work, and its own
        // PR. What this line buys meanwhile is that the duplication is VISIBLE rather than silent.
        if let Some(hub) = md_hub.as_ref() {
            tracing::warn!(
                md_venues = ?hub.served_venues(),
                "vike-datahub: RECORDING **and** serving live market data in one process — a venue \
                 named by BOTH the recorder profile and the market-data plane is subscribed TWICE \
                 from this box's IP. Ruling 10 exists to remove exactly that duplication; merging \
                 the two subscription drivers is a named follow-up"
            );
        }
        let serve_md = md_hub.clone();
        std::thread::spawn(move || {
            match crate::server::serve_authed(listener, serve_store, backfill, keys, serve_md) {
                // ⚠ NEITHER ARM IS REACHABLE IN PRACTICE, and the reaction is the same for both:
                // raise the shared stop flag. `serve_authed` loops over `listener.incoming()`,
                // which does not end for a TCP listener, and logs-and-continues on a failed accept
                // — so arriving here at all means the socket is gone. Recording on with a dead wire
                // would leave a daemon that looks healthy and answers nothing, and simply exiting
                // the thread would do exactly that. Raising the flag runs the RECORDER'S teardown
                // first (the rows are flushed) and then lets `Restart=on-failure` rebuild both
                // halves, which is the only ordering that loses nothing.
                Ok(()) => tracing::error!(
                    "vike-datahub: the serve loop ENDED — stopping the recorder gracefully so the \
                     buffered tape is flushed before this process exits and is restarted"
                ),
                Err(e) => tracing::error!(
                    error = %e,
                    "vike-datahub: serve loop ended with an error — stopping the recorder \
                     gracefully so the buffered tape is flushed before this process exits and is \
                     restarted"
                ),
            }
            vike_ops::stop::request_stop(&serve_stop);
        });
        return finish_recording(crate::recorder::record(
            &to_record_args(req),
            profile,
            store,
            stop,
            booted.settings_dir_override.as_deref(),
            cwd,
            vars,
        ));
    }

    match crate::server::serve_authed(listener, store, backfill, keys, md_hub) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!(error = %e, "vike-datahub: serve loop ended with an error");
            ExitCode::FAILURE
        }
    }
}

/// Open the ONE hist store this process serves — and, under `--record`, writes.
///
/// Extracted because the dry run opens it too, from a different point in the sequence (before the
/// bind guard, since it binds nothing), and two `DataFusionHist::open` call sites with two error
/// messages is how a store failure starts reading differently depending on which flag you passed.
#[cfg(feature = "serve-datafusion")]
fn open_hist_store(store_dir: &str) -> Result<vike_data::DataFusionHist, ExitCode> {
    vike_data::DataFusionHist::open(store_dir).map_err(|e| {
        tracing::error!(store = %store_dir, error = %e, "vike-datahub: failed to open hist store");
        ExitCode::FAILURE
    })
}

/// The recording's own exit statuses, kept in ONE place so the dry run and the daemon cannot map
/// the same outcome differently.
///
/// `EXIT_SILENT` (3) and `EXIT_DRY_RUN` (4) are deliberately distinct from `1` (a startup failure)
/// and `2` (a refused configuration): a supervisor should be able to tell "the daemon worked and
/// the DATA did not" apart from "the profile was bad". Their own docs carry the the CI box runs that
/// made each one necessary.
///
/// ⚠ **A bad PROFILE no longer reaches this function at all** — a read failure, a parse failure, a
/// store-root disagreement and a venue this build cannot record are all answered by
/// `crate::recorder::load_and_check_profile` before the listener binds, and exit **2**. What is
/// left for the `Err` arm here is a recording that STARTED and then broke: the writer thread would
/// not spawn, a feed refused to mount. That split is what makes the status readable — `2` means
/// nothing was opened or bound and the fix is in a file; `1` means the daemon ran.
#[cfg(feature = "record")]
fn finish_recording(
    outcome: Result<crate::recorder::RecordOutcome, String>,
) -> std::process::ExitCode {
    match outcome {
        Ok(crate::recorder::RecordOutcome::Stopped) => ExitCode::SUCCESS,
        Ok(crate::recorder::RecordOutcome::Silent(series)) => {
            tracing::error!(
                series = ?series,
                "vike-datahub: exiting on silence (--exit-on-silence) — these subscribed series \
                 are receiving no rows"
            );
            eprintln!("vike-datahub: exiting on silence: {}", series.join(", "));
            ExitCode::from(crate::recorder::EXIT_SILENT)
        }
        Ok(crate::recorder::RecordOutcome::DryRunFailed(reasons)) => {
            for why in &reasons {
                tracing::error!(reason = %why, "vike-datahub: --once dry run FAILED");
            }
            eprintln!(
                "vike-datahub: --once proved NOTHING — this profile would record no data:\n  {}",
                reasons.join("\n  ")
            );
            ExitCode::from(crate::recorder::EXIT_DRY_RUN)
        }
        Err(e) => {
            tracing::error!(error = %e, "vike-datahub: recording failed");
            eprintln!("vike-datahub: {e}");
            ExitCode::FAILURE
        }
    }
}

/// **WHICH MODE IS THIS DAEMON IN** — recording, or serving only — as one line, said at startup
/// before anything opens a store or binds a port.
///
/// ⚠ **It exists because forgetting `--record` is a SILENT no-op, and one shipped.** The runbook's
/// install path put a bare `datahub` on the unit's `ExecStart=`, so an install its own page called
/// "recording" came up healthy, answered every query, passed `systemctl is-active`, and
/// accumulated nothing — indistinguishable from a venue that is merely quiet, and invisible until
/// somebody queried a date range and got zero rows. Every other symptom this daemon has is loud;
/// this one had no symptom at all, so the cure is a line that makes the mode a FACT in the journal:
/// `journalctl -u vike-datahub | grep 'vike-datahub: '` answers it without reading a unit file.
///
/// It is a pure function of the parse so it can be tested in BOTH builds ([`the_mode_line_says_which_mode`]);
/// the `run` bodies below only print it. It names the profile PATH deliberately — on a box with
/// two datahubs "recording" is not enough to tell you WHICH tape.
// Only the serving `run` prints it — the feature-off stub has no subscriber and no run to describe,
// it just says what it cannot do and exits. The tests below exercise it in every build, which is
// what makes this an `allow` rather than a `#[cfg]` on the function itself.
#[cfg_attr(not(feature = "serve-datafusion"), allow(dead_code))]
fn startup_mode_line(record: Option<&RecordRequest>) -> String {
    match record {
        Some(req) => format!(
            "vike-datahub: RECORDING AND SERVING — venue feeds fill the very store this server \
             answers from (--record {})",
            req.profile.display()
        ),
        None => {
            "vike-datahub: SERVE-ONLY — no --record, so this daemon subscribes to NO venue and \
             writes NO rows; it answers from a store some other process fills. Add `--record \
             <profile>` to the unit's ExecStart= to make it record"
                .to_string()
        }
    }
}

/// The feature-free [`RecordRequest`] this file parses, as the `record` module's own argument type.
///
/// Two types rather than one because the PARSE must compile in every build and `RecordArgs` lives
/// behind the feature — see [`RecordRequest`]'s doc. The conversion is the seam, and
/// [`the_parser_defaults_match_the_recorders`] is what keeps the two spellings of each default from
/// drifting.
#[cfg(feature = "record")]
fn to_record_args(req: &RecordRequest) -> crate::recorder::RecordArgs {
    crate::recorder::RecordArgs {
        profile: req.profile.clone(),
        tick: std::time::Duration::from_secs(req.tick_secs),
        once: req.once,
        silent_secs: req.silent_secs,
        exit_on_silence: req.exit_on_silence,
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
    // The parse runs in BOTH builds and produces the same answers — that is `short_circuit`'s whole
    // job. A `--record` this build cannot honour is therefore a RUN failure below, never a rejected
    // flag: the flag is real and understood, and only the backend is missing.
    if let Err(code) = short_circuit(args) {
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
        assert!(
            matches!(parse_args(argv(&[])), Ok(Parsed::Run(None))),
            "no args is the server, recording nothing — the pre-merge behaviour, byte for byte"
        );
    }

    /// The five flags ruling 10 added, and the defaults they leave behind.
    #[test]
    fn the_record_flags_parse() {
        let Ok(Parsed::Run(Some(req))) = parse_args(argv(&["--record", "r.toml"])) else {
            panic!("`--record PATH` is a run that records");
        };
        assert_eq!(req.profile, std::path::PathBuf::from("r.toml"));
        assert_eq!(req.tick_secs, DEFAULT_TICK_SECS);
        assert_eq!(req.silent_secs, DEFAULT_SILENT_SECS);
        assert!(!req.once);
        assert!(
            !req.exit_on_silence,
            "the default reaction to a silent series is ALERT, never exit — and since the merge an \
             exit takes the data wire down with the feeds"
        );

        let Ok(Parsed::Run(Some(req))) = parse_args(argv(&[
            "--record",
            "r.toml",
            "--tick-secs",
            "5",
            "--silent-secs",
            "60",
            "--exit-on-silence",
            "--once",
        ])) else {
            panic!("a full recording command line parses");
        };
        assert_eq!(req.tick_secs, 5);
        assert_eq!(req.silent_secs, 60);
        assert!(req.exit_on_silence);
        assert!(req.once);
    }

    /// ⚠ A companion flag WITHOUT `--record` is refused rather than ignored, and the message names
    /// the flag that needs the profile. Ignoring them would start a plain server for `--once` (a
    /// dry run that never ends) and leave `--silent-secs 0` looking like a watchdog somebody turned
    /// off — the "believes something is armed that is not" class the recorder's own parser already
    /// refused within `--record`.
    #[test]
    fn a_recording_flag_without_a_profile_is_refused() {
        for flag in ["--once", "--exit-on-silence"] {
            let err = parse_args(argv(&[flag])).expect_err("{flag} alone must be a usage error");
            assert!(err.contains(flag), "the message names the offending flag: {err}");
            assert!(err.contains("--record"), "…and what it needs: {err}");
        }
        let err = parse_args(argv(&["--tick-secs", "5"])).expect_err("a value flag alone too");
        assert!(err.contains("--record"), "{err}");
    }

    /// The flag/value rules the recorder's parser carried, kept: a missing value is an error rather
    /// than a silently-defaulted knob, a zero tick would spin the resolver flat out against the
    /// venue's directory API, and `--exit-on-silence` with detection off is an exit that could
    /// never fire.
    #[test]
    fn the_record_flags_reject_what_they_always_rejected() {
        assert!(parse_args(argv(&["--record"])).is_err(), "--record needs a path");
        assert!(parse_args(argv(&["--record", "r.toml", "--tick-secs"])).is_err());
        assert!(parse_args(argv(&["--record", "r.toml", "--tick-secs", "0"])).is_err());
        assert!(parse_args(argv(&["--record", "r.toml", "--tick-secs", "x"])).is_err());
        let err =
            parse_args(argv(&["--record", "r.toml", "--silent-secs", "0", "--exit-on-silence"]))
                .expect_err("--exit-on-silence with detection off must be a usage error");
        assert!(err.contains("--silent-secs"), "{err}");
    }

    /// ⚠ The two spellings of each default are one fact, and this is what holds them equal.
    ///
    /// The parser above is FEATURE-FREE (a build that cannot record still describes its own command
    /// line), so it cannot name `crate::recorder`'s constants; this test is compiled only in a
    /// `record` build, which is exactly where both spellings exist. A drift would make
    /// `--help` promise one cadence while the mount ran another.
    #[cfg(feature = "record")]
    #[test]
    fn the_parser_defaults_match_the_recorders() {
        assert_eq!(DEFAULT_TICK_SECS, crate::recorder::DEFAULT_TICK_SECS);
        assert_eq!(DEFAULT_SILENT_SECS, crate::recorder::DEFAULT_SILENT_SECS);
    }

    /// ⚠ The mode is SAID, in both directions, and the serve-only line names what it is NOT doing.
    ///
    /// The failure this guards is asymmetric and that is why the wording is: a unit that forgot
    /// `--record` produces a daemon with no symptom whatsoever — active, healthy, answering — while
    /// recording nothing, so "SERVE-ONLY" has to be as loud in the journal as "RECORDING" is. The
    /// profile PATH is in the recording line because a box may run two of these.
    #[test]
    fn the_mode_line_says_which_mode() {
        let serve_only = startup_mode_line(None);
        assert!(serve_only.contains("SERVE-ONLY"), "{serve_only}");
        assert!(
            serve_only.contains("--record"),
            "the serve-only line must name the flag that would change it: {serve_only}"
        );

        let Ok(Parsed::Run(Some(req))) = parse_args(argv(&["--record", "r.toml"])) else {
            panic!("`--record PATH` is a run that records");
        };
        let recording = startup_mode_line(Some(&req));
        assert!(recording.contains("RECORDING"), "{recording}");
        assert!(
            recording.contains("r.toml"),
            "the recording line must name WHICH profile: {recording}"
        );
        assert!(
            !recording.contains("SERVE-ONLY"),
            "the two modes must not be confusable in a journal grep: {recording}"
        );
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
