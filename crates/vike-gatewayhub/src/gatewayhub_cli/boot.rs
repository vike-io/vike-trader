//! The boot phases: the boot itself, the venue settings read, the bind guard.

use std::collections::BTreeMap;
use std::net::{SocketAddr, ToSocketAddrs};
use std::process::ExitCode;

use vike_hub_proto::bind::{BindExposure, bind_exposure};
use vike_secrets::venue_setting::VenueSettings;

/// **Phase: the boot** — `vike_boot::boot` with this root's `BootSpec`, every arm of which names how
/// this daemon departs from the shared startup sequence and why. `run` calls it after argv is
/// parsed and the stop handlers are armed, and before the log subscriber exists, which is why the
/// loader's findings come back as DATA for `run` to log.
pub(super) fn boot_root(
    vars: &std::collections::HashMap<String, String>,
    cwd: Option<&std::path::Path>,
) -> Result<vike_boot::Booted, String> {
    // The workspace's ONE startup sequence. Every way this root departs from it is a named arm
    // below, which is the point: those departures used to be prose in a comment block and nothing
    // could see them.
    vike_boot::boot(&vike_boot::BootSpec {
        env: vars,
        cwd,
        identity: vike_boot::Identity {
            name: env!("CARGO_PKG_NAME"),
            version: env!("CARGO_PKG_VERSION"),
        },
        // ⚠ REFUSE, for the datahub's reason (decision 0095) and one of its own. Polymarket's egress
        // variables are in `REMOVED_ENV`, and this daemon is the root that now DECLARES the egress
        // from the `venue.polymarket.*` rows: ignoring a leftover `POLY_PROXY_ENABLED=false` would
        // put the bridge back on its default proxy in silence, which is the change the egress
        // contract exists to refuse. A stale variable stops this daemon too; the release's deploy
        // pre-flight names every such line before anything is installed.
        removed_env: vike_boot::RemovedEnv::Refuse,
        // ⚠ LOAD, and not `LoadAndRefuseUnsoundSeal`. That arm is for a root that can PLACE OR GATE
        // AN ORDER, because an unsound settings seal degrades the order-size ceiling to "no cap".
        // This daemon, in wave 1, places nothing: it fetches public data and holds one practice
        // credential for a history read, so there is no ceiling for a degraded seal to uncap, and
        // a daemon that will not start over a seal mark helps nobody (decision 0013). ⚠ **Wave 2
        // gives it the order path, and this arm is the first thing that wave re-argues.**
        //
        // It loads settings at all because it consumes two rows: `config.gatewayhub_addr` (its own
        // bind) and `preferences.log_file_level`, and because a settings tree that is present and
        // BROKEN is a startup failure at every root that loads, this one included. The deployed
        // unit carries an `ExecStartPre=vike-cli config check`, so on that box the refusal is met
        // before the process starts rather than as a restart loop.
        settings: vike_boot::SettingsLoad::Load,
        // ⚠ DEFERRED — the datahub's arm, for a stronger reason here. The departure is about TIMING
        // and SCOPE, not need: this root reads credentials, but never the whole map. Its reads are
        // scoped ones made later through `Booted::store` — the hub keys (their predicate is the
        // scope), and the OANDA practice token, one field of one account, once per request — so the
        // project walk stays the one this crate performed rather than a second one of the binary's.
        // `Credentials::Read` would materialise EVERY venue key on the box into this process at
        // every start, which is the opposite of what the one process that holds venue connections
        // should do with a map it does not need.
        credentials: vike_boot::Credentials::Deferred(
            "this daemon reads credentials by SCOPED reads through `Booted::store` after the boot \
             (its hub keys; one practice token, one field of one account, per request), never \
             the whole venue map, and through the boot's own project walk rather than a second \
             one of the binary's.",
        ),
        log_home: vike_boot::LogHome::UnderSettings,
        // ⚠ SKIP, argued: `vike_config::boot_lines` re-reads the settings to recover each row's
        // ORIGIN and renders the RISK CEILINGS, none of which this daemon reads or could act on in
        // wave 1. The one setting it consumes discloses ITSELF, at the line that accepts the bind
        // (`config.gatewayhub_addr`, whichever way it was resolved), so a whole-tree disclosure
        // would only describe ceilings this process cannot honour.
        disclosure: vike_boot::Disclosure::Skip(
            "the one setting this daemon consumes (`config.gatewayhub_addr`) discloses itself at \
             the bind-guard line. A whole-tree disclosure would describe the risk ceilings, which \
             this daemon neither reads nor could act on in wave 1.",
        ),
    })
}

