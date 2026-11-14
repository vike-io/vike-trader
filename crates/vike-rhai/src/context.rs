//! Owned strategy-script state: `ScriptContext`, the shared, lock-guarded state the Rhai engine's host
//! functions (see `engine/` and `crate::tactics`) read/mutate. `ScriptContext` is OWNED by the strategy
//! (via `SharedContext = Arc<RwLock<ScriptContext>>`), never a borrowed `&mut B` — the Rhai closures
//! registered onto the engine each hold a cloned `Arc` so they can record calls without borrowing
//! the strategy itself.

use std::collections::HashSet;
use std::sync::{Arc, RwLock};
use vike_indicators::Indicator;
use vike_marketdata::Bar;

/// Owned by the strategy, captured (cloned Arc) by the engine's host fns. Not a borrowed `&mut B`.
/// `current_bar`/`position`/`price`/`equity`/`index`/`now` are read by `engine::host::register_reads`,
/// `indicators`/`fed_this_bar` are read/fed by `engine::builtin::register_indicators`
/// (`indicator_value`), and `calls`/`reads`/`reads_only` are the `strategy` object's
/// (`crate::tactics`) — every field is live in the plain lib build today.
pub(crate) struct ScriptContext {
    // reads snapshotted before each eval:
    pub current_bar: Bar,
    pub position: f64,
    pub price: f64,
    pub equity: f64,
    pub index: i64,
    pub now: i64,
    // Streaming indicator cache, fed once per bar on reference. Keyed on the instance's real
    // identity — `"<name>:<coerced params>"`, e.g. `"sma:[20.0]"` — so `sma(5)` and `sma(30)` are
    // two instances while `sma()` and `sma(20)` are one (the registry default coerces to the same
    // slice). `fed_this_bar` holds the same keys; see `engine::builtin::indicator_value`.
    pub indicators: indexmap::IndexMap<String, Box<dyn Indicator + Send>>,
    // USER-written indicators (`user_data/indicators/`), keyed by name and held CONCRETE rather
    // than boxed into `indicators` above — `engine::user::register_user_indicators` explains why (the
    // per-bar fault has to be readable back out, and `Indicator` has no downcast). The key is
    // `"<name>:<call-site args>"` — the same (identity, not spelling) rule the built-ins use, so
    // every OUTPUT LINE of one indicator shares one entry and one feed; `fed_this_bar` holds the
    // same key under a `user:` prefix.
    pub user_indicators: indexmap::IndexMap<String, crate::RhaiIndicator>,
    pub fed_this_bar: HashSet<String>,
    // the `strategy` object (`crate::tactics`): the verbs a hook recorded, replayed onto the
    // language when it returns, and the reads it answers, taken as it started.
    pub calls: Vec<crate::tactics::Call>,
    pub reads: crate::tactics::Reads,
    // the hook running is `on_stop(strategy)`: its reads answer, its verbs are refused by name
    pub reads_only: bool,
    // sweepable params: `overrides` injected before compile's one-time top-level run; `param()`
    // records each (name, default) into `params_seen` (first-seen wins) for discovery. Neither is
    // touched by compile's indicator clear — a swept value must persist and discovery reads
    // params_seen after compile.
    pub overrides: indexmap::IndexMap<String, f64>,
    pub params_seen: indexmap::IndexMap<String, f64>,
}

pub(crate) type SharedContext = Arc<RwLock<ScriptContext>>;

impl ScriptContext {
    /// Constructs a fresh, zeroed `SharedContext` — called once per [`crate::RhaiStrategy::compile`].
    pub(crate) fn new() -> SharedContext {
        Arc::new(RwLock::new(ScriptContext {
            current_bar: Bar::ohlcv(0, 0.0, 0.0, 0.0, 0.0, 0.0),
            position: 0.0,
            price: 0.0,
            equity: 0.0,
            index: 0,
            now: 0,
            indicators: indexmap::IndexMap::new(),
            user_indicators: indexmap::IndexMap::new(),
            fed_this_bar: HashSet::new(),
            calls: Vec::new(),
            reads: crate::tactics::Reads::default(),
            reads_only: false,
            overrides: indexmap::IndexMap::new(),
            params_seen: indexmap::IndexMap::new(),
        }))
    }
}
