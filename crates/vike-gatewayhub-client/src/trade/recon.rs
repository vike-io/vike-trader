//! [`ReconcileClientGateway`]: the `ReconcileClient` the tradehub's `ReconcileManager` drives over the lease.
//!
//! # An empty answer is a CLAIM, never a default
//!
//! The restore planner reads an empty order report as "nothing resting" and declares every in-scope
//! restored order gone (`crates/vike-execution/CLAUDE.md`, "Restore plan"), and a reconcile pass `continue`s
//! on `Err`. So the one thing this client must never do is turn a fetch that did not happen into
//! `Ok(empty)`. Every non-answer is an `Err`, and there are exactly two kinds, which the manager treats
//! differently:
//!
//! * **NOT READY** (the pass was not attempted): the link is down, the daemon has no session for the
//!   account yet, the epoch just changed, the boot wire-id prefetch has not succeeded, a degraded feed.
//!   A typed read (`fetch_mass_status`, `fetch_balance`) says it as [`FailureOutcome::NotSent`] (decision 0301:
//!   the manager decides on that, never on the words), and the manager maps it to a SKIPPED pass (its health
//!   suppression), not to a fetch error. Its text begins [`RECONCILE_NOT_READY_PREFIX`].
//! * **A FETCH ERROR** (the venue was asked, or the link died mid-fetch): any other `Err` (unclassified),
//!   which feeds the manager's staleness escalation.
//!
//! # Waiting
//!
//! A reply is bounded per FRAME and per REQUEST: `max(reconcile_stall_floor, 3 x ReconcileProgress.next_in_ms)`
//! (the fetch family's rule, `crate::client::stall_bound`), so a slow venue that keeps reporting progress
//! (ctrader's full handshake each pass, fxcm's 30 s calls) runs as long as it would in-process while a
//! vanished daemon is still called dead. A list answer may span pages; the client concatenates them in
//! order and refuses a lost, repeated or foreign page.

use std::sync::mpsc::RecvTimeoutError;
use std::time::Duration;

use vike_execution::recon::{ReconcileClient, ReconcileFetchError, ReconcileMassStatus};
use vike_model::{FeeSchedule, FillReport, OrderStatusReport, PositionStatusReport};
use vike_utility::errors::FailureOutcome;

use super::link::{TradeLease, fill_wire_ids};
use super::shared::TradeLeaseReply;
use crate::client::stall_bound;
use crate::proto::{
    FEATURE_GATEWAY_TRADE_RECONCILE_READ, FETCH_PROGRESS_EVERY, ReconcileKind, ReconcilePayload,
    TradeDown, TradeUp,
};

/// The words every NOT READY text begins with.
pub const RECONCILE_NOT_READY_PREFIX: &str = "gatewayhub reconcile not ready: ";

/// The venues whose daemon-side `ReconcileClient` reads every symbol the engine admitted (decision 0173):
/// bybit, binance, aster, deribit, alpaca, ctrader, oanda, okx and fxcm override `admit_symbol` and read each one they were told (fxcm:
/// its Orders and Trades tables are whole-account snapshots, one read per kind, parsed once per told pair beside the mounted one; deribit:
/// each instrument's open orders and fills by name, and its positions from one `currency=any` read;
/// alpaca: its three reads span the whole account, so a told symbol widens the order filter and the
/// client-side fill and position filters; ctrader: its reconcile and deal-list reads are account-wide, so a
/// told symbol widens the client-side order, position and per-symbol-id fill filters; oanda: one order
/// read per instrument, and its account-wide fills and positions filtered per instrument client-side);
/// hyperliquid's
/// reads carry no symbol at all (`frontendOpenOrders`, `userFills` and `clearinghouseState` span the
/// whole address), so a core perp coin is read without being told, and its `admit_symbol` keeps only what
/// those reads do not span (decision 0218): a builder-dex coin (`dex:COIN`, read per dex) and a spot pair
/// (`BASE/QUOTE`, its holding a balance row), case kept, under the engine's cap; polymarket's reads carry no
/// symbol either and its `admit_symbol` stays the default no-op (the CLOB's `/data/orders` and `/data/trades`
/// and the data-api's `/positions` key off the wallet, and its client filters nothing by token). okx reads each instrument it was told with the requests it makes for the mounted
/// one, scaling each by that instrument's own `ctVal` (its positions are one account-wide read split
/// per instrument). ig reads each epic it was told: its
/// three endpoints are account-wide, so one body per pass is parsed per epic. ibkr's SOCKET client reads
/// them (its three wire reads are account-wide: one read each, parsed per told symbol, each pinned to its
/// conId on first use); its cpapi client does not, but the gatewayhub grants no ibkr facts to a cpapi account
/// (the lookup refuses that backend by name), so such an account is told nothing and claims nothing. Absent =
/// claims nothing: a new venue is safe until it reads admitted symbols and is listed.
pub const RECONCILE_READS_ADMITTED: &[&str] = &[
    "binance",
    "aster",
    "bybit",
    "deribit",
    "hyperliquid",
    "polymarket",
    "alpaca",
    "ctrader",
    "oanda",
    "ig",
    "ibkr",
    "fxcm",
    "okx",
];

