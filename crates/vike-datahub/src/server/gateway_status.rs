//! The `GatewayStatus` verb: what this datahub can say about the gatewayhub, from a cache of a second or
//! two. It gives `vike_gatewayhub_client::client::GatewayClient::status` its production caller.
//!
//! Three rules, one test each in `crates/vike-datahub/tests/default_build/gateway_status.rs`:
//!
//! * **A read loop cannot become a dial loop.** An answer is served for [`GATEWAY_STATUS_TTL`], and one
//!   ask runs at a time however many connections are reading.
//! * **Never a hang.** A dial can sit for tens of seconds on a wedged daemon (the handshake's two reads, then the
//!   status's own bound), so it runs on its own thread and a reader waits [`GATEWAY_STATUS_WAIT`], then is told.
//! * **A gatewayhub that is down is an answer.** Every outcome is a [`GatewayStatusReply`] in words, and
//!   [`gateway_status_verb`] never answers `Response::Error`. `GatewayStatus` touches no venue: the daemon answers it from
//!   its own counters (`crates/vike-gatewayhub/src/server/rpc.rs`).

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use vike_datahub_client::gateway_status::{
    GatewayBudgetCounter, GatewayStatusReply, GatewayStatusReportWire,
};
use vike_datahub_client::proto::Response;
use vike_gatewayhub_client::client::{GatewayClient, GatewayLink, gateway_text};
use vike_gatewayhub_client::proto::{BudgetCounter, GatewayStatusReport};
use vike_hub_protocol::auth::Scope;

/// How long one answer is served before the next read asks the gatewayhub again.
pub const GATEWAY_STATUS_TTL: Duration = Duration::from_secs(2);

/// How long a read waits for an ask in flight. A gatewayhub on its own box answers in milliseconds, so past
/// this it is stalled; `vike-cli trade health` waits longer than this for the reply.
pub const GATEWAY_STATUS_WAIT: Duration = Duration::from_secs(3);

/// The answer of a datahub mounted with no lane: a test's server. `crate::datahub_cli`'s `run` always mounts one.
const NO_GATEWAY_LINK: &str = "this datahub was started with no gatewayhub link";

/// The ask a [`GatewayStatusLane`] runs. A parameter so a test scripts the gatewayhub's answer.
type Probe = Box<dyn Fn() -> GatewayStatusReply + Send + Sync>;

#[derive(Default)]
struct ProbeState {
    /// The latest answer and when it landed.
    last: Option<(Instant, GatewayStatusReply)>,
    /// An ask is running.
    asking: bool,
}

/// The cached, bounded way to the gatewayhub's state. The module doc is its contract.
pub struct GatewayStatusLane {
    addr: String,
    probe: Probe,
    ttl: Duration,
    wait: Duration,
    state: Mutex<ProbeState>,
    landed: Condvar,
}

impl GatewayStatusLane {
    /// The production lane: `GatewayStatus` over `link`, under the observe key, at the two constants.
    pub fn dialling(link: GatewayLink) -> Self {
        let addr = link.addr().to_string();
        Self::with_probe(addr, move || ask(&link), GATEWAY_STATUS_TTL, GATEWAY_STATUS_WAIT)
    }

