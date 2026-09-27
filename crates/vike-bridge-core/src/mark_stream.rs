//! The shared feed-config gate for the venue REAL-mark price streams (mark-slot semantics, law-map
//! A1): whether a perp bar subscription on a mark-capable venue (binance/aster family, bybit, okx,
//! hyperliquid) ALSO opens that venue's mark-price stream, feeding `LiveDataSink::mark_tick` —
//! the `PriceBoard` MARK slot, the head of the valuation resolver chain.
//!
//! Default **ON** for the venues with DOCUMENTED mark-wire shapes (binance/bybit/okx/hyperliquid):
//! the behavior change is the point — perp valuation keys off the venue's real mark where one is
//! streamed, not the candle close. `VIKE_MARK_STREAMS=0` — the exact string, matching
//! `VIKE_RECONCILE`'s no-fuzzy-parse rule — is the MASTER KILL that disables every venue's mark
//! pump in one place, and it wins absolutely. Read off the REAL process env (`std::env::var`),
//! never the credentials workspace `.env`: this is a feed-shape knob, not a secret.
//!
//! A venue whose mark-wire grammar is UNVERIFIED against its own live feed ships default-**OFF**
//! (currently Aster, whose `@markPrice@1s` is assumed from the binance-family charter but never
//! observed live). Such a venue resolves through [`mark_streams_enabled_for`], which layers a
//! per-venue override `VIKE_MARK_STREAMS_<VENUE>` (exact `"1"` opts in, exact `"0"` opts out) on
//! top of the master kill, falling back to the venue's own charter default. Enable Aster only
//! after a market-data smoke on the prod rigs confirms the frame shape.
//!
//! [`MarkPairings`] is the shared bookkeeping every one of those venues uses to attach the mark
//! stream to its bars subscriptions: a mark stream is **per SYMBOL**, not per subscription, so
//! charting the same perp at two intervals (a 1m and a 5m `subscribe_bars`) reference-counts ONE
//! mark socket instead of opening two. The stream is stopped when the LAST bars subscription
//! holding it goes away.

use std::collections::HashMap;
use std::hash::Hash;

/// Per-symbol reference-counted registry of companion mark streams, shared by every mark-capable
/// venue feed (`Id` is the venue's subscription-id type — `vike_data::SubscriptionId` in
/// production; this crate deliberately does not depend on vike-data).
///
/// Contract, in the order a venue calls it:
/// 1. `subscribe_bars` spawns the bars feed, then calls [`Self::attach`]. `false` back means "no
///    stream runs for this symbol yet — spawn one and hand me its id via [`Self::record`]";
///    `true` means an existing stream was reference-counted and NOTHING should be spawned.
/// 2. `unsubscribe` calls [`Self::detach`], which returns `Some(mark_id)` only when the last
///    reference dropped — that id is the one to stop+join.
/// 3. `shutdown` calls [`Self::clear`] (the registry stops+joins every thread wholesale).
///
/// A spawn that FAILS simply never calls `record`: the symbol stays unregistered, the bars feed
/// stays live, and valuation falls back to the resolver's bar-close rung — the same fail-soft
/// outcome as a venue with no mark stream at all.
#[derive(Debug)]
pub struct MarkPairings<Id> {
    /// series symbol -> (the running mark stream's id, how many bars subscriptions hold it)
    by_symbol: HashMap<String, (Id, usize)>,
    /// bars subscription id -> the series symbol whose mark stream it holds a reference to
    by_bars_id: HashMap<Id, String>,
}

impl<Id> Default for MarkPairings<Id> {
    fn default() -> Self {
        MarkPairings { by_symbol: HashMap::new(), by_bars_id: HashMap::new() }
    }
}

impl<Id: Copy + Eq + Hash> MarkPairings<Id> {
    /// Reference `symbol`'s mark stream on behalf of `bars_id`. Returns `true` when a stream was
    /// ALREADY running (refcount bumped, caller spawns nothing) and `false` when the caller must
    /// spawn one and report it back through [`Self::record`].
    ///
    /// `enabled` folds in the venue's own pairing predicate (perp tag AND `VIKE_MARK_STREAMS`):
    /// `false` records nothing and always returns `true`, so a caller that spawns only on `false`
    /// spawns nothing for a spot symbol or a disabled knob.
    pub fn attach(&mut self, bars_id: Id, symbol: &str, enabled: bool) -> bool {
        if !enabled {
            return true;
        }
        match self.by_symbol.get_mut(symbol) {
            Some((_, refs)) => {
                *refs += 1;
                self.by_bars_id.insert(bars_id, symbol.to_string());
                true
            }
            None => false,
        }
    }

