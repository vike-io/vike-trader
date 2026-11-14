//! **The venue addresses at this daemon's start, and once a day** (`vike_secrets::venue_url`).
//!
//! * [`seed_at_boot`] upserts the release's seed into `venue_url` BEFORE `run_with`'s one read of
//!   the rows, so a release that moved a default dials the new address from this start; an
//!   `operator` row is never touched. A store that will not take the write is logged, and the read
//!   goes on with what the table holds.
//! * [`UrlProber::start`] probes every row once at the start and then every [`PROBE_EVERY`], and
//!   records what it found beside the row (`vike_secrets::venue_url::record_venue_url_probes`):
//!   `vike-cli trade health` warns on a row that is DOWN by the rule below, and `vike-cli config url
//!   check` prints the table. ⚠ A probe failure halts NOTHING: it is a WARN line and a row, never a
//!   refusal.
//!
//! # The rule (decision 0265's verdict 8 is the one authority; this is its code)
//!
//! One unauthenticated dial per row, from this daemon because it is the one process that opens venue
//! connections (decision 0164). No HTTP answer is judged but a resource's.
//!
//! * **A RESOURCE row must answer a GET with 2xx or 3xx, else it is DOWN.** A resource is a row whose
//!   product is `jnlp` or whose url's path ends in `.jnlp` ([`is_resource`]): the dukascopy demo JNLP
//!   that answered 404 is the case it exists for.
//! * **Every other rest, stream and ws row is UP when a TCP connect and a VERIFIED TLS handshake to
//!   its host and port succeed** ([`handshake`]). An API root may answer 404 or 503 by design, a
//!   WebSocket host need not answer a plain GET at all, and a GET to a root can hang (the CI box,
//!   2026-11-09: aster's socket hosts and `datafeed.dukascopy.com` timed out while they served).
//!   FXCM's `Hosts.jsp` is such a row, NOT a resource: it is the ForexConnect SDK's own discovery
//!   endpoint and answers 503 to a bare GET by design:
//!   `docs/ops/fxcm-forexconnect.md`'s "A login failed. What does it say?".
//!   An `http`/`ws` url is the TCP connect alone, and so is an `https` one on loopback ([`dial`]).
//! * **A socket row is a TCP connect**, nothing more.
//! * **A row no ACTIVE account dials is NOT IN USE** ([`InUse`]): probed and recorded with what it
//!   found, but never DOWN, so it raises no WARN. WHICH rows those are is the address vocabulary's
//!   rule, not this module's: `crates/vike-venues/src/venue_urls.rs`'s `unused_row` (rows dialled
//!   with no account, the accounts' types, and IBKR's one session), so this module names no venue.
//!   An account table or a settings table that cannot be read leaves every row in use: a store fault
//!   never hides a down address.

use std::collections::BTreeMap;
use std::io;
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use vike_model::AccountTier;
use vike_secrets::CredentialStore;
use vike_secrets::venue_setting::{SettingTier, VenueSettings};
use vike_secrets::venue_url::{UrlKey, UrlProbe, VenueUrlRow};
use vike_venues::venue_urls::UrlProtocol;

/// How often the probe runs after the one at the start.
pub(crate) const PROBE_EVERY: Duration = Duration::from_secs(24 * 3600);
/// How long one connect, and one read of the handshake, may take.
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// The release's seed into `venue_url`, said in the log. Never fatal.
pub(crate) fn seed_at_boot(settings_dir: &Path) {
    match vike_secrets::venue_url::seed_venue_urls_in(settings_dir) {
        Ok(s) if s.inserted + s.updated > 0 => tracing::info!(
            inserted = s.inserted,
            updated = s.updated,
            "vike-gatewayhub: venue addresses seeded (operator rows untouched)"
        ),
        Ok(_) => {}
        Err(e) => tracing::error!(
            error = %e,
            "vike-gatewayhub: the venue addresses could not be seeded; every venue dials what the \
             venue_url table holds (the release's defaults where it has no table yet)"
        ),
    }
}

/// The probe thread, for the process's life.
pub(crate) struct UrlProber;

impl UrlProber {
    /// Start the probe thread over `settings_dir`, whose accounts it reads through `store` (rows
    /// only, never a credential). A thread that cannot be spawned is logged.
    pub(crate) fn start(settings_dir: PathBuf, store: CredentialStore) -> UrlProber {
        let spawned =
            std::thread::Builder::new().name("venue-url-probe".to_string()).spawn(move || {
                loop {
                    probe_all(&settings_dir, &store);
                    std::thread::sleep(PROBE_EVERY);
                }
            });
        if let Err(e) = spawned {
            tracing::error!(error = %e, "vike-gatewayhub: the venue address probe did not start");
        }
        UrlProber
    }
}

