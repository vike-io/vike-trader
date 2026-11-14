//! The boot phases: the boot itself, the venue settings read, the bind guard.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::process::ExitCode;

use vike_secrets::venue_setting::VenueSettings;

use crate::server::bind::{BindRefusal, resolve_loopback};

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
        // ⚠ LOAD, and not `LoadAndRefuseUnsoundSeal`. That arm is for a root that JUDGES an order
        // against the ceiling an unsound settings seal degrades to "no cap". This daemon relays and
        // signs orders, but it reads no ceiling: every enforcing site of
        // `crates/vike-config/src/ceilings.rs`'s `PRE_TRADE_CEILINGS` is on the tradehub's side of the
        // hop, and that root takes the refusing arm. So there is no ceiling here for a degraded seal
        // to uncap, and a daemon that will not start over a seal mark helps nobody (decision 0013).
        // ⚠ The arm dates from wave 1, when this daemon placed nothing, and the order path has not
        // been ruled on since: `crates/vike-gatewayhub/CLAUDE.md`'s "The run order and the boot".
        //
        // It loads settings at all because it consumes rows: `config.gatewayhub_addr` (its own
        // bind), `preferences.log_file_level` and the flags (`flags.hyperliquid_hip3`, and the five
        // the venue half folds), and because a settings tree that is present and
        // BROKEN is a startup failure at every root that loads, this one included. The deployed
        // unit carries an `ExecStartPre=vike-cli config check`, so on that box the refusal is met
        // before the process starts rather than as a restart loop.
        settings: vike_boot::SettingsLoad::Load,
        // ⚠ DEFERRED — the datahub's arm, for a stronger reason here. The departure is about TIMING
        // and SCOPE, not need: this root reads credentials, but never the whole by-NAME map
        // (`read_all`). Its reads are scoped ones made later through `Booted::store` — the hub keys
        // (their predicate is the scope), the OANDA practice token, one field of one account, once per
        // request, a credentialed data feed's ONE account (`demo` or `paper`, chosen from the rows, its
        // fields and its venue's application fields selected by number, through a handle narrowed to
        // `NoLiveKeys`; `data/accounts.rs`) once per open and, in a `trade` build, the venue half's
        // settings (`vars.rs`'s `trade_source`): the deployment NAMES the bridges look up (a `CredentialKeyScope`
        // of exactly those names) and the credential BOOK, every account's fields, because the order
        // path signs for every account it arms; re-read at each step a tradehub restart drives — so
        // the project walk stays the one this crate performed rather than a second one of the binary's.
        // `Credentials::Read` would materialise EVERY venue key on the box into this process at
        // every start, which is the opposite of what the one process that holds venue connections
        // should do with a map it does not need.
        credentials: vike_boot::Credentials::Deferred(
            "this daemon reads credentials by SCOPED reads through `Booted::store` after the boot \
             (its hub keys; one practice token, one field of one account, per request; one \
             demo or paper account's fields, per credentialed data open; and, in a `trade` build, \
             the deployment names the bridges look up plus the credential book the order path \
             signs from, at each step a tradehub restart drives), never the whole by-name venue \
             map, and through the boot's own project walk rather than a second one of the \
             binary's.",
        ),
        log_home: vike_boot::LogHome::UnderSettings,
        // ⚠ SKIP, argued: `vike_config::boot_lines` re-reads the settings to recover each row's
        // ORIGIN and renders the RISK CEILINGS, none of which this daemon reads or could act on. The
        // bind row discloses ITSELF, at the line that accepts it (`config.gatewayhub_addr`, whichever
        // way it was resolved), so a whole-tree disclosure would mostly describe ceilings this
        // process cannot honour.
        disclosure: vike_boot::Disclosure::Skip(
            "the bind row (`config.gatewayhub_addr`) discloses itself at the bind-guard line. A \
             whole-tree disclosure would mostly describe the risk ceilings, which this daemon does \
             not read: the tradehub judges every order against them before the hop.",
        ),
    })
}

/// **Phase: the venue settings** — the venue clients' copy of the `venue_setting` rows and the
/// `venue_url` addresses, taken from `run`'s ONE read of the store (`None`: no settings directory).
/// No directory, and a store that will not open, read as the release's addresses alone
/// (`vike_secrets::venue_setting::seeded_venue_settings`): no setting row, every address its seed.
///
/// A store that will not open is logged HERE for what it costs the venue clients (every venue takes
/// its built-in defaults). Polymarket's egress takes the same read through
/// `declare_polymarket_egress`, whose shared policy logs the egress half.
pub(super) fn venue_settings_of(
    loaded: Option<&Result<BTreeMap<String, VenueSettings>, vike_secrets::StoreError>>,
) -> BTreeMap<String, VenueSettings> {
    match loaded {
        Some(Ok(all)) => all.clone(),
        Some(Err(e)) => {
            tracing::error!(
                error = %e,
                "the venue_setting table could not be read; every venue's market feed takes its \
                 built-in defaults (each venue's charter mark-stream default, the default socket \
                 batching, the release's addresses)"
            );
            vike_secrets::venue_setting::seeded_venue_settings()
        }
        None => vike_secrets::venue_setting::seeded_venue_settings(),
    }
}

/// **Phase: the bind guard** — resolve the `config.gatewayhub_addr` target and accept it only when
/// EVERY resolved address is loopback. The rule is `crate::server::bind`'s ONE guard
/// ([`resolve_loopback`], the same decision `serve` asks again of the listener it is handed); this
/// phase only logs its refusal and turns it into the exit status (2: a refused configuration —
/// nothing was tried and failed, the configuration was refused before anything opened).
///
/// The rule has no opt-in. The datahub's guard (`flags.datahub_allow_public_bind`, composed with
/// key presence) is a posture for a server that can be reached over a tunnel by people; this daemon
/// holds every venue connection and the order path, and its handshake is
/// plaintext (it authenticates the connection, not each frame). A flag that consents to being
/// reachable off-box would be the first line of an incident report, so there is none: the only way
/// to reach it from elsewhere is a tunnel to its loopback port.
pub(super) fn check_bind(addr: &str) -> Result<Vec<SocketAddr>, ExitCode> {
    match resolve_loopback(addr) {
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
        Err(BindRefusal::Unresolvable(why)) => {
            tracing::error!(
                %addr, %why,
                "config.gatewayhub_addr does not resolve to any address — refusing to start. It must \
                 be a loopback host:port such as 127.0.0.1:7883"
            );
            Err(ExitCode::from(2))
        }
        // The guard's own arm: `resolve_loopback` binds nothing, so it does not arise here today,
        // and it is refused under its reason rather than folded into another arm.
        Err(refusal @ BindRefusal::Io(_)) => {
            tracing::error!(%addr, %refusal, "config.gatewayhub_addr was refused — refusing to start");
            Err(ExitCode::from(2))
        }
    }
}