    /// A lane over any ask: `addr` is only for the words a stalled ask is answered with.
    pub fn with_probe(
        addr: impl Into<String>,
        probe: impl Fn() -> GatewayStatusReply + Send + Sync + 'static,
        ttl: Duration,
        wait: Duration,
    ) -> Self {
        Self {
            addr: addr.into(),
            probe: Box::new(probe),
            ttl,
            wait,
            state: Mutex::default(),
            landed: Condvar::new(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, ProbeState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The answer a reader gets: the cached one, or the next ask's within the lane's wait, or the words
    /// saying that ask has not come back.
    pub fn answer(self: &Arc<Self>) -> GatewayStatusReply {
        let mut state = self.lock();
        if let Some((at, reply)) = &state.last
            && at.elapsed() < self.ttl
        {
            return reply.clone();
        }
        if !state.asking {
            let lane = Arc::clone(self);
            let spawned =
                std::thread::Builder::new().name("dh-gw-status".into()).spawn(move || {
                    // A panic here would leave `asking` set, and every later read would call a live daemon stalled.
                    let reply = catch_unwind(AssertUnwindSafe(|| (lane.probe)()))
                        .unwrap_or_else(|_| lane.not_reached("the status ask panicked"));
                    let mut state = lane.lock();
                    state.last = Some((Instant::now(), reply));
                    state.asking = false;
                    lane.landed.notify_all();
                });
            match spawned {
                Ok(_) => state.asking = true,
                Err(e) => return self.not_reached(&format!("could not start the status ask: {e}")),
            }
        }
        let (state, _) = self
            .landed
            .wait_timeout_while(state, self.wait, |s| s.asking)
            .unwrap_or_else(PoisonError::into_inner);
        match &state.last {
            Some((_, reply)) if !state.asking => reply.clone(),
            _ => self.not_reached(&format!("no answer within {:?}", self.wait)),
        }
    }

    fn not_reached(&self, why: &str) -> GatewayStatusReply {
        GatewayStatusReply {
            reached: false,
            error: Some(gateway_text(&self.addr, &why)),
            report: None,
        }
    }
}

/// One real ask. Which step failed is what `reached` says: a daemon that took the handshake and gave no
/// report (stalled, or older than `gateway_status`) is not one the datahub cannot reach.
fn ask(link: &GatewayLink) -> GatewayStatusReply {
    let mut client = match GatewayClient::connect(link, Scope::Read) {
        Ok(client) => client,
        Err(e) => {
            return GatewayStatusReply {
                reached: false,
                error: Some(gateway_text(link.addr(), &e)),
                report: None,
            };
        }
    };
    match client.status() {
        Ok(report) => {
            GatewayStatusReply { reached: true, error: None, report: Some(mirrored(report)) }
        }
        Err(e) => GatewayStatusReply {
            reached: true,
            error: Some(gateway_text(link.addr(), &e)),
            report: None,
        },
    }
}

/// The daemon's report as this wire carries it, destructured whole so a new field does not compile until
/// somebody decides. `budget` is carried row for row; no health renderer shows it (SHADOW counters of orders alone,
/// which would read as headroom: `vike_datahub_client::gateway_status`'s module doc).
fn mirrored(report: GatewayStatusReport) -> GatewayStatusReportWire {
    let GatewayStatusReport {
        epoch,
        started_ms,
        uptime_ms,
        connections,
        stream_sessions,
        fetches_running,
        market_data_venues,
        credentialed_data_venues,
        budget,
    } = report;
    GatewayStatusReportWire {
        epoch,
        started_ms,
        uptime_ms,
        connections,
        stream_sessions,
        fetches_running,
        market_data_venues,
        credentialed_data_venues,
        budget: Some(budget.into_iter().map(mirrored_counter).collect()),
    }
}

/// One budget row as this wire carries it, destructured whole for [`mirrored`]'s reason.
fn mirrored_counter(row: BudgetCounter) -> GatewayBudgetCounter {
    let BudgetCounter { venue, host, scope, class, used, cap, window_ms, would_throttle } = row;
    GatewayBudgetCounter { venue, host, scope, class, used, cap, window_ms, would_throttle }
}

/// The `Request::GatewayStatus` handler. Every outcome is the same variant: the module doc.
pub(super) fn gateway_status_verb(lane: Option<&Arc<GatewayStatusLane>>) -> Response {
    Response::GatewayStatus(match lane {
        Some(lane) => lane.answer(),
        None => GatewayStatusReply {
            reached: false,
            error: Some(NO_GATEWAY_LINK.to_string()),
            report: None,
        },
    })
}
