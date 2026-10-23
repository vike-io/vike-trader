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
/// bybit, binance, aster, deribit, alpaca, ctrader, oanda and fxcm override `admit_symbol` and read each one they were told (fxcm:
/// its Orders and Trades tables are whole-account snapshots, one read per kind, parsed once per told pair beside the mounted one; deribit:
/// each instrument's open orders and fills by name, and its positions from one `currency=any` read;
/// alpaca: its three reads span the whole account, so a told symbol widens the order filter and the
/// client-side fill and position filters; ctrader: its reconcile and deal-list reads are account-wide, so a
/// told symbol widens the client-side order, position and per-symbol-id fill filters; oanda: one order
/// read per instrument, and its account-wide fills and positions filtered per instrument client-side);
/// hyperliquid's
/// reads carry no symbol at all (`frontendOpenOrders`, `userFills` and `clearinghouseState` span the
/// whole address), so every admitted coin is read without being told and `admit_symbol` stays its
/// default no-op. The other `Admits` venue, okx, still ignores it (its recon client is built per
/// mounted `instId` and scales by one `ctVal`), so its lease claims nothing. ig reads each epic it was told: its
/// three endpoints are account-wide, so one body per pass is parsed per epic. ibkr's SOCKET client reads
/// them (its three wire reads are account-wide: one read each, parsed per told symbol, each pinned to its
/// conId on first use); its cpapi client does not, but the gatewayhub grants no ibkr facts to a cpapi account
/// (the lookup refuses that backend by name), so such an account is told nothing and claims nothing. Absent =
/// claims nothing: a new venue is safe until it reads admitted symbols and is listed.
pub const RECON_READS_ADMITTED: &[&str] = &[
    "binance",
    "aster",
    "bybit",
    "deribit",
    "hyperliquid",
    "alpaca",
    "ctrader",
    "oanda",
    "ig",
    "ibkr",
    "fxcm",
];

/// The venues whose daemon-side `ReconClient` CLAIMS its open-order read lists trigger orders
/// (`ReconClient::lists_trigger_orders`), which this proxy cannot ask the daemon: binance (its perp lane
/// appends the Algo Service's `openAlgoOrders` to the report; its spot lane places no trigger kind, so
/// the claim cannot matter there). The kind twin of [`RECON_READS_ADMITTED`]. Absent = no claim: a new
/// venue is safe until its bridge proves the read (the conformance harness' resting-kind row in
/// `crates/vike-bridge-core/tests/bridge_conformance.rs`) and is listed here, and until then the
/// restore HOLDS an absent stop instead of declaring it gone. That harness holds this table EQUAL to
/// the bridges it proves (`the_nodes_claim_table_names_exactly_the_bridges_that_prove_a_claim`).
pub const RECON_LISTS_TRIGGER_ORDERS: &[&str] = &["binance"];

/// The venues whose daemon-side reconcile BALANCE depends on which symbols its client was told the engine admitted:
/// ibkr's is the `CashBalance` of the mounted contract's currency, and its socket client withholds the figure itself
/// once told the engine holds a stock in another currency (`balance_unless_foreign_stock`). The list that client
/// keeps is empty in a new daemon until [`TradeSession::retell_symbols`], so [`RemoteReconClient`] does not ask for
/// the balance while symbols are owed. Absent = the balance does not depend on it: a wallet balance is the same
/// whatever the engine holds.
pub(crate) const RECON_BALANCE_COUNTS_ADMITTED: &[&str] = &["ibkr"];

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

    /// ⚠ **Not asked while the daemon's reconcile client is owed symbols, for a venue whose balance counts only
    /// what that client was told** (`RECON_BALANCE_COUNTS_ADMITTED`): a new daemon (a gatewayhub restart) knows
    /// nothing of the symbols the engine admitted until [`TradeSession::retell_symbols`] lands them, and a symbol it
    /// refuses stays owed. Its figure would be one currency's cash for an engine that may hold a stock in another,
    /// to be adopted into a ledger it does not count. "Not reported" (`None`) is the safe answer: no balance is
    /// folded, no drift raised, and the venue is not asked.
    fn fetch_balance(&self) -> Result<Option<f64>, String> {
        let shared = self.session.shared();
        if RECON_BALANCE_COUNTS_ADMITTED.contains(&shared.account.venue.as_str())
            && !shared.told.owed().is_empty()
        {
            return Ok(None);
        }
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

    /// The venue's claim, from [`RECON_LISTS_TRIGGER_ORDERS`]: this proxy cannot ask the daemon-side
    /// client, so a venue outside the list claims nothing whatever its bridge could do.
    fn lists_trigger_orders(&self) -> bool {
        RECON_LISTS_TRIGGER_ORDERS.contains(&self.session.shared().account.venue.as_str())
    }
    // `fetch_mass_status` is deliberately NOT overridden: the trait's default composes orders, then
    // fills, then positions - the order the driver runs them and the only one any bridge has - and
    // each of the three is a fetch above, so the composition crosses as those three frames.
}