/// Probe every row once, record it, and say each row in use that is down at WARN.
fn probe_all(settings_dir: &Path, store: &CredentialStore) {
    let rows = match vike_secrets::venue_url::read_venue_urls(settings_dir) {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(error = %e, "vike-gatewayhub: the venue addresses could not be read to probe");
            return;
        }
    };
    let in_use = InUse::read(store, settings_dir);
    let probes: Vec<(UrlKey, String, UrlProbe)> = rows
        .iter()
        .map(|r: &VenueUrlRow| {
            let found = probe(r.key.protocol, &r.key.product, &r.url);
            let (ok, words) = verdict(in_use.as_ref().and_then(|u| u.unused(&r.key)), found);
            if ok == Some(false) {
                tracing::warn!(
                    venue = %r.key.venue,
                    url = %r.url,
                    "vike-gatewayhub: venue address {}.{}.{}.{} {:?} did not answer: {words}",
                    r.key.venue,
                    r.key.ty,
                    r.key.channel,
                    r.key.protocol,
                    r.key.product
                );
            }
            let at_ms = vike_model::now_ms();
            (r.key.clone(), r.url.clone(), UrlProbe { at_ms, ok, words })
        })
        .collect();
    if let Err(e) = vike_secrets::venue_url::record_venue_url_probes(settings_dir, &probes) {
        tracing::warn!(error = %e, "vike-gatewayhub: the venue address probe could not be recorded");
    }
}

/// What a probe records: `(up, found)` as found, unless no active account dials the row (`unused`
/// says why), which is NOT IN USE: `None`, never down, the words still saying what was found.
fn verdict(unused: Option<String>, (up, found): (bool, String)) -> (Option<bool>, String) {
    match unused {
        Some(why) => (None, format!("not in use ({why}); {found}")),
        None => (Some(up), found),
    }
}

/// What the NOT IN USE rule reads (the module doc), afresh at each pass, so an `account.active` flip
/// needs no restart of this daemon.
pub(crate) struct InUse {
    /// The venue and tier of every ACTIVE `account` row.
    active: Vec<(String, AccountTier)>,
    /// Every venue's `venue_setting` rows (a tier's `backend` decides which IBKR row is dialled).
    settings: BTreeMap<String, VenueSettings>,
}

impl InUse {
    /// The account rows through `store` (rows only: no key name, no value) and the venue settings.
    /// `None` when either cannot be read: every row is then in use.
    fn read(store: &CredentialStore, settings_dir: &Path) -> Option<InUse> {
        let unread = |what: &str, e: &dyn std::fmt::Display| {
            tracing::warn!(
                error = %e,
                "vike-gatewayhub: the {what} could not be read, so every venue address is probed \
                 as in use"
            );
        };
        let accounts = store.accounts().inspect_err(|e| unread("accounts", e)).ok()?;
        let active: Vec<(String, AccountTier)> = accounts
            .known()?
            .iter()
            .filter(|a| a.active)
            .map(|a| (a.venue.clone(), a.tier))
            .collect();
        let settings = vike_secrets::venue_setting::load_venue_settings(settings_dir)
            .inspect_err(|e| unread("venue settings", e))
            .ok()?;
        Some(InUse { active, settings })
    }

    /// Why no active account dials `key`, or `None` when one may: `vike_venues`'s rule over the
    /// rows read, a tier's setting word read as every reader reads it (exactly the tier's row,
    /// trimmed, a blank one absent: `crates/vike-cli/src/cmd/secrets/ibkr_login.rs`'s
    /// `StoredSession::resolve`).
    fn unused(&self, key: &UrlKey) -> Option<String> {
        let UrlKey { venue, ty, channel, protocol, product } = key;
        let settings = self.settings.get(venue);
        vike_venues::venue_urls::unused_row(
            venue,
            *ty,
            *channel,
            *protocol,
            product,
            &self.active,
            |tier, field| {
                settings
                    .and_then(|s| s.get_exact(SettingTier::Tier(tier), field))
                    .map(str::trim)
                    .filter(|w| !w.is_empty())
            },
        )
    }
}

/// `(up, what happened)` for one address, by the module doc's rule ([`verdict`] then says whether it
/// is in use).
fn probe(protocol: UrlProtocol, product: &str, url: &str) -> (bool, String) {
    let found = match protocol {
        UrlProtocol::Socket => connect(url).map(|_| "connected".to_string()),
        UrlProtocol::Rest | UrlProtocol::Stream if is_resource(product, url) => get(url),
        UrlProtocol::Rest | UrlProtocol::Stream | UrlProtocol::Websocket => dial(url),
    };
    match found {
        Ok(words) => (true, words),
        Err(words) => (false, words),
    }
}

