//! `vike-datahub` — the data service command line, as a LIBRARY function.
//!
//! This was the binary's body until the multicall merge. `main` became [`run`], taking the
//! environment, the working directory and argv as parameters. Both cfg arms travelled — the
//! datafusion-serve server and the feature-off stub — so a build that cannot serve still answers
//! `--help` identically, which is the property `short_circuit` exists to hold.
//!
//! The working directory is a parameter for the same reason the environment is, and the settings
//! registry cannot see it: a `current_dir()` inside a library is ambient state its caller cannot
//! override. This body read it TWICE before the move and now receives one answer.
//!
//! Everything below is the binary's own documentation, unchanged.
//! The real server (a `DataFusionHist`-backed [`serve`](crate::serve) loop) is compiled
//! ONLY under the `datafusion-serve` feature, which is what makes the concrete `DataFusionHist`
//! backend nameable here. A default build compiles the inert stub `main` below instead, so
//! `cargo build -p vike-datahub` always produces a runnable (if non-serving) binary that compiles
//! cleanly. (NB this crate has no edge to vike-backtest at all — ruling 7 removed it — so nothing
//! about that crate's feature shape reaches here; `datafusion-serve` is what pulls the
//! DataFusion/Arrow tree on its own account, and a default build is DataFusion-free.)
//!
//! Settings (rows of the settings database; no variable reaches any of them, decision 0111):
//! - `config.datahub_bind_addr` — listen address; default `127.0.0.1:7878`. ⚠ A NON-LOOPBACK
//!   address is REFUSED at startup unless the `flags.datahub_allow_public_bind` row is on
//!   **AND at least one hub key is configured** — the tradehub `bind_decision` treatment,
//!   mirrored and then composed with this server's own key presence (see `crate::server`'s
//!   module doc, and the guard's own comment at the match below). The opt-in alone used to be
//!   enough, behind a `warn!`; that is the state `docs/decisions/0025-datahub-remote-posture.md`
//!   names verbatim as the one it exists to prevent. The guard did NOT soften when authentication
//!   landed: the handshake is plaintext and authenticates the CONNECTION, not each frame, so the
//!   tunnel is still what supplies confidentiality and integrity.
//! - `config.node_idle_read_timeout_ms` — how long a connection may go without a request frame
//!   before this server closes it; default `vike_hub_proto::limits::SERVER_IDLE_READ_TIMEOUT`.
//!   The backtesthub reads the same row for its own connections. Refused outside its range at the
//!   write and at the load; `crate::server::ServerTimings` carries it into the connection loop.
//! - `config.md_heartbeat_ms` — the market-data stream's heartbeat period; default
//!   `vike_datahub_client::market::MD_HEARTBEAT`. Sent to the client in `MdSubscribed.heartbeat_ms`,
//!   so the client's read deadline follows it. Refused outside its range at the write and the load.
//!
//! Node-key and credential stores (the settings database, `<project>/settings/db/vike.db` — the
//! only store) — `docs/decisions/0025-datahub-remote-posture.md`:
//! - `VIKE_DATAHUB_OBSERVE_KEY` / `VIKE_DATAHUB_CONTROL_KEY` — the scoped HMAC hub keys. ⚠ **Their
//!   ABSENCE is the switch**: with neither set (the default) this server authenticates NOTHING and
//!   serves every verb that predates the keys exactly as every build before them did. With either
//!   set, EVERY connection must complete the `Hello`/`Auth` handshake, Observe reads history and
//!   the catalog, and Control additionally admits the `Backfill` and `ImportArchive` WRITEs and the
//!   `DeleteSeries` REMOVAL. ⚠ It used to say "and every `Run*` verb" — those verbs are the compute
//!   daemon's now (ruling 7), and the scope table that governs them moved with them to
//!   `vike_datahub_client::proto`'s `required_scope`, which BOTH daemons consult.
//!   those COMPILE CLIENT-SUPPLIED RHAI server-side, so they are not reads whatever they return.
//!
//!   ⚠ **The absence is no longer ONLY an authentication switch, and this bullet said it was until
//!   2026-09-07** ("behaves exactly as every build before them did"). It also decides one VERB: the
//!   destructive `DeleteSeries` is served by a KEYED server alone.
//!   `crates/vike-datahub/src/server/delete.rs`'s `delete_series_verb` refuses it outright without keys
//!   and the `Welcome` does not advertise it, because `Scope::Write` is a word nothing enforces
//!   where nothing authenticates. `Backfill` is Control-scoped too and is NOT withheld for
//!   key-lessness — only `delete_series_verb` gates on `keyed` — so in a `backfill-serve` build
//!   with a table mounted it is served here. That asymmetry is the argument rather than an
//!   oversight, since a backfill writes rows a re-fetch restores and a removal takes the only copy.
//!   `docs/decisions/0050-a-key-less-datahub-serves-no-delete-verb.md` is the record.
//! - NO venue credential (decision 0164). The OANDA practice account's API token — the `API_KEY`
//!   field of the active unlabelled oanda account at tier `demo`, the ONE venue credential this
//!   daemon read until the gatewayhub took every venue call — is read by the GATEWAYHUB, once per
//!   connection, and never here
//!   (docs/decisions/0097-the-datahub-reads-one-practice-token-for-a-credentialed-history-lane.md;
//!   `crates/vike-gatewayhub/src/rest/oanda.rs`'s `oanda_history_credentials` is the read and
//!   `crate::backfill::credentialed_klines_row` is this side: one session-scoped remote source per
//!   `Backfill` request).
//!   ⚠ On a KEY-LESS server the Control scope that fences it is a word nothing enforces — 0097
//!   declares that rather than gating it, for 0050's reason: the lane is what a local end user
//!   fetches with.
//! - `config.store_root` — hist-store root dir. Unset, it falls through the SHARED precedence that
//!   `resolve_store_root_from` documents at its call site below, which is the authority: the
//!   CWD-relative `market_data/hist` this line used to name as the last resort is gone, and it was the
//!   whole bug (launching from anywhere but the repo root created an empty store beside the shell).
//! - `config.recorder_retention_days_by_kind` — per-kind retention for the maintenance pass
//!   (`book:14,depth:14,trade:90`). Absent or empty is the profile's scalar `retention_days` for
//!   every kind. ⚠ IRREVERSIBLE deletion; the execution kinds are refused by name. Parsed by
//!   `recording::load_retention_by_kind` BEFORE the bind guard (a refusal exits 2) and handed to
//!   `crate::recorder::record` as a parameter; a process that runs no maintenance logs that it
//!   deletes nothing.
//! - `config.datahub_news_poll_secs`, `config.datahub_calendar_poll_secs`,
//!   `config.datahub_equity_poll_secs` — the tool-feeds lane's cadences, folded into
//!   `crate::tool_feeds::Cadences` by `lanes::load_feed_cadences` before the bind guard, and mounted
//!   by `lanes::start_tool_feeds` (60 to 86400 seconds; the lane's own defaults when absent).
//!
//! ⚠ `VIKE_USER_DATA_DIR` is NOT read here any more. It named the `indicators/` directory this
//! server installed process-wide so a CLIENT-SUPPLIED Rhai strategy could call the operator's own
//! indicators; ruling 7 moved every verb that compiles a strategy to `vike-backend backtesthub
//! --addr`, so this daemon compiles none and has nothing to install. The read went with the verbs
//! — `crates/vike-backtesthub/src/backtesthub_server.rs`'s `install_user_indicators`.
//!
//! ⚠ **Since ruling 10 this binary also RECORDS**, and `--recorder-profile [NAME]` is the whole of
//! the new surface. `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md` §0.5
//! merged `vike-recorder`'s daemon into this one — one process owning venue connections, the store
//! and serving — so `crate::recorder` is the mount and this file is where its flags are parsed and
//! its thread layout is decided. Read that module's doc before changing the order of anything below:
//! the recording keeps the MAIN thread and `serve_authed` moves to a spawned one, deliberately.
//!
//! ⚠ **`--record PATH` — the FILE spelling this surface shipped with — is RETIRED (decision 0086:
//! "settings live only in the database", verdict 1: no binary reads a profile TOML).** It is
//! accepted for one release, its VALUE ignored, and a startup warning fires — the exact
//! `RETIRED_PROFILE_FLAG` migration-window shape this file already used once for the flag's own
//! rename. See [`ProfileSource`]'s doc for the deployment hazard that shape exists to avoid and the
//! residual an operator must clear (a recorder profile ROW, via `vike-cli config
//! bootstrap-recorder`) before this release reaches a box that has only ever used `--record`.
//!
//! ⚠ **No new environment variable and no new settings key.** The profile arrives as a FLAG, the
//! way it did on the retired `vike-recorder --profile`, so a deployment moves one `ExecStart=` line
//! and nothing else — and `crates/vike-ops/tests/settings_secrets/settings_registry.rs` and
//! `vike_config::CONSUMPTION` gain no row for a knob nobody asked for.
//!
//! ⚠ **`--help`/`--version` are answered in BOTH builds, identically, before anything else.** This
//! binary parsed NO argv at all: under the feature it went straight to opening a store and BINDING
//! A LISTENER, so `vike-datahub --help` STARTED A SERVER; without it, every invocation printed the
//! missing-feature message and exited 2, so `--help` looked like a first-class feature error when it
//! was simply not implemented. Help is a question about the COMMAND LINE, which both builds have;
//! the missing backend is a question about running, which is why only the RUN path still fails.

