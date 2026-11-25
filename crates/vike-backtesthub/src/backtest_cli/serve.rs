//! `backtesthub --addr`: become the backtesthub (settings, hub keys, the bind decision, serve).

use std::path::PathBuf;
use std::process::ExitCode;

use vike_config::config::{DEFAULT_BACKTEST_BAR_CACHE_MIB, DEFAULT_BACKTEST_TICK_CACHE_MIB};
use vike_datahub_client::route::history_route;
use vike_model::run_request::flag_vocab::store_flag_removed;

use super::args::{AddressFlag, RunArgs};

/// The `--addr` value LADDER, mirroring `datahub`'s exactly: an explicit flag value, then the
/// `config.backtest_addr` row, then `vike_config::DEFAULT_BACKTEST_ADDRESS`.
///
/// No variable is a rung (decision 0111): `configured` is the row the one loader resolved, which
/// keeps this workspace's "one loader decides precedence" property intact and stops a second,
/// subtly different ladder existing inside a binary. Pure, so the ladder is unit-testable without
/// a settings directory and without an environment.
fn resolve_serve_address(flag: &AddressFlag, configured: Option<&str>) -> String {
    match flag {
        AddressFlag::Explicit(v) => v.clone(),
        _ => configured
            .map(str::to_string)
            .unwrap_or_else(|| vike_config::DEFAULT_BACKTEST_ADDRESS.to_string()),
    }
}

/// This binary's settings — the DATABASE first, the files only on a box that has none.
///
/// ⚠ **Owner ruling, 2026-09-23: the datahub address comes from the settings DATABASE** — *"it had
/// to get address from sqlite, no files; all settings have to live in sqlite"*. That is what the
/// `StoreLayer` below already does, and what a narrower ladder would have broken: reading
/// a variable and falling through to the compiled default would have left a configured address
/// unread on exactly the boxes that were migrated on 2026-09-14
/// (`docs/decisions/0054-settings-move-into-one-database.md`).
///
/// ⚠ **A settings directory is OPTIONAL** (a checkout run from anywhere has none) **but a
/// MALFORMED store is FATAL.** That rule used to be argued for the socket alone — *"on the path
/// that opens a socket, a config this box has and cannot parse must never be silently skipped"* —
/// and it now governs the RUN path too, which is a real behaviour change: a plain `backtest run`
/// on a box with a broken settings store used to ignore it and run on defaults. It is the same
/// argument and it is stronger here, not weaker — a run that silently used default settings would
/// publish a result nobody could reproduce, and its own record would name settings it never read.
///
/// ⚠ **THROUGH THE SETTINGS SOURCE, and this is an UNDECLARED composition root** (it walks for its
/// own settings directory). It is invisible to `crates/vike-boot/tests/one_owner.rs` only because
/// that gate's `production_half` truncates this file at its `#[cfg(test)]` module. The hole is
/// real and this is what fell into it: a store-blind `vike_config::load` on a box that has run
/// `vike-cli config adopt` reads `config.*` out of files nothing resolves from. Hoisting did not
/// CLOSE that hole — it kept it at ONE, which is the most this change can honestly claim.
///
/// ⚠ **A retired settings variable REFUSES, here as at every booted root** (decision 0111's verdict
/// 1): no settings key is read from the environment, so a unit line still setting one would
/// otherwise configure nothing in silence. Exit 2, the code a malformed store gets; the `--addr` daemon
/// parks on both instead (`daemon_before_logging`).
///
/// `who` prefixes every line, so each arm still says which invocation is talking.
pub(super) fn load_backtest_settings(
    vars: &std::collections::HashMap<String, String>,
    who: &str,
) -> Result<vike_config::Settings, ExitCode> {
    if let Err(refusal) = vike_config::refuse_removed_env(vars) {
        eprintln!("{who}: {refusal}");
        return Err(ExitCode::from(2));
    }
    // ⚠ `VIKE_SETTINGS_DIR` names the directory outright and beats the walk, and it is spelled as a
    // LITERAL rather than through `vike_config`'s constant: the settings registry's map-lookup
    // sweep resolves constants CRATE-WIDE, so importing one would make this read invisible to
    // `crates/vike-ops/tests/settings_secrets/settings_registry.rs`.
    let settings_override = vars.get("VIKE_SETTINGS_DIR").map(String::as_str);
    let settings_dir = std::env::current_dir()
        .ok()
        .and_then(|cwd| vike_paths::state_path::project_settings_dir_from(settings_override, &cwd));
    let store = settings_dir.as_deref().map(vike_secrets::read_settings_in);
    let mut store_refusal = String::new();
    let source = vike_config::StoreLayer::of(store.as_ref(), &mut store_refusal);
    let settings = match vike_config::load_with_source(
        settings_dir.as_deref(),
        source,
        &vike_config::CliOverrides::default(),
    ) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{who}: {e}");
            return Err(ExitCode::from(2));
        }
    };
    for w in &settings.warnings {
        eprintln!("{who}: {w}");
    }
    Ok(settings)
}

