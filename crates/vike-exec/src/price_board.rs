//! PriceBoard — per-(venue, symbol) multi-source price cells + the side-aware read-side
//! resolver (portfolio-observer PR-1; Rust-native surface, no Python twin; chain semantics
//! source-verified against NautilusTrader `crates/portfolio` `get_price` 2026-07-12).
//!
//! WRITE side (hot fold): the five existing mark write-sites additionally store `(px, ts)`
//! into the matching source slot — plain field stores, no logging, no reads. Non-positive
//! or NaN prices are ignored (the same `> 0.0` guard the `set_mark` call sites use).
//! READ side (cold paths only — snapshot publish, sweeps, timers): `resolve` walks
//! fresh mark → Bid for longs / Ask for shorts (conservative liquidation-side valuation) →
//! last trade → bar close; `note` maintains the per-venue missing-price set with a
//! warn-once-per-episode discipline. Deliberately NOT serialized: the board sits outside
//! `EngineSnapshot`, so the journal determinism-fence hash is untouched.
//!
//! # THE VALUATION LAW — what each slot means and who fills it
//!
//! The chain is ordered by how closely a price answers "what is this position worth to me RIGHT
//! NOW, if I had to act on it". A slot is consulted only when every slot above it is absent or
//! stale, so the ordering IS the law:
//!
//! | rung | slot        | meaning                                                        | producers |
//! |------|-------------|----------------------------------------------------------------|-----------|
//! | 1    | `mark`      | the VENUE's own mark/index price — the number its liquidation and funding engines key off. The only price that answers "will I be liquidated". | crypto perps: binance-family `@markPrice@1s`, bybit `tickers.markPrice`, okx `mark-price`, hyperliquid `activeAssetCtx.markPx` — each default ON; aster `@markPrice@1s` is default OFF (its grammar is unverified against Aster's own feed — opt in with `VIKE_MARK_STREAMS_ASTER=1`). All off at once via the master kill `VIKE_MARK_STREAMS=0`. ALSO, on ANY venue and regardless of those knobs: a fill's `mark_price` and a reconcile pass's `ExecReport::position_mark_px` (`apply_snapshot`) — both are the venue's own mark, just event-sampled instead of streamed |
//! | 2    | `bid`/`ask` | the live top of book, read SIDE-AWARE: a LONG is valued at the BID and a SHORT at the ASK, because that is the side it would have to cross to exit. Conservative by construction — it books the spread against the position, never for it. | any venue with a quote/BBO or L2 feed subscribed (`set_quote`, from both the quote lane and a two-sided book top) |
//! | 3    | `last_trade`| the last price something actually traded at. Real and recent, but one-sided and already stale by the time it lands. | any venue with a trade feed subscribed — including venues subscribed only because a chart asked for one (`ensure_trade_feed_on`) |
//! | 4    | `bar_close` | the close of the last completed candle. The LOWEST rung by design: it is an aggregate of a window that has already ended, so on a 1m chart it can be a full minute old, and on a 1h chart an hour. It is the fallback of last resort, not a mark. | EVERY kline feed, every venue — binance/aster/bybit/okx/hyperliquid/alpaca/ibkr |
//! | 5    | LAST-KNOWN | not a slot — the SAME four rungs walked a SECOND time with the freshness gate RELAXED, taking the highest-priority slot that holds ANY value however stale. The opt-in floor under the chain (`PriceCfg::stale_fallback`, OFF by default): the answer to "we lost every fresh feed — do you want the last price we saw, or nothing?". Surfaced as [`Resolution::Stale`], never as a fresh [`Resolution::Priced`], so a consumer valuing off it KNOWS it is stale. | the freshest surviving slot of rungs 1-4 |
//!
//! The last-known rung ONLY changes behavior when a caller has BOTH set a freshness window
//! (rung 1-4 all aged out) AND opted into `stale_fallback`; with the permissive default cfg no
//! slot is ever stale, so rung 5 is unreachable and [`PriceBoard::resolve`] is byte-identical to
//! before it existed. A fresh venue mark still early-returns at rung 1 — the common case pays
//! nothing. [`PriceBoard::classify`] is the ONE place fresh-vs-stale-vs-missing is decided;
//! `resolve` is a thin projection of it, and the missing/stale operator queries read it too.
//!
//! A candle close sitting at the TAIL rather than the head is the deliberate correction the
//! mark-slot split (W2-T4) made: before it, every kline feed wrote its close into the `mark` slot,
//! so a stale candle OUTRANKED a live quote and a live trade on the same symbol. The consequence
//! is a real, intended behavior change on every venue that has no venue mark but does have a
//! quote or trade feed running (alpaca, IBKR, binance spot, an aster perp not opted into its
//! mark stream, and any perp with `VIKE_MARK_STREAMS=0`): those positions are now valued at the
//! live side-aware quote or the
//! last trade instead of the candle close. Valuation moves by up to the spread, and it moves
//! toward the price the position could actually be exited at.
//!
//! Freshness is per-rung and OFF by default ([`PriceCfg`] max-age fields are all `None`), so a
//! caller that has not opted into a window sees the pure priority order.
//!
//! # The board is NOT the account mark slot
//!
//! Every write here is UNCONDITIONAL: a slot is source-tagged by construction, so a candle close
//! and a venue mark can both land without either destroying the other. That is exactly why the
//! precedence question does not arise on this side.
//!
//! `Account.marks` is the opposite shape — ONE untagged scalar per symbol, written by the same
//! lanes, read by the pre-trade gate / margin-call law / `LiveBroker.price`. Its precedence rule
//! lives in [`crate::MarkSource`] and is enforced inside `Account::set_mark_from`, the only door
//! into that map. Do not reimplement it here: a freshness predicate on this board is how round 1
//! of that fix ended up bypassable by every lane that did not happen to call it.