/// The venues whose daemon-side `ReconcileClient` CLAIMS its open-order read lists trigger orders
/// (`ReconcileClient::lists_trigger_orders`), which this proxy cannot ask the daemon: binance (its perp lane
/// appends the Algo Service's `openAlgoOrders` to the report; its spot lane's `openOrders` lists the
/// `STOP_LOSS`/`STOP_LOSS_LIMIT` it places as ordinary rows, so the ONE venue entry is true of both
/// lanes), bybit (`order/realtime` is asked with no `orderFilter`, which lists the
/// resting conditional stop beside the active orders), okx (its report appends the paged
/// `orders-algo-pending` read, and a fired stop's fills are named by the stop's own id) and deribit
/// (`get_open_orders_by_instrument` with `type: "all"` lists an `untriggered` stop with its label, and the
/// fill of a fired stop carries that label). The kind twin of
/// [`RECONCILE_READS_ADMITTED`]. Absent = no claim: a new venue is safe until its bridge proves the read (the
/// conformance harness' resting-kind row in `crates/vike-bridge-core/tests/bridge_conformance.rs`) and is
/// listed here, and until then the restore HOLDS an absent stop instead of declaring it gone. That harness
/// holds this table EQUAL to the bridges it proves (`the_nodes_claim_table_names_exactly_the_bridges_that_prove_a_claim`).
pub const RECONCILE_LISTS_TRIGGER_ORDERS: &[&str] = &["binance", "bybit", "deribit", "okx"];

/// The venues whose daemon-side reconcile BALANCE depends on which symbols its client was told the engine admitted:
/// ibkr's is the `CashBalance` of the mounted contract's currency, and its socket client withholds the figure itself
/// once told the engine holds a stock in another currency (`balance_unless_foreign_stock`). The list that client
/// keeps is empty in a new daemon until [`TradeLease::retell_symbols`], so [`ReconcileClientGateway`] does not ask for
/// the balance while symbols are owed. Absent = the balance does not depend on it: a wallet balance is the same
/// whatever the engine holds.
pub(crate) const RECONCILE_BALANCE_COUNTS_ADMITTED: &[&str] = &["ibkr"];

/// A NOT READY: the pass was not attempted, so nothing reached the venue ([`FailureOutcome::NotSent`], which the
/// reconcile manager answers with a skipped pass instead of a failed fetch).
fn not_ready(reason: &str) -> ReconcileFetchError {
    ReconcileFetchError::new(
        FailureOutcome::NotSent,
        format!("{RECONCILE_NOT_READY_PREFIX}{reason}"),
    )
}

/// A FETCH ERROR: the venue was asked, or the link died mid-fetch.
fn failed(text: String) -> ReconcileFetchError {
    ReconcileFetchError::unclassified(text)
}

/// The gatewayhub-backed `ReconcileClient`. Built by [`TradeLease::recon_client`].
pub struct ReconcileClientGateway {
    session: TradeLease,
}

impl ReconcileClientGateway {
    pub(crate) fn new(session: TradeLease) -> Self {
        Self { session }
    }

