//! Strategy-mount builders.

use vike_core::{LiveBroker, StrategyMount};
use vike_model::Strategy;

/// `strategy` mounted on the `venue`/`symbol`/`interval` bar series — the ONE spelling of the
/// default mount shape.
///
/// DEFAULTS: `account: None` (the venue's DEFAULT account, so the mount trades the engine whose
/// `route_key` is the bare venue id), `symbols` empty (no extra legs: the single-symbol contract, in
/// which a `Broker` verb's `symbol` argument is ignored), `controller_id: None` (so the mount id is
/// the derived `{venue}__{symbol}__{interval}`, which names its state sidecar, its schedule keys and
/// its journal attribution), and `underlying_symbol: None` (no cross-symbol mark is routed to
/// `on_mark`). A test whose subject is one of those — an account label, declared legs, a
/// controller id, an underlying, or the derived mount id itself — builds its `StrategyMount` by
/// hand.
pub(crate) fn mount_of(
    venue: &str,
    symbol: &str,
    interval: &str,
    strategy: Box<dyn Strategy<LiveBroker> + Send>,
) -> StrategyMount {
    StrategyMount {
        account: None,
        symbols: Vec::new(),
        controller_id: None,
        underlying_symbol: None,
        venue: venue.into(),
        symbol: symbol.into(),
        interval: interval.into(),
        strategy,
    }
}
