//! The test builders BOTH of this crate's test homes share — the white-box suites under
//! `src/runtime/tests/` (through `crate::runtime::test_support` and `cfg(test)`) and the
//! `integration` binary's kit under `tests/support/` (through the `test-support` feature, which only
//! dev edges turn on: the self dev-dependency, and vike-tradehub-core's for the reconcile-manager
//! suites' `crates/vike-tradehub-core/tests/common/core_kit.rs`) — spelled once here instead of once
//! per home. Only shapes built from PUBLIC items live here; a builder that needs the crate-private
//! `assemble_core` or `CoreThread` stays in `crate::runtime::test_support`.
//!
//! The contract is `vike_marketdata::test_support`'s: every builder fixes its DEFAULTS and says which,
//! and a test that READS a pinned field builds by hand.
//!
//! ⚠ No clock, scratch directory, environment or compile-time path: the source-walking gates read
//! this file as production code (`any(test, feature = ..)` is not a test-only marker to them).
//! [`test_config`]'s clock is a counter, not a clock read, which is the point.

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};

use vike_marketdata::QuoteTick;
use vike_model::{Broker, Strategy};

use crate::{CoreConfig, LiveBroker, StrategyMount};

/// `strategy` mounted on the `venue`/`symbol`/`interval` bar series — the ONE spelling of the default
/// mount shape.
///
/// DEFAULTS: `account: None` (the venue's DEFAULT account, so the mount trades the engine whose
/// `route_key` is the bare venue id), `symbols` empty (no extra legs: the single-symbol contract, in
/// which a `Broker` verb's `symbol` argument is ignored), `controller_id: None` (so the mount id is
/// the derived `{venue}__{symbol}__{interval}`, which names its state sidecar, its schedule keys and
/// its journal attribution), and `underlying_symbol: None` (no cross-symbol mark is routed to
/// `on_mark`). The interval stays an argument because the callers do not share one. A test whose
/// subject is one of those — an account label, declared legs, a controller id, an underlying, or the
/// derived mount id itself — builds its `StrategyMount` by hand.
pub fn mount_of(
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

/// Submits a 1-lot BTCUSDT market buy on EVERY quote tick and counts the ticks it saw into `calls`.
pub struct AlwaysSubmitStrategy {
    pub calls: Arc<AtomicUsize>,
}

impl Strategy<LiveBroker> for AlwaysSubmitStrategy {
    fn on_quote_tick(&mut self, broker: &mut LiveBroker, _q: &QuoteTick) {
        self.calls.fetch_add(1, Ordering::Relaxed);
        broker.submit_market("BTCUSDT", 1, 1.0);
    }
}

/// A core config with a self-advancing clock: every call advances an `AtomicI64` by one, so message
/// `k` dispatches at `now_ms == k` and no real wall-clock wait is needed to reach a future instant.
///
/// DEFAULTS: every other field is `CoreConfig::default()` — no strategy mount, no journal. Built
/// with struct-update rather than by reassigning fields of a `Default::default()` value, which the
/// workspace clippy gate forbids.
pub fn test_config(seed_cash: f64) -> CoreConfig {
    let t = Arc::new(AtomicI64::new(0));
    CoreConfig {
        seed_cash,
        clock: Box::new(move || t.fetch_add(1, Ordering::Relaxed)),
        ..CoreConfig::default()
    }
}
