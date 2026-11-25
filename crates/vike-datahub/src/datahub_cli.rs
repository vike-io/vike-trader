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
//! The real server (a `HistoryStoreDataFusion`-backed [`serve`](crate::serve) loop) is compiled
//! ONLY under the `datafusion-serve` feature, which is what makes the concrete `HistoryStoreDataFusion`
//! backend nameable here. A default build compiles the inert stub `main` below instead, so
//! `cargo build -p vike-datahub` always produces a runnable (if non-serving) binary that compiles
//! cleanly. (NB this crate has no edge to vike-backtest at all — ruling 7 removed it — so nothing
//! about that crate's feature shape reaches here; `datafusion-serve` is what pulls the
//! DataFusion/Arrow tree on its own account, and a default build is DataFusion-free.)
//!
//! Settings (rows of the settings database; no variable reaches any of them, decision 0111):
//! - `config.datahub_bind_addr` — listen address; default `127.0.0.1:7878`. ⚠ A NON-LOOPBACK
//!   address is REFUSED at startup unless the `flags.datahub_allow_public_bind` row is on
//!   **AND its settings database can be read** (decision 0288 item 8) — the tradehub `bind_decision` treatment,
//!   mirrored and then composed with that store's readability (see `crate::server`'s
//!   module doc, and the guard's own comment at the match below). The opt-in alone used to be
//!   enough, behind a `warn!`; that is the state `docs/decisions/0025-datahub-remote-posture.md`
//!   names verbatim as the one it exists to prevent. The guard did NOT soften when authentication
//!   landed: the handshake is plaintext and authenticates the CONNECTION, not each frame, so the
//!   tunnel is still what supplies confidentiality and integrity.
//! - `config.node_idle_read_timeout_ms` — how long a connection may go without a request frame
//!   before this server closes it; default `vike_hub_protocol::limits::SERVER_IDLE_READ_TIMEOUT`.
//!   The backtesthub reads the same row for its own connections. Refused outside its range at the
//!   write and at the load; `crate::server::ServerTimings` carries it into the connection loop.
//! - `config.md_heartbeat_ms` — the market-data stream's heartbeat period; default
//!   `vike_datahub_client::market::MARKET_DATA_HEARTBEAT`. Sent to the client in `MarketDataSubscribed.heartbeat_ms`,
//!   so the client's read deadline follows it. Refused outside its range at the write and the load.
//!
//! Node-key and credential stores (the settings database, `<project>/settings/db/vike.db` — the
//! only store) — `docs/decisions/0025-datahub-remote-posture.md`:
//! - The ACCESS LIST (decision 0288 item 8; `read_hub_keys`): this server ALWAYS authenticates. EVERY
//!   connection must complete the `Hello`/`Auth` handshake, signed by a device key the list holds,
//!   and the device's role decides the scope: Observe reads history and the catalog, and Control
//!   additionally admits the `Backfill` and `ImportArchive` WRITEs and the `DeleteSeries` REMOVAL.
//!   The old shared pair (`VIKE_DATAHUB_OBSERVE_KEY` / `VIKE_DATAHUB_CONTROL_KEY`) is read by
//!   nothing. ⚠ It used to say "and every `Run*` verb" — those verbs are the compute
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
//!   `crates/bridges/oanda/src/credentialed_klines.rs`'s `oanda_history_credentials` is the read and
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
//!   every kind. ⚠ IRREVERSIBLE deletion; the account kinds (`equity`, `exec_*`) are refused by name. Parsed by
//!   `recording::load_retention_by_kind` BEFORE the bind guard (a refusal exits 78, parked) and handed to
//!   `crate::recorder::record` as a parameter; a process that runs no maintenance logs that it
//!   deletes nothing.
//! - `config.datahub_news_poll_secs`, `config.datahub_calendar_poll_secs`,
//!   `config.datahub_equity_poll_secs` — the tool-feeds lane's cadences, folded into
//!   `crate::tool_feeds::ToolFeedCadences` by `lanes::load_feed_cadences` before the bind guard, and mounted
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
//! profile create recorder`) before this release reaches a box that has only ever used `--record`.
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
#[cfg(feature = "datafusion-serve")]
mod store_cmd;

