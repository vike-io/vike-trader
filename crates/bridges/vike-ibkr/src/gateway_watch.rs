//! **IBKR's gateway, as the gatewayhub watches it** (decision 0164 verdict 7(b), wave 2 W14 and W14b): this venue's
//! half of the daemon's one link watch (`vike_bridge_core::venue_link::VenueLinkProbe`), declared by the adapter's
//! `link_probe` (`crate::adapter::VenueAdapterIbkr`, amended 2026-11-21: the daemon asks the bridge, never a venue name).
//!
//! The daemon is the ONE API client of IB Gateway (4102 demo / 4101 live, the DEMO unit
//! `deploy/vike-ibkr-gateway.service`). IBKR logs every gateway off once a day (23:45 UTC on the box, the
//! gateway process exits and its supervisor starts it again a minute later), and when it does the
//! order-update stream the SOCKET bridge built ends for good: that adapter has NO reconnect, deliberately
//! (`crates/bridges/vike-ibkr/CLAUDE.md`, `IbkrInbound::StreamDead`), and every later submit is refused.
//! Before wave 2 that ended IBKR trading until a restart. The daemon now notices the loss (the bridge REPORTS its own
//! stream's death with an `ExecLink{up:false}`, which is acted on at once; two probes of the gateway's port failing in
//! a row is the fallback), takes the dead adapter down, and rebuilds it once the port answers again, for as long as
//! the account rows say the account runs.
//!
//! **A rebuilt bridge starts BLIND to the orders of the one before it** (its order client id map is empty), so a session
//! rebuilt over orders it sent and never saw finish is HELD until the bridge announces `ExecLink{up:true}`, which
//! it does only after it has asked the venue for its open orders and replayed the day's executions; a bridge whose
//! learning fails never announces and the hold stays (the gatewayhub's `trade/session/link.rs`, "Held").
//! **The hold and its release on a rebuild after a logoff with an order resting have run over doubles only**;
//! the first attended observation is the IBKR demo step of `docs/ops/gatewayhub-the CI box.md`. Orders, fills and the
//! bridge's learn line have run over this lane on the CI box's paper gateway (`crates/vike-gatewayhub/CLAUDE.md`).
//!
//! **The Client Portal Gateway (cpapi) is NOT supervised.** Its pump redials by itself and sends
//! `IbkrInbound::StreamResync`, which re-requests the open orders and executions and rebinds the order client id map;
//! a rebuilt cpapi adapter would START without that resync, so supervising it would replace a working
//! recovery with a worse one. [`IbkrGateway::supervised`] answers `None` for it, as for `oauth`.
//!
//! # What this file is
//!
//! * [`IbkrGateway`], the address the socket bridge dials, read from the SAME rows the bridge reads
//!   (`crates/bridges/vike-ibkr/src/config.rs`'s `load_ibkr_config_from`): the tier's
//!   `venue.ibkr.<tier>.backend` row (exactly the tier's, trimmed, a blank row reading as no row) and the
//!   tier's `ibkr <type> trade socket gateway` `venue_url` row through the one lookup (decision 0265).
//! * Its [`VenueLinkProbe`]: `reachable` is one bounded TCP connect to that address (the probe
//!   `deploy/ibkr-gateway/wait-gateway.sh` runs against the same port at every gateway start), and the cadence
//!   ([`PROBE_EVERY`], and the socket row's `reconnect` ladder up to [`MAX_WAIT`], `vike_venues::venue_timings::IBKR`).
//!
//! The loop, the fresh read of the account rows before every attempt and the session half are the gatewayhub's
//! (`crates/vike-gatewayhub/src/trade/adapters/link_watch.rs`'s `VenueLinkWatch`): this file opens no venue session, takes no
//! client id and reads no credential. **IB client ids stay the adapter's**: the rebuilt adapter is the same start
//! over the same `AdapterStart`, so it takes the same exec, data and reconcile ids the old one held.
//!
//! # What a TCP probe can and cannot see
//!
//! * **It sees the daily logoff**, a gateway restart and a crash: the port closes and stays closed for the
//!   restart's length (a minute at least on the box).
//! * **It does not see a gateway that lost its link to IBKR** (errors 1100/2110, 2103, 2157; the gateway's
//!   port keeps answering: 2026-10-17, 13:36 and 17:00). The BRIDGE does: its link watchdog
//!   (`crates/bridges/vike-ibkr/CLAUDE.md`'s "Local traps", the link watchdog bullet) declares the stream dead after 5 min of it, and that
//!   arrives at the daemon as the bridge's own `ExecLink{up:false}`, like any other death. The rebuild that follows is
//!   spaced by the bridge (its exec connect refuses until the schedule allows it), so the daemon's own backoff
//!   only ever sees a cheap refused start. A blip that restores inside 5 min needs nothing rebuilt.
//! * **A blip shorter than the probe cadence is missed by the probe**, and one failed probe is forgiven
//!   (the gatewayhub's reconnect loop takes `LOSS_PROBES` in a row). What the probe misses the bridge reports
//!   itself: when its order-update stream ends it announces `ExecLink{up:false}` (`execution/mod.rs`, `announce_link`),
//!   which the session acts on at once. A half-open socket whose stream never ends is the probe's. The cadence is
//!   [`PROBE_EVERY`].

