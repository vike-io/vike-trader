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
//! cleanly. (NB this crate has no edge to vike-backtest at all — ruling 7 removed it — so nothing
//! about that crate's feature shape reaches here; `serve-datafusion` is what pulls the
//! DataFusion/Arrow tree on its own account, and a default build is DataFusion-free.)
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
//! Node-key and credential stores (the settings database; `node.env` / `secrets.env` under
//! `<project>/settings` only on a box that has not migrated) —
//! `docs/decisions/0025-datahub-remote-posture.md`:
//! - `VIKE_DATAHUB_OBSERVE_KEY` / `VIKE_DATAHUB_CONTROL_KEY` — the scoped HMAC node keys. ⚠ **Their
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
//! - the OANDA practice account's API token, in a `backfill-serve` build — the ONE venue credential
//!   this daemon reads, through a scoped read made per `Backfill` request and dropped with it
//!   (docs/decisions/0097-the-datahub-reads-one-practice-token-for-a-credentialed-history-lane.md;
//!   `datahub_cli::oanda_history`'s `oanda_history_credentials` and `crate::backfill::credentialed_klines_row` carry
//!   the how).
//!   ⚠ On a KEY-LESS server the Control scope that fences it is a word nothing enforces — 0097
//!   declares that rather than gating it, for 0050's reason: the lane is what a local end user
//!   fetches with.
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
//! and nothing else — and `crates/vike-ops/tests/settings/settings_registry.rs` and
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
#[cfg(feature = "serve-datafusion")]
mod boot;
mod mounts;
#[cfg(feature = "backfill-serve")]
mod oanda_history;
mod recording;

#[cfg(doc)]
use self::args::ProfileSource;
use self::args::short_circuit;
#[cfg(feature = "serve-datafusion")]
use self::boot::{
    boot_root, check_bind, open_hist_store, refuse_stranded_venue_settings_in, resolve_hist_store,
    venue_settings_of,
};
#[cfg(feature = "backfill-serve")]
use self::mounts::mount_backfill;
#[cfg(feature = "serve-datafusion")]
use self::mounts::{
    mount_catalog_lane, mount_import_lane, mount_market_data_plane, mount_seed_lane,
};
#[cfg(feature = "serve-datafusion")]
use self::recording::startup_mode_line;
#[cfg(feature = "record")]
use self::recording::{finish_recording, load_recording_profile, to_record_args};

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

/// **Declare Polymarket's egress into the bridge** (decision 0095 — it opens no store and reads no
/// environment).
///
/// `loaded` is the ONE read of the `venue_setting` table [`run`] makes (`None`: no settings
/// directory); [`venue_settings_of`] takes the broker's copy from the same read. Everything that
/// decides what the rows MEAN for egress — the precedence and the ONE store-error policy — lives in
/// `vike_polymarket::declare_from_rows`, which the trading daemon calls too: five copies of this
/// body once handled an unreadable store two different ways.
///
/// Gated to match its one call site's own `#[cfg]` exactly (inside [`run`], itself
/// `serve-datafusion`-only, under the additional `any(venue-polymarket, catalog-serve)` the call
/// carries) — a looser gate would leave this function uncalled, and therefore unused, in a build
/// that compiles `run` without either venue feature.
///
/// ⚠ It read the credential store's five legacy egress names too, for one release, as the
/// temporary fallback for a box that had not moved them into rows. Decision 0095's Task 7 deleted
/// the fallback; such a credential row now stops this server at startup instead
/// ([`refuse_stranded_venue_settings_in`]).
#[cfg(all(
    feature = "serve-datafusion",
    any(feature = "venue-polymarket", feature = "catalog-serve")
))]
fn declare_polymarket_egress(
    loaded: Option<
        Result<
            std::collections::BTreeMap<String, vike_secrets::venue_setting::VenueSettings>,
            vike_secrets::DbError,
        >,
    >,
) {
    vike_polymarket::declare_from_rows(loaded);
}

