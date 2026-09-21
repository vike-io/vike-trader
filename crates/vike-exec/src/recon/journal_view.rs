//! Point-in-time read of what the journal believes happened — the third leg of the
//! three-way cross-check `recon::diff` runs when handed one. Built from the vike-data journal's
//! materialized `exec_fill`/`exec_order` series (the live store read is injected via
//! `CoreConfig::journal_view_provider` at the `reconcile_reports` call site in `vike-core::runtime`
//! — the binary owns the store); callers/tests may also construct a `JournalView` directly, which
//! is what the unit tests below do. Optional: `diff(..., journal: None)` stays byte-identical to the
//! plain local-vs-venue (two-way) comparison; `Some` unlocks `Divergence::JournalDivergence` for
//! the restore/persistence-bug class this edge exists to catch — a venue fill the journal
//! recorded but live in-memory local state (the `Account`/registry) has since lost, e.g. after a
//! bad restore or a fold that silently dropped an event.

use std::collections::HashSet;

use indexmap::IndexMap;

use crate::order::OrderStatus;

/// A point-in-time snapshot of the journal's view of orders and fills, borrowed by
/// `recon::diff` alongside the live `LocalView` and the venue reports.
///
/// `Default` is the EMPTY view — no recorded trade ids, no recorded orders — and it is
/// deliberately available because a builder that could not read the journal has to return
/// something. ⚠ An empty view is behaviourally IDENTICAL to `diff(..., journal: None)`: every
/// `seen_trade_ids.contains` and every `orders.get` misses, so the fill leg falls through to
/// `MissingFill` and the order leg to `UnknownOrder`, exactly as the two-way comparison does. That
/// is why `vike_core::journal_view_from_store` LOGS on the paths that produce one — the value
/// itself cannot tell a caller that the third leg went missing.
#[derive(Default)]
pub struct JournalView {
    /// venue trade_ids the journal has recorded (fill dedup source of truth, journal side).
    pub seen_trade_ids: HashSet<String>,
    /// coid -> last known order status, per the journal.
    pub orders: IndexMap<String, OrderStatus>,
}