/// **The `--addr` DAEMON's flag and settings, read BEFORE the log subscriber exists** — `Ok(None)`
/// for every invocation that is not the daemon: no `--addr`. (A BLANK one never gets here: the parse
/// refuses it, for every argument.)
///
/// Before the subscriber because the daemon's rolling file takes its LEVEL from the
/// `preferences.log_file_level` row (`docs/decisions/0111-no-setting-lives-in-the-environment-or-a-toml-file.md`,
/// verdict 3), and a subscriber built first could only ever honour the compiled default. The order costs the daemon nothing: [`load_backtest_settings`] reports through
/// stderr and never through `tracing`, a MALFORMED store (or a retired variable) refuses the daemon
/// with `vike_utility::cli::OPERATOR_NEEDED_EXIT` — a restart meets it again, so the unit and the
/// server install's supervisor park it — and a store that will not OPEN leaves every key at its
/// compiled default and prints the loader's own warning saying so.
///
/// ⚠ `--addr` is the request to become the daemon wherever it stands on the line, so a line that
/// ALSO asks for `--list` now reads its settings before answering (a subcommand cannot share the
/// line with it). Every other arm
/// keeps its load where it was, after its own argv triage.
pub(super) fn daemon_before_logging(
    vars: &std::collections::HashMap<String, String>,
    flag: AddressFlag,
) -> Result<Option<(AddressFlag, vike_config::Settings)>, ExitCode> {
    match flag {
        AddressFlag::Absent => Ok(None),
        flag => load_backtest_settings(vars, "backtesthub --addr")
            .map(|s| Some((flag, s)))
            .map_err(|_| ExitCode::from(vike_utility::cli::OPERATOR_NEEDED_EXIT)),
    }
}