use std::process::ExitCode;

mod args;
#[cfg(feature = "datafusion-serve")]
mod boot;
// Built by `run` alone, and `run` is `datafusion-serve`-only: ungated, a default build would carry a
// `gateway_link` nothing calls, which `-D warnings` refuses as dead code. The module's own tests build
// under `cfg(test)` in every configuration.
#[cfg(any(feature = "datafusion-serve", test))]
mod gateway;
mod lanes;
mod recording;

#[cfg(doc)]
use self::args::ProfileSource;
use self::args::short_circuit;
#[cfg(feature = "datafusion-serve")]
use self::boot::{boot_root, check_bind, open_hist_store, resolve_hist_store};
#[cfg(feature = "backfill-serve")]
use self::lanes::start_backfill;
#[cfg(feature = "datafusion-serve")]
use self::lanes::{
    arm_start_history, load_feed_cadences, start_catalog_lane, start_import_lane,
    start_journal_materializer, start_market_data_plane, start_options_lane, start_seed_lane,
    start_strategy_lane, start_tool_feeds,
};
#[cfg(feature = "record")]
use self::recording::{finish_recording, load_record_args, load_recording_profile};
#[cfg(feature = "datafusion-serve")]
use self::recording::{load_retention_by_kind, startup_mode_line};