use indexmap::{IndexMap, IndexSet};

/// One (venue, symbol)'s last-known price per source. `(px, ts)` — ts is the producer's
/// timestamp (ms) where the lane carries one, else the dispatch clock.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct PriceCell {
    pub mark: Option<(f64, i64)>,
    pub bid: Option<(f64, i64)>,
    pub ask: Option<(f64, i64)>,
    pub last_trade: Option<(f64, i64)>,
    pub bar_close: Option<(f64, i64)>,
}

/// Which source priced a resolution — surfaced to PR-2 snapshot views for display.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PriceSource {
    Mark,
    Bid,
    Ask,
    LastTrade,
    BarClose,
}

/// Outcome of a read-side price resolution. `Missing` is a first-class answer — callers
/// decide skip/zero (today's semantics) and feed it back through [`PriceBoard::note`].
///
/// `Stale` is the last-known floor (rung 5): a real price from `source` that is past its
/// freshness window, returned ONLY when the caller opted into [`PriceCfg::stale_fallback`]. It
/// is DISTINCT from `Priced` on purpose — a consumer valuing a position must be able to tell a
/// fresh price from a stale one — but it carries the same `(px, source, ts)` so a caller that
/// treats "any price beats zero" can use it uniformly. With `stale_fallback` off (the default)
/// `resolve` never yields `Stale`, so the two-variant world is byte-identical.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Resolution {
    Priced { px: f64, source: PriceSource, ts: i64 },
    Stale { px: f64, source: PriceSource, ts: i64 },
    Missing,
}

/// Freshness classification of a (venue, symbol) cell — the ONE decision [`PriceBoard::classify`]
/// makes, that `resolve` and the missing/stale operator queries all project from. Unlike
/// [`Resolution`] it is independent of [`PriceCfg::stale_fallback`]: it ALWAYS reports whether the
/// best available price is fresh, stale, or absent, so an operator query can see "priced but stale"
/// even when valuation is not configured to fall back to it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MarkStatus {
    /// A price within its freshness window (rungs 1-4 respecting `*_max_age_ms`).
    Fresh { px: f64, source: PriceSource, ts: i64 },
    /// No fresh price, but a last-known value survives on some slot (rung 5). `age_ms` is how far
    /// past `now_ms` the value's `ts` sits (≥ 0; 0 for a value stamped in the future).
    Stale { px: f64, source: PriceSource, ts: i64, age_ms: i64 },
    /// No usable price at all — the cell is unknown or every slot is empty/dead.
    Missing,
}

