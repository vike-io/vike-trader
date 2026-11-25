//! The CEILINGS a budget counts against: `vike_venues::rate_limits_for`'s row for a venue, turned
//! into one [`BudgetCeiling`] per meter the row actually declares.
//!
//! The table is the authority and this module only reads it. A meter the row does not declare
//! ([`vike_venues::VenueRateLimitsMeter::Ungated`], [`vike_venues::VenueRateLimitsHistory::NotPaged`], a history without a
//! weight ceiling) yields no [`BudgetCeiling`] at all — never an invented number — so a request on that
//! axis is counted as UNMETERED by [`crate::budget::ledger::Budget`] rather than judged against a
//! fabricated limit.

use std::time::Duration;

use serde::Serialize;
use vike_venues::{
    VenueRateLimitsHistory, VenueRateLimitsMarket, VenueRateLimitsMeter, rate_limits_for,
};

use super::key::BudgetScope;

/// The label of a venue whose table row is one pool for both markets.
pub const SINGLE_HOST: &str = "all";

/// The window `VenueRateLimitsHistory::Weighted`'s per-minute ceiling is metered over.
const HISTORY_WINDOW: Duration = Duration::from_secs(60);

/// Which meter of a venue's `VenueRateLimits` row a counter follows — the four axes the table declares.
///
/// `VenueRateLimitsHistory` and `RestIpWeight` are both per-IP REST weight pools; the table keeps them apart
/// (`vike_venues::venue_rate_limits::VenueRateLimits::rest_ip_weight` explains why, and that no row
/// declares both), so this enum does too and a caller names the axis it spends on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetCeilingAxis {
    /// `VenueRateLimits::orders` — the order meter: submit, cancel, amend.
    Orders,
    /// `VenueRateLimits::history` — the paged-history `REQUEST_WEIGHT` budget (Binance-grammar venues).
    History,
    /// `VenueRateLimits::websocket_sends` — the client-to-server WebSocket send meter.
    WsSends,
    /// `VenueRateLimits::rest_ip_weight` — the shared per-IP REST weight pool (Hyperliquid's).
    RestIpWeight,
}

impl BudgetCeilingAxis {
    /// Every axis, in the table's field order.
    pub const ALL: [BudgetCeilingAxis; 4] = [
        BudgetCeilingAxis::Orders,
        BudgetCeilingAxis::History,
        BudgetCeilingAxis::WsSends,
        BudgetCeilingAxis::RestIpWeight,
    ];

    /// The word the snapshot and a warning print.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            BudgetCeilingAxis::Orders => "orders",
            BudgetCeilingAxis::History => "history",
            BudgetCeilingAxis::WsSends => "ws_sends",
            BudgetCeilingAxis::RestIpWeight => "rest_ip_weight",
        }
    }

    /// WHOSE budget this axis is, by the convention wave 1 counts under: the order meter is per
    /// account, the WebSocket send meter per connection, and both REST weight pools per IP.
    ///
    /// ⚠ A CONVENTION, not a table fact: `VenueRateLimits` does not say whose a meter is, and a venue
    /// that meters orders per IP or per key would need its own answer. Wave 1 counts and refuses
    /// nothing, so a wrong scope mis-groups a counter and costs no request; wave 2, which sees the
    /// tradehub's account traffic too, re-derives this per venue before anything is enforced.
    #[must_use]
    pub const fn scope(self) -> BudgetScope {
        match self {
            BudgetCeilingAxis::Orders => BudgetScope::Account,
            BudgetCeilingAxis::WsSends => BudgetScope::Connection,
            BudgetCeilingAxis::History | BudgetCeilingAxis::RestIpWeight => BudgetScope::Ip,
        }
    }

    /// The percentage of this axis's cap held back for orders ([`crate::budget::reserve::split`]'s
    /// second argument).
    ///
    /// The order meter is ALL orders (a read never spends it), so its whole cap is the order
    /// reserve; every other axis holds [`crate::budget::reserve::ORDER_RESERVE_PERCENT`] back,
    /// because an order action spends the REST weight pool and a WebSocket send as well.
    #[must_use]
    pub const fn order_share_percent(self) -> u64 {
        match self {
            BudgetCeilingAxis::Orders => 100,
            BudgetCeilingAxis::History
            | BudgetCeilingAxis::WsSends
            | BudgetCeilingAxis::RestIpWeight => super::reserve::ORDER_RESERVE_PERCENT,
        }
    }
}

