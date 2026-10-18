//! **IBKR's gateway, as the daemon watches it** (decision 0164 verdict 7(b), wave 2 W14 and W14b).
//!
//! The daemon is the ONE API client of IB Gateway (4102 demo / 4101 live, the DEMO unit
//! `deploy/vike-ibkr-gateway.service`). IBKR logs every gateway off once a day (23:45 UTC on the box, the
//! gateway process exits and its supervisor starts it again a minute later), and when it does the
//! order-update stream the SOCKET bridge built ends for good: that adapter has NO reconnect, deliberately
//! (`crates/bridges/vike-ibkr/CLAUDE.md`, `IbInbound::StreamDead`), and every later submit is refused.
//! Before wave 2 that ended IBKR trading until a restart. The daemon now notices the loss (the bridge REPORTS its own
//! stream's death with an `ExecLink{up:false}`, which is acted on at once; two probes of the gateway's port failing in
//! a row is the fallback), takes the dead adapter down, and rebuilds it once the port answers again, for as long as
//! the account rows say the account runs.
//!
//! **A rebuilt bridge starts BLIND to the orders of the one before it** (its coid map is empty), so a session
//! rebuilt over orders it sent and never saw finish is HELD until the bridge announces `ExecLink{up:true}`, which
//! it does only after it has asked the venue for its open orders and replayed the day's executions; a bridge whose
//! learning fails never announces and the hold stays (`crate::trade::session`'s `link` module, "Held").
//! **None of this has run against a real gateway**; the first attended observation is the IBKR rollout step.
//!
//! **The Client Portal Gateway (cpapi) is NOT supervised.** Its pump redials by itself and sends
//! `IbInbound::StreamResync`, which re-requests the open orders and executions and rebinds the coid map;
//! a rebuilt cpapi adapter would START without that resync, so supervising it would replace a working
//! recovery with a worse one. `from_settings` answers `None` for it, as for `oauth`.
//!
//! # What this file is
//!
//! * [`IbkrGateway`], the address the socket bridge dials, read from the SAME `venue.ibkr.<tier>.{backend,
//!   host,port}` rows the bridge reads (`crates/bridges/vike-ibkr/src/config.rs`'s
//!   `load_ibkr_config_from`): exactly the tier's row, trimmed, a blank row reading as no row.
//! * [`IbkrWatch`], the [`LinkWatch`] over it: `reachable` is one bounded TCP connect to that address (the
//!   probe `deploy/ibkr-gateway/wait-gateway.sh` runs against the same port at every gateway start),
//!   `tier_now` is the fresh read of the account rows the supervision asks before every attempt.
//!
//! The loop is `crate::session::reconnect`'s and the session half is `AccountSession`'s
//! (`crate::trade::session`): this file names no socket of the venue's, no bridge, no client id and no
//! credential. **IB client ids stay the bridge's**: the rebuilt adapter is `TradeLane::start_adapter` over
//! the same `AdapterStart`, so it takes the same exec, data and reconcile ids the old one held (centralising
//! them is wave 3).
//!
//! # What a TCP probe can and cannot see
//!
//! * **It sees the daily logoff**, a gateway restart and a crash: the port closes and stays closed for the
//!   restart's length (a minute at least on the box).
//! * **It does not see a gateway that lost its link to IBKR** (errors 1100/2110, 2103, 2157; the gateway's
//!   port keeps answering: 2026-10-17, 13:36 and 17:00). The BRIDGE does: its link watchdog
//!   (`crates/bridges/vike-ibkr/CLAUDE.md`'s "Local traps", the link watchdog bullet) declares the stream dead after 5 min of it, and that
//!   arrives here as the bridge's own `ExecLink{up:false}`, like any other death. The rebuild that follows is
//!   spaced by the bridge (its exec connect refuses until the schedule allows it), so this loop's own backoff
//!   only ever sees a cheap refused start. A blip that restores inside 5 min needs nothing rebuilt.
//! * **A blip shorter than the probe cadence is missed by the probe**, and one failed probe is forgiven
//!   (it takes [`crate::session::reconnect::LOSS_PROBES`] in a row). What the probe misses the bridge reports
//!   itself: when its order-update stream ends it announces `ExecLink{up:false}` (`vike-ibkr`'s `exec.rs`,
//!   `announce_link`), which the session acts on at once. A half-open socket whose stream never ends is
//!   the probe's. The cadence is [`PROBE_EVERY`].

use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::time::Duration;

use vike_config::ArmingBlock;
use vike_model::AccountTier;
use vike_secrets::venue_setting::{SettingTier, VenueSettings};
use vike_venues::IbkrBackend;

use super::lane::LinkWatch;
use crate::session::reconnect::Schedule;

/// How often the gateway's port is probed while the adapter is up, and the rows are re-read while it is
/// down. The probe is a local connect and an immediate close, the one `wait-gateway.sh` makes once a second
/// at a gateway start.
pub const PROBE_EVERY: Duration = Duration::from_secs(2);

/// The bound on one probe's connect. A local port answers or refuses at once; a wedged one is a loss.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(1);

/// The ceiling the wait between failed restores grows to. The first wait is the IBKR row's own redial
/// (`vike_venues::venue_timings::IBKR`'s exec `reconnect.initial`, 2 s: the cpapi pump's wait after a failed
/// dial); the socket backend declares no timing of its own.
pub const MAX_WAIT: Duration = Duration::from_secs(60);

