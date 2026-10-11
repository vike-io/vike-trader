//! [`PARAM_ROUTES`]: the declared keys a single-leg mount overrides, and [`misrouted_params`].

use toml::Value;

#[cfg(doc)]
use super::echo::resolved_params;
#[cfg(doc)]
use super::keys::{PARAM_KEYS, ParamKeys, mistyped_params, unknown_params};
#[cfg(doc)]
use super::{Capability, LIVE_CAPABLE, PORTABLE_STRATEGIES};

/// What a declared [`PARAM_KEYS`] key NAMES when it names a ROUTE — WHERE an order goes — rather
/// than a knob of the strategy's own arithmetic. A route key is read by the strategy and then
/// **OVERWRITTEN by the mount**, so it is the one kind of declared key that can be well-typed,
/// well-spelled, genuinely read — and still configure nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteKind {
    /// The INSTRUMENT the strategy stamps on its orders (the `symbol` argument of every
    /// [`vike_model::Broker`] submit verb).
    Symbol,
    /// The VENUE half of the `(venue, symbol)` pair the strategy keys its state under and echoes
    /// into the intents it produces.
    Venue,
    /// A `SYMBOL = "venue"` routing TABLE — one route per row.
    VenueMap,
}

/// Which of a name's declared keys name a ROUTE, and whether a SINGLE-LEG mount may check them.
///
/// ⚠ **This table exists because a route key is INERT on every mount this workspace builds, and
/// silently so.** `crates/vike-core/src/runtime/strategy_drive/broker_drain.rs`'s
/// `resolve_intent_symbol`/`resolve_intent_venue` return the MOUNT's own route while
/// `CoreThread::any_mount_multi` is false — every mount, since `vike_mount::MountSpec`'s `legs` is
/// empty on every spec. So `symbol = "OTHER"` on a mount of `"MOUNTED"` loads, type-checks, echoes
/// as `symbol=OTHER` — and the orders go to `"MOUNTED"`: the settings-CONSUMPTION defect (a key
/// that "hands the operator positive confirmation of something false"), worse here because it
/// names where real orders go. [`misrouted_params`] makes that state unreachable.
///
/// Exhaustive over [`PORTABLE_STRATEGIES`] (`param_routes_table_is_exhaustive`); every key is a
/// declared [`PARAM_KEYS`] key of the right type
/// (`every_route_key_is_a_declared_key_of_the_right_type`), and every route-shaped declared key
/// has a row (`every_route_shaped_declared_key_has_a_route_row`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParamRoutes {
    /// SINGLE-LEG: every route key below names the ONE market this strategy trades, so a
    /// single-symbol mount can compare each against its own `(venue, symbol)` — and must, because
    /// it would otherwise override it. An EMPTY slice means the name has no route key at all.
    SingleLeg(&'static [(&'static str, RouteKind)]),
    /// MULTI-LEG: these keys name LEGS — markets that are not the mount's own — so
    /// [`misrouted_params`] declines to judge them; the reason is data. ⚠ Not a licence to mount
    /// one: every multi-leg name is a [`Capability::NotLive`] row in [`LIVE_CAPABLE`], and a
    /// consumer checks capability FIRST, so it is refused by NAME before its params are looked at.
    MultiLeg(&'static [(&'static str, RouteKind)], &'static str),
    /// Deliberately NOT enumerated, mirroring this name's [`ParamKeys::NotEnumerated`] row — the key
    /// set is unknown, so its route subset is too. [`misrouted_params`] reports nothing and the
    /// consumer owes its own stricter rule.
    NotEnumerated(&'static str),
}

/// Per-name ROUTE-key declaration — see [`ParamRoutes`] for why a route key is the one declared key
/// that can be read and still configure nothing.
pub const PARAM_ROUTES: &[(&str, ParamRoutes)] = &[
    ("buy_hold", ParamRoutes::SingleLeg(&[("symbol", RouteKind::Symbol)])),
    ("grid", ParamRoutes::SingleLeg(&[("symbol", RouteKind::Symbol)])),
    ("dca_accumulate", ParamRoutes::SingleLeg(&[("symbol", RouteKind::Symbol)])),
    (
        "spread_maker",
        ParamRoutes::NotEnumerated(
            "mirrors this name's `ParamKeys::NotEnumerated` row — the ~60-key A-S bag is not \
             enumerated here, so its route subset cannot be either. The live consumer refuses the \
             whole table, which is strictly stronger",
        ),
    ),
    (
        "gueant_maker",
        ParamRoutes::NotEnumerated("the `spread_maker` bag, forced to `SpreadModel::Gueant`"),
    ),
    // No route key at all: every declared key is a size/price/time knob, and the instrument comes
    // from the bar/tick the harness dispatched on.
    ("trailing_scalper", ParamRoutes::SingleLeg(&[])),
    // ⚠ NO `symbol` key — this one routes off `bar.symbol`/`tick.symbol`. Its route keys are the
    // two VENUE knobs: the harness keys its executors under `(venue_for(symbol), symbol)`, echoes
    // that venue into its `PositionIntent`, and the live core discards it (`resolve_intent_venue`).
    //
    // ⚠ DECLARED RESIDUAL — an ABSENT `venue` is NOT covered, and cannot be: `harness_venue` falls
    // back to `"sim"` and the echo says `venue=sim`, which is TRUE about the harness and no order
    // destination (`Broker` submit verbs take no venue —
    // `the_harness_venue_is_a_label_and_not_an_order_destination` drives that). What this row buys
    // is that a venue the operator DOES write must be the one they are mounted on.
    (
        "momentum",
        ParamRoutes::SingleLeg(&[("venue", RouteKind::Venue), ("venues", RouteKind::VenueMap)]),
    ),
    (
        "funding_carry",
        ParamRoutes::MultiLeg(
            &[
                ("symbol", RouteKind::Symbol),
                ("venue", RouteKind::Venue),
                ("venues", RouteKind::VenueMap),
            ],
            "TWO-LEG and cross-venue by construction: `venues` is the per-leg venue map its carry \
             book is built from (≥2 venues, or there is no carry to open) and an EMPTY `symbol` is \
             its REAL two-leg mode, so no value of these keys is required to name the mount's own \
             single market. `LIVE_CAPABLE` refuses the name for the same missing mount legs",
        ),
    ),
    ("funding_capture", ParamRoutes::SingleLeg(&[("symbol", RouteKind::Symbol)])),
    (
        "pairs_zscore",
        ParamRoutes::MultiLeg(
            &[("symbol_a", RouteKind::Symbol), ("symbol_b", RouteKind::Symbol)],
            "TWO-LEG: the two keys name the two legs of one spread trade, so at most ONE of them \
             could ever equal a single-leg mount's symbol and requiring both to would be \
             incoherent. `LIVE_CAPABLE` refuses the name for the missing second mount leg",
        ),
    ),
];