#[cfg(doc)]
use self::args::ProfileSource;
use self::args::short_circuit;
#[cfg(feature = "datafusion-serve")]
use self::boot::{boot_root, check_bind, open_hist_store, resolve_hist_store};
#[cfg(feature = "backfill-serve")]
use self::lanes::start_backfill;
#[cfg(feature = "datafusion-serve")]
use self::lanes::{
    arm_bar_recorder, arm_start_history, load_feed_cadences, start_catalog_lane,
    start_execution_stream, start_import_lane, start_market_data_plane, start_options_lane,
    start_properties_tape, start_seed_lane, start_strategy_lane, start_text_recorder,
    start_tool_feeds, warm_catalog,
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
// compiles nothing has no reason to hold a script compiler, a `vike-rhai` edge or an indicator
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
    // A local tool over a store on this box: no boot, no listener, no lane. `store_command`'s module doc.
    if let Some(sub) = launch.store_tool {
        return store_cmd::run_store(vars, cwd, sub);
    }
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
             with: cargo build -p vike-datahub --features record"
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
        // A refused configuration (a retired variable, an illegal settings row, a bad
        // `VIKE_NO_LIVE_KEYS` word), which a restart meets again: the operator-needed status, so the
        // supervisor parks this daemon instead of restarting it every 5 s.
        Err(e) => {
            eprintln!("vike-datahub: {e}");
            return ExitCode::from(vike_utility::cli::OPERATOR_NEEDED_EXIT);
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
    // ...AND ITS BUILD ON THE WIRE: every `Welcome` states the sha256 of this program once it is hashed
    // (`crate::server::advertise_running_program`), what the backtesthub keys a run's answer from a saved run with.
    crate::server::advertise_running_program();

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
    // keep: a refused profile must exit 78 (parked) before a port is bound, never crash-loop after one.
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
    // same reason: a row the reader refuses exits 78 before a port is bound. It is handed to
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
    // `config.datahub_addr`: that is a CLIENT's dial. The default is the one a client dials when
    // that row is absent, so an unconfigured pair meets.
    let bind_row = booted.settings.config.datahub_bind_addr.clone();
    let addr = bind_row.unwrap_or_else(|| vike_config::DEFAULT_DATAHUB_ADDRESS.to_string());

    // THE ONE OWNER OF VENUE CLIENTS in this process (`crate::feeds`). Built before either plane is
    // used, so both can take their client from it and a key both want is subscribed once. Building
    // it opens nothing: a venue client is constructed on its first subscribe. Its two holders are
    // the market data plane (`crate::market_data::venues::market_builder`) and the recorder
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
            &crate::live_family_tokens::LiveFamilyTokensHeld::default(),
        ));
    }

    // The bind guard — `check_bind` carries the whole argument. It runs BEFORE the store opens or
    // the listener binds, and a refusal exits 78 (parked): nothing was tried and failed, the
    // configuration was refused before anything opened.
    if let Err(code) = check_bind(&booted.settings.flags, &addr, &keys, store_unreadable) {
        return code;
    }

    let store = match open_hist_store(&store_dir) {
        Ok(s) => s,
        Err(code) => return code,
    };
    // Kept lists an older build wrote are named here, once, BEFORE the listen: the tradehub starts only once this
    // daemon listens, so it never reads an unnamed list (the called function's doc says how).
    if let Some(dir) =
        vike_paths::state_path::instruments_dir_beside(booted.settings_dir.as_deref())
    {
        crate::instrument_lists::name_older_kept_lists(&dir, booted.settings_dir.as_deref());
    }
    let listener = match TcpListener::bind(&addr) {
        Ok(l) => l,
        Err(e) => {
            tracing::error!(addr = %addr, error = %e, "vike-datahub: failed to bind listener");
            return ExitCode::FAILURE;
        }
    };
    tracing::info!(addr = %addr, store = %store_dir, "vike-datahub: listening");

    let store = Arc::new(store);
    // The SPLIT WARNING (`crate::split_watch`), shown every bar and quote write of this one store handle BEFORE any
    // lane writes through it; it reads the split rows of this project's settings database and writes nothing.
    let split_watch = Arc::new(crate::split_watch::SplitWatch::new(Some(booted.store.clone())));
    if store.watch_prices(split_watch.price_watch()).is_err() {
        tracing::error!("vike-datahub: the store already had a price watch; no split is watched");
    }
    warm_catalog(&store);

    // The OPTIONAL writers' free-space floor (`config.optional_writes_min_free_mib`, REVIEW-LIVE-MONEY H1), over
    // the store's root: the live-bar and text recorders and the execution stream's forecast rows ask it before they
    // write, so none of them fills the disk a trading node's journal and vike.db share.
    let store_floor: Arc<dyn Fn() -> Option<String> + Send + Sync> = {
        let floor = vike_boot::disk_floor::DiskFloor::of(&booted.settings.config);
        let root = resolved.root.clone();
        Arc::new(move || floor.refusal(&root))
    };

    // The EXECUTION STREAM, mounted by the settings database it keeps its cursors in and by nothing else
    // (`start_execution_stream`): a tradehub's live executions arrive over the wire and are written through the
    // SAME store handle the server serves. This process reads no journal file.
    let execution_stream = start_execution_stream(&booted, Arc::clone(&store_floor));

    // The live market-data plane, armed by an operator and not by a build — see
    // `start_market_data_plane`. Its hub and the recorder both take their venue clients from the
    // ONE `broker` above.
    let market_data_hub = start_market_data_plane(&booted.settings, &broker);

    // The STRATEGY LANE, armed by the KEY and by nothing else - see `start_strategy_lane`. It takes its
    // venue clients from the same ONE broker, as the third holder.
    let strategy_lane = start_strategy_lane(keys.as_ref(), &broker, &gateway);

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

    // The venues' KEPT instrument lists, `<project>/instruments` (`crate::instrument_lists`): this daemon is their only
    // writer, and writes one only when the operator refreshes it (`Request::RefreshInstrumentLists`; no timer, the
    // owner's ruling of 2026-11-16). No project, no lists.
    let instruments_dir =
        vike_paths::state_path::instruments_dir_beside(booted.settings_dir.as_deref());
    // ...which this process's own name lookups read too (`vike_catalog::kept_list`: the history doors name a
    // series by its user name and ask the venue by its row's wire name, decision 0290).
    if let Some(dir) = &instruments_dir {
        vike_catalog::use_kept_lists(dir.clone());
    }
    // How an instrument the operator adds by hand is looked up on its venue: the gatewayhub, over a session it holds.
    let instrument_lookup = Some(crate::server::gateway_lookup(gateway.clone()));
    // ...and how a symbol's facts are read for a tradehub paper account when the store holds none: the gatewayhub's
    // keyless catalog search, on this datahub's own Read key (`Request::StrategySymbolFacts`).
    let keyless_lookup = Some(crate::server::keyless_lookup(gateway.clone()));

    // The writer of the `kind=properties` tape, over that lane and the served store, when the
    // `flags.record_properties` row is on, the default (`start_properties_tape`).
    start_properties_tape(&booted, catalog_lane.as_ref(), &store);

    // The archive import lane, mounted by the project and not by a switch (`start_import_lane`).
    let import_lane = start_import_lane(&booted, &store);

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
        strategy_lane.as_ref(),
        &store,
        &gateway,
        &booted.settings.flags,
        compaction_runs,
    );
    #[cfg(not(feature = "record"))]
    drop(start_history);
    // The strategy lane's LIVE-BAR RECORDER (`flags.record_bars`, on by default), on the same conditions; its
    // handle is dropped at the orderly stop (below), which writes the closes still queued.
    // Its closes flush by the tape recorder's policy at their own age (`bar_flush_policy`).
    #[cfg(feature = "record")]
    let bar_flush = record_args.as_ref().map(crate::recorder::bar_flush_policy).unwrap_or_default();
    #[cfg(not(feature = "record"))]
    let bar_flush = vike_store::RecorderConfig::default();
    let bar_flush =
        vike_store::RecorderConfig { write_floor: Some(Arc::clone(&store_floor)), ..bar_flush };
    let bar_recorder = arm_bar_recorder(
        strategy_lane.as_ref(),
        &store,
        &booted.settings.flags,
        compaction_runs,
        bar_flush,
    );
    #[cfg(not(feature = "record"))]
    drop(bar_recorder);

    // The options lane, mounted by the build and carrying the `kind=chain` recorder when the
    // operator armed it and the store is compacted (`start_options_lane`).
    let options_lane = start_options_lane(&booted, &store, compaction_runs, &gateway);

    // The tool-feeds lane, mounted by the build with no runtime switch; it polls nothing until the
    // first request and reads its two API keys from this box's credential store (`start_tool_feeds`).
    let tool_feeds_lane = start_tool_feeds(&booted, feed_cadences);

    // The news and calendar recorder over that lane and the served store, when the operator armed it
    // and the store is compacted (`start_text_recorder`).
    start_text_recorder(&booted, tool_feeds_lane.as_ref(), &store, compaction_runs, store_floor);

    // What `GatewayStatus` answers from. It dials nothing until a client asks, so a gatewayhub that is
    // down costs this start nothing.
    let gateway_status_lane =
        Some(Arc::new(crate::server::GatewayStatusLane::dialling(gateway.clone())));
    // How an operator's credential change reaches the gatewayhub (decision 0287): one dial per change, under the
    // control key this datahub already holds for it, and nothing until a client asks.
    let credentials_relay = Some(crate::server::credentials_relay(gateway.clone()));
    // What `GatewayLinks` and the symbol-list verbs read: the handle over this project's settings database, no
    // connection until a client asks.
    let settings_store = Some(booted.store.clone());

    // The two connection timings an operator may set: the idle read timeout of every connection and
    // the market-data stream's heartbeat period. Read HERE, once, from the boot's rows and handed to
    // the server as a parameter (`ServerTimings::from_rows` takes plain values and reads nothing); a
    // box with neither row runs `ServerTimings::PRODUCTION`'s constants, byte for byte. The rows were
    // range-checked by the loader, so a daemon that got this far holds values inside the range.
    let timings = crate::server::ServerTimings::from_rows(
        booted.settings.config.node_idle_read_timeout_ms,
        booted.settings.config.market_data_heartbeat_ms,
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
        let serve_store = Arc::clone(&store) as Arc<dyn vike_data::HistoryStore + Send + Sync>;
        let serve_stop = stop.flag();
        // Both planes hold clients from ONE broker (docs/decisions/0092), so a shared key is
        // subscribed once.
        let serve_market_data = market_data_hub.clone();
        let serve_seed = seed_lane.clone();
        let serve_catalog = catalog_lane.clone();
        let serve_import = import_lane.clone();
        let serve_options = options_lane.clone();
        let serve_tool_feeds = tool_feeds_lane.clone();
        let serve_strategy = strategy_lane.clone();
        let serve_gateway_status = gateway_status_lane.clone();
        let serve_credentials_relay = credentials_relay.clone();
        let serve_settings_store = settings_store.clone();
        let serve_instruments = instruments_dir.clone();
        let serve_lookup = instrument_lookup.clone();
        let serve_keyless_lookup = keyless_lookup.clone();
        let serve_split_watch = Some(Arc::clone(&split_watch));
        let serve_execution_stream = execution_stream.clone();
        // What the recorder below holds of the families it records, read by the served `Request::LiveFamilyTokens`.
        let live_family_tokens = crate::live_family_tokens::LiveFamilyTokensHeld::default();
        let serve_live_family_tokens = Some(live_family_tokens.clone());
        std::thread::Builder::new().name("dh-serve".into()).spawn(move || {
            match crate::server::serve_with_lanes(
                listener,
                serve_store,
                backfill,
                keys,
                serve_market_data,
                serve_seed,
                crate::server::ServerLanes {
                    catalog: serve_catalog,
                    options: serve_options,
                    tool_feeds: serve_tool_feeds,
                    strat: serve_strategy,
                    gateway_status: serve_gateway_status,
                    credentials_relay: serve_credentials_relay,
                    settings_store: serve_settings_store,
                    instruments: serve_instruments,
                    instrument_lookup: serve_lookup,
                    keyless_lookup: serve_keyless_lookup,
                    split_watch: serve_split_watch,
                    live_family_tokens: serve_live_family_tokens,
                    execution_stream: serve_execution_stream,
                },
                serve_import,
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
            &live_family_tokens,
        ));
        // The closes still queued for the start history's saver are written before the process goes (a
        // second at most; an empty queue returns at once): they are the last minute of every saved key.
        // This second is OUTSIDE `crates/vike-datahub/src/recorder.rs`'s `TOTAL_STOP_BUDGET_SECS` (the
        // bounded teardown has already returned): the shipped unit's `TimeoutStopSec=` of 45 s leaves
        // 13 s over that total, so 12 s remain after it. Raise it only with that margin in view.
        if let Some(history) = &start_history {
            history.settle(std::time::Duration::from_secs(1));
        }
        // ...and the live-bar recorder's: its drop flushes the open buffers (at most one flush window of closes).
        drop(bar_recorder);
        return code;
    }

    let serve = move || {
        crate::server::serve_with_lanes(
            listener,
            store,
            backfill,
            keys,
            market_data_hub,
            seed_lane,
            crate::server::ServerLanes {
                catalog: catalog_lane,
                options: options_lane,
                tool_feeds: tool_feeds_lane,
                strat: strategy_lane,
                gateway_status: gateway_status_lane,
                credentials_relay,
                settings_store,
                instruments: instruments_dir,
                instrument_lookup,
                keyless_lookup,
                split_watch: Some(split_watch),
                // No recorder runs on this arm, so no family is followed; the verb says so in words.
                live_family_tokens: None,
                execution_stream,
            },
            import_lane,
            timings,
        )
        .map_err(|e| e.to_string())
    };
    // A recording was asked for and there is NOTHING TO RECORD YET (`load_recording_profile`'s fresh-server arm):
    // its stop handlers are armed, so the serve loop runs on a thread and this one ends at the stop, or restarts to
    // record the first recorder profile stored.
    #[cfg(feature = "record")]
    if let Some(stop) = stop.as_ref() {
        return recording::serve_with_nothing_to_record(
            stop,
            serve,
            booted.settings_dir_override.as_deref(),
            cwd,
        );
    }
    match serve() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!(error = %e, "vike-datahub: serve loop ended with an error");
            ExitCode::FAILURE
        }
    }
}

