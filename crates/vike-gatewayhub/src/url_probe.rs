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
//! * **A `429`, or a lookup, connect or TLS handshake that TIMED OUT, is still a failure, but in other words**: they
//!   start with `vike_secrets::venue_url::PROBE_REFUSED_FOR_NOW`, because a rate limit or a temporary ban looks exactly
//!   so (the CI box, 2026-11-16: `datafeed.dukascopy.com` timed out, then answered 429, then 200), and `trade health` says
//!   "rate-limited or temporarily refused" for it rather than "down". A refused connect, a name that does not resolve
//!   or a bad certificate stays down.
//! * **A row no ACTIVE account dials is NOT IN USE** ([`InUse`]): probed and recorded with what it
//!   found, but never DOWN, so it raises no WARN. WHICH rows those are is the address vocabulary's
//!   rule, not this module's: `crates/vike-venues/src/venue_urls.rs`'s `unused_row` (rows dialled
//!   with no account, the accounts' types, and IBKR's one session), so this module names no venue.
//!   An account table or a settings table that cannot be read leaves every row in use: a store fault
//!   never hides a down address.

use std::collections::BTreeMap;
use std::io;
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use vike_model::{AccountTier, MS_PER_DAY_U64};
use vike_secrets::CredentialStore;
use vike_secrets::venue_setting::{SettingTier, VenueSettings};
use vike_secrets::venue_url::{UrlKey, UrlProbe, VenueUrlRow};
use vike_venues::venue_urls::UrlProtocol;

use crate::refusals::UrlProbeRefusal;

/// How often the probe runs after the one at the start.
pub(crate) const PROBE_EVERY: Duration = Duration::from_millis(MS_PER_DAY_U64);
/// How long one address's connect, and one read of the handshake, may take.
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);
/// The whole dial's one window: the name lookup and every address it names (`vike_transport::dial::dial_bounded`
/// hands each address an equal share of what is LEFT, at most [`PROBE_TIMEOUT`]). Three timeouts, so a dual-stack
/// host whose FIRST address this box cannot reach still gives the second a full [`PROBE_TIMEOUT`], even after a
/// lookup that took one itself (the CI box, 2026-11-09: `datafeed.dukascopy.com`'s first answer timed out while the
/// history downloads ran).
const PROBE_DIAL_BOUND: Duration = Duration::from_secs(3 * PROBE_TIMEOUT.as_secs());

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
        // A socket row always carries its port (the write refuses one without), so the default is never dialled.
        UrlProtocol::Socket => vike_transport::dial::split_host_port(url, 0)
            .map_err(|e| UrlProbeRefusal::NoAddress { error: e.to_string() }.to_string())
            .and_then(|(host, port)| connect(&host, port))
            .map(|_| "connected".to_string()),
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
    match vike_transport::http::blocking_agent_with_timeout(PROBE_TIMEOUT).get(url).call() {
        Ok(response) => {
            let status = response.status().as_u16();
            match status {
                200..400 => Ok(status.to_string()),
                429 => Err(UrlProbeRefusal::RateLimited { status }.to_string()),
                _ => Err(UrlProbeRefusal::Status { status }.to_string()),
            }
        }
        Err(e) if vike_transport::http::is_timeout(&e) => {
            Err(UrlProbeRefusal::NoAnswerInTime { error: e.to_string() }.to_string())
        }
        Err(e) => Err(UrlProbeRefusal::NoAnswer { error: e.to_string() }.to_string()),
    }
}

/// A base host: a TCP connect to its host and port, then, for `https`/`wss`, a verified TLS
/// handshake. No byte of HTTP is sent.
fn dial(url: &str) -> Result<String, String> {
    let (tls, host, port) = host_port(url)
        .ok_or_else(|| UrlProbeRefusal::NotABaseAddress { url: url.to_string() }.to_string())?;
    let tcp = connect(&host, port)?;
    if !tls {
        return Ok("connected".to_string());
    }
    // shortcut: a LOOPBACK https row (the IBKR Client Portal gateway, whose certificate is self-signed)
    // is its TCP connect alone; an unverified handshake is the upgrade if a loopback gateway that
    // listens but cannot speak TLS is ever seen.
    if vike_utility::url::is_loopback_url(url) {
        return Ok("connected (loopback: TLS not judged)".to_string());
    }
    handshake(tcp, &host).map(|()| "TLS ok".to_string())
}

/// `(tls, host, port)` of a `scheme://host[:port][/base]` url; the port defaults to the scheme's, and an IPv6 host
/// comes back without its brackets (`vike_transport::dial::split_host_port`).
fn host_port(url: &str) -> Option<(bool, String, u16)> {
    let (scheme, rest) = url.split_once("://")?;
    let tls = match scheme {
        "https" | "wss" => true,
        "http" | "ws" => false,
        _ => return None,
    };
    let authority = rest.split('/').next()?;
    let (host, port) =
        vike_transport::dial::split_host_port(authority, if tls { 443 } else { 80 }).ok()?;
    Some((tls, host, port))
}

/// One TCP connect to `host:port` through the network layer's bounded dial: the lookup and EVERY address it names
/// under one [`PROBE_DIAL_BOUND`] window, each address at most [`PROBE_TIMEOUT`], as ureq's connector tries them (a
/// host whose FIRST address this box cannot reach is still up). A window or a lookup that ran out (`TimedOut`) is
/// refused-for-now; any other failure is down.
fn connect(host: &str, port: u16) -> Result<TcpStream, String> {
    vike_transport::dial::dial_bounded(host, port, PROBE_DIAL_BOUND, Some(PROBE_TIMEOUT)).map_err(
        |e| {
            let error = e.to_string();
            if e.kind() == io::ErrorKind::TimedOut {
                UrlProbeRefusal::ConnectTimedOut { error }.to_string()
            } else {
                UrlProbeRefusal::ConnectFailed { error }.to_string()
            }
        },
    )
}

/// A TLS handshake over `tcp`, the certificate verified for `host` against the webpki roots: the
/// shared stack's one verified config (`crates/vike-transport/src/tls.rs`'s `verified_tls_stream`, which
/// `crates/bridges/ctrader/src/transport/connection/stream.rs`'s `wrap_tls` wraps its socket with too).
fn handshake(tcp: TcpStream, host: &str) -> Result<(), String> {
    let failed =
        |e: &dyn std::fmt::Display| UrlProbeRefusal::TlsFailed { error: e.to_string() }.to_string();
    tcp.set_read_timeout(Some(PROBE_TIMEOUT))
        .and_then(|()| tcp.set_write_timeout(Some(PROBE_TIMEOUT)))
        .map_err(|e| failed(&e))?;
    let mut tls = vike_transport::tls::verified_tls_stream(host, tcp).map_err(|e| failed(&e))?;
    while tls.conn.is_handshaking() {
        match tls.conn.complete_io(&mut tls.sock) {
            Ok((0, 0)) => return Err(failed(&"the server stopped answering")),
            Ok(_) => {}
            Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {
                return Err(UrlProbeRefusal::NoTlsAnswer.to_string());
            }
            Err(e) => return Err(failed(&e)),
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "url_probe_tests.rs"]
mod tests;