/// One declared meter of one venue row: the cap to stay under and the window it is metered over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BudgetCeiling {
    /// Which meter of the row this is.
    pub axis: BudgetCeilingAxis,
    /// The venue's OWN published cap per [`Self::window`], or `None` when the venue publishes none
    /// and the number below is vike's own brake (`vike_venues::VenueRateLimitsMeter::Unpublished`).
    pub published: Option<u64>,
    /// What vike's gate admits per window today — the cap this budget judges against, a margin
    /// under [`Self::published`] where the venue publishes one. For `VenueRateLimitsHistory::Weighted` it is the
    /// pager's soft limit, the used-weight figure at which it cools down.
    pub admitted: u64,
    /// The window both numbers are metered over.
    pub window: Duration,
}

/// A table count as the `u64` the arithmetic runs in. A `usize` wider than `u64` does not exist on a
/// supported target; saturating keeps the conversion total without a cast.
fn wide(n: usize) -> u64 {
    u64::try_from(n).unwrap_or(u64::MAX)
}

/// One `VenueRateLimitsMeter` as a ceiling, or `None` when the row runs no gate on it.
fn from_meter(axis: BudgetCeilingAxis, meter: &VenueRateLimitsMeter) -> Option<BudgetCeiling> {
    match *meter {
        VenueRateLimitsMeter::Published { published, admitted, window } => Some(BudgetCeiling {
            axis,
            published: Some(wide(published)),
            admitted: wide(admitted),
            window,
        }),
        VenueRateLimitsMeter::Unpublished { admitted, window } => {
            Some(BudgetCeiling { axis, published: None, admitted: wide(admitted), window })
        }
        VenueRateLimitsMeter::Ungated => None,
    }
}

/// `VenueRateLimitsHistory` as a ceiling: only a WEIGHTED history has a number to stay under.
fn from_history(history: &VenueRateLimitsHistory) -> Option<BudgetCeiling> {
    match *history {
        VenueRateLimitsHistory::Weighted { ceiling_per_min, soft_limit, .. } => {
            Some(BudgetCeiling {
                axis: BudgetCeilingAxis::History,
                published: Some(ceiling_per_min),
                admitted: soft_limit,
                window: HISTORY_WINDOW,
            })
        }
        VenueRateLimitsHistory::Unweighted { .. } | VenueRateLimitsHistory::NotPaged => None,
    }
}

/// Every ceiling `venue`'s table row declares for `market`, in [`BudgetCeilingAxis::ALL`] order. Empty for a
/// venue the table does not know (`VenueRateLimits::NOT_DECLARED`) — an unknown venue string off the
/// wire can therefore never create a counter.
#[must_use]
pub fn ceilings_for(venue: &str, market: VenueRateLimitsMarket) -> Vec<BudgetCeiling> {
    let limits = rate_limits_for(venue, market);
    [
        from_meter(BudgetCeilingAxis::Orders, &limits.orders),
        from_history(&limits.history),
        from_meter(BudgetCeilingAxis::WsSends, &limits.websocket_sends),
        from_meter(BudgetCeilingAxis::RestIpWeight, &limits.rest_ip_weight),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// The host label of `venue` on `market`: the market's word where the table meters the two
/// separately, [`SINGLE_HOST`] where it does not.
///
/// A venue is market-split exactly when its two rows DIFFER, so this compares them rather than
/// naming binance and aster: a venue that gains a split row is picked up with no edit here, and the
/// table's own `market_split_venues_are_exactly_binance_and_aster` pin is the one place the roster
/// of splits is written.
#[must_use]
pub fn host_label(venue: &str, market: VenueRateLimitsMarket) -> &'static str {
    if rate_limits_for(venue, VenueRateLimitsMarket::Spot)
        == rate_limits_for(venue, VenueRateLimitsMarket::Perp)
    {
        SINGLE_HOST
    } else {
        market.as_str()
    }
}

/// The market a user name is on at a market-split venue: its futures lane (`vike_catalog::on_futures_lane`: a
/// perpetual's `.P`, a dated `BTCUSDT_261225`, decision 0290). A venue whose symbols carry no suffix is `Spot` — which
/// [`host_label`] then ignores, because only a suffix venue can be market-split.
#[must_use]
pub fn market_of(venue: &str, symbol: &str) -> VenueRateLimitsMarket {
    if vike_catalog::uses_perp_suffix(venue) && vike_catalog::on_futures_lane(venue, symbol) {
        VenueRateLimitsMarket::Perp
    } else {
        VenueRateLimitsMarket::Spot
    }
}