    /// The boot wire-id prefetch gate: retry an owed prefetch, and report NOT READY while it fails.
    fn ensure_wire_ids(&self) -> Result<(), ReconcileFetchError> {
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
    fn fetch(&self, kind: ReconcileKind) -> Result<ReconcilePayload, ReconcileFetchError> {
        self.ensure_wire_ids()?;
        let shared = self.session.shared();
        let addr = shared.link.addr();
        let rid = shared.next_rid();
        let rx = shared
            .ask(rid, TradeUp::Reconcile { rid, kind })
            .map_err(|e| not_ready(&format!("the trade link is unavailable ({e})")))?;
        let floor = shared.timings.reconcile_stall_floor;
        let mut bound = stall_bound(floor, millis(FETCH_PROGRESS_EVERY));
        let mut assembled: Option<ReconcilePayload> = None;
        let mut next_page = 0u32;
        // What the daemon said on page 0 about the symbols its client read (`TradeDown::ReconcileReply::read`).
        let mut read: Option<Vec<String>> = None;
        loop {
            let reply = match rx.recv_timeout(bound) {
                Ok(reply) => reply,
                Err(RecvTimeoutError::Timeout) => {
                    shared.forget(rid);
                    return Err(failed(format!(
                        "gatewayhub {addr}: no reconcile frame for {bound:?} - the link is dead or the \
                         daemon is wedged"
                    )));
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(failed(format!(
                        "gatewayhub {addr}: the trade link ended during the fetch"
                    )));
                }
            };
            match reply {
                // Never sent: the link was down. A pass that did not happen, not a failed one.
                TradeLeaseReply::Down => return Err(not_ready("the trade link is down")),
                TradeLeaseReply::Lost(why) => {
                    return Err(failed(format!(
                        "gatewayhub {addr}: the trade link was lost during the reconcile fetch ({why})"
                    )));
                }
                TradeLeaseReply::Frame(frame) => {
                    match *frame {
                        TradeDown::ReconcileProgress { next_in_ms, .. } => {
                            bound = stall_bound(floor, next_in_ms);
                        }
                        TradeDown::ReconcileNotReady { reason, .. } => {
                            return Err(not_ready(&reason));
                        }
                        TradeDown::ReconcileFailed { error, .. } => return Err(failed(error)),
                        TradeDown::ReconcileReply { page, more, payload, read: named, .. } => {
                            if page != next_page {
                                shared.forget(rid);
                                return Err(failed(format!(
                                    "gatewayhub {addr}: reconcile page {page} arrived where {next_page} was \
                                 due (a lost or repeated page is an error, never a short answer)"
                                )));
                            }
                            next_page += 1;
                            if page == 0 {
                                read = named;
                            }
                            match assembled.as_mut() {
                                None => assembled = Some(payload),
                                Some(held) => {
                                    if let Err(why) = held.extend(payload) {
                                        shared.forget(rid);
                                        return Err(failed(format!("gatewayhub {addr}: {why}")));
                                    }
                                }
                            }
                            if !more {
                                let done = assembled.ok_or_else(|| {
                                    failed(format!(
                                        "gatewayhub {addr}: a reconcile answer with no page"
                                    ))
                                })?;
                                // Only a list kind carries the daemon's word on what its client read, and only a
                                // daemon that advertises the string is believed about it (an older one names
                                // nothing; the claim is then the mirror's, as it always was).
                                if matches!(
                                    done,
                                    ReconcilePayload::Orders(_)
                                        | ReconcilePayload::Fills(_)
                                        | ReconcilePayload::Positions(_)
                                ) {
                                    shared.told.name(read.take().filter(|_| {
                                        shared.has_feature(FEATURE_GATEWAY_TRADE_RECONCILE_READ)
                                    }));
                                }
                                return Ok(done);
                            }
                            bound = stall_bound(floor, millis(FETCH_PROGRESS_EVERY));
                        }
                        other => {
                            shared.forget(rid);
                            return Err(failed(format!(
                                "gatewayhub {addr}: unexpected {other:?} in a reconcile reply"
                            )));
                        }
                    }
                }
            }
        }
    }

    fn mismatch(&self, wanted: &str, got: &ReconcilePayload) -> ReconcileFetchError {
        failed(format!(
            "gatewayhub {}: asked for {wanted} and the reply was {}",
            self.session.shared().link.addr(),
            got.kind()
        ))
    }

    fn orders(&self, since: i64) -> Result<Vec<OrderStatusReport>, ReconcileFetchError> {
        match self.fetch(ReconcileKind::Orders { since })? {
            ReconcilePayload::Orders(rows) => Ok(rows),
            other => Err(self.mismatch("Orders", &other)),
        }
    }

    fn fills(&self, since: i64) -> Result<Vec<FillReport>, ReconcileFetchError> {
        match self.fetch(ReconcileKind::Fills { since })? {
            ReconcilePayload::Fills(rows) => Ok(rows),
            other => Err(self.mismatch("Fills", &other)),
        }
    }

    fn positions(&self) -> Result<Vec<PositionStatusReport>, ReconcileFetchError> {
        match self.fetch(ReconcileKind::Positions)? {
            ReconcilePayload::Positions(rows) => Ok(rows),
            other => Err(self.mismatch("Positions", &other)),
        }
    }
}

fn millis(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

impl ReconcileClient for ReconcileClientGateway {
    fn fetch_order_status_reports(&self, since: i64) -> Result<Vec<OrderStatusReport>, String> {
        self.orders(since).map_err(|e| e.text)
    }

    fn fetch_fill_reports(&self, since: i64) -> Result<Vec<FillReport>, String> {
        self.fills(since).map_err(|e| e.text)
    }

    fn fetch_position_status_reports(&self) -> Result<Vec<PositionStatusReport>, String> {
        self.positions().map_err(|e| e.text)
    }

    /// The trait's composition (orders, then fills, then positions: the driver's order) over the typed reads, so a
    /// NOT READY stays [`FailureOutcome::NotSent`] and the reconcile manager skips that pass instead of counting a
    /// failed fetch.
    fn fetch_mass_status(&self, since: i64) -> Result<ReconcileMassStatus, ReconcileFetchError> {
        let orders = self.orders(since)?;
        let fills = self.fills(since)?;
        let positions = self.positions()?;
        Ok(ReconcileMassStatus { orders, positions, fills })
    }

    /// ⚠ **Not asked while the daemon's reconcile client is owed symbols, for a venue whose balance counts only
    /// what that client was told** (`RECONCILE_BALANCE_COUNTS_ADMITTED`): a new daemon (a gatewayhub restart) knows
    /// nothing of the symbols the engine admitted until [`TradeLease::retell_symbols`] lands them, and a symbol it
    /// refuses stays owed. Its figure would be one currency's cash for an engine that may hold a stock in another,
    /// to be adopted into a ledger it does not count. "Not reported" (`None`) is the safe answer: no balance is
    /// folded, no drift raised, and the venue is not asked.
    ///
    /// shortcut: the reconcile wire carries the venue's words alone (`TradeDown::ReconcileFailed`), so the outcome the
    /// daemon's bridge set is lost here and every venue failure reads unclassified (a NOT READY is `NotSent`); carry
    /// it on the frame (a wire change) when a node-side reader decides on it (today the node only logs it).
    fn fetch_balance(&self) -> Result<Option<f64>, ReconcileFetchError> {
        let shared = self.session.shared();
        if RECONCILE_BALANCE_COUNTS_ADMITTED.contains(&shared.account.venue.as_str())
            && !shared.told.owed().is_empty()
        {
            return Ok(None);
        }
        match self.fetch(ReconcileKind::Balance)? {
            ReconcilePayload::Balance(balance) => Ok(balance),
            other => Err(self.mismatch("Balance", &other)),
        }
    }

    fn fetch_fee_rates(&self) -> Result<Option<FeeSchedule>, String> {
        match self.fetch(ReconcileKind::FeeRates).map_err(|e| e.text)? {
            ReconcilePayload::FeeRates(fee) => Ok(fee),
            other => Err(self.mismatch("FeeRates", &other).text),
        }
    }

    fn fetch_account_identity(&self) -> Result<Option<String>, String> {
        match self.fetch(ReconcileKind::AccountIdentity).map_err(|e| e.text)? {
            ReconcilePayload::AccountIdentity(id) => Ok(id),
            other => Err(self.mismatch("AccountIdentity", &other).text),
        }
    }

    /// What the daemon's reconcile client reads beyond the mounted symbol, as this lease knows it
    /// (`TradeLease::told_symbols`): always a list, so a gatewayhub that restarted claims NOTHING
    /// until it was told again, and the core holds, not retires, the orders it cannot see.
    ///
    /// ⚠ A venue outside [`RECONCILE_READS_ADMITTED`] claims nothing whatever it was told: its daemon
    /// client ignores `admit_symbol`, so a claim would let the restore planner retire a resting order
    /// on an admitted symbol that no pass ever read.
    ///
    /// **Narrowed by the daemon's own word.** A daemon that advertises `gateway_trade_reconcile_read` names, in each
    /// list-kind reply, the symbols its client read; the lease then claims only the told symbols that list
    /// carries ([`super::told::Told::readable`]). So a symbol the mirror holds and the daemon's client does
    /// not (the last slot raced, or the adapter was restarted inside one daemon session and its new client
    /// was told nothing) is not claimed, and the restore HOLDS the orders on it instead of retiring them.
    /// The named list never widens the claim.
    fn admitted_symbols(&self) -> Option<Vec<String>> {
        let shared = self.session.shared();
        if !RECONCILE_READS_ADMITTED.contains(&shared.account.venue.as_str()) {
            return Some(Vec::new());
        }
        let (read, unnamed) = shared.told.readable();
        match unnamed {
            Some(unnamed) if !unnamed.is_empty() => tracing::warn!(
                venue = %shared.account.venue,
                symbols = %unnamed.join(","),
                "gatewayhub reconcile: the daemon's client does not name symbols this lease told it; \
                 they are not claimed, so the restore holds their orders instead of retiring them"
            ),
            Some(_) => tracing::info!(
                venue = %shared.account.venue,
                "gatewayhub reconcile: the daemon's client names every symbol this lease told it again"
            ),
            None => {}
        }
        Some(read)
    }

    /// The venue's claim, from [`RECONCILE_LISTS_TRIGGER_ORDERS`]: this proxy cannot ask the daemon-side
    /// client, so a venue outside the list claims nothing whatever its bridge could do.
    fn lists_trigger_orders(&self) -> bool {
        RECONCILE_LISTS_TRIGGER_ORDERS.contains(&self.session.shared().account.venue.as_str())
    }
}