/// **Phase: the venue settings** — the broker-side copy of the `venue_setting` rows, taken from
/// `run`'s ONE read of the table (`None`: no settings directory). Empty for no directory, no
/// database or no rows.
///
/// A store that will not open is logged HERE for what it costs the venue clients (every venue takes
/// its built-in defaults), and reads as empty. Polymarket's egress takes the same read through
/// `declare_polymarket_egress`, whose shared policy logs the egress half.
pub(super) fn venue_settings_of(
    loaded: Option<&Result<BTreeMap<String, VenueSettings>, vike_secrets::DbError>>,
) -> BTreeMap<String, VenueSettings> {
    match loaded {
        Some(Ok(all)) => all.clone(),
        Some(Err(e)) => {
            tracing::error!(
                error = %e,
                "the venue_setting table could not be read; every venue's market feed takes its \
                 built-in defaults (each venue's charter mark-stream default, the default socket \
                 batching)"
            );
            BTreeMap::new()
        }
        None => BTreeMap::new(),
    }
}

/// Why a configured address was refused. Carries the operator-facing text, never a value that is
/// not already in the row.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum BindRefusal {
    /// At least one resolved address is NOT loopback (a specific interface, or a wildcard bind).
    NotLoopback(SocketAddr),
    /// The address resolves to nothing, so `bind` could not succeed either way. Classified apart
    /// from the exposed case: "not loopback" would be a false diagnosis of a typo.
    Unresolvable,
}

/// **Phase: the bind guard** — resolve the `config.gatewayhub_addr` target and accept it only when
/// EVERY resolved address is loopback.
///
/// The rule has no opt-in. The datahub's guard (`flags.datahub_allow_public_bind`, composed with
/// key presence) is a posture for a server that can be reached over a tunnel by people; this daemon
/// holds every venue connection and, in a later wave, the order path, and its handshake is
/// plaintext (it authenticates the connection, not each frame). A flag that consents to being
/// reachable off-box would be the first line of an incident report, so there is none: the only way
/// to reach it from elsewhere is a tunnel to its loopback port.
///
/// Resolution is the one `TcpListener::bind` performs (`to_socket_addrs`), so a hostname that
/// resolves to a loopback AND a LAN address is refused: the strictest answer is the true one
/// (`vike_hub_proto::bind::bind_exposure`).
pub(super) fn resolve_bind(addr: &str) -> Result<Vec<SocketAddr>, BindRefusal> {
    let resolved: Vec<SocketAddr> =
        addr.to_socket_addrs().map(|it| it.collect()).unwrap_or_default();
    match bind_exposure(&resolved) {
        BindExposure::Loopback => Ok(resolved),
        BindExposure::Public(exposed) => Err(BindRefusal::NotLoopback(exposed)),
        BindExposure::Unresolvable => Err(BindRefusal::Unresolvable),
    }
}

/// [`resolve_bind`] as `run` uses it: the loopback targets, or the refusal LOGGED and the exit
/// status (2: a refused configuration — nothing was tried and failed, the configuration was refused
/// before anything opened).
pub(super) fn check_bind(addr: &str) -> Result<Vec<SocketAddr>, ExitCode> {
    match resolve_bind(addr) {
        Ok(targets) => Ok(targets),
        Err(BindRefusal::NotLoopback(exposed)) => {
            tracing::error!(
                %addr, %exposed,
                "config.gatewayhub_addr is NOT loopback — refusing to start. This daemon holds \
                 every venue connection and its handshake is plaintext, so it serves this host \
                 only, and NO flag or key allows another address. Put the row back on 127.0.0.1 \
                 (`vike-cli config set config.gatewayhub_addr 127.0.0.1:7883`, or \
                 `vike-cli config unset config.gatewayhub_addr` for the default) and reach it from \
                 elsewhere with `ssh -L 7883:localhost:7883 <host>`"
            );
            Err(ExitCode::from(2))
        }
        Err(BindRefusal::Unresolvable) => {
            tracing::error!(
                %addr,
                "config.gatewayhub_addr does not resolve to any address — refusing to start. It must \
                 be a loopback host:port such as 127.0.0.1:7883"
            );
            Err(ExitCode::from(2))
        }
    }
}
