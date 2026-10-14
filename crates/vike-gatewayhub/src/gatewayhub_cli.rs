//! `vike-gatewayhub` — the gateway daemon's command line, as a LIBRARY function.
//!
//! [`run`] takes the environment, the working directory and argv as parameters, exactly as
//! `vike_datahub::datahub_cli::run` does, so the standalone binary
//! (`crates/vike-gatewayhub/src/bin/vike-gatewayhub.rs`) and the `vike-backend` multicall reach the
//! same body. The environment and the working directory are parameters because a library that reads
//! `current_dir()` or `env::vars()` holds ambient state its caller cannot override, and the settings
//! registry cannot see the first at all; the binary collects each ONCE.
//!
//! # The run order IS the contract
//!
//! [`run_with`] is a SEQUENCE of named phases, each a function in this module's children, called in
//! the order below. The comment at each call says what depends on it.
//!
//! 1. **argv** (`args::short_circuit`). `--help`, `--version` and a usage error are answered here
//!    and EXIT: a command line that does not parse never starts the daemon, and `--help` must not
//!    open a store or bind a port.
//! 2. **the stop handlers**, armed before ANYTHING that can block or build a client. Until they run,
//!    SIGTERM has the OS default disposition and the process dies where it stands with no teardown.
//!    A handler only stores a bool, so installing one can never be too early and can very easily be
//!    too late. The outcome is carried as data and LOGGED at phase 4, once a subscriber exists.
//! 3. **the boot** (`boot::boot_root`): `vike_boot::boot` with this root's `BootSpec`. The startup
//!    sequence has ONE owner and a root may not re-spell it; every departure is an arm of the spec,
//!    argued there.
//! 4. **the log subscriber** (`vike_log::init`, prefix `vike-gatewayhub`), then the identity line, the
//!    handler outcome and the loader's own findings (the boot returns them as data).
//! 5. **Polymarket's egress** is DECLARED from the `venue.polymarket.*` rows
//!    (`declare_polymarket_egress`) before ANY venue client can be built. The bridge reads its
//!    egress from one process-wide cell and an undeclared process resolves the built-in default (a
//!    SOCKS proxy at 127.0.0.1:1080) IN SILENCE, so the declaration comes first and every client
//!    comes after it. `crates/vike-ops/tests/venues/polymarket_egress_declared_gate.rs` is the
//!    machine-checked form of that rule, and it reads THIS function's text: the declaration is a
//!    statement of [`run_with`]'s own body, under the `#[cfg]` that names exactly the features that
//!    link the bridge (`data`), and every venue client is built inside the `wire` hook below it. A
//!    build without the `data` feature links no bridge, so it declares nothing and has no client
//!    to configure.
//! 6. **the bind guard** (`boot::check_bind`): `config.gatewayhub_addr` must resolve to LOOPBACK
//!    addresses only, or the process exits 2. There is no allow-public flag.
//! 7. **the wire** (`wire::serve_wire`): the `wire` hook is handed everything above as a [`Ready`]
//!    and owns the rest of the process. This module does NOT bind the listener. The hook reads the
//!    hub keys (no keys, no start: exit 2), announces the OANDA lane, builds the venue lanes, binds
//!    exactly [`Ready::bind`] and serves until the stop flag, then drains; its module doc is the
//!    order inside it. A build without the `data` feature has no venue lane and the hook says so and
//!    exits 2.
//!
//! # Why `run_with` takes a hook
//!
//! The order is the thing under test, so the wire is a parameter and not a trait object or a cargo
//! feature: [`run`] passes the real hook, and a test passes a recording closure and asserts what had
//! happened before it was called, or the real server over scripted lanes
//! (`wire::serve_ready`) and watches it serve and stop. Plugging a different wire in changes one
//! argument of [`run`], not phases 1-6.
//!
//! # Settings this daemon reads (rows of the settings database, decision 0111)
//!
//! * `config.gatewayhub_addr` — the bind address; default
//!   [`vike_config::config::DEFAULT_GATEWAYHUB_ADDR`]. ⚠ A NON-LOOPBACK value is refused outright.
//! * `preferences.log_file_level` — the rolling file's level, read by the boot before the
//!   subscriber exists.
//! * every venue's `venue_setting` rows, read once and handed to [`Ready`] (and, for Polymarket's
//!   egress, to `declare_polymarket_egress`).
//!
//! No environment variable configures any of it; a retired one refuses startup naming its row.
//!
//! # Credentials
//!
//! None are read at boot (`Credentials::Deferred`, argued in `boot::boot_root`). The reads that
//! come later are SCOPED, through [`Ready::booted`]'s store, and a credential value reaches no log
//! line, reply or `Debug`: the wire hook reads the hub keys (`keys::gateway_keys`), makes ONE
//! announcing read of the OANDA practice token whose value is dropped before the startup line is
//! logged, and the OANDA lane itself reads the token once per CONNECTION that needs it.