/// Become the backtesthub: resolve the address, load the hub keys, classify the bind, open the
/// store and serve the seven verbs forever.
///
/// The composition order is the data daemon's, deliberately (`vike_datahub::datahub_cli`'s `run`):
/// settings, then keys, then the BIND DECISION — before the store is opened and before a listener
/// exists — then the listener, then `serve_authed`. A refused configuration must cost nothing and
/// touch nothing.
///
/// ⚠ **A refusal EXITS** rather than degrading: serving is the only thing `--addr` was asked to do,
/// so "do not bind" and "do not run" are the same decision. It exits
/// `vike_utility::cli::OPERATOR_NEEDED_EXIT` (78), the refused configuration a restart meets again,
/// which `deploy/vike-backtesthub.service`'s `RestartPreventExitStatus=` and the server install's
/// supervisor park on — not `FAILURE`, which is restarted every 5 s, because nothing was tried and
/// failed. A bad flag keeps `USAGE_EXIT` (2).
///
/// `settings` are this daemon's own, loaded by [`daemon_before_logging`] before the log subscriber
/// existed — the one load this arm performs.
pub(super) fn run_serve(
    vars: &std::collections::HashMap<String, String>,
    run: &RunArgs,
    flag: AddressFlag,
    settings: vike_config::Settings,
    // The binary's commit (`vike_runs::BuildStamp::git_sha`), which a saved bar run's event log records.
    build: Option<&str>,
) -> ExitCode {
    use std::net::{TcpListener, ToSocketAddrs};

    use vike_datahub_client::bind::{BindDecision, ServerAuth, bind_decision};

    // ⚠ `VIKE_SETTINGS_DIR` names the directory outright and beats the walk — the ONE fact this
    // root pulls out of the swept map to find its project, and spelled as a LITERAL for the reason
    // the indicator read above gives (the settings registry's map-lookup sweep resolves constants
    // crate-wide, so importing the constant would make the read invisible to the gate).
    let settings_override = vars.get("VIKE_SETTINGS_DIR").map(String::as_str);
    // ⚠ **The walk and the load live in `load_backtest_settings`, which the RUN path calls too.**
    // They used to be spelled out right here, which made the `--addr` daemon the ONLY arm of this
    // binary that could see a configured `config.datahub_addr`; the run path now needs the same
    // key to know where its history comes from. ONE function walks for the settings directory, so
    // there is still one place that does it — but it is CALLED from each arm at that arm's own
    // moment, because they are not the same moment: this one is before the log subscriber (its file
    // level is a row — `daemon_before_logging` carries why), and the run path's is after the argv
    // triage that refuses a command line without performing any I/O. The two arms are mutually
    // exclusive, so at most one call ever runs. That function's doc carries why the walk happens,
    // why a malformed store is fatal, and which gate cannot see any of it.
    //
    // **`preferences.sweep_threads` reaches the sweep pool from HERE** — this arm INSTALLS it even
    // though it no longer LOADS it, and the halves are deliberately apart: installing is
    // process-wide and once, so it belongs where a daemon is about to serve, while the load now
    // serves both arms (the run path installs it beside its own load, in `open_store.rs`).
    // `vike_backtest::search::paramscan::install_paramscan_threads` is the process-wide handle the pool reads at
    // construction; without this call the row would be inert on this daemon. Before the listener
    // binds, so the first served sweep already has it.
    vike_backtest::search::paramscan::install_paramscan_threads(settings.preferences.sweep_threads);
    let addr = resolve_serve_address(&flag, settings.config.backtest_addr.as_deref());

    // The served keys (decision 0288 item 8): NO shared pair is read any more — a person is admitted by the
    // access list over this root's settings directory alone, the list the data daemon admits by too
    // (`crate::backtesthub_server`'s `access_list`). ALWAYS keyed (below): an empty list, or no located store,
    // admits nobody.
    //
    // ⚠ A store that EXISTS and cannot be READ is NOT "no credentials". The first is a permissions
    // bug; the second is the ordinary unconfigured state. They must never look the same to an operator —
    // and on a NON-LOOPBACK bind the difference decides whether this process starts at all, so the store
    // is still PROBED (a read admitting no name) and the two refusals below say which case they are.
    //
    // The store is opened out of this root's one sweep (`vike_secrets::open_credential_store`),
    // and a `VIKE_NO_LIVE_KEYS` word it cannot read refuses the daemon, exit 78, before anything
    // binds.
    let credential_store = match vike_secrets::open_credential_store(vars) {
        Ok(store) => store,
        Err(e) => {
            eprintln!("backtesthub --addr: {e}");
            return ExitCode::from(vike_utility::cli::OPERATOR_NEEDED_EXIT);
        }
    };
    let mut store_unreadable = false;
    match credential_store.hub_keys(|_| false) {
        Ok(resolved) => {
            if let Some(w) = &resolved.warning {
                eprintln!("backtesthub --addr: {w}");
            }
        }
        Err(e) => {
            eprintln!(
                "backtesthub --addr: credential store PRESENT but UNREADABLE ({e}) — the access list \
                     cannot be read. On a LOOPBACK bind this server admits nobody; on a NON-LOOPBACK \
                     one it will REFUSE TO START. Either way the cause is that store: fix its \
                     permissions and restart"
            );
            store_unreadable = true;
        }
    }
    // This server ALWAYS authenticates (decision 0288 item 8, the owner: never cut security), as the data daemon
    // whose list it shares: an EMPTY list admits nobody and says "no device registered"; with no located store there
    // is no list to register in, so nobody is admitted. A public bind is the bind guard's own question below.
    let access = match credential_store.location() {
        Some(at) => crate::backtesthub_server::access_list(at.settings_dir().to_path_buf()),
        None => vike_hub_protocol::access::AccessList::without_store(),
    };
    let keys =
        Some(vike_hub_protocol::auth::HubKeys::new(Vec::new(), Vec::new()).with_access(access));

    // Classify the bind BEFORE the store opens or the listener binds — the same guard, from the
    // same function, the data daemon runs (`vike_datahub_client::bind`). It matters at least as
    // much here: what a key-less non-loopback bind would expose on THIS daemon is a Rhai compiler
    // running source the client supplied.
    //
    // The opt-in is the data daemon's own row, `flags.datahub_allow_public_bind`, on purpose: it
    // consents to "this box's hub protocol may be reached off-box", which is one posture decision
    // for one box, and a second row would let an operator believe they had answered it while the
    // other daemon still refused. Read exactly as the data daemon's `check_bind` reads it.
    let allow_public = settings.flags.datahub_allow_public_bind;
    let resolved_addresses: Vec<std::net::SocketAddr> =
        addr.to_socket_addrs().map(|it| it.collect()).unwrap_or_default();
    // Always keyed now, but a list that cannot be READ authenticates nobody: the guard counts it as no keys, so a
    // public bind still refuses, the arm below naming the cause.
    let auth = if store_unreadable { ServerAuth::Keyless } else { ServerAuth::of(keys.as_ref()) };
    match bind_decision(&resolved_addresses, allow_public, auth) {
        BindDecision::Proceed => {}
        BindDecision::ProceedExposed(exposed) => {
            eprintln!(
                "backtesthub --addr: binding a NON-LOOPBACK address {exposed} \
                 (flags.datahub_allow_public_bind). Hub keys ARE configured, so every connection \
                 must authenticate — but the handshake is PLAINTEXT and authenticates the \
                 CONNECTION, not each frame, so keep an SSH tunnel or a VPN in front of it"
            );
        }
        BindDecision::RefuseUnauthenticated(exposed) if store_unreadable => {
            eprintln!(
                "backtesthub --addr: {exposed} is NOT loopback and this server could not READ its \
                 credential store, so it has no hub keys to authenticate with — refusing to \
                 start. This is NOT a missing configuration: the store is present and its keys may \
                 well be correct. Fix the file's permissions (`vike-cli secrets path` prints it) \
                 and restart"
            );
            return ExitCode::from(vike_utility::cli::OPERATOR_NEEDED_EXIT);
        }
        BindDecision::RefuseUnauthenticated(exposed) => {
            eprintln!(
                "backtesthub --addr: {exposed} is NOT loopback and this server has NO hub keys — \
                 refusing to start. flags.datahub_allow_public_bind consents to being REACHABLE; \
                 it is not consent to compile and run RHAI THE CLIENT SUPPLIES, unauthenticated, \
                 for anyone who can open a socket. Either register a device on this box's access \
                 list (`vike-cli backend mint-missing`), or put the address back on 127.0.0.1 and \
                 reach it with `ssh -L`"
            );
            return ExitCode::from(vike_utility::cli::OPERATOR_NEEDED_EXIT);
        }
        BindDecision::Refuse(exposed) => {
            eprintln!(
                "backtesthub --addr: {exposed} is NOT loopback — refusing to start. This server is \
                 meant to be reached over an SSH tunnel (its handshake is plaintext even when node \
                 keys ARE set). If this box genuinely must listen on a trusted network, run \
                 `vike-cli config set flags.datahub_allow_public_bind true` — and set the node \
                 keys first, so what is exposed is authenticated"
            );
            return ExitCode::from(vike_utility::cli::OPERATOR_NEEDED_EXIT);
        }
    }

    // ---- WHERE THE HISTORY COMES FROM -----------------------------------------------------------
    //
    // ⚠ **This daemon used to OPEN the store, and that was the second reader
    // `docs/decisions/0084-only-the-datahub-touches-the-store.md` forbids.** The comment that stood
    // here argued the opposite and was right at the time: "compute-to-data means the profile
    // crosses the wire and the history does not, which only holds if this process opens the same
    // root the data daemon serves". It did open the same root — on the deployed box both daemons
    // point at one directory — which is exactly the arrangement 0084 rules out: two processes with
    // the files open, one wire that is therefore optional, and a verb set that drifts because the
    // consumer who needs it can go around.
    //
    // So the DEFAULT is now the wire. The dial is loopback on the deployed box (the data daemon is
    // the same machine), so what this costs is a socket hop, and what it buys is that the datahub
    // is the only thing holding the files.
    //
    // ⚠ **The store root does not choose.** The `config.store_root` row is the DATAHUB's; this
    // daemon does not consult it to decide where history comes from — leaving it in charge would
    // keep the daemon local and prove nothing where it matters.
    //
    // ⚠ **The availability coupling is REAL and is the point, not a side effect**: a datahub outage
    // takes every compute run with it. That is what one reader means.
    //
    // ⚠ **`--store` on the line was this daemon's local escape until 2026-09-25 and is REFUSED now**,
    // by name, because the owner closed the local READ door: "the documented way to compute against
    // local files when the data daemon is down" is now to START a data daemon on those files, which
    // needs no keys and binds loopback only. VERIFIED before the cut: `deploy/vike-backtesthub.service`
    // runs `backtesthub --addr` with no `--store`, so the deployed daemon reads over the wire already
    // and starts exactly as before.
    if run.store.is_some() {
        eprintln!("{}", store_flag_removed("backtesthub --addr"));
        return ExitCode::FAILURE;
    }
    let route = history_route(settings.config.datahub_addr.as_deref());
    // The dial signs as THIS daemon's own device, `server/backtesthub` (role `observe`, minted by `backend
    // mint-missing`; decision 0288 item 8), never with the SERVED keys, which hold no pair to sign with. The bins'
    // warn-and-continue read (`crate::binutil::backtest_device_key`): the served side's stricter disposition is the
    // bind guard's above. `with_keys` when a device key resolved, `new` only when none did.
    let dial = vike_datahub_client::route::routed_hub_keys(
        crate::binutil::backtest_device_key(&credential_store),
        route.hub(),
    );
    let datahub = std::sync::Arc::new(match dial {
        Some(k) => vike_datahub_client::HistoryStoreDatahub::with_keys(route.hub(), k),
        None => vike_datahub_client::HistoryStoreDatahub::new(route.hub()),
    });
    let store: std::sync::Arc<dyn vike_data::HistoryStore + Send + Sync> = datahub.clone();
    // What an identical run is answered from a saved run WITH (decision 0078's amendment of 2026-11-20): the program
    // this daemon's sandboxed children run, hashed now, and the datahub as a FRESH dial's `Welcome` states it, asked by
    // every request that may be kept (its build; an older datahub's features), never off a pooled connection.
    let describe_datahub: std::sync::Arc<dyn Fn() -> Option<String> + Send + Sync> =
        std::sync::Arc::new(move || {
            let features = datahub.dialled_features().ok()?;
            Some(crate::backtesthub_server::answer_from_saved::datahub_identity(&features))
        });
    let answers_from_saved =
        crate::backtesthub_server::answer_from_saved::AnswersFromSaved::of_running_program(Some(
            describe_datahub,
        ));
    // The bar and tick cache (`crate::bar_cache`) in front of that one handle, so every run and the sandboxed
    // child's relay read through it; `config.backtest_bar_cache_mib` sizes its bars and
    // `config.backtest_tick_cache_mib` its ticks, 0 is none of that family.
    let bar_cache_mib =
        settings.config.backtest_bar_cache_mib.unwrap_or(DEFAULT_BACKTEST_BAR_CACHE_MIB);
    let tick_cache_mib =
        settings.config.backtest_tick_cache_mib.unwrap_or(DEFAULT_BACKTEST_TICK_CACHE_MIB);
    let store: std::sync::Arc<dyn vike_data::HistoryStore + Send + Sync> =
        if bar_cache_mib == 0 && tick_cache_mib == 0 {
            store
        } else {
            let clock = Box::new(vike_model::ClockLive);
            std::sync::Arc::new(crate::bar_cache::CachedHistoryStore::new(
                store,
                bar_cache_mib,
                tick_cache_mib,
                clock,
            ))
        };
    let listener = match TcpListener::bind(&addr) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("backtesthub --addr: failed to bind {addr}: {e}");
            return ExitCode::FAILURE;
        }
    };
    // ⚠ The route is DISCLOSED, and it names the address rather than a path on the remote arm: the
    // resolved root is the SERVER's, and a directory printed client-side would be a guess about
    // another box's filesystem. `crates/vike-cli/src/cmd/data.rs`'s remote arm withholds it for
    // the same reason.
    eprintln!(
        "backtesthub --addr: listening on {addr}, history over the wire from {}, bar cache {bar_cache_mib} MiB, \
         tick cache {tick_cache_mib} MiB",
        route.label()
    );

    // ⚠ THE STUDY'S TWO DIRECTORIES ARE RESOLVED HERE, from THIS daemon's own walk. The composition root named
    // nothing (`crates/vike-ops/tests/container_deploy/multicall_gate.rs`'s
    // `the_dispatcher_starts_nothing` forbids it a `state_path::` call at all), so the two paths
    // the study runner reads are resolved on the rung that already resolved this daemon's settings —
    // one walk, one answer. Both are the DAEMON's configuration and neither is a wire field: a
    // client may not steer a filesystem it cannot see, which is what `vike-cli research study`
    // refuses `--store` and `--lightgbm` by name for.
    //
    // ⚠ An ABSENT trainer is a WORKING state, not an error: the study refuses BY NAME
    // (`vike_user_research::StudyError::NoLearner`) and a run made with no learner is a different
    // experiment rather than a failed one. So a box with no `bin/lightgbm/` still serves
    // non-fitting studies.
    let study = serving_study(vars, settings_override, &settings, build, answers_from_saved);
    // The saved runs an identical request may be answered from, read into memory now so no request pays for the scan.
    let keyed = crate::backtesthub_server::answer_from_saved::warm(&study.runs_root);
    tracing::info!("backtesthub --addr: {keyed} saved run(s) may answer an identical request");

    // The NAMED-RUN lane (`docs/decisions/0064-a-named-run-carries-no-source.md`'s decision 8),
    // resolved HERE out of this daemon's own command line rather than inside the server.
    // ⚠ `NamedRunLane::from_args` scans for the exact token `--named-run`; `clap` has read the line
    // already, so the lane is armed from the declared switch (`the_named_run_switch_is_the_lanes_flag`
    // holds the field's spelling to `NAMED_RUN_FLAG`).
    let named_run = if run.named_run {
        crate::named_run::NamedRunLane::ARMED
    } else {
        crate::named_run::NamedRunLane::DISARMED
    };
    if named_run.armed() {
        // Said ONCE at startup, beside the auth verdict, because arming this lane is the act that
        // lets a read-only credential spend this box's CPU — and publishes the operator's own
        // compiled-in strategy names. An operator who did not mean to should see it in the log.
        tracing::info!(
            "backtesthub --addr: the NAMED-RUN lane is ARMED ({}) — an OBSERVE-scope peer may run \
             one strategy from this daemon's own roster over one bounded window, and may enumerate \
             that roster. It carries no source and writes nothing \
             (docs/decisions/0064-a-named-run-carries-no-source.md)",
            crate::named_run::NAMED_RUN_FLAG
        );
    }

    // The idle read timeout of every connection: the `config.node_idle_read_timeout_ms` row this
    // daemon's one settings load resolved, `SERVER_IDLE_READ_TIMEOUT` when the box wrote none (the data
    // daemon reads the same row for its own connections). The loader refused a row outside its range
    // before this point, so it is handed over as it is.
    let idle_read_timeout = vike_hub_protocol::limits::server_idle_read_timeout(
        settings.config.node_idle_read_timeout_ms,
    );
    match crate::backtesthub_server::serve_authed(
        listener,
        store,
        study,
        keys,
        named_run,
        idle_read_timeout,
    ) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("backtesthub --addr: serve loop ended with an error: {e}");
            ExitCode::FAILURE
        }
    }
}

