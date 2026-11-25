//! What a symbol IS and the grid it trades on.

// (Its docs are the module's own `//!` block: an outer `///` here would make rustdoc resolve that block's
// links from THIS scope.)
pub mod any_symbol;
pub mod contract;
/// Live-`RiskGate` pre-fetch: a best-effort `contractDetails` → `SymbolProperties` round-trip over a
/// dedicated throwaway socket connection, for `adapter::VenueAdapterIbkr`'s start. Behind
/// `ibkr-socket` (where ibapi exists — the socket connection it opens).
#[cfg(feature = "ibkr-socket")]
pub(crate) mod properties;
/// An order's price held to the venue's real grid at that price (decision 0220): the exec's pre-send refusal over the
/// market rules the facts source noted.
pub(crate) mod tick_grid;
