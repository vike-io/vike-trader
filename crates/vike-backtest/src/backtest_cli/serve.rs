//! `backtest --addr`: become the COMPUTE daemon (settings, node keys, the bind decision, serve).

use std::path::PathBuf;
use std::process::ExitCode;

use vike_analytics::binutil::{arg, has_flag};
use vike_datahub_client::flag_vocab::store_flag_removed;
use vike_datahub_client::route::history_route;

use super::args::AddrFlag;

/// The `--addr` value LADDER, mirroring `datahub`'s exactly: an explicit flag value, then
/// `VIKE_BACKTEST_ADDR`, then `config.backtest_addr`, then `vike_config::DEFAULT_BACKTEST_ADDR`.
///
/// ⚠ The env rung is not read here, and that is the point. `vike_config::Config::apply_env` folds
/// `VIKE_BACKTEST_ADDR` OVER the file layer before this function sees it, so `configured` already
/// carries whichever of the two won — which keeps this workspace's "one loader decides precedence"
/// property intact and stops a second, subtly different ladder existing inside a binary. Pure, so
/// the ladder is unit-testable without a settings directory and without an environment.
fn resolve_serve_addr(flag: &AddrFlag, configured: Option<&str>) -> String {
    match flag {
        AddrFlag::Explicit(v) => v.clone(),
        _ => configured
            .map(str::to_string)
            .unwrap_or_else(|| vike_config::DEFAULT_BACKTEST_ADDR.to_string()),
    }
}

