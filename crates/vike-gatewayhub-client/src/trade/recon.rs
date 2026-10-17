//! [`RemoteReconClient`]: the `ReconClient` the tradehub's `ReconManager` drives over the lease.
//!
//! # An empty answer is a CLAIM, never a default
//!
//! The restore planner reads an empty order report as "nothing resting" and declares every in-scope
//! restored order gone (`crates/vike-exec/CLAUDE.md`, "Restore plan"), and a reconcile pass `continue`s
//! on `Err`. So the one thing this client must never do is turn a fetch that did not happen into
//! `Ok(empty)`. Every non-answer is an `Err`, and there are exactly two kinds, which the manager treats
//! differently:
//!
//! * **NOT READY** (the pass was not attempted): the link is down, the daemon has no session for the
//!   account yet, the epoch just changed, the boot wire-id prefetch has not succeeded, a degraded feed.
//!   Its text begins [`RECON_NOT_READY_PREFIX`]; [`is_recon_not_ready`] is the ONE test for it, and the
//!   manager maps it to a SKIPPED pass (its health suppression), not to a fetch error.
//! * **A FETCH ERROR** (the venue was asked, or the link died mid-fetch): any other `Err`, which feeds
//!   the manager's staleness escalation.
//!
//! # Waiting
//!
//! A reply is bounded per FRAME and per REQUEST: `max(recon_stall_floor, 3 x ReconProgress.next_in_ms)`
//! (the fetch family's rule, `crate::client::stall_bound`), so a slow venue that keeps reporting progress
//! (ctrader's full handshake each pass, fxcm's 30 s calls) runs as long as it would in-process while a
//! vanished daemon is still called dead. A list answer may span pages; the client concatenates them in
//! order and refuses a lost, repeated or foreign page.

use std::sync::mpsc::RecvTimeoutError;
use std::time::Duration;

use vike_exec::recon::ReconClient;
use vike_model::{FeeSchedule, FillReport, OrderStatusReport, PositionStatusReport};

use super::link::{TradeSession, fill_wire_ids};
use super::shared::Reply;
use crate::client::stall_bound;
use crate::proto::{FETCH_PROGRESS_EVERY, ReconKind, ReconPayload, TradeDown, TradeUp};

/// The words every NOT READY text begins with.
pub const RECON_NOT_READY_PREFIX: &str = "gatewayhub reconcile not ready: ";

/// The venues whose daemon-side `ReconClient` reads every symbol the engine admitted (decision 0173):
/// bybit, binance and aster override `admit_symbol` and read each one they were told; hyperliquid's
/// reads carry no symbol at all (`frontendOpenOrders`, `userFills` and `clearinghouseState` span the
/// whole address), so every admitted coin is read without being told and `admit_symbol` stays its
/// default no-op. The other `Admits` venue, okx, still ignores it (its recon client is built per
/// mounted `instId` and scales by one `ctVal`), so its lease claims nothing. Absent = claims nothing:
/// a new venue is safe until it reads admitted symbols and is listed.
pub const RECON_READS_ADMITTED: &[&str] = &["binance", "aster", "bybit", "hyperliquid"];

/// Whether a `ReconClient` error text is a NOT READY (a pass that was not attempted) and not a fetch
/// error. The reconcile manager skips such a pass without escalating staleness.
pub fn is_recon_not_ready(error: &str) -> bool {
    error.starts_with(RECON_NOT_READY_PREFIX)
}

fn not_ready(reason: &str) -> String {
    format!("{RECON_NOT_READY_PREFIX}{reason}")
}

/// The gatewayhub-backed `ReconClient`. Built by [`TradeSession::recon_client`].
pub struct RemoteReconClient {
    session: TradeSession,
}

impl RemoteReconClient {
    pub(crate) fn new(session: TradeSession) -> Self {
        Self { session }
    }

    /// The boot wire-id prefetch gate: retry an owed prefetch, and report NOT READY while it fails.
    fn ensure_wire_ids(&self) -> Result<(), String> {
        let shared = self.session.shared();
        let owed =
            shared.wire_ids.read().unwrap_or_else(std::sync::PoisonError::into_inner).is_pending();
        if !owed {
            return Ok(());
        }
        fill_wire_ids(&self.session).map_err(|e| {
            not_ready(&format!("the wire-id prefetch has not succeeded yet ({e}); a restore must not plan with an empty translation map"))
        })
    }