use std::collections::{BTreeMap, HashMap};
use std::net::SocketAddr;
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;

use vike_boot::stop::{self, StopSignal};
use vike_config::config::DEFAULT_GATEWAYHUB_ADDR;
use vike_secrets::venue_setting::VenueSettings;

use crate::budget::ledger::Budget;
use crate::budget::mode::MODE;

mod args;
mod boot;
#[cfg(any(feature = "data", test))]
mod keys;
mod wire;

use self::args::short_circuit;
use self::boot::{boot_root, check_bind, venue_settings_of};

/// Everything phases 1-6 of [`run_with`] produced: what the wire hook is handed, and owns from
/// there on.
pub struct Ready {
    /// The boot's answers: the resolved settings, the ONE project walk's directories, and
    /// `Booted::store`, the credential store every later scoped read goes through.
    pub booted: vike_boot::Booted,
    /// The process's stop signal, with SIGTERM and SIGINT already pointed at it. The wire's accept
    /// loop and its teardown must observe it, and the teardown must fit the unit's
    /// `TimeoutStopSec`.
    pub stop: StopSignal,
    /// The resolved bind targets, ALL loopback (phase 6). Bind exactly these: re-resolving the
    /// string could answer differently and walk around the guard.
    pub bind: Vec<SocketAddr>,
    /// Every venue's `venue_setting` rows, from the ONE read of the table (empty for no settings
    /// directory, no database or no rows). The venue clients take their per-venue knobs from here.
    pub venue_settings: BTreeMap<String, VenueSettings>,
    /// The shadow request budget ([`crate::budget`]): the call sites count against it, and a status
    /// verb will serialise its snapshot.
    pub budget: Arc<Budget>,
}

/// The gateway daemon past `--help`: [`run_with`] with the real wire (`wire::serve_wire`).
///
/// This is the signature the multicall dispatcher and the standalone binary call
/// (`run(vars, cwd, args)`, `args` WITHOUT the program name).
pub fn run(vars: &HashMap<String, String>, cwd: Option<&Path>, args: &[String]) -> ExitCode {
    run_with(vars, cwd, args, wire::serve_wire)
}

/// **Declare Polymarket's egress into the bridge** (decision 0095) — the one call
/// `vike_polymarket::declare_from_rows(loaded)`, which owns the precedence and the ONE store-error
/// policy (an unreadable store is handled the same way by every root that declares).
///
/// `loaded` is the ONE read of the `venue_setting` table [`run_with`] makes (`None`: no settings
/// directory); [`venue_settings_of`] takes the venue clients' copy from the same read.
///
/// ⚠ Gated to match its one call site's own `#[cfg]` exactly (inside [`run_with`], under
/// `data`, the only feature of this crate that links the bridge): a looser gate would leave this
/// function uncalled, and therefore unused, in a build that has no bridge to declare into. The
/// gate behind that rule reads the call's `#[cfg]` against the manifest's features.
///
/// ⚠ Its place in [`run_with`] is the point: ABOVE the wire, which is where every venue client is
/// built.
#[cfg(feature = "data")]
fn declare_polymarket_egress(
    loaded: Option<Result<BTreeMap<String, VenueSettings>, vike_secrets::DbError>>,
) {
    vike_polymarket::declare_from_rows(loaded);
}