    /// Register the mark stream `bars_id` just spawned for `symbol` (refcount 1). Only ever called
    /// after an [`Self::attach`] returned `false`.
    pub fn record(&mut self, bars_id: Id, symbol: &str, mark_id: Id) {
        self.by_symbol.insert(symbol.to_string(), (mark_id, 1));
        self.by_bars_id.insert(bars_id, symbol.to_string());
    }

    /// Drop `bars_id`'s reference. `Some(mark_id)` — and only then — when that was the LAST
    /// reference, meaning the caller must stop+join the mark stream. An unknown id is a no-op.
    pub fn detach(&mut self, bars_id: Id) -> Option<Id> {
        let symbol = self.by_bars_id.remove(&bars_id)?;
        let (mark_id, refs) = self.by_symbol.get_mut(&symbol)?;
        let mark_id = *mark_id;
        *refs -= 1;
        if *refs == 0 {
            self.by_symbol.remove(&symbol);
            return Some(mark_id);
        }
        None
    }

    /// Forget everything (wholesale `shutdown`, where the feed registry joins every thread anyway).
    pub fn clear(&mut self) {
        self.by_symbol.clear();
        self.by_bars_id.clear();
    }

    /// The mark stream currently running for `symbol`, if any — test/introspection only.
    pub fn mark_id_of(&self, symbol: &str) -> Option<Id> {
        self.by_symbol.get(symbol).map(|(id, _)| *id)
    }

    /// How many distinct mark streams are running.
    pub fn len(&self) -> usize {
        self.by_symbol.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_symbol.is_empty()
    }
}

/// Whether perp bar subscriptions should also open the venue's real mark-price stream.
/// Reads the real process env on each call (cheap — called once per `subscribe_bars`).
pub fn mark_streams_enabled() -> bool {
    decide(std::env::var("VIKE_MARK_STREAMS").ok().as_deref())
}

/// The pure decision: only the EXACT string `"0"` disables (unset/anything else stays ON —
/// default-on is deliberate, see the module doc).
fn decide(raw: Option<&str>) -> bool {
    raw != Some("0")
}

/// Per-venue mark-stream enablement layered on the global gate, for a venue whose charter default
/// is not simply "ON". The master kill `VIKE_MARK_STREAMS=0` wins absolutely (every venue OFF);
/// otherwise a per-venue override `VIKE_MARK_STREAMS_<VENUE>` (exact `"1"` ON, exact `"0"` OFF)
/// decides, and unset falls back to the venue's own `default_on`. `venue` is upper-cased for the
/// env-var name (e.g. `"aster"` → `VIKE_MARK_STREAMS_ASTER`). Read on each `subscribe_bars` build
/// — cheap, and never a secret. See the module doc for why Aster ships `default_on = false`.
pub fn mark_streams_enabled_for(venue: &str, default_on: bool) -> bool {
    let global = std::env::var("VIKE_MARK_STREAMS").ok();
    let per_venue = std::env::var(format!("VIKE_MARK_STREAMS_{}", venue.to_uppercase())).ok();
    decide_for(global.as_deref(), per_venue.as_deref(), default_on)
}

/// The pure per-venue decision (env read out for testability). Master kill first, then the
/// per-venue exact-match override, then the charter default.
fn decide_for(global: Option<&str>, per_venue: Option<&str>, default_on: bool) -> bool {
    if global == Some("0") {
        return false; // master kill is absolute
    }
    match per_venue {
        Some("1") => true,
        Some("0") => false,
        _ => default_on,
    }
}