/// Resolver knobs. Defaults are permissive (mark enabled, no freshness limits) so adopting
/// the resolver is behavior-preserving until a caller opts into tighter windows.
#[derive(Debug, Clone, Copy)]
pub struct PriceCfg {
    pub use_mark: bool,
    pub mark_max_age_ms: Option<i64>,
    pub quote_max_age_ms: Option<i64>,
    pub trade_max_age_ms: Option<i64>,
    pub bar_max_age_ms: Option<i64>,
    /// Opt-in last-known floor (rung 5). When `true`, a resolution that would otherwise be
    /// [`Resolution::Missing`] because every slot aged out of its freshness window instead returns
    /// [`Resolution::Stale`] with the freshest surviving slot's price. `false` (the default) keeps
    /// today's behavior exactly — a stale-everywhere cell resolves `Missing`. INERT under the
    /// permissive default cfg (no freshness windows ⇒ nothing is ever stale ⇒ rung 5 unreachable).
    pub stale_fallback: bool,
}

impl Default for PriceCfg {
    fn default() -> Self {
        PriceCfg {
            use_mark: true,
            mark_max_age_ms: None,
            quote_max_age_ms: None,
            trade_max_age_ms: None,
            bar_max_age_ms: None,
            stale_fallback: false,
        }
    }
}

/// One resolver rung: the slot's `(px, ts)`, its freshness window, and the source it labels.
type Rung = (Option<(f64, i64)>, Option<i64>, PriceSource);

#[inline]
fn fresh(slot: Option<(f64, i64)>, max_age: Option<i64>, now_ms: i64) -> Option<(f64, i64)> {
    let (px, ts) = slot?;
    match max_age {
        Some(a) if now_ms - ts > a => None,
        _ => Some((px, ts)),
    }
}

#[derive(Debug, Default)]
pub struct PriceBoard {
    /// (venue, symbol) -> cell, insertion-ordered like every other venue-keyed map here.
    cells: IndexMap<(String, String), PriceCell>,
    /// venue -> symbols that failed to resolve (maintained by `note`, Task 2).
    missing: IndexMap<String, IndexSet<String>>,
}

impl PriceBoard {
    /// `false` for zero, negative, and NaN — the shared dead-price guard.
    #[inline]
    fn live(px: f64) -> bool {
        px > 0.0
    }

    #[inline]
    fn cell_mut(&mut self, venue: &str, symbol: &str) -> &mut PriceCell {
        self.cells.entry((venue.to_string(), symbol.to_string())).or_default()
    }

    pub fn set_mark(&mut self, venue: &str, symbol: &str, px: f64, ts: i64) {
        if Self::live(px) {
            self.cell_mut(venue, symbol).mark = Some((px, ts));
        }
    }

    /// Both quote sides in one call; a dead side (0/neg/NaN) leaves its slot untouched.
    pub fn set_quote(&mut self, venue: &str, symbol: &str, bid: f64, ask: f64, ts: i64) {
        if !Self::live(bid) && !Self::live(ask) {
            return;
        }
        let c = self.cell_mut(venue, symbol);
        if Self::live(bid) {
            c.bid = Some((bid, ts));
        }
        if Self::live(ask) {
            c.ask = Some((ask, ts));
        }
    }

    pub fn set_last_trade(&mut self, venue: &str, symbol: &str, px: f64, ts: i64) {
        if Self::live(px) {
            self.cell_mut(venue, symbol).last_trade = Some((px, ts));
        }
    }

    pub fn set_bar_close(&mut self, venue: &str, symbol: &str, px: f64, ts: i64) {
        if Self::live(px) {
            self.cell_mut(venue, symbol).bar_close = Some((px, ts));
        }
    }

    pub fn cell(&self, venue: &str, symbol: &str) -> Option<&PriceCell> {
        // IndexMap<(String,String)> can't borrow-lookup a (&str,&str) key; iterate instead
        // of allocating — cells are few (symbols the engine accepts) and this is read-side.
        self.cells.iter().find(|((v, s), _)| v == venue && s == symbol).map(|(_, c)| c)
    }