/// This name's [`PARAM_ROUTES`] row, or `None` for a name this registry does not resolve.
pub fn param_routes(name: &str) -> Option<&'static ParamRoutes> {
    PARAM_ROUTES.iter().find(|(n, _)| *n == name).map(|(_, r)| r)
}

/// One route key whose value names a market the mount does not have.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteMismatch {
    /// The `[strategy.params]` key, exactly as the operator spelled it — or `venues.<SYMBOL>` for
    /// one ROW of a routing table.
    pub key: String,
    /// What that value NAMES, in the operator's own vocabulary.
    pub named: String,
    /// What this mount actually routes to.
    pub mounted: String,
}

impl std::fmt::Display for RouteMismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "`{}` names {}, but this mount routes {}", self.key, self.named, self.mounted)
    }
}

/// The route keys in `params` that name a market OTHER than the single-leg mount's own
/// `(venue, symbol)` — i.e. the ones the mount would silently override.
///
/// The third sibling of [`unknown_params`] and [`mistyped_params`], and the case both of those
/// pass: the key is read, the value is well-typed, and the mount overwrites it anyway (see
/// [`ParamRoutes`]).
///
/// Empty for an unknown name, for a [`ParamRoutes::NotEnumerated`] row, for a
/// [`ParamRoutes::MultiLeg`] name (the consumer refuses those names outright, one step earlier)
/// and for a non-table `params`.
///
/// A `Symbol` key that is ABSENT (the working default: the runtime stamps the mount's own symbol on
/// the feed) or EMPTY (a mount that cannot trade, which [`resolved_params`]' `opt_sym` reports) is
/// NOT reported, and a value of the wrong TYPE is [`mistyped_params`]' finding — a consumer runs
/// that check first. A `VenueMap` ROW is legal only when it names this mount's own route outright
/// (`<mount symbol> = "<mount venue>"`); a row whose VALUE is not a string is reported too, since
/// `harness_venue_map`'s `filter_map` drops it.
pub fn misrouted_params(
    name: &str,
    params: &Value,
    venue: &str,
    symbol: &str,
) -> Vec<RouteMismatch> {
    let Some(ParamRoutes::SingleLeg(routes)) = param_routes(name) else {
        return Vec::new();
    };
    let Some(table) = params.as_table() else {
        return Vec::new();
    };
    let mut out: Vec<RouteMismatch> = Vec::new();
    for (key, kind) in routes.iter() {
        let Some(v) = table.get(*key) else {
            continue;
        };
        match kind {
            RouteKind::Symbol => {
                if let Some(s) = v.as_str()
                    && !s.is_empty()
                    && s != symbol
                {
                    out.push(RouteMismatch {
                        key: (*key).to_string(),
                        named: format!("instrument {s:?}"),
                        mounted: format!("{symbol:?}"),
                    });
                }
            }
            RouteKind::Venue => {
                if let Some(s) = v.as_str()
                    && s != venue
                {
                    out.push(RouteMismatch {
                        key: (*key).to_string(),
                        named: format!("venue {s:?}"),
                        mounted: format!("{venue:?}"),
                    });
                }
            }
            RouteKind::VenueMap => {
                if let Some(rows) = v.as_table() {
                    for (row_symbol, row_value) in rows.iter() {
                        if row_symbol == symbol && row_value.as_str() == Some(venue) {
                            continue;
                        }
                        let named = match row_value.as_str() {
                            Some(row_venue) => format!("{row_symbol:?} on venue {row_venue:?}"),
                            None => format!(
                                "{row_symbol:?} on a {} (which this reader DROPS)",
                                row_value.type_str()
                            ),
                        };
                        out.push(RouteMismatch {
                            key: format!("{key}.{row_symbol}"),
                            named,
                            mounted: format!("{symbol:?} on venue {venue:?}"),
                        });
                    }
                }
            }
        }
    }
    out.sort_by(|a, b| a.key.cmp(&b.key));
    out
}
