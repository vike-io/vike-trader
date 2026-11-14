//! `BudgetClass` — WHAT KIND of request is spending, which decides which part of a split budget it may
//! draw from ([`crate::budget::reserve::BudgetSplit`]).

use serde::Serialize;

/// The four kinds of traffic a venue budget carries, in priority order.
///
/// * [`BudgetClass::Order`] — an order submit, cancel or amend. The one class that must never WAIT behind
///   a read, and the only one that may draw the order reserve.
/// * [`BudgetClass::Live`] — the live market-data plane's own traffic: a WebSocket subscribe frame, a
///   book re-seed. Its absence is a gap in a recorded tape.
/// * [`BudgetClass::Interactive`] — a request a person is waiting on: the chart seed, a catalog listing.
/// * [`BudgetClass::Bulk`] — a request nobody waits on: a backfill's pages.
///
/// ⚠ The order here is PRIORITY, not draw order: the split has two pools (the order reserve and the
/// shared pool), and the only distinction the arithmetic draws is `Order` against the rest. The
/// other three are named separately so a later wave's enforcement can queue them differently
/// without changing the call sites that classify their traffic today.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetClass {
    /// Order submit, cancel, amend.
    Order,
    /// The live market-data plane.
    Live,
    /// A request a person is waiting on.
    Interactive,
    /// Bulk paging nobody waits on.
    Bulk,
}

impl BudgetClass {
    /// Every class, in priority order.
    pub const ALL: [BudgetClass; 4] =
        [BudgetClass::Order, BudgetClass::Live, BudgetClass::Interactive, BudgetClass::Bulk];

    /// May this class draw the order reserve? Only [`BudgetClass::Order`] does.
    #[must_use]
    pub const fn draws_order_reserve(self) -> bool {
        matches!(self, BudgetClass::Order)
    }

    /// The word the snapshot and a warning print.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            BudgetClass::Order => "order",
            BudgetClass::Live => "live",
            BudgetClass::Interactive => "interactive",
            BudgetClass::Bulk => "bulk",
        }
    }
}