/// **Phase: the hub keys** — the access list over the boot's settings directory and the strategy-lane key,
/// read from the NODE store and never from the venue credential store (no shared pair: decision 0288 item 8),
/// plus whether that store was PRESENT BUT UNREADABLE (the bind guard words its refusal differently for that
/// cause). `run` calls this once, after the stop handlers are armed.
///
/// Formerly `read_node_keys`, renamed in the shape-and-names change (decision 0166).
#[cfg(feature = "datafusion-serve")]
fn read_hub_keys(booted: &vike_boot::Booted) -> (Option<vike_hub_protocol::auth::HubKeys>, bool) {
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
    // point. This server needs its strategy-lane key and NOTHING else (decision 0288 item 8: a person
    // is admitted by the access list, never by a shared pair): it does not trade, holds no venue
    // account and signs no order. Until 2026-09-08 it read the whole credential map, which opens the
    // store holding every venue key on the box, 168 names of them, to find two.
    // `CredentialStore::hub_keys` reads the settings database's `hub_key` table, the only hub-key store,
    // and hands back the one name the predicate admits and nothing else.
    //
    // A deployment can give this service a settings database whose `credential` table is empty, and
    // it starts correctly authenticated (by its access list) — which is what "compute-to-data" should
    // have meant from the start.
    //
    // ⚠ The scope is the strategy-lane key BY NAME (0164's daemon class: the tradehub holds it to open the
    // lane), not the datahub's observe/control pair, which no reader of this server holds any more
    // (decision 0288 item 8): `hub_keys` hands back the names the predicate admits and no others.
    let credentials: std::collections::HashMap<String, String> =
        match booted.store.hub_keys(|key| key == vike_hub_protocol::auth::DATAHUB_TRADE_KEY_ENV) {
            Ok(resolved) => {
                // The store's own findings. `vike-secrets`' reads return them as DATA (they log
                // nothing) and `vike_secrets::CredentialRead::report` is the one place they become log
                // lines, so this root owes that call rather than being the one binary where a 0644
                // credential store is read in silence. The UNREADABLE arm below keeps this root's
                // own louder line instead.
                resolved.report();
                resolved.secrets.into_map()
            }
            Err(e) => {
                tracing::error!(
                    error = %e,
                    "vike-datahub: credential store PRESENT but UNREADABLE — the strategy-lane \
                     key was NOT loaded and the access list in the same database cannot be read, \
                     so this server admits NOBODY. On a NON-LOOPBACK bind it will now REFUSE TO \
                     START a few lines below. Either way the cause is this store: fix its \
                     permissions and restart"
                );
                store_unreadable = true;
                Default::default()
            }
        };
    // The served keys (decision 0288 item 8): this server ALWAYS authenticates (the owner: never cut security) — a
    // person admitted by the access list alone (no shared pair is read), an EMPTY list admitting nobody and saying
    // "no device registered" (`backend mint-missing` registers this box's own devices at every server start, before
    // the daemons), and the strategy lane by its own key when held (`HubKeys::with_trade`, the one door that arms
    // `Scope::Trade` here; absent, the lane fails closed). No settings directory: no list to register in, so the
    // list is empty and nobody is admitted. A public bind is the bind guard's own question (its consent row).
    let access = match booted.settings_dir.as_ref() {
        Some(dir) => crate::server::access_list(dir.clone()),
        None => vike_hub_protocol::access::AccessList::without_store(),
    };
    let keys = vike_hub_protocol::auth::HubKeys::new(Vec::new(), Vec::new()).with_access(access);
    let keys = match strategy_lane_key(&credentials) {
        Some(value) => keys.with_trade(value.as_bytes().to_vec()),
        None => keys,
    };
    (Some(keys), store_unreadable)
}

/// The strategy-lane key's value out of the hub-key map, `None` when absent or blank. Read by the
/// imported constant `DATAHUB_TRADE_KEY_ENV` (a `concat!`-split name in the hub protocol crate, which
/// is why this crate declares no settings row for it: the name is not a literal here).
#[cfg(feature = "datafusion-serve")]
fn strategy_lane_key(credentials: &std::collections::HashMap<String, String>) -> Option<&str> {
    credentials
        .get(vike_hub_protocol::auth::DATAHUB_TRADE_KEY_ENV)
        .map(|v| v.trim())
        .filter(|v| !v.is_empty())
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
use self::lanes::bar_recorder_refusal;
#[cfg(test)]
#[cfg(feature = "options-serve")]
use self::lanes::options_chain_recorder;
#[cfg(test)]
use self::lanes::text_recorder_arming;
#[cfg(test)]
use self::lanes::{ChainRecorderArming, chain_recorder_arming, chain_recorder_refusal_line};
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