// ⚠ `install_user_indicators` is GONE, and its absence is a POSTURE change worth naming rather than
// a deletion. This binary used to load `<project>/user_data/indicators/*.rhai` and install them
// process-wide, because it COMPILED CLIENT-SUPPLIED RHAI: `RunBacktest` resolved a profile's
// `[strategy.params].src` through `harness::registry`, and `RunSlice` went through
// `wire_run::to_strategy_spec`. Ruling 7 moved every one of those verbs to
// `vike-backend backtesthub --addr`, so this daemon compiles no strategy at all — and a server that
// compiles nothing has no reason to hold a script compiler, a `vike-script` edge or an indicator
// set whose contents a client cannot see. The whole apparatus (and the ⚠⚠ "the server's set wins
// and the client cannot see it" hazard it carried) moved with the verbs to
// `crates/vike-backtesthub/src/backtesthub_server.rs`, which is where it now belongs.

/// The data daemon past `--help` — **a SEQUENCE of named phases**, each a function below, called in
/// the order its statements ran when this was one body.
///
/// ⚠ **The ORDER is the contract**, and the comment at each call says what depends on it. Two
/// things read this function's own text: `crates/vike-ops/tests/hygiene/compile_time_path_gate.rs` keys
/// the one compile-time path on `resolve_hist_store`, and `crate::recorder::arm` has to install the
/// stop handlers before the store opens, the port binds or a feed thread starts. (A third, the
/// gate that wanted Polymarket's egress declared above every call that can build a client, left
/// with the declaration: this process links no Polymarket crate since decision 0164, and the
/// gateway daemon's own `run` declares the egress now.)
#[cfg(feature = "datafusion-serve")]
pub fn run(
    vars: &std::collections::HashMap<String, String>,
    cwd: Option<&std::path::Path>,
    args: &[String],
) -> std::process::ExitCode {
    use std::net::TcpListener;
    use std::sync::Arc;

    // BEFORE the log subscriber, the store and the listener: `--help` must not open a store, and it
    // certainly must not bind a port. (It did both — this binary read no argv at all.)
    let launch = short_circuit(args);
    let (store_flag, record) = (launch.store, launch.record);
    // ⚠ A `--record` on a build without the `record` feature is a RUN failure naming the feature,
    // never a silent serve-only start. This is `crate::recording::build_recording_feed`'s rule one
    // level up: "a startup ERROR naming the missing feature, never a silent no-record", and it
    // matters more here — the daemon would otherwise come up healthy, answer every query, and accumulate
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
    // `vike-desktop`'s `PROCESS_ENV` and `vike-cli`'s `resolve_policy` use. Two sweeps could
    // disagree if anything mutated the environment between them, and a composition root is the one
    // place that question should have a single answer.

    // The workspace's ONE startup sequence — `boot_root` is where every way this root departs from
    // it is named.
    let booted = match boot_root(vars, cwd) {
        Ok(b) => b,
        // Unreachable with the arms of `boot_root` (nothing there can fail), but a refusal must
        // never become an unwrap in a server's `main`.
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
    //
    // `file_level` is the `preferences.log_file_level` ROW (decision 0111 verdict 3), resolved by
    // the boot above — which is why the boot runs first; no environment variable outranks it. A
    // store that will not open leaves the compiled default and says so in
    // `booted.settings.warnings`, logged just below.
    let _log_guards = vike_log::init(vike_log::LogConfig {
        file_prefix: "vike-datahub".to_string(),
        file_level: booted.settings.preferences.log_file_level.clone(),
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

    // The loader's OWN non-fatal resolutions, emitted here because `vike_boot::boot` runs before a
    // subscriber exists and therefore returns them as DATA. This root had none to emit while it
    // took `SettingsLoad::Skip` (the arm returns `Settings::default()`, whose `warnings` are
    // empty); the join above is what gives it some. Dropping them would make a loader notice
    // unreachable on the only box it is about.
    for w in &booted.settings.warnings {
        tracing::warn!("{w}");
    }

    // The one value every remote lane dials the gatewayhub from (decision 0164; `gateway`'s module
    // doc). It opens nothing and is built here, before any lane, so each mount below takes it as a
    // parameter: the broker's venue clients, the recorder's families, the backfill table, the venue
    // catalog and the options poll all reach a venue through it and through nothing else.
    //
    // ⚠ This root reads NO `venue_setting` row and declares NO Polymarket egress any more: it links no
    // venue bridge, so there is no process-wide cell here to fill, and the rows (Polymarket's egress
    // and socket batching, each CEX feed's mark-stream row) are read where the venue client is built,
    // by the gatewayhub's own `run`.
    let gateway = gateway::gateway_link(&booted.settings, &booted.store);

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

    // The hub keys, off the NODE store and this boot's own settings directory — see `read_hub_keys`
    // for why that store and not the venue credential store, and what `store_unreadable` is for.
    let (keys, store_unreadable) = read_hub_keys(&booted);

    // The hist-store root, by the shared precedence and logged before anything opens it.
    let resolved = resolve_hist_store(store_flag, &booted.settings.config, vars, cwd);
    let store_dir = resolved.root.display().to_string();

    // ⚠⚠ **EVERY RECORDING REFUSAL HAPPENS HERE — BEFORE THE BIND GUARD, THE STORE OPEN AND THE
    // LISTENER** — `load_recording_profile` carries the argument. The ORDER is this function's to
    // keep: a refused profile must exit 2 before a port is bound, never crash-loop after one.
    #[cfg(feature = "record")]
    let profile = match load_recording_profile(record.as_ref(), &booted, &resolved, cwd) {
        Ok(p) => p,
        Err(code) => return code,
    };

    // ...and the recording's own arguments, for the same reason and at the same position: the
    // flags folded with this box's `config.recorder_*` rows (`to_record_args`), and the one
    // contradiction between a row and a flag that only a settings load can find refused here,
    // before a port is bound.
    #[cfg(feature = "record")]
    let record_args =
        match load_record_args(record.as_ref(), &booted.settings.config, &booted.settings.flags) {
            Ok(a) => a,
            Err(code) => return code,
        };

    // The per-kind RETENTION row (`config.recorder_retention_days_by_kind`), parsed here for the
    // same reason: a row the reader refuses exits 2 before a port is bound. It is handed to
    // `crate::recorder::record` as a parameter — the recorder library reads no setting — and only
    // a process that RUNS maintenance acts on it (see the warning below `compaction_runs`).
    let retention_by_kind = match load_retention_by_kind(&booted) {
        Ok(map) => map,
        Err(code) => return code,
    };

    // The tool-feed CADENCE rows, parsed here too (`load_feed_cadences`), and mounted below.
    let feed_cadences = match load_feed_cadences(&booted) {
        Ok(cadences) => cadences,
        Err(code) => return code,
    };

    // The listen address: the `config.datahub_bind_addr` row, the compiled-in default otherwise
    // (decision 0111: `VIKE_DATAHUB_ADDR` refuses startup naming the row). NOT
    // `config.datahub_addr`: that is a CLIENT's dial.
    let bind_row = booted.settings.config.datahub_bind_addr.clone();
    let addr = bind_row.unwrap_or_else(|| crate::server::DEFAULT_ADDR.to_string());

    // THE ONE OWNER OF VENUE CLIENTS in this process (`crate::feeds`). Built before either plane is
    // used, so both can take their client from it and a key both want is subscribed once. Building
    // it opens nothing: a venue client is constructed on its first subscribe. Its two holders are
    // the md plane (`crate::md::venues::market_builder`) and the recorder
    // (`crate::recording::build_recording_feed`, reached from both `crate::recorder::record` calls
    // below).
    let broker = crate::feeds::FeedBroker::new(
        crate::feeds::venues::real_client_table(&gateway),
        crate::feeds::venues::sharing_for,
    );

    // ⚠ **THE DRY RUN BINDS NOTHING**, so it is answered here — before the bind guard, before the
    // listener, and before anything can fail on a port. `--once` is a commissioning check an
    // operator runs BY HAND, usually while the daemon it is checking is already running and already
    // holding `config.datahub_bind_addr`; a dry run that died on `EADDRINUSE` would be answering a
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
    if let (Some(args), Some(profile), Some(stop)) =
        (record_args.as_ref().filter(|a| a.once), profile.clone(), stop.as_ref())
    {
        let store = match open_hist_store(&store_dir) {
            Ok(s) => Arc::new(s),
            Err(code) => return code,
        };
        return finish_recording(crate::recorder::record(
            args,
            profile,
            store,
            stop,
            &booted.store,
            cwd,
            &broker,
            &gateway,
            &retention_by_kind,
        ));
    }

    // The bind guard — `check_bind` carries the whole argument. It runs BEFORE the store opens or
    // the listener binds, and a refusal exits 2: nothing was tried and failed, the configuration
    // was refused before anything opened.
    if let Err(code) = check_bind(&booted.settings.flags, &addr, &keys, store_unreadable) {
        return code;
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

    // The JOURNAL MATERIALIZER, armed by an operator and not by a build
    // (`start_journal_materializer`), over the SAME store handle. Held for the process lifetime
    // under a NAMED binding (a bare `_` would drop it at once): dropping it is the final drain and
    // the join, so that runs when `run` returns — after the recorder's teardown on the recording
    // path, at exit on the serve path.
    let journal_materializer = start_journal_materializer(
        &booted.settings,
        Arc::clone(&store) as Arc<dyn vike_data::HistStore + Send + Sync>,
    );

    // The live market-data plane, armed by an operator and not by a build — see
    // `start_market_data_plane`. Its hub and the recorder both take their venue clients from the
    // ONE `broker` above.
    let md_hub = start_market_data_plane(&booted.settings, &broker);

    // The STRATEGY LANE, armed by the KEY and by nothing else - see `start_strategy_lane`. It takes its
    // venue clients from the same ONE broker, as the third holder.
    let strat_lane = start_strategy_lane(keys.as_ref(), &broker, &gateway);

    // Backfill-on-demand: a `backfill-serve` build mounts the REAL collector table over the SAME
    // store handle the server serves (`start_backfill`); any other build mounts none and the verb
    // is a clean refusal.
    #[cfg(feature = "backfill-serve")]
    let backfill = start_backfill(&store, &gateway);
    #[cfg(not(feature = "backfill-serve"))]
    let backfill: Option<crate::backfill::BackfillTable> = None;

    // The chart-gap seed lane, which needs a collector table to arm anything (`start_seed_lane`).
    let seed_lane = start_seed_lane(&booted.settings.flags, backfill.as_ref());

    // The venue-catalog lane, on by default with its refusal as the one switch
    // (`start_catalog_lane`).
    let catalog_lane = start_catalog_lane(&booted, &gateway);

    // The archive import lane, mounted by the project and not by a switch (`start_import_lane`).
    let import_lane = start_import_lane(&booted, &store);
    // Whether THIS process runs the journal materializer, which writes live executions into
    // `kind=exec_fill` / `kind=exec_order`: the boot's own answer, handed to the Welcome as the
    // `exec_materialize` advertisement (`crate::server::serve_with_import`'s parameter of the same
    // name): `true` exactly when `start_journal_materializer` above spawned one.
    let journal_materializer_mounted = journal_materializer.is_some();

    // Whether THIS process compacts its store: only the recording path below starts a maintenance
    // scheduler (`crate::recorder::record`, off the profile's `[maintenance]`), and a profile whose
    // `interval_secs` is 0 starts none. The chain recorder is refused without it
    // (`lanes::chain_recorder_arming`).
    #[cfg(feature = "record")]
    let compaction_runs = profile
        .as_ref()
        .is_some_and(|p| p.maintenance.scheduler_args(&retention_by_kind).is_some());
    #[cfg(not(feature = "record"))]
    let compaction_runs = false;

    // ⚠ A per-kind retention row on a process that runs NO maintenance deletes nothing, and the
    // operator who wrote it must be told rather than left believing the tape is being trimmed.
    // The row is the box's, shared by every daemon on it, so a serve-only data daemon, a build
    // without the `record` feature and a recording profile with `interval_secs = 0` all land here.
    if !retention_by_kind.is_empty() && !compaction_runs {
        tracing::warn!(
            retention_days_by_kind = ?retention_by_kind,
            "vike-datahub: config.recorder_retention_days_by_kind is set, but this process runs no \
             maintenance (it needs a recording profile with [maintenance].interval_secs above 0, \
             in a `record` build), so NOTHING is deleted by it here"
        );
    }

    // The strategy lane's START HISTORY: saves the closed bars of the keys it holds on a saved venue and
    // compares the venue's seed with the store - armed only here, once `compaction_runs` is known, because
    // a closed bar is one part until a maintenance pass merges them (`arm_start_history`).
    // The holder is kept to settle its save queue at an orderly stop (below); a build without the recorder
    // has no orderly stop, and nothing is armed there.
    let start_history = arm_start_history(
        strat_lane.as_ref(),
        &store,
        &gateway,
        &booted.settings.flags,
        compaction_runs,
    );
    #[cfg(not(feature = "record"))]
    drop(start_history);

    // The options lane, mounted by the build and carrying the `kind=chain` recorder when the
    // operator armed it and the store is compacted (`start_options_lane`).
    let options_lane = start_options_lane(&booted, &store, compaction_runs, &gateway);

    // The tool-feeds lane, mounted by the build with no runtime switch; it polls nothing until the
    // first request and reads its two API keys from this box's credential store (`start_tool_feeds`).
    let tool_feeds_lane = start_tool_feeds(&booted, feed_cadences);

    // The two connection timings an operator may set: the idle read timeout of every connection and
    // the market-data stream's heartbeat period. Read HERE, once, from the boot's rows and handed to
    // the server as a parameter (`ServerTimings::from_rows` takes plain values and reads nothing); a
    // box with neither row runs `ServerTimings::PRODUCTION`'s constants, byte for byte. The rows were
    // range-checked by the loader, so a daemon that got this far holds values inside the range.
    let timings = crate::server::ServerTimings::from_rows(
        booted.settings.config.node_idle_read_timeout_ms,
        booted.settings.config.md_heartbeat_ms,
    );

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
    if let (Some(args), Some(profile), Some(stop)) = (record_args.as_ref(), profile, stop.as_ref())
    {
        let serve_store = Arc::clone(&store) as Arc<dyn vike_data::HistStore + Send + Sync>;
        let serve_stop = stop.flag();
        // Both planes hold clients from ONE broker (docs/decisions/0092), so a shared key is
        // subscribed once.
        let serve_md = md_hub.clone();
        let serve_seed = seed_lane.clone();
        let serve_catalog = catalog_lane.clone();
        let serve_import = import_lane.clone();
        let serve_options = options_lane.clone();
        let serve_tool_feeds = tool_feeds_lane.clone();
        let serve_strat = strat_lane.clone();
        std::thread::Builder::new().name("dh-serve".into()).spawn(move || {
            match crate::server::serve_with_lanes(
                listener,
                serve_store,
                backfill,
                keys,
                serve_md,
                serve_seed,
                crate::server::ServerLanes {
                    catalog: serve_catalog,
                    options: serve_options,
                    tool_feeds: serve_tool_feeds,
                    strat: serve_strat,
                },
                serve_import,
                journal_materializer_mounted,
                timings,
            ) {
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
            vike_boot::stop::request_stop(&serve_stop);
        }).expect("spawn serve thread");
        let code = finish_recording(crate::recorder::record(
            args,
            profile,
            store,
            stop,
            &booted.store,
            cwd,
            &broker,
            &gateway,
            &retention_by_kind,
        ));
        // The closes still queued for the start history's saver are written before the process goes (a
        // second at most; an empty queue returns at once): they are the last minute of every saved key.
        // This second is OUTSIDE `crates/vike-datahub/src/recorder.rs`'s `TOTAL_STOP_BUDGET_SECS` (the
        // bounded teardown has already returned): the shipped unit's `TimeoutStopSec=` of 45 s leaves
        // 13 s over that total, so 12 s remain after it. Raise it only with that margin in view.
        if let Some(history) = &start_history {
            history.settle(std::time::Duration::from_secs(1));
        }
        return code;
    }

    match crate::server::serve_with_lanes(
        listener,
        store,
        backfill,
        keys,
        md_hub,
        seed_lane,
        crate::server::ServerLanes {
            catalog: catalog_lane,
            options: options_lane,
            tool_feeds: tool_feeds_lane,
            strat: strat_lane,
        },
        import_lane,
        journal_materializer_mounted,
        timings,
    ) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!(error = %e, "vike-datahub: serve loop ended with an error");
            ExitCode::FAILURE
        }
    }
}

/// **Phase: the hub keys** — the datahub pair, read from the NODE store and never from the venue
/// credential store, plus whether that store was PRESENT BUT UNREADABLE (the bind guard words its
/// refusal differently for that cause). `run` calls this once, after the stop handlers are armed.
/// The argument `hub_keys_from_vars` is handed is what
/// `crates/vike-ops/tests/settings_secrets/hub_key_store_gate.rs` reads, so it stays this file's `credentials`.
///
/// Formerly `read_node_keys`, renamed in the shape-and-names change (decision 0166).
#[cfg(feature = "datafusion-serve")]
fn read_hub_keys(booted: &vike_boot::Booted) -> (Option<vike_hub_proto::auth::HubKeys>, bool) {
    // The datahub NODE KEYS (`docs/decisions/0025-datahub-remote-posture.md`), from the credential
    // store — read HERE, through `booted.store`, which is opened over what THIS process's ONE
    // project walk answered. Using the boot's answer rather than a walk of this binary's own is
    // what keeps the log home, the store root and the credentials all pointing at one project;
    // the recorder's `webhook_targets` reads through the same handle for exactly that reason, after
    // the same defect bit it.
    //
    // ⚠ `vike_secrets::CredentialStore`, the store's own read, and never through `vike-bridge-core`:
    // that is a WEIGHT decision (the canonical credential wrapper sat there, beside the
    // ureq/tungstenite/rustls transport stack, until docs/decisions/0135), and that stack is absent
    // from a DEFAULT vike-datahub graph and must not join it to read a `.env`.
    // vike-secrets names nothing above the vocabulary floor but the change journal one rung over it
    // (a declared tier-15 exception row) and is already in this crate's closure. (⚠ That read "is
    // a zero-vike-dependency leaf" until
    // `docs/decisions/0072-vike-secrets-takes-one-vike-edge-and-is-not-split.md`, accepted
    // 2026-09-20, admitted `vike-model`. The weight argument is unchanged — the RANK is what it
    // rested on — and that one edge is in this closure already.)
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
    // they have "NO hub keys" — and `Restart=on-failure` repeats that wrong diagnosis forever.
    // The loader's behaviour is untouched (that is `0013`'s degrade case, and other consumers rely
    // on it); only this binary's ability to SAY WHICH case it is changes.
    let mut store_unreadable = false;
    // ⚠ THE NODE STORE, NOT THE CREDENTIAL STORE — and for this binary the distinction is the whole
    // point. This server needs a hub key pair and NOTHING else: it does not trade, holds no venue
    // account and signs no order. Until 2026-09-08 it read the whole credential map, which opens the
    // store holding every venue key on the box, 168 names of them, to find two.
    // `CredentialStore::hub_keys` reads the settings database's `hub_key` table, the only hub-key store,
    // and hands back this service's pair and nothing else.
    //
    // A deployment can give this service a settings database whose `hub_key` table holds its two
    // keys and whose `credential` table is empty, and it starts correctly authenticated — which is
    // what "compute-to-data" should have meant from the start.
    //
    // ⚠ The scope is THIS SERVICE'S FAMILY (`is_datahub_daemon_key`: the pair and the strategy-lane
    // key), not all the platform names: `hub_keys` hands back the names the predicate admits and no
    // others, so this daemon never holds the tradehub's pair. The family is WIDER than the pair every
    // other reader of the datahub keys holds (`is_datahub_pair_key`): this root verifies
    // `Scope::Trade` of the strategy lane with the strategy-lane key, and only this root and the definition name the daemon
    // predicate (the hub-key store gate's `FENCED_PREDICATES`).
    let credentials: std::collections::HashMap<String, String> =
        match booted.store.hub_keys(vike_venues::credential_keys::is_datahub_daemon_key) {
            Ok(resolved) => {
                // The store's own findings. `vike-secrets`' reads return them as DATA (they log
                // nothing) and `vike_secrets::Resolved::report` is the one place they become log
                // lines, so this root owes that call rather than being the one binary where a 0644
                // credential store is read in silence. The UNREADABLE arm below keeps this root's
                // own louder line instead.
                resolved.report();
                resolved.secrets.into_map()
            }
            Err(e) => {
                tracing::error!(
                    error = %e,
                    "vike-datahub: credential store PRESENT but UNREADABLE — any configured \
                     datahub hub keys were NOT loaded. On a LOOPBACK bind this server is about \
                     to serve UNAUTHENTICATED behind the bind guard alone; on a NON-LOOPBACK one \
                     it will now REFUSE TO START a few lines below, because keyless plus \
                     off-box is the combination the guard exists for. Either way the cause is \
                     this store: fix its permissions and restart"
                );
                store_unreadable = true;
                Default::default()
            }
        };
    let keys = vike_hub_proto::auth::hub_keys_from_vars(&credentials)
        .map(|pair| with_strategy_lane_key(pair, &credentials));
    if keys.is_none() && strategy_lane_key(&credentials).is_some() {
        tracing::warn!(
            "vike-datahub: the strategy-lane key is stored but this server holds NO hub-key pair, so it \
             is keyless and serves no lane (a lane is a keyed surface). Run `vike-cli datahub setup` \
             to mint the pair, then restart"
        );
    }
    (keys, store_unreadable)
}

/// The strategy-lane key's value out of the hub-key map, `None` when absent or blank. Read by the
/// imported constant `DATAHUB_TRADE_KEY_ENV` (a `concat!`-split name in the hub protocol crate, which
/// is why this crate declares no settings row for it: the name is not a literal here).
#[cfg(feature = "datafusion-serve")]
fn strategy_lane_key(credentials: &std::collections::HashMap<String, String>) -> Option<&str> {
    credentials
        .get(vike_hub_proto::auth::DATAHUB_TRADE_KEY_ENV)
        .map(|v| v.trim())
        .filter(|v| !v.is_empty())
}

/// The datahub's keys with the strategy-lane key beside the pair, when the store holds it: the one door
/// that arms `Scope::Trade` here (`HubKeys::with_trade`). Absent, the keys are the pair alone and the
/// lane fails closed (no key, no `Scope::Trade`, no lane).
#[cfg(feature = "datafusion-serve")]
fn with_strategy_lane_key(
    keys: vike_hub_proto::auth::HubKeys,
    credentials: &std::collections::HashMap<String, String>,
) -> vike_hub_proto::auth::HubKeys {
    match strategy_lane_key(credentials) {
        Some(value) if lane_key_reuses_the_pair(value, credentials) => {
            tracing::error!(
                "vike-datahub: the strategy-lane key is the SAME value as the observe or the control \
                 key, so whoever holds that key (the desktop's) could open the lane; the lane is NOT \
                 served. Mint a distinct one with `vike-cli datahub setup --strategy-lane`, then restart"
            );
            keys
        }
        Some(value) => keys.with_trade(value.as_bytes().to_vec()),
        None => keys,
    }
}

/// Whether the strategy-lane key `value` equals the observe or the control key (compared trimmed, as
/// the keys are read). The scope byte is in the MAC, but a holder of the same key bytes signs a Trade MAC
/// as easily as its own, so a reused value is a lane open to a lower scope's key.
#[cfg(feature = "datafusion-serve")]
fn lane_key_reuses_the_pair(
    value: &str,
    credentials: &std::collections::HashMap<String, String>,
) -> bool {
    [vike_hub_proto::auth::DATAHUB_OBSERVE_KEY_ENV, vike_hub_proto::auth::DATAHUB_CONTROL_KEY_ENV]
        .iter()
        .any(|name| credentials.get(*name).is_some_and(|v| v.trim() == value))
}

/// Inert stub for a default (feature-off) build: there is no DataFusion backend to serve, so print
/// a rebuild hint and exit non-zero. Keeps `cargo build -p vike-datahub` producing a runnable bin.
///
/// ⚠ The missing-feature message is a RUN failure, and only that. `--help` and `--version` are
/// answered first, exactly as under the feature: a build that cannot serve can still describe its
/// own command line, and answering `vike-datahub --help` with "rebuild with --features …" told a
/// caller the wrong thing about the wrong question.
#[cfg(not(feature = "datafusion-serve"))]
// ⚠ The two parameters are UNUSED in this arm and keep their names underscored rather than being
// dropped: the signature must match the `datafusion-serve` `run` exactly, or the dispatcher and
// the bin would fail to compile in whichever configuration they were not written against.
pub fn run(
    _vars: &std::collections::HashMap<String, String>,
    _cwd: Option<&std::path::Path>,
    args: &[String],
) -> std::process::ExitCode {
    // The parse runs in BOTH builds and produces the same answers — that is `short_circuit`'s whole
    // job. A `--record` this build cannot honour is therefore a RUN failure below, never a rejected
    // flag: the flag is real and understood, and only the backend is missing.
    short_circuit(args);
    eprintln!(
        "vike-datahub was built without the `datafusion-serve` feature, so it has no data backend \
         to serve. Rebuild with: cargo run -p vike-datahub --features datafusion-serve"
    );
    ExitCode::from(2)
}

#[cfg(test)]
use self::args::{
    Cli, DEFAULT_SILENT_SECS, DEFAULT_TICK_SECS, Launch, ProfileSource, RECORDER_PROFILE_FLAG,
    RETIRED_PROFILE_FLAG, RecordRequest, read_args,
};
#[cfg(test)]
#[cfg(feature = "options-serve")]
use self::lanes::options_chain_recorder;
#[cfg(test)]
#[cfg(not(feature = "datafusion-serve"))]
use self::lanes::start_journal_materializer;
#[cfg(test)]
use self::lanes::{ChainRecorderArming, chain_recorder_arming, chain_recorder_refusal_line};
#[cfg(test)]
use self::lanes::{MaterializerArming, materializer_arming, materializer_refusal_line};
#[cfg(test)]
use self::recording::retention_by_kind_of;
#[cfg(test)]
#[cfg(not(feature = "datafusion-serve"))]
use self::recording::startup_mode_line;
#[cfg(test)]
#[cfg(feature = "record")]
use self::recording::to_record_args;

#[path = "datahub_cli_tests.rs"]
#[cfg(test)]
mod datahub_cli_tests;
