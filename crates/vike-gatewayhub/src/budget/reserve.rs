//! The RESERVE CALCULATOR: a cap split into an order reserve and a shared pool. PURE arithmetic —
//! no clock, no state, no table.
//!
//! The split exists so that, once a later wave turns enforcement on, an order action is never made
//! to wait behind a backfill: reads (`Live`, `Interactive`, `Bulk`) may spend only the shared pool,
//! and an order spends the reserve first and the pool only for what the reserve cannot cover
//! ([`crate::budget::line::Line::spend`]).
//!
//! ⚠ **The reserve FRACTION is a placeholder, and it is wave 1's only invented number.** Nothing in
//! the tree measures how much of a venue's budget the tradehub's orders really use (its traffic is
//! invisible to this daemon until wave 2), so [`ORDER_RESERVE_PERCENT`] is a round figure chosen so
//! that orders, which are rare, are not starved by a burst of reads. It is exercised only in SHADOW
//! mode ([`crate::budget::mode::MODE`]): it decides what a counter would have said, and refuses
//! nothing. The PR that turns enforcement on re-derives it from measured order traffic.

/// The percentage of a shared axis's cap held back for orders (see the module doc: a placeholder).
pub const ORDER_RESERVE_PERCENT: u64 = 20;

/// A cap divided into the part only orders may draw and the part everyone shares.
///
/// `order_reserve + shared_pool == cap` for every input ([`split`]), so a split never creates or
/// loses a unit of a venue's budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Split {
    /// Units only [`crate::budget::class::Class::Order`] may draw first.
    pub order_reserve: u64,
    /// Units every class draws; an order draws it only after the reserve is spent.
    pub shared_pool: u64,
}

/// Split `cap` into an order reserve of `order_share_percent` percent (rounded UP, so a non-zero
/// share of a non-zero cap is never zero) and a shared pool of the rest.
///
/// A percentage above 100 is 100: the reserve never exceeds the cap. `0` percent reserves nothing
/// and `100` leaves the pool empty (the order meter's own shape: nothing but orders spends it).
#[must_use]
pub fn split(cap: u64, order_share_percent: u64) -> Split {
    let percent = order_share_percent.min(100);
    // `cap * percent` cannot overflow a u128; the quotient is at most `cap`, so it fits back.
    let reserve = (u128::from(cap) * u128::from(percent)).div_ceil(100);
    let order_reserve = u64::try_from(reserve).unwrap_or(cap).min(cap);
    Split { order_reserve, shared_pool: cap - order_reserve }
}
