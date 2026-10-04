//! The shared feed-config gate for the venue REAL-mark price streams (mark-slot semantics, law-map
//! A1): whether a perp bar subscription on a mark-capable venue (binance/aster family, bybit, okx,
//! hyperliquid) ALSO opens that venue's mark-price stream, feeding `LiveDataSink::mark_tick` —
//! the `PriceBoard` MARK slot, the head of the valuation resolver chain.
//!
//! Default **ON** for the venues with DOCUMENTED mark-wire shapes (binance/bybit/okx/hyperliquid):
//! the behavior change is the point — perp valuation keys off the venue's real mark where one is
//! streamed, not the candle close.
//!
//! Each venue's `venue.<venue>.mark_streams` row decides (decision 0095 retired `VIKE_MARK_STREAMS`
//! and its per-venue twin); a venue whose mark-wire grammar is unverified ships default-OFF (Aster,
//! whose `@markPrice@1s` is assumed from the binance-family charter but never observed live), every
//! other default-ON, and a root reads the row once and hands it to the feed
//! ([`mark_streams_from`]). Enable Aster only after a market-data smoke on the prod rigs confirms
//! the frame shape.
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
/// production; this type is ungated and deliberately names no vike-data type — the `full`-gated
/// [`unsubscribe_with_mark`] is where it is bound to `vike_data`'s).
///
/// Contract, in the order a venue calls it:
/// 1. `subscribe_bars` spawns the bars feed, then calls [`Self::attach`]. `false` back means "no
///    stream runs for this symbol yet — spawn one and hand me its id via [`Self::record`]";
///    `true` means an existing stream was reference-counted and NOTHING should be spawned.
/// 2. `unsubscribe` calls [`Self::detach`], which returns `Some(mark_id)` only when the last
///    reference dropped — that id is the one to stop+join ([`unsubscribe_with_mark`] does both).
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
    /// `enabled` folds in the venue's own pairing predicate (the perp tag AND the venue's
    /// `mark_streams` row): `false` records nothing and always returns `true`, so a caller that
    /// spawns only on `false` spawns nothing for a spot symbol or a disabled knob.
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

/// The body of `DataClient::unsubscribe` for a feed that keeps its threads in a
/// [`vike_data::FeedRegistry`] and its mark streams in a [`MarkPairings`]: stop + JOIN exactly the
/// stream `id` names, plus its companion perp mark stream when `id` was the LAST bars subscription
/// holding that symbol's mark stream; every other subscription on the feed keeps running. Unknown ids
/// (already stopped, never issued) are a no-op ([`MarkPairings::detach`] and
/// [`vike_data::FeedRegistry::stop_join`] both say so).
///
/// Used by bybit, binance, okx and hyperliquid. Aster is NOT a caller: its `Feeds` keeps its own
/// `(stop flag, JoinHandle)` map rather than a registry, so its `unsubscribe` is a different body.
/// `full`-gated because it names vike-data's types (the type-level note on [`MarkPairings`]).
#[cfg(feature = "full")]
pub fn unsubscribe_with_mark(
    pairings: &mut MarkPairings<vike_data::SubscriptionId>,
    registry: &mut vike_data::FeedRegistry,
    id: vike_data::SubscriptionId,
) {
    if let Some(mark_id) = pairings.detach(id) {
        registry.stop_join(mark_id);
    }
    registry.stop_join(id);
}

/// Whether a perp bar subscription also opens the venue's mark-price stream, from that venue's
/// `venue.<venue>.mark_streams` row (decision 0095): the exact `"1"` (trimmed) turns it on, the
/// exact `"0"` off, and anything else — no row included — keeps the venue's charter default. PURE:
/// the composition root reads the row and hands it to the feed (`Feeds::with_mark_streams`).
pub fn mark_streams_from(stored: Option<&str>, default_on: bool) -> bool {
    match stored.map(str::trim) {
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

    /// Decision 0095: a venue's `mark_streams` row decides — exact `1` on, exact `0` off — and no
    /// row (or anything else) keeps that venue's charter default.
    #[test]
    fn mark_streams_from_is_the_row_else_the_charter_default() {
        for default_on in [true, false] {
            assert!(mark_streams_from(Some("1"), default_on));
            assert!(!mark_streams_from(Some("0"), default_on));
            assert!(mark_streams_from(Some(" 1 "), default_on));
            assert_eq!(mark_streams_from(None, default_on), default_on, "no row keeps the default");
            assert_eq!(mark_streams_from(Some("true"), default_on), default_on);
        }
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

    /// `enabled == false` (spot symbol, or a `mark_streams = 0` row) must record nothing and must
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

    /// `unsubscribe_with_mark` stops AND JOINS the stream named, and the companion mark stream only
    /// when that was its LAST bars subscription. Each feed body records that it ran to its end, which
    /// — once `stop_join` has returned — proves the thread was joined rather than merely flagged.
    #[cfg(feature = "full")]
    #[test]
    fn unsubscribe_joins_the_stream_and_the_mark_stream_only_with_its_last_holder() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::time::Duration;
        use vike_data::{FeedRegistry, SubscriptionId};

        let spawn = |reg: &mut FeedRegistry| -> (SubscriptionId, Arc<AtomicBool>) {
            let done = Arc::new(AtomicBool::new(false));
            let finished = Arc::clone(&done);
            let id = reg
                .spawn("vike-test-feed".to_string(), move |stop| {
                    while !stop.load(Ordering::Relaxed) {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    finished.store(true, Ordering::Relaxed);
                })
                .expect("spawn a test feed thread");
            (id, done)
        };
        let mut reg = FeedRegistry::new();
        let mut pairings: MarkPairings<SubscriptionId> = MarkPairings::default();
        let (bars_1m, done_1m) = spawn(&mut reg);
        let (bars_5m, done_5m) = spawn(&mut reg);
        let (mark, done_mark) = spawn(&mut reg);
        // two bars subscriptions on ONE perp share one mark stream
        assert!(!pairings.attach(bars_1m, "BTCUSDT.P", true));
        pairings.record(bars_1m, "BTCUSDT.P", mark);
        assert!(pairings.attach(bars_5m, "BTCUSDT.P", true));

        unsubscribe_with_mark(&mut pairings, &mut reg, bars_1m);
        assert!(done_1m.load(Ordering::Relaxed), "the stream named is joined");
        assert!(!done_mark.load(Ordering::Relaxed), "the 5m chart still holds the mark stream");
        assert!(!pairings.is_empty());

        unsubscribe_with_mark(&mut pairings, &mut reg, bars_5m);
        assert!(done_5m.load(Ordering::Relaxed));
        assert!(
            done_mark.load(Ordering::Relaxed),
            "the last holder stops and joins the mark stream"
        );
        assert!(pairings.is_empty());
    }
}