    /// Classify a cell's best available price: fresh (within its window), stale (a last-known
    /// value survives past every window), or missing. The ONE decision site — [`Self::resolve`]
    /// projects a [`Resolution`] from it and the missing/stale operator queries read it directly,
    /// so fresh-vs-stale-vs-missing is defined in exactly one place. Walks the SAME chain
    /// `resolve` always has (mark -> Bid/Ask -> last trade -> bar close); the `Fresh` early-return
    /// on rung 1 is the common case and pays for at most one slot check, so this is as cheap on
    /// the fresh-mark path as the old inline `resolve`. Only when NO rung is fresh does it walk the
    /// four slots a second time with the freshness gate relaxed (highest-priority present slot
    /// wins) to report the last-known value. Allocation-free (`cell` is a non-allocating linear
    /// find; the rung table is a fixed stack array). COLD path only, like `resolve`.
    pub fn classify(
        &self,
        venue: &str,
        symbol: &str,
        is_long: bool,
        now_ms: i64,
        cfg: &PriceCfg,
    ) -> MarkStatus {
        let Some(c) = self.cell(venue, symbol) else {
            return MarkStatus::Missing;
        };
        let (side_slot, side_src) =
            if is_long { (c.bid, PriceSource::Bid) } else { (c.ask, PriceSource::Ask) };
        // The chain in priority order. `use_mark = false` drops the mark rung from BOTH the fresh
        // and the last-known walk, exactly as the old `resolve` never touched `mark` in that mode.
        let rungs: [Rung; 4] = [
            (if cfg.use_mark { c.mark } else { None }, cfg.mark_max_age_ms, PriceSource::Mark),
            (side_slot, cfg.quote_max_age_ms, side_src),
            (c.last_trade, cfg.trade_max_age_ms, PriceSource::LastTrade),
            (c.bar_close, cfg.bar_max_age_ms, PriceSource::BarClose),
        ];
        for (slot, max_age, source) in rungs {
            if let Some((px, ts)) = fresh(slot, max_age, now_ms) {
                return MarkStatus::Fresh { px, source, ts };
            }
        }
        // No fresh rung: the last-known floor — the highest-priority slot holding any value.
        for (slot, _max_age, source) in rungs {
            if let Some((px, ts)) = slot {
                return MarkStatus::Stale {
                    px,
                    source,
                    ts,
                    age_ms: now_ms.saturating_sub(ts).max(0),
                };
            }
        }
        MarkStatus::Missing
    }

    /// Walk the chain: fresh mark -> Bid (long) / Ask (short) -> last trade -> bar close, then
    /// (only if [`PriceCfg::stale_fallback`] is on) the last-known floor. A thin projection of
    /// [`Self::classify`]: `Fresh` -> `Priced` (byte-identical to the old resolver), `Stale` ->
    /// `Resolution::Stale` when `stale_fallback`, else `Missing` (today's behavior). Allocation-free
    /// on the priced path.
    pub fn resolve(
        &self,
        venue: &str,
        symbol: &str,
        is_long: bool,
        now_ms: i64,
        cfg: &PriceCfg,
    ) -> Resolution {
        match self.classify(venue, symbol, is_long, now_ms, cfg) {
            MarkStatus::Fresh { px, source, ts } => Resolution::Priced { px, source, ts },
            MarkStatus::Stale { px, source, ts, .. } if cfg.stale_fallback => {
                Resolution::Stale { px, source, ts }
            }
            _ => Resolution::Missing,
        }
    }

    /// Missing-price bookkeeping (Nautilus `venues_missing_price` port): a Missing inserts
    /// the symbol into the per-venue set and warns ONCE per episode (first insertion only,
    /// with the actionable Nautilus message); a Priced removes it, re-arming the warn for a
    /// future episode. COLD path only — callers are snapshot publish/sweeps, never the fold.
    pub fn note(&mut self, venue: &str, symbol: &str, res: &Resolution) {
        match res {
            Resolution::Missing => {
                let set = self.missing.entry(venue.to_string()).or_default();
                if set.insert(symbol.to_string()) {
                    tracing::warn!(
                        venue,
                        symbol,
                        "no price for open position — subscribe to quotes, trades or bars \
                         for continuous mark-to-market"
                    );
                }
            }
            // A Stale resolution is still a PRICE (a last-known value) — the position is not
            // unpriceable, so it clears the missing set exactly like a fresh Priced does. The
            // "priced but stale" distinction is surfaced by the dedicated stale-mark query, not
            // by this no-price episode set.
            Resolution::Priced { .. } | Resolution::Stale { .. } => {
                if let Some(set) = self.missing.get_mut(venue) {
                    set.shift_remove(symbol);
                }
            }
        }
    }