/// The shared dead-price guard every venue's mark decoder applies before emitting: a mark must be
/// finite and strictly positive (mirrors the trades feeds' `is_valid_trade` — a 0/NaN/∞ mark
/// would poison the `PriceBoard` mark slot every decision path resolves through first).
pub fn is_valid_mark(px: f64) -> bool {
    px.is_finite() && px > 0.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_exact_zero_string_disables() {
        assert!(decide(None), "unset -> ON (default-on is the contract)");
        assert!(!decide(Some("0")), "the exact \"0\" disables");
        // No fuzzy truthiness — same rule as VIKE_RECONCILE's exact-match gate.
        for v in ["false", "no", "off", "1", "true", ""] {
            assert!(decide(Some(v)), "{v:?} is not the exact \"0\" -> stays ON");
        }
    }

    /// The per-venue resolver (`decide_for`) for a venue whose charter default is OFF (Aster):
    /// unset -> OFF, the exact `"1"` opts in, the exact `"0"` opts out, and the master kill wins
    /// over everything.
    #[test]
    fn per_venue_default_off_needs_an_explicit_opt_in() {
        // unset per-venue -> the charter default (OFF for a default-off venue)
        assert!(!decide_for(None, None, false), "default-off venue stays OFF unless opted in");
        // exact-match override, no fuzzy truthiness
        assert!(decide_for(None, Some("1"), false), "VIKE_MARK_STREAMS_<V>=1 opts in");
        assert!(!decide_for(None, Some("0"), false), "VIKE_MARK_STREAMS_<V>=0 opts out");
        for v in ["true", "yes", "on", ""] {
            assert!(!decide_for(None, Some(v), false), "{v:?} is not the exact \"1\" -> stays OFF");
        }
        // the master kill is absolute — it disables even an explicit per-venue opt-in
        assert!(!decide_for(Some("0"), Some("1"), false), "master kill overrides the opt-in");
        assert!(
            !decide_for(Some("0"), Some("1"), true),
            "master kill overrides a default-on venue"
        );
    }

    /// The same resolver for a default-ON venue: unset stays ON, and the per-venue `"0"` can still
    /// disable one venue without touching the others.
    #[test]
    fn per_venue_default_on_can_be_disabled_individually() {
        assert!(decide_for(None, None, true), "default-on venue stays ON when nothing is set");
        assert!(
            !decide_for(None, Some("0"), true),
            "VIKE_MARK_STREAMS_<V>=0 disables just this one"
        );
        assert!(decide_for(None, Some("1"), true), "explicit opt-in is redundant but valid");
    }

    /// The per-symbol dedupe: two bars subscriptions on the SAME perp (a 1m and a 5m chart) share
    /// ONE mark stream, and it is stopped only when the second one goes.
    #[test]
    fn two_intervals_on_one_symbol_share_a_single_mark_stream() {
        let mut p: MarkPairings<u64> = MarkPairings::default();
        // 1m chart: nothing running yet -> the caller spawns
        assert!(!p.attach(1, "BTCUSDT.P", true));
        p.record(1, "BTCUSDT.P", 100);
        // 5m chart on the same symbol: refcounted, caller spawns NOTHING
        assert!(p.attach(2, "BTCUSDT.P", true), "an existing stream must be reused");
        assert_eq!(p.len(), 1, "exactly one mark stream for the symbol");
        assert_eq!(p.mark_id_of("BTCUSDT.P"), Some(100));
        // dropping the 1m chart must NOT stop the stream the 5m chart still needs
        assert_eq!(p.detach(1), None);
        assert_eq!(p.mark_id_of("BTCUSDT.P"), Some(100));
        // the last reference releases it
        assert_eq!(p.detach(2), Some(100));
        assert!(p.is_empty());
    }

    #[test]
    fn distinct_symbols_get_distinct_streams() {
        let mut p: MarkPairings<u64> = MarkPairings::default();
        assert!(!p.attach(1, "BTCUSDT.P", true));
        p.record(1, "BTCUSDT.P", 100);
        assert!(!p.attach(2, "ETHUSDT.P", true), "a different symbol needs its own stream");
        p.record(2, "ETHUSDT.P", 200);
        assert_eq!(p.len(), 2);
        assert_eq!(p.detach(1), Some(100));
        assert_eq!(p.mark_id_of("ETHUSDT.P"), Some(200), "the sibling stream survives");
    }

    /// `enabled == false` (spot symbol, or `VIKE_MARK_STREAMS=0`) must record nothing and must
    /// report "already handled" so the caller's spawn branch never runs.
    #[test]
    fn disabled_pairing_never_spawns_and_never_registers() {
        let mut p: MarkPairings<u64> = MarkPairings::default();
        assert!(p.attach(1, "BTCUSDT", false), "disabled -> caller must not spawn");
        assert!(p.is_empty());
        assert_eq!(p.detach(1), None, "nothing to release");
    }

    #[test]
    fn unknown_and_repeated_detach_are_no_ops() {
        let mut p: MarkPairings<u64> = MarkPairings::default();
        assert_eq!(p.detach(42), None);
        assert!(!p.attach(1, "S.P", true));
        p.record(1, "S.P", 9);
        assert_eq!(p.detach(1), Some(9));
        assert_eq!(p.detach(1), None, "a second detach of the same id releases nothing");
    }

    #[test]
    fn clear_forgets_every_pairing() {
        let mut p: MarkPairings<u64> = MarkPairings::default();
        assert!(!p.attach(1, "S.P", true));
        p.record(1, "S.P", 9);
        p.clear();
        assert!(p.is_empty());
        assert_eq!(p.detach(1), None);
    }

    #[test]
    fn mark_validity_guard() {
        assert!(is_valid_mark(100.5));
        assert!(!is_valid_mark(0.0));
        assert!(!is_valid_mark(-1.0));
        assert!(!is_valid_mark(f64::NAN));
        assert!(!is_valid_mark(f64::INFINITY));
    }
}