/// This binary's settings — the DATABASE first, the files only on a box that has none.
///
/// ⚠ **Owner ruling, 2026-09-23: the datahub address comes from the settings DATABASE** — *"it had
/// to get address from sqlite, no files; all settings have to live in sqlite"*. That is what the
/// `StoreLayer` below already does, and what a narrower ladder would have broken: reading
/// `$VIKE_DATAHUB_ADDR` and falling through to the compiled default would have left a configured
/// address unread on exactly the boxes that were migrated on 2026-09-14
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
/// `who` prefixes every line, so each arm still says which invocation is talking.
pub(super) fn load_backtest_settings(
    vars: &std::collections::HashMap<String, String>,
    who: &str,
) -> Result<vike_config::Settings, ExitCode> {
    // ⚠ `VIKE_SETTINGS_DIR` names the directory outright and beats the walk, and it is spelled as a
    // LITERAL rather than through `vike_config`'s constant: the settings registry's map-lookup
    // sweep resolves constants CRATE-WIDE, so importing one would make this read invisible to
    // `crates/vike-ops/tests/settings_secrets/settings_registry.rs`.
    let settings_override = vars.get("VIKE_SETTINGS_DIR").map(String::as_str);
    let settings_dir = std::env::current_dir().ok().and_then(|cwd| {
        vike_model::paths::state_path::project_settings_dir_from(settings_override, &cwd)
    });
    let store = settings_dir.as_deref().map(vike_secrets::read_settings_in);
    let mut store_refusal = String::new();
    let source = vike_config::StoreLayer::of(store.as_ref(), &mut store_refusal);
    let settings = match vike_config::load_with_source(
        settings_dir.as_deref(),
        source,
        vars,
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

/// Become the COMPUTE daemon: resolve the address, load the node keys, classify the bind, open the
/// store and serve the seven verbs forever.
///
/// The composition order is the data daemon's, deliberately (`vike_datahub::datahub_cli`'s `run`):
/// settings, then keys, then the BIND DECISION — before the store is opened and before a listener
/// exists — then the listener, then `serve_authed`. A refused configuration must cost nothing and
/// touch nothing.
///
/// ⚠ **A refusal EXITS 2** rather than degrading, and that is this invocation's own rule rather than
/// the workspace's: serving is the only thing `--addr` was asked to do, so "do not bind" and "do not
/// run" are the same decision. Exit 2 is the refused-configuration family this binary already uses
/// for a rejected argument — not `FAILURE`, because nothing was tried and failed.
///
/// `studio` is the injected [`crate::compute_server::StudioRunTable`] the composition root handed
/// down; see this function's mount comment for why a `None` here is a build fact rather than an
/// omission.
pub(super) fn run_serve(
    vars: &std::collections::HashMap<String, String>,
    args: &[String],
    flag: AddrFlag,
    studio: Option<crate::compute_server::StudioRunTable>,
    study: Option<crate::compute_server::StudyRunFactory>,
) -> ExitCode {
    use std::net::{TcpListener, ToSocketAddrs};

    use vike_datahub_client::bind::{BindDecision, ServerAuth, bind_decision};

    // ⚠ `VIKE_SETTINGS_DIR` names the directory outright and beats the walk — the ONE fact this
    // root pulls out of the swept map to find its project, and spelled as a LITERAL for the reason
    // the indicator read above gives (the settings registry's map-lookup sweep resolves constants
    // crate-wide, so importing the constant would make the read invisible to the gate).
    let settings_override = vars.get("VIKE_SETTINGS_DIR").map(String::as_str);
    // ⚠ **The walk and the load moved into `load_backtest_settings`, which the RUN path calls too.**
    // They used to be spelled out right here, which made the `--addr` daemon the ONLY arm of this
    // binary that could see a configured `config.datahub_addr`; the run path now needs the same
    // key to know where its history comes from. ONE function walks for the settings directory, so
    // there is still one place that does it — but it is CALLED from each arm at that arm's own
    // moment, because they are not the same moment: this one is before a socket opens, and the run
    // path's is after the argv triage that refuses a command line without performing any I/O. The
    // two arms are mutually exclusive, so at most one call ever runs. That function's doc carries
    // what this comment used to: why the walk happens, why a malformed store is fatal, and which
    // gate cannot see any of it.
    let settings = match load_backtest_settings(vars, "backtest --addr") {
        Ok(s) => s,
        Err(code) => return code,
    };
    // **`preferences.sweep_threads` reaches the sweep pool from HERE** — this arm INSTALLS it even
    // though it no longer LOADS it, and the halves are deliberately apart: installing is
    // process-wide and once, so it belongs where a daemon is about to serve, while the load now
    // serves both arms. ⚠ The RUN path therefore still leaves this key inert — unchanged by the
    // hoist, and the state `0057`s Phase 0 already names as owed; wiring it would be a parallelism
    // change riding a routing PR. The value is the loader's resolved answer
    // (`env > store > file > default`), and
    // `crate::harness::install_sweep_threads` is the process-wide handle the pool reads at
    // construction; without this call the FILE key would be inert on this daemon and only
    // `VIKE_SWEEP_THREADS` would bite, which is the state
    // `docs/decisions/0057-the-seven-settings-files-answered-one-at-a-time.md`'s Phase 0 names as
    // owed. Before the listener binds, so the first served sweep already has it.
    crate::harness::install_sweep_threads(settings.preferences.sweep_threads);
    let addr = resolve_serve_addr(&flag, settings.config.backtest_addr.as_deref());

    // The node keys, from the NODE store (`node.env`, falling back to the venue-key file only while
    // a box has not migrated) — the same pair, under the same domain separator, the data daemon
    // authenticates with (`crate::compute_server`'s `serve_authed` carries why one pair rather than
    // two). `vike_secrets`, not `vike_bridge_core::credentials`: the canonical wrapper drags the
    // ureq/tungstenite/rustls transport stack, and nothing here needs a transport.
    //
    // ⚠ A store that EXISTS and cannot be READ is NOT "no credentials". The first is a permissions
    // bug that silently drops this server to unauthenticated; the second is the ordinary
    // unconfigured state. They must never look the same to an operator — and on a NON-LOOPBACK bind
    // the difference decides whether this process starts at all, so the two refusals below say
    // which case they are.
    //
    // ⚠ The scope is the DATAHUB family (this daemon authenticates with that pair), not all four
    // platform names: `resolve_node_keys` hands back the names the predicate admits and no others.
    let mut store_unreadable = false;
    let credentials: std::collections::HashMap<String, String> =
        match vike_secrets::resolve_node_keys(
            settings_override,
            vike_model::credential_keys::is_datahub_node_key,
        ) {
            Ok(resolved) => {
                if let Some(w) = &resolved.warning {
                    eprintln!("backtest --addr: {w}");
                }
                if let Some(w) = &resolved.legacy {
                    eprintln!("backtest --addr: {w}");
                }
                // A node-key FILE on a box with no settings database: NOT read, said out loud.
                if let Some(u) = &resolved.unread {
                    eprintln!("backtest --addr: {u}");
                }
                resolved.secrets.into_map()
            }
            Err(e) => {
                eprintln!(
                    "backtest --addr: credential store PRESENT but UNREADABLE ({e}) — any \
                     configured node keys were NOT loaded. On a LOOPBACK bind this server is about \
                     to serve UNAUTHENTICATED behind the bind guard alone; on a NON-LOOPBACK one it \
                     will REFUSE TO START. Either way the cause is that file: fix its permissions \
                     and restart"
                );
                store_unreadable = true;
                Default::default()
            }
        };
    let keys = vike_node_proto::auth::node_keys_from_vars(&credentials);

    // Classify the bind BEFORE the store opens or the listener binds — the same guard, from the
    // same function, the data daemon runs (`vike_datahub_client::bind`). It matters at least as
    // much here: what a key-less non-loopback bind would expose on THIS daemon is a Rhai compiler
    // running source the client supplied.
    //
    // The opt-in keeps the datahub's spelling, `VIKE_DATAHUB_ALLOW_PUBLIC_BIND=1`, on purpose: it
    // consents to "this box's node protocol may be reached off-box", which is one posture decision
    // for one box, and a second variable would let an operator believe they had answered it while
    // the other daemon still refused.
    let allow_public = vars.get("VIKE_DATAHUB_ALLOW_PUBLIC_BIND").map(String::as_str) == Some("1");
    let resolved_addrs: Vec<std::net::SocketAddr> =
        addr.to_socket_addrs().map(|it| it.collect()).unwrap_or_default();
    match bind_decision(&resolved_addrs, allow_public, ServerAuth::of(keys.as_ref())) {
        BindDecision::Proceed => {}
        BindDecision::ProceedExposed(exposed) => {
            eprintln!(
                "backtest --addr: binding a NON-LOOPBACK address {exposed} \
                 (VIKE_DATAHUB_ALLOW_PUBLIC_BIND=1). Node keys ARE configured, so every connection \
                 must authenticate — but the handshake is PLAINTEXT and authenticates the \
                 CONNECTION, not each frame, so keep an SSH tunnel or a VPN in front of it"
            );
        }
        BindDecision::RefuseUnauthenticated(exposed) if store_unreadable => {
            eprintln!(
                "backtest --addr: {exposed} is NOT loopback and this server could not READ its \
                 credential store, so it has no node keys to authenticate with — refusing to \
                 start. This is NOT a missing configuration: the store is present and its keys may \
                 well be correct. Fix the file's permissions (`vike-cli secrets path` prints it) \
                 and restart"
            );
            return ExitCode::from(2);
        }
        BindDecision::RefuseUnauthenticated(exposed) => {
            eprintln!(
                "backtest --addr: {exposed} is NOT loopback and this server has NO node keys — \
                 refusing to start. VIKE_DATAHUB_ALLOW_PUBLIC_BIND=1 consents to being REACHABLE; \
                 it is not consent to compile and run RHAI THE CLIENT SUPPLIES, unauthenticated, \
                 for anyone who can open a socket. Either set VIKE_DATAHUB_OBSERVE_KEY and \
                 VIKE_DATAHUB_CONTROL_KEY in the credential store, or put the address back on \
                 127.0.0.1 and reach it with `ssh -L`"
            );
            return ExitCode::from(2);
        }
        BindDecision::Refuse(exposed) => {
            eprintln!(
                "backtest --addr: {exposed} is NOT loopback — refusing to start. This server is \
                 meant to be reached over an SSH tunnel (its handshake is plaintext even when node \
                 keys ARE set). If this box genuinely must listen on a trusted network, set \
                 VIKE_DATAHUB_ALLOW_PUBLIC_BIND=1 — and set the node keys first, so what is \
                 exposed is authenticated"
            );
            return ExitCode::from(2);
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
    // ⚠ **The ENVIRONMENT does not choose.** `VIKE_HIST_STORE` is the DATAHUB's variable; this
    // daemon does not consult it to decide where history comes from. That was a deliberate break
    // when 0084 landed: the deployed unit set that variable, so leaving it in charge would have
    // kept the live daemon local and proved nothing where it matters. The unit dropped the line on
    // 2026-09-26, once nothing it runs read it — `deploy/vike-backtest.service`'s history block
    // carries the measurement.
    //
    // ⚠ **The availability coupling is REAL and is the point, not a side effect**: a datahub outage
    // takes every compute run with it. That is what one reader means.
    //
    // ⚠ **`--store` on the line was this daemon's local escape until 2026-09-25 and is REFUSED now**,
    // by name, because the owner closed the local READ door: "the documented way to compute against
    // local files when the data daemon is down" is now to START a data daemon on those files, which
    // needs no keys and binds loopback only. VERIFIED before the cut: `deploy/vike-backtest.service`
    // runs `backtest --addr` with no `--store`, so the deployed daemon reads over the wire already
    // and starts exactly as before.
    if has_flag(args, "--store") || arg(args, "--store").is_some() {
        eprintln!("{}", store_flag_removed("backtest --addr"));
        return ExitCode::FAILURE;
    }
    let route = history_route(settings.config.datahub_addr.as_deref());
    // `with_keys` whenever a pair resolved, `new` only when none did — `with_keys` degrades to a
    // plain unauthenticated connect against a KEY-LESS server, so one spelling works against the
    // keyed production datahub and a bare local one. This daemon resolves `keys` itself above,
    // with a STRICTER disposition than `open_routed_history` (an unreadable store refuses the bind
    // here rather than warning), so it builds the client itself instead of calling that.
    let store: std::sync::Arc<dyn vike_data::HistStore + Send + Sync> = match keys.clone() {
        Some(k) => {
            std::sync::Arc::new(vike_datahub_client::RemoteHistStore::with_keys(route.hub(), k))
        }
        None => std::sync::Arc::new(vike_datahub_client::RemoteHistStore::new(route.hub())),
    };
    let listener = match TcpListener::bind(&addr) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("backtest --addr: failed to bind {addr}: {e}");
            return ExitCode::FAILURE;
        }
    };
    // ⚠ The route is DISCLOSED, and it names the address rather than a path on the remote arm: the
    // resolved root is the SERVER's, and a directory printed client-side would be a guess about
    // another box's filesystem. `crates/vike-cli/src/cmd/data.rs`'s remote arm withholds it for
    // the same reason.
    eprintln!("backtest --addr: listening on {addr}, history over the wire from {}", route.label());

    // ⚠ `studio` is `None` on a bare `cargo run -p vike-backtest --bin backtest -- --addr`, and
    // `Some` under `vike-backend backtest --addr`. That is a LAYER fact, not an omission: the three
    // Studio verbs run `vike_studio_core`'s slice runners, and that crate sits ABOVE this one in the
    // layer graph, so only a composition root that can name both can hand them down.
    // `crates/vike/src/main.rs`'s `backtest_main` does. A daemon without the table advertises none
    // of the three and refuses them by name — the `FEATURE_BACKFILL` shape, applied to a mount
    // rather than to a feature.
    // ⚠ THE STUDY RUNNER IS BUILT HERE, from THIS daemon's own walk. The composition root named
    // the constructor and resolved nothing (`crates/vike-ops/tests/container_deploy/multicall_gate.rs`'s
    // `the_dispatcher_starts_nothing` forbids it a `state_path::` call at all), so the two paths
    // the runner captures are resolved on the rung that already resolved this daemon's settings —
    // one walk, one answer. Both are the DAEMON's configuration and neither is a wire field: a
    // client may not steer a filesystem it cannot see, which is what `vike-cli research study`
    // refuses `--store` and `--lightgbm` by name for.
    //
    // ⚠ An ABSENT trainer is a WORKING state, not an error: the study refuses BY NAME
    // (`vike_user_research::StudyError::NoLearner`) and a run made with no learner is a different
    // experiment rather than a failed one. So a box with no `bin/lightgbm/` still serves
    // non-fitting studies.
    let study = study.map(|build| {
        let cwd = std::env::current_dir().ok();
        let runs_root = cwd
            .as_deref()
            .and_then(|c| {
                vike_model::paths::state_path::user_runs_dir_from(
                    vars.get("VIKE_USER_DATA_DIR").map(String::as_str),
                    c,
                )
            })
            .unwrap_or_else(|| PathBuf::from("user_data").join("runs"));
        let trainer = cwd
            .as_deref()
            .and_then(|c| vike_model::paths::state_path::project_bin_dir_from(settings_override, c))
            .map(|bin| {
                bin.join("lightgbm").join(if cfg!(windows) { "lightgbm.exe" } else { "lightgbm" })
            })
            .filter(|p| p.is_file());
        build(runs_root, trainer)
    });

    // The NAMED-RUN lane (`docs/decisions/0064-a-named-run-carries-no-source.md`'s decision 8),
    // resolved HERE out of the sweep this function already owns rather than inside the server.
    // Two reasons, and the second is a gate: a library that reads the process environment is
    // configuration its caller can neither see nor override, and
    // `crates/vike-ops/tests/settings_secrets/settings_registry.rs`'s `LIBRARY_PIN` is a RATCHET that may only
    // shrink, so a new `Layer::Library` row would redden CI rather than merely being untidy.
    let named_run = crate::named_run::NamedRunLane::from_vars(vars);
    if named_run.armed() {
        // Said ONCE at startup, beside the auth verdict, because arming this lane is the act that
        // lets a read-only credential spend this box's CPU — and publishes the operator's own
        // compiled-in strategy names. An operator who did not mean to should see it in the log.
        tracing::info!(
            "backtest --addr: the NAMED-RUN lane is ARMED ({}=1) — an OBSERVE-scope peer may run \
             one strategy from this daemon's own roster over one bounded window, and may enumerate \
             that roster. It carries no source and writes nothing \
             (docs/decisions/0064-a-named-run-carries-no-source.md)",
            crate::named_run::NAMED_RUN_ENV
        );
    }

    match crate::compute_server::serve_authed(listener, store, studio, study, keys, named_run) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("backtest --addr: serve loop ended with an error: {e}");
            ExitCode::FAILURE
        }
    }
}
