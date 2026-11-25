//! The connection chain's READS, charged into the one ledger ([`super::ledger::Budget`]): the bridges' history replays
//! (the reconnect replay and its delayed twin, the gap-sentinel's, each foreign symbol of a walk) through the ONE sink
//! [`install`] gives `vike_bridge_core::read_budget`, class [`BudgetClass::Live`]; and each reconcile fetch of a trade
//! lease (`crate::trade::lease`'s `TradeUp::Reconcile` arm), class [`BudgetClass::Bulk`].
//!
//! A read spends the per-IP REST read meter its venue's rate-limit row declares ([`read_axis`]): `History` where the row
//! declares a weighted history (binance, aster), else `RestIpWeight` (hyperliquid). A venue whose row declares neither
//! (bybit, okx, deribit, polymarket, every undeclared row) is counted under the ledger's `unmetered`, judged against
//! nothing, and no number is invented for it. **A read never spends the `Orders` axis, and never the order reserve**:
//! only [`BudgetClass::Order`] draws it (`super::line::BudgetLine::spend`), so no read can take what orders are owed.
//!
//! Today the ledger is shadow ([`super::mode::MODE`]): a read over its split is counted, warned once per line per
//! window, and admitted. Under an enforcing ledger it is refused and the read is skipped: the next step of the chain
//! covers it (`vike_bridge_core::read_budget`'s module doc), and a refused reconcile fetch answers
//! [`READ_REFUSED_REASON`] as the lease's existing NOT READY.

use std::time::Instant;

use super::ceiling::{BudgetCeilingAxis, ceilings_for, market_of};
use super::class::BudgetClass;
use super::ledger::Budget;
use super::mode::BudgetAdmission;
use vike_venues::VenueRateLimitsMarket;

/// The NOT READY words of a reconcile fetch the budget refused: the tradehub's not-ready retry asks that leg again.
pub const READ_REFUSED_REASON: &str = "gatewayhub: the venue's REST read budget is over its split; this fetch was skipped and the next pass asks again";

/// The per-IP REST read meter of `venue` on `market`: [`BudgetCeilingAxis::History`] where its row declares one, else
/// [`BudgetCeilingAxis::RestIpWeight`] (which counts as unmetered on a row that gates neither). Never the `Orders` axis.
#[must_use]
pub fn read_axis(venue: &str, market: VenueRateLimitsMarket) -> BudgetCeilingAxis {
    if ceilings_for(venue, market).iter().any(|c| c.axis == BudgetCeilingAxis::History) {
        BudgetCeilingAxis::History
    } else {
        BudgetCeilingAxis::RestIpWeight
    }
}

/// Spend `cost` read units of `class` for `venue`'s `symbol` (its market is the symbol's, [`market_of`]) at `now`.
pub fn spend_read(
    budget: &Budget,
    venue: &str,
    symbol: &str,
    class: BudgetClass,
    cost: u64,
    now: Instant,
) -> BudgetAdmission {
    let market = market_of(venue, symbol);
    budget.spend(venue, market, read_axis(venue, market), class, cost, now)
}

/// Give the bridges' history replays this daemon's ledger, once, at boot: every replay is a [`BudgetClass::Live`] read.
#[cfg(feature = "data")]
pub fn install(budget: std::sync::Arc<Budget>) {
    vike_bridge_core::read_budget::install(Box::new(move |venue, symbol, cost| {
        spend_read(&budget, venue, symbol, BudgetClass::Live, cost, Instant::now())
            == BudgetAdmission::Admit
    }));
}