/// The default ports the bridge falls back on when no `port` row exists: TWS paper 7497 / live 7496
/// (`crates/bridges/vike-ibkr/src/config.rs`'s `default_port`, private). A deployment writes the row (the box
/// runs IB Gateway on 4102 and 4101), so this is the answer of an unconfigured box only.
const DEFAULT_PORT_LIVE: u16 = 7496;
const DEFAULT_PORT_DEMO: u16 = 7497;

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
    /// The gateway `tier`'s rows name, or `Ok(None)` for a backend the daemon does not supervise: `oauth`
    /// (nothing connects through it) and `cpapi` (its pump redials and resyncs by itself, so a rebuilt
    /// adapter would only lose that resync).
    ///
    /// # Errors
    /// Words naming the row that cannot be read (a backend word that is none of the three, a port that is
    /// no number). The bridge refuses the same rows at its own start, so a started adapter never reaches
    /// this with one.
    pub fn from_settings(
        tier: AccountTier,
        settings: &VenueSettings,
    ) -> Result<Option<Self>, String> {
        let row = |field: &str| {
            settings
                .get_exact(SettingTier::Tier(tier), field)
                .map(str::trim)
                .filter(|v| !v.is_empty())
        };
        let word = row("backend").unwrap_or("socket");
        let backend = IbkrBackend::parse(word).ok_or_else(|| {
            format!("venue.ibkr.{}.backend `{word}` is none of socket, cpapi, oauth", tier.as_str())
        })?;
        match backend {
            IbkrBackend::Oauth | IbkrBackend::Cpapi => Ok(None),
            IbkrBackend::Socket => {
                let host = row("host").unwrap_or("127.0.0.1").to_string();
                let port = match row("port") {
                    Some(p) => p.parse().map_err(|_| {
                        format!("venue.ibkr.{}.port `{p}` is not a port", tier.as_str())
                    })?,
                    None => match tier {
                        AccountTier::Live => DEFAULT_PORT_LIVE,
                        AccountTier::Demo | AccountTier::Paper => DEFAULT_PORT_DEMO,
                    },
                };
                Ok(Some(Self { backend, host, port }))
            }
        }
    }

    /// Whether the gateway answers a connect now, within [`PROBE_TIMEOUT`]. The connection is closed at
    /// once: nothing is sent, no client id is taken.
    ///
    /// # Errors
    /// Why not: the name did not resolve, or the connect was refused or timed out.
    pub fn probe(&self) -> Result<(), String> {
        let addrs: Vec<SocketAddr> = (self.host.as_str(), self.port)
            .to_socket_addrs()
            .map_err(|e| format!("{}:{} does not resolve: {e}", self.host, self.port))?
            .collect();
        let mut last = format!("{}:{} resolved to no address", self.host, self.port);
        for addr in addrs {
            match TcpStream::connect_timeout(&addr, PROBE_TIMEOUT) {
                Ok(stream) => {
                    drop(stream);
                    return Ok(());
                }
                Err(e) => last = format!("{addr} refused the connect: {e}"),
            }
        }
        Err(last)
    }
}

/// The rows' answer RIGHT NOW: a fresh read of the account table on every call.
pub type TierNow = Box<dyn Fn() -> Result<AccountTier, ArmingBlock> + Send + Sync>;

/// The [`LinkWatch`] of one IBKR account: a gateway to probe and a fresh read of its rows.
pub struct IbkrWatch {
    gateway: IbkrGateway,
    tier_now: TierNow,
    schedule: Schedule,
}

impl IbkrWatch {
    /// A watch over `gateway`, asking `tier_now` for the rows, on the production schedule.
    #[must_use]
    pub fn new(gateway: IbkrGateway, tier_now: TierNow) -> Self {
        Self { gateway, tier_now, schedule: Self::production_schedule() }
    }

    /// The same watch on another schedule (a test's, in milliseconds).
    #[must_use]
    pub fn with_schedule(mut self, schedule: Schedule) -> Self {
        self.schedule = schedule;
        self
    }

    /// The production schedule: probe every [`PROBE_EVERY`], the first failed restore waits the IBKR
    /// row's redial, growing to [`MAX_WAIT`].
    #[must_use]
    pub fn production_schedule() -> Schedule {
        Schedule {
            probe_every: PROBE_EVERY,
            first_wait: vike_venues::venue_timings::IBKR
                .exec()
                .reconnect
                .map_or(Duration::from_secs(2), |r| r.initial),
            max_wait: MAX_WAIT,
        }
    }
}

impl std::fmt::Debug for IbkrWatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IbkrWatch")
            .field("gateway", &self.gateway)
            .field("schedule", &self.schedule)
            .finish_non_exhaustive()
    }
}

impl LinkWatch for IbkrWatch {
    fn reachable(&self) -> Result<(), String> {
        self.gateway.probe()
    }

    fn tier_now(&self) -> Result<AccountTier, ArmingBlock> {
        (self.tier_now)()
    }

    fn schedule(&self) -> Schedule {
        self.schedule
    }
}

#[cfg(test)]
#[path = "ibkr_tests.rs"]
mod ibkr_tests;