/// The data daemon past `--help` — **a SEQUENCE of named phases**, each a function below, called in
/// the order its statements ran when this was one body.
///
/// ⚠ **The ORDER is the contract**, and the comment at each call says what depends on it. Three
/// gates read this function's own text: `crates/vike-ops/tests/wiring/polymarket_egress_declared_gate.rs`
/// wants Polymarket's egress declared above every call that can build a client,
/// `crates/vike-ops/tests/hygiene/compile_time_path_gate.rs` keys the one compile-time path on
/// `resolve_hist_store`, and `crate::recorder::arm` has to install the stop handlers before the
/// store opens, the port binds or a feed thread starts.
#[cfg(feature = "serve-datafusion")]
pub fn run(
    vars: &std::collections::HashMap<String, String>,
    cwd: Option<&std::path::Path>,
    args: &[String],
) -> std::process::ExitCode {
    use std::net::TcpListener;
    use std::sync::Arc;

    // BEFORE the log subscriber, the store and the listener: `--help` must not open a store, and it
    // certainly must not bind a port. (It did both — this binary read no argv at all.)
    let record = match short_circuit(args) {
        Ok(r) => r,
        Err(code) => return code,
    };
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

    // The loader's OWN non-fatal resolutions, emitted here because `vike_boot::boot` runs before a
    // subscriber exists and therefore returns them as DATA. This root had none to emit while it
    // took `SettingsLoad::Skip` (the arm returns `Settings::default()`, whose `warnings` are
    // empty); the join above is what gives it some, and the one an operator of THIS daemon is most
    // likely to see is `vike_config::flags::venue_catalog_refusal_ignored` — the notice that a
    // `VIKE_DATAHUB_VENUE_CATALOG` they exported with a value that used to mean OFF no longer
    // turns anything off. Dropping them would make that notice unreachable on the only box it is
    // about.
    for w in &booted.settings.warnings {
        tracing::warn!("{w}");
    }

    // Decision 0095, Task 7: a venue SETTING still filed as a credential row is read by nothing —
    // this server reads its venue settings from the `venue_setting` rows below and nowhere else —
    // so it refuses to start rather than run on defaults the operator did not choose. Before the
    // rows are read, before a port binds, and before any feed dials.
    if let Some(dir) = booted.settings_dir.as_deref()
        && let Err(why) = refuse_stranded_venue_settings_in(dir)
    {
        tracing::error!("{why}");
        eprintln!("vike-datahub: {why}");
        return ExitCode::from(2);
    }

    // Decision 0095: every venue's `venue_setting` rows, read ONCE from the boot's settings
    // directory. Polymarket's egress is DECLARED from that read into the bridge before any client
    // exists (the broker's feed clients, the recorder's rolling families and the venue catalog all
    // dial through it) — a build that links neither Polymarket feature has no client to configure
    // and declares nothing. The broker's client table below takes its copy of the same read
    // (Polymarket's socket batching, each CEX feed's mark-stream row).
    let loaded_venue_settings =
        booted.settings_dir.as_deref().map(vike_secrets::venue_setting::load_venue_settings);
    let venue_settings = venue_settings_of(loaded_venue_settings.as_ref());
    #[cfg(any(feature = "venue-polymarket", feature = "catalog-serve"))]
    declare_polymarket_egress(loaded_venue_settings);

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

    // The node keys, off the NODE store and this boot's own settings directory — see `read_node_keys`
    // for why that store and not the venue credential store, and what `store_unreadable` is for.
    let (keys, store_unreadable) = read_node_keys(&booted);

    // The hist-store root, by the shared precedence and logged before anything opens it.
    let resolved = resolve_hist_store(vars, cwd);
    let store_dir = resolved.root.display().to_string();

    // ⚠⚠ **EVERY RECORDING REFUSAL HAPPENS HERE — BEFORE THE BIND GUARD, THE STORE OPEN AND THE
    // LISTENER** — `load_recording_profile` carries the argument. The ORDER is this function's to
    // keep: a refused profile must exit 2 before a port is bound, never crash-loop after one.
    #[cfg(feature = "record")]
    let profile = match load_recording_profile(record.as_ref(), &booted, &resolved, cwd) {
        Ok(p) => p,
        Err(code) => return code,
    };

    let addr = vars
        .get("VIKE_DATAHUB_ADDR")
        .cloned()
        .unwrap_or_else(|| crate::server::DEFAULT_ADDR.to_string());

    // THE ONE OWNER OF VENUE CLIENTS in this process (`crate::feeds`). Built before either plane is
    // used, so both can take their client from it and a key both want is subscribed once. Building
    // it opens nothing: a venue client is constructed on its first subscribe. Its two holders are
    // the md plane (`crate::md::venues::market_builder`) and the recorder
    // (`crate::recording::build_recording_feed`, reached from both `crate::recorder::record` calls
    // below).
    let broker = crate::feeds::FeedBroker::new(
        crate::feeds::venues::real_client_table(venue_settings),
        crate::feeds::venues::sharing_for,
    );

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
            &broker,
        ));
    }

    // The bind guard — `check_bind` carries the whole argument. It runs BEFORE the store opens or
    // the listener binds, and a refusal exits 2: nothing was tried and failed, the configuration
    // was refused before anything opened.
    if let Err(code) = check_bind(vars, &addr, &keys, store_unreadable) {
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

    // The live market-data plane, armed by an operator and not by a build — see
    // `mount_market_data_plane`. Its hub and the recorder both take their venue clients from the
    // ONE `broker` above.
    let md_hub = mount_market_data_plane(vars, &broker);

    // Backfill-on-demand: a `backfill-serve` build mounts the REAL collector table over the SAME
    // store handle the server serves (`mount_backfill`); any other build mounts none and the verb
    // is a clean refusal.
    #[cfg(feature = "backfill-serve")]
    let backfill = mount_backfill(&booted, &store);
    #[cfg(not(feature = "backfill-serve"))]
    let backfill: Option<crate::backfill::BackfillTable> = None;

    // The chart-gap seed lane, which needs a collector table to arm anything (`mount_seed_lane`).
    let seed_lane = mount_seed_lane(vars, backfill.as_ref());

    // The venue-catalog lane, on by default with its refusal as the one switch
    // (`mount_catalog_lane`).
    let catalog_lane = mount_catalog_lane(&booted);

    // The archive import lane, mounted by the project and not by a switch (`mount_import_lane`).
    let import_lane = mount_import_lane(&booted, &store);

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
        // Both planes hold clients from ONE broker (docs/decisions/0092), so a shared key is
        // subscribed once.
        let serve_md = md_hub.clone();
        let serve_seed = seed_lane.clone();
        let serve_catalog = catalog_lane.clone();
        let serve_import = import_lane.clone();
        std::thread::spawn(move || {
            match crate::server::serve_with_import(
                listener,
                serve_store,
                backfill,
                keys,
                serve_md,
                serve_seed,
                serve_catalog,
                serve_import,
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
            &broker,
        ));
    }

    match crate::server::serve_with_import(
        listener,
        store,
        backfill,
        keys,
        md_hub,
        seed_lane,
        catalog_lane,
        import_lane,
    ) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!(error = %e, "vike-datahub: serve loop ended with an error");
            ExitCode::FAILURE
        }
    }
}