/// **The study directories and the answer-from-saved identity a serving daemon runs with**: what [`run_serve`] builds
/// once its settings are loaded, from THIS process's working directory, `vars` and `settings_override` (the walk that
/// found its settings: `VIKE_SETTINGS_DIR`), `build` its commit and `answers_from_saved` what it answers identical requests from its saved runs with
/// (`crate::backtesthub_server::answer_from_saved::AnswersFromSaved`). `pub(crate)`: `crate::test_support` serves
/// over the very same construction.
pub(crate) fn serving_study(
    vars: &std::collections::HashMap<String, String>,
    settings_override: Option<&str>,
    settings: &vike_config::Settings,
    build: Option<&str>,
    answers_from_saved: Option<crate::backtesthub_server::answer_from_saved::AnswersFromSaved>,
) -> crate::backtesthub_server::StudyDirs {
    let cwd = std::env::current_dir().ok();
    let runs_root = cwd
        .as_deref()
        .and_then(|c| {
            vike_paths::state_path::user_runs_dir_from(
                vars.get("VIKE_USER_DATA_DIR").map(String::as_str),
                c,
            )
        })
        .unwrap_or_else(|| PathBuf::from("user_data").join("runs"));
    let trainer = cwd
        .as_deref()
        .and_then(|c| crate::backtesthub_server::installed_trainer(settings_override, c));
    let neural_trainer = cwd
        .as_deref()
        .and_then(|c| crate::backtesthub_server::installed_neural_trainer(settings_override, c));
    // The strategy FILES every profile names, beside this daemon's settings (the walk that
    // found them, under the same override); none is built in this process.
    let settings_dir = cwd
        .as_deref()
        .and_then(|c| vike_paths::state_path::project_settings_dir_from(settings_override, c));
    let strategies_dir = vike_paths::state_path::strategies_dir_beside(settings_dir.as_deref())
        .unwrap_or_else(|| PathBuf::from(vike_paths::state_path::PROJECT_STRATEGIES_DIR));
    // The project those settings sit in, under which the strategy SDKs this daemon serves the Studio
    // are read (`crate::sdk_source`): the same walk, its parent, never an empty one.
    let project_root = settings_dir
        .as_deref()
        .and_then(std::path::Path::parent)
        .filter(|p| !p.as_os_str().is_empty())
        .map(std::path::Path::to_path_buf);
    // The saved-run retention (`config.saved_runs_per_strategy`), OFF without the row. It reads the marks
    // beside the runs root and the alert rules in this project's state directory, whose drift rules name a
    // reference run by id.
    let run_retention = settings.config.saved_runs_per_strategy.map(|keep| {
        let alerts = cwd
            .as_deref()
            .and_then(|c| vike_paths::state_path::project_state_dir_from(settings_override, c));
        vike_runs::RunRetention {
            keep_per_strategy: usize::try_from(keep).unwrap_or(usize::MAX),
            marks_root: runs_root.with_file_name(vike_paths::state_path::MARKS_SUBDIR),
            referencing_documents: alerts
                .map(|state| state.join(vike_paths::state_path::ALERTS_FILE))
                .into_iter()
                .collect(),
        }
    });
    crate::backtesthub_server::StudyDirs {
        runs_root,
        learner_bin: trainer,
        neural_trainer_bin: neural_trainer,
        strategies: crate::studio_run::StrategyFiles::hub(strategies_dir),
        save_backtest_runs: true,
        run_retention,
        // The optional writers' free-space floor (`config.optional_writes_min_free_mib`): no save fills the
        // disk the node's journal shares.
        disk_floor: Some(vike_boot::disk_floor::DiskFloor::of(&settings.config)),
        build: build.map(str::to_string),
        answers_from_saved,
        project_root,
    }
}
