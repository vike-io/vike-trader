//! Owned strategy-script state: the `Intent` order-verb vocabulary + `ScriptCtx`, the shared,
//! lock-guarded state the Rhai engine's host functions (see `engine.rs`) read/mutate. `ScriptCtx`
//! is OWNED by the strategy (via `SharedCtx = Arc<RwLock<ScriptCtx>>`), never a borrowed `&mut B`
//! — the Rhai closures registered onto the engine each hold a cloned `Arc` so they can push
//! `Intent`s without borrowing the strategy itself.

use std::collections::HashSet;
use std::sync::{Arc, RwLock};
use vike_indicators::Indicator;
use vike_model::Bar;

/// One order-verb call recorded by a Rhai script during a bar's evaluation. Pure data — no
/// broker/strategy wiring yet; later tasks drain `ScriptCtx::intents` into real orders.
#[derive(Debug, Clone, PartialEq)]
pub enum Intent {
    Market { side: i32, qty: f64 },
    Limit { side: i32, qty: f64, price: f64 },
}

/// Owned by the strategy, captured (cloned Arc) by the engine's host fns. Not a borrowed `&mut B`.
/// `cur_bar`/`position`/`price`/`equity`/`index`/`now` are read by `engine::register_reads`,
/// `intents` is written by `engine::register_verbs`, and `indicators`/`fed_this_bar` are read/fed
/// by `engine::register_indicators` (`indicator_value`) — every field is live in the plain lib
/// build today.
pub(crate) struct ScriptCtx {
    // reads snapshotted before each eval:
    pub cur_bar: Bar,
    pub position: f64,
    pub price: f64,
    pub equity: f64,
    pub index: i64,
    pub now: i64,
    // Streaming indicator cache, fed once per bar on reference. Keyed on the instance's real
    // identity — `"<name>:<coerced params>"`, e.g. `"sma:[20.0]"` — so `sma(5)` and `sma(30)` are
    // two instances while `sma()` and `sma(20)` are one (the registry default coerces to the same
    // slice). `fed_this_bar` holds the same keys; see `engine::indicator_value`.
    pub indicators: indexmap::IndexMap<String, Box<dyn Indicator + Send>>,
    // USER-written indicators (`user_data/indicators/`), keyed by name and held CONCRETE rather
    // than boxed into `indicators` above — `engine::register_user_indicators` explains why (the
    // per-bar fault has to be readable back out, and `Indicator` has no downcast). The key is
    // `"<name>:<call-site args>"` — the same (identity, not spelling) rule the built-ins use, so
    // every OUTPUT LINE of one indicator shares one entry and one feed; `fed_this_bar` holds the
    // same key under a `user:` prefix.
    pub user_indicators: indexmap::IndexMap<String, crate::RhaiIndicator>,
    pub fed_this_bar: HashSet<String>,
    // output:
    pub intents: Vec<Intent>,
    // sweepable params: `overrides` injected before compile's one-time top-level run; `param()`
    // records each (name, default) into `params_seen` (first-seen wins) for discovery. Neither is
    // touched by compile's intent/indicator clear — a swept value must persist and discovery reads
    // params_seen after compile.
    pub overrides: indexmap::IndexMap<String, f64>,
    pub params_seen: indexmap::IndexMap<String, f64>,
}

pub(crate) type SharedCtx = Arc<RwLock<ScriptCtx>>;

impl ScriptCtx {
    /// Constructs a fresh, zeroed `SharedCtx` — called once per [`crate::RhaiStrategy::compile`].
    pub fn new() -> SharedCtx {
        Arc::new(RwLock::new(ScriptCtx {
            cur_bar: zero_bar(),
            position: 0.0,
            price: 0.0,
            equity: 0.0,
            index: 0,
            now: 0,
            indicators: indexmap::IndexMap::new(),
            user_indicators: indexmap::IndexMap::new(),
            fed_this_bar: HashSet::new(),
            intents: Vec::new(),
            overrides: indexmap::IndexMap::new(),
            params_seen: indexmap::IndexMap::new(),
        }))
    }
}

pub(crate) fn zero_bar() -> Bar {
    Bar {
        ts: 0,
        open: 0.0,
        high: 0.0,
        low: 0.0,
        close: 0.0,
        volume: 0.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}