/// **Phase: the node keys** — the datahub pair, read from the NODE store and never from the venue
/// credential store, plus whether that store was PRESENT BUT UNREADABLE (the bind guard words its
/// refusal differently for that cause). `run` calls this once, after the stop handlers are armed.
/// The argument `node_keys_from_vars` is handed is what
/// `crates/vike-ops/tests/credentials/node_key_store_gate.rs` reads, so it stays this file's `credentials`.
#[cfg(feature = "serve-datafusion")]
fn read_node_keys(booted: &vike_boot::Booted) -> (Option<vike_node_proto::auth::NodeKeys>, bool) {
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
    // vike-secrets names nothing above the vocabulary floor and is already in this crate's
    // closure. (⚠ That read "is a zero-vike-dependency leaf" until
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
    //
    // ⚠ Decision 0097 added ONE optional read of the CREDENTIAL store, and it is not this one: the
    // OANDA history lane's practice token, one scoped name per Backfill request for oanda
    // (`oanda_history_credentials`). A box without it — or without a credential store at all —
    // starts and serves every verb exactly as before; only a Backfill for oanda is refused.
    //
    // ⚠ The probe is THIS SERVICE'S FAMILY, not all four platform names. `resolve_node_keys`
    // answers WHICH FILE and this root reads its own pair out of it, so the wide predicate let the
    // OTHER service's migration decide this one's: a `node.env` holding only the TRADEHUB pair
    // (what `vike-cli node setup` writes) answered `NodeFile`, and this daemon's datahub pair still
    // in `secrets.env` resolved to nothing — keyless, with no migration notice to explain it.
    let (credentials, key_source): (std::collections::HashMap<String, String>, _) =
        match vike_secrets::resolve_node_keys(
            booted.settings_dir_override.as_deref(),
            vike_model::credential_keys::is_datahub_node_key,
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
    let keys = vike_node_proto::auth::node_keys_from_vars(&credentials);
    (keys, store_unreadable)
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
use self::args::{DEFAULT_SILENT_SECS, DEFAULT_TICK_SECS, Parsed, ProfileSource, parse_args};
#[cfg(test)]
#[cfg(feature = "backfill-serve")]
use self::oanda_history::{oanda_history_credentials, oanda_history_presence};
#[cfg(test)]
#[cfg(feature = "backfill-serve")]
use self::oanda_history::{oanda_history_lane_line, oanda_history_presence_probe};
#[cfg(test)]
#[cfg(feature = "backfill-serve")]
use self::oanda_history::{oanda_history_token_reader, read_oanda_history_token};
#[cfg(test)]
#[cfg(not(feature = "serve-datafusion"))]
use self::recording::startup_mode_line;

#[path = "datahub_cli_tests.rs"]
#[cfg(test)]
mod datahub_cli_tests;
