//! `balance_gate` — **the NODE holds the gate on a venue's reconcile balance**, because only the node knows what
//! the engine it feeds was bound with (third review of `fix/usd-default-currency`, HIGH-A and HIGH-B).
//!
//! The reconcile balance of oanda, ig and fxcm is the account's cash in ITS currency, with no currency beside it
//! (the account summary, the account row and the Accounts table carry none), and the core adopts it into a ledger
//! LABELLED with whatever currency the engine was bound with at its start. Three bindings make that wrong, and in
//! each the balance is not handed to the engine:
//!
//! * the engine's currency is only ASSUMED (`account_ccy_assumed`: the venue said nothing, or the read failed):
//!   the real currency's cash would be labelled USD;
//! * the venue STATED a currency and the mount fence DECLINED to anchor the ledger to it (EUR on the USD-quoted
//!   `EURUSD` mount): the engine books its mounted symbol in another money than the cash is in;
//! * the venue stated a currency other than USD and this account does not convert (no exposure ceiling), so the
//!   engine anchors to nothing and keeps its dollar label.
//!
//! The gatewayhub keeps a gate of its own (`TradeLane::balance_withheld`), but it follows the last START the
//! gatewayhub SAW, not what the node's engine was bound with: the node's re-teach after a gatewayhub restart
//! re-sends the `AdapterStart`, the gatewayhub reads the currency again and may now get a statement, and the node
//! throws that reply away because its engine is not rebuilt. So the gateway's gate is a belt, and this decorator,
//! built in `RemoteBackend::start_adapter` from the values that bind the engine, is the authority: nothing a
//! gatewayhub later reads can open it.
//!
//! A second, dynamic hold covers ibkr's boot window: ibkr's balance is the cash of the MOUNTED contract's
//! currency, which its daemon-side client withholds itself once told the engine holds a stock in another currency;
//! a tradehub that boots with such a stock in its journal has not yet told the new client (the boot re-admission
//! runs after the start), so the balance is not asked until every journaled foreign symbol was told.
//!
//! Every other verb is forwarded as it is, and venues outside [`crate::server::symbol_admission`]'s
//! `balance_carries_no_currency` (ctrader fences itself, dukascopy has no balance leg, ibkr's client withholds
//! itself) pass through untouched.

use vike_exec::recon::ReconClient;
use vike_model::{FeeSchedule, FillReport, OrderStatusReport, PositionStatusReport};

use crate::server::symbol_admission::balance_carries_no_currency;

/// How the engine of an account of a no-currency-balance venue was bound, as far as its balance is concerned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Binding<'a> {
    /// The ledger is anchored to the currency the venue stated (or the account does not convert and the venue
    /// stated nothing that could disagree): the balance is in the ledger's money.
    Anchored,
    /// The ledger's currency is only the ruling's USD ASSUMED.
    Assumed,
    /// The venue stated a currency and the engine's mount fence declined to anchor the ledger to it (`why` is
    /// the fence's own sentence).
    Declined { why: &'a str },
    /// The venue stated `ccy`, other than USD, and this account does not convert: the engine anchors to nothing.
    Unanchored { ccy: &'a str },
}

/// **Why `venue`'s reconcile balance must not be handed to an engine bound like this**, or `None`.
pub(super) fn held_reason(venue: &str, binding: &Binding<'_>) -> Option<String> {
    if !balance_carries_no_currency(venue) {
        return None;
    }
    match binding {
        Binding::Anchored => None,
        Binding::Assumed => Some(format!(
            "{venue}: the engine's currency is only ASSUMED (USD) and the venue's cash balance carries no \
             currency, so it is not adopted into this ledger (the account's real currency would be labelled \
             USD); set the account's `account.active` off and on to start it again and read the currency"
        )),
        Binding::Declined { why } => Some(format!(
            "{venue}: {why}; the venue's cash balance is in the account's own currency, which is not this \
             ledger's, so it is not adopted into it"
        )),
        Binding::Unanchored { ccy } => Some(format!(
            "{venue}: the venue states the account's currency is {ccy} and this account does not convert, so \
             its engine is anchored to nothing and keeps a dollar ledger; the venue's cash balance, in {ccy}, \
             is not adopted into it"
        )),
    }
}

/// The symbols still owed to the daemon's reconcile client, asked on every balance fetch (empty = none).
pub(super) type Untold = Box<dyn Fn() -> Vec<String> + Send>;

/// The bound reconcile client with the balance gate in front of it: see the module doc.
pub(super) struct BalanceGate {
    inner: Box<dyn ReconClient>,
    venue: String,
    /// The binding's reason, fixed when the engine was bound; `None` = the balance is handed on.
    held: Option<String>,
    /// ibkr: the journaled foreign symbols the daemon's client has not been told yet.
    untold: Option<Untold>,
}

/// `inner` with the balance gate in front of it, or `inner` itself when nothing is held and nothing is owed.
pub(super) fn gate(
    inner: Box<dyn ReconClient>,
    venue: &str,
    held: Option<String>,
    untold: Option<Untold>,
) -> Box<dyn ReconClient> {
    if held.is_none() && untold.is_none() {
        return inner;
    }
    if let Some(why) = &held {
        tracing::warn!(venue, "RECONCILE BALANCE HELD BACK for this engine's life: {why}");
    }
    Box::new(BalanceGate { inner, venue: venue.to_string(), held, untold })
}

impl ReconClient for BalanceGate {
    fn fetch_order_status_reports(&self, since: i64) -> Result<Vec<OrderStatusReport>, String> {
        self.inner.fetch_order_status_reports(since)
    }

    fn fetch_fill_reports(&self, since: i64) -> Result<Vec<FillReport>, String> {
        self.inner.fetch_fill_reports(since)
    }

    fn fetch_position_status_reports(&self) -> Result<Vec<PositionStatusReport>, String> {
        self.inner.fetch_position_status_reports()
    }

    /// `Ok(None)` ("not reported": no balance folded, no drift raised, the venue not asked) while the binding
    /// holds it or the daemon's client is owed a journaled foreign symbol; else the inner client's answer.
    fn fetch_balance(&self) -> Result<Option<f64>, String> {
        if let Some(why) = &self.held {
            tracing::debug!(venue = %self.venue, "balance not reported: {why}");
            return Ok(None);
        }
        if let Some(untold) = &self.untold {
            let owed = untold();
            if !owed.is_empty() {
                tracing::debug!(
                    venue = %self.venue, symbols = ?owed,
                    "balance not reported: the daemon's reconcile client was not told these foreign symbols yet"
                );
                return Ok(None);
            }
        }
        self.inner.fetch_balance()
    }

    fn fetch_fee_rates(&self) -> Result<Option<FeeSchedule>, String> {
        self.inner.fetch_fee_rates()
    }

    fn fetch_account_identity(&self) -> Result<Option<String>, String> {
        self.inner.fetch_account_identity()
    }

    fn admit_symbol(&self, symbol: &str) {
        self.inner.admit_symbol(symbol);
    }

    fn admitted_symbols(&self) -> Option<Vec<String>> {
        self.inner.admitted_symbols()
    }
    // `fetch_mass_status` is deliberately NOT overridden: the trait's default composes the three forwarded
    // fetches above in the order the driver runs them, exactly as it does over the inner client.
}

#[path = "balance_gate_tests.rs"]
#[cfg(test)]
mod balance_gate_tests;