/// The gateway daemon past `--help` — **a SEQUENCE of named phases** (the module doc lists them),
/// then `wire`.
///
/// ⚠ **The ORDER is the contract**, and the comment at each call says what depends on it.
/// `wire` is called LAST, with everything the earlier phases produced, and its return value is the
/// process's exit status.
pub fn run_with(
    vars: &HashMap<String, String>,
    cwd: Option<&Path>,
    args: &[String],
    wire: impl FnOnce(Ready) -> ExitCode,
) -> ExitCode {
    // BEFORE the stop handlers, the log subscriber, the store and the listener: `--help` must not
    // open a store, and it certainly must not bind a port.
    short_circuit(args);

    // ⚠ THE STOP HANDLERS, FIRST. Every phase below can block (a store open takes locks, a bind can
    // fail slowly, a venue client dials), and until the handler is installed SIGTERM still has the
    // OS default disposition: the process dies where it stands with no teardown. The handler only
    // stores a bool, so this can never be too early. Its outcome is data, logged below once a
    // subscriber exists.
    let stop = StopSignal::new();
    let handlers = stop::install_handlers(&stop.flag());

    // The workspace's ONE startup sequence — `boot_root` is where every way this root departs from
    // it is named.
    let booted = match boot_root(vars, cwd) {
        Ok(b) => b,
        // A refusal must never become an unwrap in a daemon's entry point.
        Err(e) => {
            eprintln!("vike-gatewayhub: {e}");
            return ExitCode::from(2);
        }
    };

    // Hold the appender guards for the process lifetime (dropping flushes the non-blocking writer).
    // The log home comes off the boot's OWN project walk, which honours `$VIKE_SETTINGS_DIR`;
    // `file_level` is the `preferences.log_file_level` ROW (decision 0111 verdict 3).
    let _log_guards = vike_log::init(vike_log::LogConfig {
        file_prefix: "vike-gatewayhub".to_string(),
        file_level: booted.settings.preferences.log_file_level.clone(),
        project_dir: booted.log_home.clone(),
        ..Default::default()
    });

    // WHICH BINARY IS THIS — the first line in the log, and the same string `--version` prints.
    tracing::info!("{}", booted.identity_line);

    // The stop handlers' outcome, now that it can be said. The one case an operator MUST see is a
    // failed registration: the daemon runs, but a `systemctl stop` is back to killing it where it
    // stands, and nothing else would say so.
    match handlers {
        stop::HandlerOutcome::Installed => {
            tracing::info!("vike-gatewayhub: SIGTERM/SIGINT will stop gracefully");
        }
        stop::HandlerOutcome::Unsupported => {
            tracing::info!("vike-gatewayhub: no POSIX signals on this platform; stop with Ctrl-C");
        }
        stop::HandlerOutcome::Failed(e) => {
            tracing::error!(
                error = %e,
                "vike-gatewayhub: could NOT install the signal handler — a SIGTERM will kill this \
                 process where it stands, with no teardown"
            );
        }
    }

    // The loader's OWN non-fatal resolutions, emitted here because `vike_boot::boot` runs before a
    // subscriber exists and therefore returns them as DATA.
    for w in &booted.settings.warnings {
        tracing::warn!("{w}");
    }

    // Decision 0095: every venue's `venue_setting` rows, read ONCE from the boot's settings
    // directory. Polymarket's egress is DECLARED from that read before any client exists (every
    // venue client is built inside the wire hook, below); the venue clients take their copy of
    // the same read. A build that links no bridge (no `data` feature) has no client to configure
    // and declares nothing.
    let loaded_venue_settings =
        booted.settings_dir.as_deref().map(vike_secrets::venue_setting::load_venue_settings);
    let venue_settings = venue_settings_of(loaded_venue_settings.as_ref());
    #[cfg(feature = "data")]
    declare_polymarket_egress(loaded_venue_settings);

    // The bind guard — `boot::resolve_bind` carries the argument. The row, the compiled-in default
    // otherwise; ⚠ NOT `config.datahub_addr` or any other row: each address row is one plane's.
    // A refusal exits 2 before anything is bound or dialled.
    let bind_row = booted.settings.config.gatewayhub_addr.clone();
    let addr = bind_row.unwrap_or_else(|| DEFAULT_GATEWAYHUB_ADDR.to_string());
    let bind = match check_bind(&addr) {
        Ok(targets) => targets,
        Err(code) => return code,
    };
    tracing::info!(%addr, "vike-gatewayhub: config.gatewayhub_addr is loopback; this daemon serves this host only");

    // The request budget: shadow mode, refuses nothing and counts only what a lane spends (none does yet) (`crate::budget`).
    let budget = Arc::new(Budget::new());
    tracing::info!(mode = ?MODE, "vike-gatewayhub: request budget armed (shadow mode; refuses nothing)");

    wire(Ready { booted, stop, bind, venue_settings, budget })
}

#[path = "gatewayhub_cli_tests.rs"]
#[cfg(test)]
mod gatewayhub_cli_tests;