    /// One reconcile fetch, assembled.
    fn fetch(&self, kind: ReconKind) -> Result<ReconPayload, String> {
        self.ensure_wire_ids()?;
        let shared = self.session.shared();
        let addr = shared.link.addr();
        let rid = shared.next_rid();
        let rx = shared
            .ask(rid, TradeUp::Recon { rid, kind })
            .map_err(|e| not_ready(&format!("the trade link is unavailable ({e})")))?;
        let floor = shared.timings.recon_stall_floor;
        let mut bound = stall_bound(floor, millis(FETCH_PROGRESS_EVERY));
        let mut assembled: Option<ReconPayload> = None;
        let mut next_page = 0u32;
        loop {
            let reply = match rx.recv_timeout(bound) {
                Ok(reply) => reply,
                Err(RecvTimeoutError::Timeout) => {
                    shared.forget(rid);
                    return Err(format!(
                        "gatewayhub {addr}: no reconcile frame for {bound:?} - the link is dead or the \
                         daemon is wedged"
                    ));
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(format!(
                        "gatewayhub {addr}: the trade link ended during the fetch"
                    ));
                }
            };
            match reply {
                // Never sent: the link was down. A pass that did not happen, not a failed one.
                Reply::Down => return Err(not_ready("the trade link is down")),
                Reply::Lost(why) => {
                    return Err(format!(
                        "gatewayhub {addr}: the trade link was lost during the reconcile fetch ({why})"
                    ));
                }
                Reply::Frame(frame) => match *frame {
                    TradeDown::ReconProgress { next_in_ms, .. } => {
                        bound = stall_bound(floor, next_in_ms);
                    }
                    TradeDown::ReconNotReady { reason, .. } => return Err(not_ready(&reason)),
                    TradeDown::ReconFailed { error, .. } => return Err(error),
                    TradeDown::ReconReply { page, more, payload, .. } => {
                        if page != next_page {
                            shared.forget(rid);
                            return Err(format!(
                                "gatewayhub {addr}: reconcile page {page} arrived where {next_page} was \
                                 due (a lost or repeated page is an error, never a short answer)"
                            ));
                        }
                        next_page += 1;
                        match assembled.as_mut() {
                            None => assembled = Some(payload),
                            Some(held) => {
                                if let Err(why) = held.extend(payload) {
                                    shared.forget(rid);
                                    return Err(format!("gatewayhub {addr}: {why}"));
                                }
                            }
                        }
                        if !more {
                            return assembled.ok_or_else(|| {
                                format!("gatewayhub {addr}: a reconcile answer with no page")
                            });
                        }
                        bound = stall_bound(floor, millis(FETCH_PROGRESS_EVERY));
                    }
                    other => {
                        shared.forget(rid);
                        return Err(format!(
                            "gatewayhub {addr}: unexpected {other:?} in a reconcile reply"
                        ));
                    }
                },
            }
        }
    }

    fn mismatch(&self, wanted: &str, got: &ReconPayload) -> String {
        format!(
            "gatewayhub {}: asked for {wanted} and the reply was {}",
            self.session.shared().link.addr(),
            got.kind()
        )
    }
}

fn millis(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

impl ReconClient for RemoteReconClient {
    fn fetch_order_status_reports(&self, since: i64) -> Result<Vec<OrderStatusReport>, String> {
        match self.fetch(ReconKind::Orders { since })? {
            ReconPayload::Orders(rows) => Ok(rows),
            other => Err(self.mismatch("Orders", &other)),
        }
    }

    fn fetch_fill_reports(&self, since: i64) -> Result<Vec<FillReport>, String> {
        match self.fetch(ReconKind::Fills { since })? {
            ReconPayload::Fills(rows) => Ok(rows),
            other => Err(self.mismatch("Fills", &other)),
        }
    }

    fn fetch_position_status_reports(&self) -> Result<Vec<PositionStatusReport>, String> {
        match self.fetch(ReconKind::Positions)? {
            ReconPayload::Positions(rows) => Ok(rows),
            other => Err(self.mismatch("Positions", &other)),
        }
    }

    fn fetch_balance(&self) -> Result<Option<f64>, String> {
        match self.fetch(ReconKind::Balance)? {
            ReconPayload::Balance(balance) => Ok(balance),
            other => Err(self.mismatch("Balance", &other)),
        }
    }

    fn fetch_fee_rates(&self) -> Result<Option<FeeSchedule>, String> {
        match self.fetch(ReconKind::FeeRates)? {
            ReconPayload::FeeRates(fee) => Ok(fee.map(Into::into)),
            other => Err(self.mismatch("FeeRates", &other)),
        }
    }

    fn fetch_account_identity(&self) -> Result<Option<String>, String> {
        match self.fetch(ReconKind::AccountIdentity)? {
            ReconPayload::AccountIdentity(id) => Ok(id),
            other => Err(self.mismatch("AccountIdentity", &other)),
        }
    }

    /// What the daemon's reconcile client reads beyond the mounted symbol, as this lease knows it
    /// (`TradeSession::told_symbols`): always a list, so a gatewayhub that restarted claims NOTHING
    /// until it was told again, and the core holds, not retires, the orders it cannot see.
    ///
    /// ⚠ A venue outside [`RECON_READS_ADMITTED`] claims nothing whatever it was told: its daemon
    /// client ignores `admit_symbol`, so a claim would let the restore planner retire a resting order
    /// on an admitted symbol that no pass ever read.
    fn admitted_symbols(&self) -> Option<Vec<String>> {
        if !RECON_READS_ADMITTED.contains(&self.session.shared().account.venue.as_str()) {
            return Some(Vec::new());
        }
        Some(self.session.told_symbols())
    }
    // `fetch_mass_status` is deliberately NOT overridden: the trait's default composes orders, then
    // fills, then positions - the order the driver runs them and the only one any bridge has - and
    // each of the three is a fetch above, so the composition crosses as those three frames.
}