    /// Symbols currently unpriceable on `venue` (None = venue never had a miss).
    pub fn missing_price_instruments(&self, venue: &str) -> Option<&IndexSet<String>> {
        self.missing.get(venue)
    }
}

/// Per-position resolver result for the snapshot read-model (PR-2). NOT journaled.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ResolvedPosition {
    pub unrealized: f64,
    pub mark_source: Option<PriceSource>,
}

/// Mode-aware, resolver-priced equity for one account (PR-2 cold publish path).
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedEquity {
    pub equity: f64,
    pub unrealized_total: f64,
    pub missing: u32,
    pub per_position: Vec<ResolvedPosition>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setters_fill_the_matching_slot() {
        let mut b = PriceBoard::default();
        b.set_mark("bybit", "BTCUSDT", 50_000.0, 1_000);
        b.set_quote("bybit", "BTCUSDT", 49_999.0, 50_001.0, 1_001);
        b.set_last_trade("bybit", "BTCUSDT", 50_000.5, 1_002);
        b.set_bar_close("bybit", "BTCUSDT", 49_990.0, 1_003);
        let c = b.cell("bybit", "BTCUSDT").expect("cell exists");
        assert_eq!(c.mark, Some((50_000.0, 1_000)));
        assert_eq!(c.bid, Some((49_999.0, 1_001)));
        assert_eq!(c.ask, Some((50_001.0, 1_001)));
        assert_eq!(c.last_trade, Some((50_000.5, 1_002)));
        assert_eq!(c.bar_close, Some((49_990.0, 1_003)));
    }

    #[test]
    fn later_write_overwrites_same_slot_only() {
        let mut b = PriceBoard::default();
        b.set_mark("okx", "ETHUSDT", 3_000.0, 1);
        b.set_mark("okx", "ETHUSDT", 3_001.0, 2);
        let c = b.cell("okx", "ETHUSDT").unwrap();
        assert_eq!(c.mark, Some((3_001.0, 2)));
        assert_eq!(c.bid, None);
    }

    #[test]
    fn non_positive_and_nan_prices_are_ignored() {
        let mut b = PriceBoard::default();
        b.set_mark("bybit", "X", 0.0, 1);
        b.set_mark("bybit", "X", -1.0, 2);
        b.set_mark("bybit", "X", f64::NAN, 3);
        b.set_last_trade("bybit", "X", 0.0, 4);
        b.set_bar_close("bybit", "X", -5.0, 5);
        assert!(b.cell("bybit", "X").is_none(), "no slot may be created by dead prices");
        // a quote with ONE dead side stores only the live side
        b.set_quote("bybit", "X", 0.0, 101.0, 6);
        let c = b.cell("bybit", "X").unwrap();
        assert_eq!(c.bid, None);
        assert_eq!(c.ask, Some((101.0, 6)));
    }

    #[test]
    fn cells_are_venue_scoped() {
        let mut b = PriceBoard::default();
        b.set_mark("bybit", "BTCUSDT", 1.0, 1);
        b.set_mark("okx", "BTCUSDT", 2.0, 1);
        assert_eq!(b.cell("bybit", "BTCUSDT").unwrap().mark, Some((1.0, 1)));
        assert_eq!(b.cell("okx", "BTCUSDT").unwrap().mark, Some((2.0, 1)));
    }

    fn full_cell() -> PriceBoard {
        let mut b = PriceBoard::default();
        b.set_mark("v", "S", 100.0, 1_000);
        b.set_quote("v", "S", 99.0, 101.0, 2_000);
        b.set_last_trade("v", "S", 100.5, 3_000);
        b.set_bar_close("v", "S", 98.0, 4_000);
        b
    }

    #[test]
    fn chain_prefers_fresh_mark_then_side_quote_then_trade_then_bar() {
        let b = full_cell();
        let cfg = PriceCfg::default();
        // 1. mark wins when enabled
        assert_eq!(
            b.resolve("v", "S", true, 5_000, &cfg),
            Resolution::Priced { px: 100.0, source: PriceSource::Mark, ts: 1_000 }
        );
        // 2. mark disabled -> side-appropriate quote: Bid for long, Ask for short
        let cfg_nm = PriceCfg { use_mark: false, ..PriceCfg::default() };
        assert_eq!(
            b.resolve("v", "S", true, 5_000, &cfg_nm),
            Resolution::Priced { px: 99.0, source: PriceSource::Bid, ts: 2_000 }
        );
        assert_eq!(
            b.resolve("v", "S", false, 5_000, &cfg_nm),
            Resolution::Priced { px: 101.0, source: PriceSource::Ask, ts: 2_000 }
        );
    }

    #[test]
    fn stale_sources_fall_through_the_chain() {
        let b = full_cell();
        // everything but bar_close aged out at now=10_000
        let cfg = PriceCfg {
            use_mark: true,
            mark_max_age_ms: Some(1_000), // mark ts 1_000, age 9_000 -> stale
            quote_max_age_ms: Some(1_000), // quote ts 2_000, age 8_000 -> stale
            trade_max_age_ms: Some(1_000), // trade ts 3_000, age 7_000 -> stale
            bar_max_age_ms: None,         // no limit -> fresh
            stale_fallback: false,        // bar is fresh here; fallback irrelevant (default off)
        };
        assert_eq!(
            b.resolve("v", "S", true, 10_000, &cfg),
            Resolution::Priced { px: 98.0, source: PriceSource::BarClose, ts: 4_000 }
        );
    }

    #[test]
    fn freshness_boundary_is_inclusive_at_exactly_max_age() {
        let mut b = PriceBoard::default();
        b.set_mark("v", "S", 100.0, 1_000);
        let cfg = PriceCfg { use_mark: true, mark_max_age_ms: Some(500), ..PriceCfg::default() };
        // age == max_age (500) -> still FRESH (the impl's `fresh` uses strict `>`)
        assert_eq!(
            b.resolve("v", "S", true, 1_500, &cfg),
            Resolution::Priced { px: 100.0, source: PriceSource::Mark, ts: 1_000 }
        );
        // age == max_age + 1 (501) -> stale; only mark is set, so falls through to Missing
        assert_eq!(b.resolve("v", "S", true, 1_501, &cfg), Resolution::Missing);
    }

    #[test]
    fn one_sided_quote_falls_to_next_source_for_the_missing_side() {
        let mut b = PriceBoard::default();
        b.set_quote("v", "S", 99.0, 0.0, 1_000); // bid only
        b.set_last_trade("v", "S", 100.5, 2_000);
        let cfg = PriceCfg { use_mark: false, ..PriceCfg::default() };
        // long -> bid exists
        assert_eq!(
            b.resolve("v", "S", true, 3_000, &cfg),
            Resolution::Priced { px: 99.0, source: PriceSource::Bid, ts: 1_000 }
        );
        // short -> no ask -> falls to last trade
        assert_eq!(
            b.resolve("v", "S", false, 3_000, &cfg),
            Resolution::Priced { px: 100.5, source: PriceSource::LastTrade, ts: 2_000 }
        );
    }

    #[test]
    fn unknown_symbol_and_empty_cell_resolve_missing() {
        let b = PriceBoard::default();
        assert_eq!(b.resolve("v", "NOPE", true, 1, &PriceCfg::default()), Resolution::Missing);
    }

    #[test]
    fn note_tracks_missing_and_rearms_on_recovery() {
        let mut b = PriceBoard::default();
        let miss = Resolution::Missing;
        b.note("v", "S", &miss);
        b.note("v", "S", &miss); // second miss of the same episode: still tracked once
        assert_eq!(
            b.missing_price_instruments("v").map(|s| s.iter().cloned().collect::<Vec<_>>()),
            Some(vec!["S".to_string()])
        );
        // recovery clears the set (and re-arms the warn for a future episode)
        let ok = Resolution::Priced { px: 1.0, source: PriceSource::Mark, ts: 1 };
        b.note("v", "S", &ok);
        assert!(b.missing_price_instruments("v").is_none_or(|s| s.is_empty()));
        // a new episode is tracked again
        b.note("v", "S", &miss);
        assert!(b.missing_price_instruments("v").is_some_and(|s| s.contains("S")));
    }

    // --- last-known floor (rung 5) + classify (Ext 1) -----------------------------------------

    /// Aged-out windows on every rung (all `Some(1)`), so at a late `now` the fresh chain finds
    /// nothing and the last-known floor is what's under test.
    fn all_aged() -> PriceCfg {
        PriceCfg {
            mark_max_age_ms: Some(1),
            quote_max_age_ms: Some(1),
            trade_max_age_ms: Some(1),
            bar_max_age_ms: Some(1),
            ..PriceCfg::default()
        }
    }

    /// THE inert guarantee: a FRESH venue mark early-returns `Priced { Mark }` no matter how
    /// `stale_fallback` is set — the common case is byte-identical, the floor only ever helps a
    /// stale-everywhere cell.
    #[test]
    fn fresh_mark_is_byte_identical_regardless_of_stale_fallback() {
        let b = full_cell();
        let want = Resolution::Priced { px: 100.0, source: PriceSource::Mark, ts: 1_000 };
        assert_eq!(b.resolve("v", "S", true, 5_000, &PriceCfg::default()), want);
        assert_eq!(
            b.resolve(
                "v",
                "S",
                true,
                5_000,
                &PriceCfg { stale_fallback: true, ..Default::default() }
            ),
            want
        );
    }

    /// `classify` reports Fresh / Stale / Missing independent of `stale_fallback`.
    #[test]
    fn classify_buckets_fresh_stale_and_missing() {
        let b = full_cell();
        assert_eq!(
            b.classify("v", "S", true, 5_000, &PriceCfg::default()),
            MarkStatus::Fresh { px: 100.0, source: PriceSource::Mark, ts: 1_000 }
        );
        // every window aged out -> Stale, highest-priority present slot = mark
        assert_eq!(
            b.classify("v", "S", true, 100_000, &all_aged()),
            MarkStatus::Stale { px: 100.0, source: PriceSource::Mark, ts: 1_000, age_ms: 99_000 }
        );
        // unknown symbol -> Missing (classify is stale_fallback-independent)
        assert_eq!(b.classify("v", "NOPE", true, 1, &all_aged()), MarkStatus::Missing);
    }

    /// The behavior switch: the SAME aged-out cell resolves `Missing` with the floor OFF (today's
    /// behavior) and `Stale` with it ON — carrying the highest-priority surviving slot.
    #[test]
    fn stale_fallback_flips_missing_to_last_known() {
        let b = full_cell();
        assert_eq!(b.resolve("v", "S", true, 100_000, &all_aged()), Resolution::Missing);
        assert_eq!(
            b.resolve("v", "S", true, 100_000, &PriceCfg { stale_fallback: true, ..all_aged() }),
            Resolution::Stale { px: 100.0, source: PriceSource::Mark, ts: 1_000 }
        );
    }

    /// Last-known picks by CHAIN PRIORITY, not recency: with the mark rung disabled the floor
    /// falls to the side quote (Bid for a long) even though last-trade / bar-close are NEWER.
    #[test]
    fn last_known_follows_chain_priority_not_recency() {
        let b = full_cell();
        let on = PriceCfg {
            use_mark: false,
            mark_max_age_ms: Some(1),
            quote_max_age_ms: Some(1),
            trade_max_age_ms: Some(1),
            bar_max_age_ms: Some(1),
            stale_fallback: true,
        };
        assert_eq!(
            b.resolve("v", "S", true, 100_000, &on),
            Resolution::Stale { px: 99.0, source: PriceSource::Bid, ts: 2_000 }
        );
    }

    /// The floor cannot invent a price: a cell that never held any value is Missing even ON.
    #[test]
    fn stale_fallback_never_prices_an_empty_cell() {
        let b = PriceBoard::default();
        let on = PriceCfg { stale_fallback: true, ..PriceCfg::default() };
        assert_eq!(b.resolve("v", "GHOST", true, 1, &on), Resolution::Missing);
        assert_eq!(b.classify("v", "GHOST", true, 1, &on), MarkStatus::Missing);
    }

    /// `note` treats a Stale resolution as a price (clears the missing set), like Priced.
    #[test]
    fn note_clears_missing_on_a_stale_resolution() {
        let mut b = PriceBoard::default();
        b.note("v", "S", &Resolution::Missing);
        assert!(b.missing_price_instruments("v").is_some_and(|s| s.contains("S")));
        b.note("v", "S", &Resolution::Stale { px: 1.0, source: PriceSource::BarClose, ts: 1 });
        assert!(b.missing_price_instruments("v").is_none_or(|s| s.is_empty()));
    }
}