/// A RESOURCE (a file the venue serves) rather than a base host: the row's product is `jnlp`, or its
/// url's path ends in `.jnlp`. FXCM's `Hosts.jsp` is not one (the module doc).
fn is_resource(product: &str, url: &str) -> bool {
    let path = url.split_once("://").and_then(|(_, rest)| rest.split_once('/')).map(|(_, p)| p);
    product == "jnlp" || path.is_some_and(|p| p.ends_with(".jnlp"))
}

/// One GET of a resource: UP on 2xx or 3xx only (no redirect is followed, so a 3xx is its answer).
fn get(url: &str) -> Result<String, String> {
    match vike_bridge_core::http::blocking_agent_with_timeout(PROBE_TIMEOUT).get(url).call() {
        Ok(response) => {
            let status = response.status().as_u16();
            let words = status.to_string();
            if (200..400).contains(&status) { Ok(words) } else { Err(words) }
        }
        Err(e) => Err(format!("no answer: {e}")),
    }
}

/// A base host: a TCP connect to its host and port, then, for `https`/`wss`, a verified TLS
/// handshake. No byte of HTTP is sent.
fn dial(url: &str) -> Result<String, String> {
    let (tls, host, port) = host_port(url).ok_or_else(|| format!("not a base address: {url}"))?;
    let tcp = connect(&format!("{host}:{port}"))?;
    if !tls {
        return Ok("connected".to_string());
    }
    // shortcut: a LOOPBACK https row (the IBKR Client Portal gateway, whose certificate is self-signed)
    // is its TCP connect alone; an unverified handshake is the upgrade if a loopback gateway that
    // listens but cannot speak TLS is ever seen.
    if vike_bridge_core::http::is_loopback_url(url) {
        return Ok("connected (loopback: TLS not judged)".to_string());
    }
    handshake(tcp, host.trim_start_matches('[').trim_end_matches(']'))
        .map(|()| "TLS ok".to_string())
}

/// `(tls, host, port)` of a `scheme://host[:port][/base]` url; the port defaults to the scheme's.
fn host_port(url: &str) -> Option<(bool, &str, u16)> {
    let (scheme, rest) = url.split_once("://")?;
    let tls = match scheme {
        "https" | "wss" => true,
        "http" | "ws" => false,
        _ => return None,
    };
    let authority = rest.split('/').next()?;
    match authority.rsplit_once(':') {
        // A bare `[::1]` splits inside its brackets: it names no port.
        Some((host, port)) if !port.ends_with(']') => Some((tls, host, port.parse().ok()?)),
        _ => Some((tls, authority, if tls { 443 } else { 80 })),
    }
}

/// One TCP connect to `host:port` ([`first_to_connect`] of what it resolves to).
fn connect(addr: &str) -> Result<TcpStream, String> {
    first_to_connect(addr.to_socket_addrs().map_err(|e| format!("no address: {e}"))?)
}

/// The first of `addrs` to take a TCP connect, each tried in turn within [`PROBE_TIMEOUT`], as ureq's
/// connector tries them: a host whose FIRST address this box cannot reach is still up (the CI box,
/// 2026-11-09: `datafeed.dukascopy.com`'s first answer timed out while the history downloads ran).
fn first_to_connect(addrs: impl Iterator<Item = SocketAddr>) -> Result<TcpStream, String> {
    let mut failed = "no address".to_string();
    for sock in addrs {
        match TcpStream::connect_timeout(&sock, PROBE_TIMEOUT) {
            Ok(tcp) => return Ok(tcp),
            Err(e) => failed = format!("connect failed: {e}"),
        }
    }
    Err(failed)
}

/// A TLS handshake over `tcp`, the certificate verified for `host` against the webpki roots: the
/// shared stack, configured as `crates/bridges/ctrader/src/transport/connection/stream.rs`'s `wrap_tls` configures it.
fn handshake(mut tcp: TcpStream, host: &str) -> Result<(), String> {
    let failed = |e: &dyn std::fmt::Display| format!("TLS handshake failed: {e}");
    tcp.set_read_timeout(Some(PROBE_TIMEOUT))
        .and_then(|()| tcp.set_write_timeout(Some(PROBE_TIMEOUT)))
        .map_err(|e| failed(&e))?;
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|e| failed(&e))?
    .with_root_certificates(roots)
    .with_no_client_auth();
    let name = rustls::pki_types::ServerName::try_from(host.to_string()).map_err(|e| failed(&e))?;
    let mut conn = rustls::ClientConnection::new(Arc::new(config), name).map_err(|e| failed(&e))?;
    while conn.is_handshaking() {
        match conn.complete_io(&mut tcp) {
            Ok((0, 0)) => return Err(failed(&"the server stopped answering")),
            Ok(_) => {}
            Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {
                return Err("no TLS answer: timeout".to_string());
            }
            Err(e) => return Err(failed(&e)),
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "url_probe_tests.rs"]
mod tests;
