//! `BudgetKey` — WHICH budget a request is spent against: the venue, the host the venue meters
//! separately, and WHOSE budget it is ([`Scope`]).
//!
//! The key is the identity of a counter, so it carries only facts that make two counters different:
//! two requests share a budget exactly when they share a key (and an [`crate::budget::ceiling::Axis`],
//! which names the meter inside the key).

use std::fmt;

use serde::Serialize;

/// WHOSE budget a counter is — the unit a venue's limiter counts per.
///
/// The word is the venue's: Binance meters `REQUEST_WEIGHT` per IP and `ORDERS` per account,
/// Hyperliquid meters its weight pool per IP, the WebSocket send meters count per CONNECTION, and a
/// venue that rate-limits per API key (OKX's sub-account limits, Bybit's per-UID ones) is a
/// [`Scope::Key`]. Two requests from different scopes never compete, so two scopes are two counters
/// even for one venue and one host.
///
/// ⚠ A venue's table row (`vike_venues::venue_rate_limits::RateLimits`) does not NAME the scope of
/// its meters; [`crate::budget::ceiling::Axis::scope`] states the convention wave 1 counts under, and
/// `Key` is in the vocabulary now so the tradehub's account budgets (wave 2) join without changing
/// the key's type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    /// The source IP address of this box: every process on it spends one budget.
    Ip,
    /// One venue account (a sub-account counts as its own).
    Account,
    /// One API key.
    Key,
    /// One network connection — a WebSocket's send meter.
    Connection,
}

impl Scope {
    /// The word `config show`-style output and the snapshot print.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Scope::Ip => "ip",
            Scope::Account => "account",
            Scope::Key => "key",
            Scope::Connection => "connection",
        }
    }
}

/// The identity of one budget: `venue/host/scope`.
///
/// `host` is the table's `vike_venues::Market` word (`spot` / `perp`) for a venue whose two hosts are
/// metered separately — binance and aster, which `crate::budget::ceiling::host_label` finds by
/// comparing the two rows rather than by a list — and the single label
/// [`crate::budget::ceiling::SINGLE_HOST`] for every other venue.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
pub struct BudgetKey {
    /// The roster venue id, lowercase.
    pub venue: String,
    /// The host label; see the type's doc.
    pub host: &'static str,
    /// Whose budget it is.
    pub scope: Scope,
}

impl BudgetKey {
    /// A key from its three parts.
    #[must_use]
    pub fn new(venue: &str, host: &'static str, scope: Scope) -> Self {
        Self { venue: venue.to_string(), host, scope }
    }
}

impl fmt::Display for BudgetKey {
    /// `binance/perp/ip` — the form a warning names the budget by.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}/{}", self.venue, self.host, self.scope.as_str())
    }
}