use std::time::Duration;

use vike_bridge_core::venue_link::VenueLinkProbe;
use vike_model::AccountTier;
use vike_secrets::venue_setting::{SettingTier, VenueSettings};
use vike_venues::IbkrBackend;
use vike_venues::venue_urls::{UrlChannel, UrlProtocol, UrlSocket, UrlType};

use crate::refusals::IbkrConfigRefusal;

/// How often the gateway's port is probed while the adapter is up, and the rows are re-read while it is
/// down. The probe is a local connect and an immediate close, the one `wait-gateway.sh` makes once a second
/// at a gateway start.
pub const PROBE_EVERY: Duration = Duration::from_secs(2);

/// The bound on one probe's connect. A local port answers or refuses at once; a wedged one is a loss.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(1);

/// The ceiling the wait between failed restores grows to: the `max` of the socket product's exec `reconnect` in
/// `vike_venues::venue_timings::IBKR`, whose `initial` is the first wait.
pub const MAX_WAIT: Duration = vike_venues::venue_timings::IBKR.exec().reconnect().max;

/// What a start PARKED behind the gateway says while its port does not answer yet (owner ruling 2026-11-16: the
/// server start no longer waits for IB Gateway), which `trade health` shows: the probe's
/// `VenueLinkProbe::not_ready_words`.
pub const GATEWAY_NOT_LOGGED_IN: &str = "IBKR gateway not logged in yet";

/// The address a SOCKET bridge of this account dials.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IbkrGateway {
    /// Which product: always the socket (IB Gateway / TWS) today; the field stays so a second supervised
    /// backend would be a new value here, not a new type.
    pub backend: IbkrBackend,
    /// The host the product listens on.
    pub host: String,
    /// The port it listens on.
    pub port: u16,
}

impl IbkrGateway {
    /// The gateway the daemon supervises for `tier`: its backend from the `venue.ibkr.<tier>.backend` row
    /// and its address from the ONE lookup the bridge asks, the `ibkr <type> trade socket gateway`
    /// `venue_url` row (`crates/bridges/vike-ibkr/src/config.rs`'s `gateway_addresses`). `Ok(None)` for a
    /// backend the daemon does not supervise: `oauth` (nothing connects through it) and `cpapi` (its pump
    /// redials and resyncs by itself, so a rebuilt adapter would only lose that resync).
    ///
    /// # Errors
    /// The refusal naming the row that cannot be read (a backend word that is none of the three, a refused
    /// address, a socket row that is not `host:port`). The bridge refuses the same rows at its own start,
    /// so a started adapter never reaches this with one.
    pub fn supervised(
        tier: AccountTier,
        settings: &VenueSettings,
    ) -> Result<Option<Self>, IbkrConfigRefusal> {
        let word = settings
            .get_exact(SettingTier::Tier(tier), &vike_venues::venue_fields::BACKEND)
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .unwrap_or("socket");
        let backend =
            IbkrBackend::parse(word).ok_or_else(|| IbkrConfigRefusal::BackendWordUnknown {
                tier: tier.as_str().to_string(),
                word: word.to_string(),
            })?;
        if backend != IbkrBackend::Socket {
            return Ok(None);
        }
        let socket = settings
            .urls()
            .url(UrlType::of_tier(tier), UrlChannel::Trade, UrlProtocol::Socket, "gateway")
            .map_err(|refusal| IbkrConfigRefusal::AddressRow { refusal })?;
        let UrlSocket { host, port } = UrlSocket::parse(socket).map_err(|_| {
            IbkrConfigRefusal::GatewayAddressNotHostPort { socket: socket.to_string() }
        })?;
        Ok(Some(Self { backend, host, port }))
    }

    /// Whether the gateway answers a connect now, the name lookup and every address under one [`PROBE_TIMEOUT`]
    /// (`crates/vike-transport/src/dial.rs`'s `dial_bounded`). The connection is closed at once: nothing is sent,
    /// no client id is taken.
    ///
    /// # Errors
    /// Why not: the name did not resolve, or the connect was refused or timed out.
    pub fn probe(&self) -> Result<(), String> {
        vike_transport::dial::dial_bounded(&self.host, self.port, PROBE_TIMEOUT, None)
            .map(drop)
            .map_err(|e| e.to_string())
    }
}

/// The production cadence: probe every [`PROBE_EVERY`], the first failed restore waits the socket row's first
/// `reconnect` wait, growing to [`MAX_WAIT`].
impl VenueLinkProbe for IbkrGateway {
    fn reachable(&self) -> Result<(), String> {
        self.probe()
    }

    fn probe_every(&self) -> Duration {
        PROBE_EVERY
    }

    fn first_wait(&self) -> Duration {
        vike_venues::venue_timings::IBKR.exec().reconnect().initial
    }

    fn max_wait(&self) -> Duration {
        MAX_WAIT
    }

    fn not_ready_words(&self) -> Option<&'static str> {
        Some(GATEWAY_NOT_LOGGED_IN)
    }
}

#[cfg(test)]
#[path = "gateway_watch_tests.rs"]
mod gateway_watch_tests;
