//! PriceBoard — per-(venue, symbol) multi-source price cells + the side-aware read-side
//! resolver (Rust-native surface, no Python twin; chain semantics source-verified against
//! NautilusTrader `crates/portfolio` `get_price`).
//!
//! WRITE side (hot fold): each mark write-site also stores `(px, ts)` into the matching source
//! slot — plain field stores, no logging, no reads; non-positive or NaN prices are ignored.
//! READ side (cold paths only — snapshot publish, sweeps, timers): [`PriceBoard::resolve`] walks
//! the chain below, and `note` keeps the per-venue missing-price set, warning once per episode.
//! Deliberately NOT serialized: the board sits outside `EngineSnapshot`, so the journal
//! determinism-fence hash is untouched.
//!
//! # THE VALUATION LAW — what each slot means and who fills it
//!
//! The chain is ordered by how closely a price answers "what is this position worth to me RIGHT
//! NOW, if I had to act on it"; a slot is consulted only when every slot above it is absent or
//! stale, so the ordering IS the law:
//!
//! | rung | slot | meaning | producers |
//! |------|------|---------|-----------|
//! | 1 | `mark` | the VENUE's own mark/index price — the number its liquidation and funding engines key off | crypto perps' mark streams: binance-family `@markPrice@1s`, bybit `tickers.markPrice`, okx `mark-price`, hyperliquid `activeAssetCtx.markPx` default ON; aster `@markPrice@1s` default OFF (grammar unverified — opt in with a `venue.aster.mark_streams = 1` row); a `venue.<venue>.mark_streams = 0` row turns one off. ALSO, on ANY venue: a fill's `mark_price` and a reconcile pass's `ExecReport::position_mark_px` (`apply_snapshot`) |
//! | 2 | `bid`/`ask` | the live top of book, SIDE-AWARE: a LONG at the BID, a SHORT at the ASK (the side it would cross to exit), so the spread is booked against the position, never for it | any subscribed quote/BBO or L2 feed (`set_quote`) |
//! | 3 | `last_trade` | the last traded price: real and recent, but one-sided | any subscribed trade feed, including one a chart asked for (`ensure_trade_feed_on`) |
//! | 4 | `bar_close` | the last completed candle's close — LOWEST by design, up to a whole bar old; the last resort, not a mark | every kline feed, every venue |
//! | 5 | LAST-KNOWN | not a slot: rungs 1-4 walked AGAIN with the freshness gate RELAXED. Opt-in (`PriceCfg::stale_fallback`, OFF by default) and surfaced as [`Resolution::Stale`], never as a fresh [`Resolution::Priced`] | the freshest surviving slot of rungs 1-4 |
//!
//! Freshness is per-rung and OFF by default ([`PriceCfg`]'s max-age fields are all `None`), so
//! under the default cfg nothing is ever stale and rung 5 is unreachable. [`PriceBoard::classify`]
//! is the ONE place fresh-vs-stale-vs-missing is decided; `resolve` and the missing/stale operator
//! queries project from it.
//!
//! The candle close sits at the TAIL deliberately: when kline feeds wrote into `mark`, a stale
//! candle OUTRANKED a live quote and trade. So a venue with no venue mark but a quote or trade feed
//! (alpaca, IBKR, binance spot, an aster perp not opted into its mark stream, any perp with
//! `mark_streams = 0`) is valued at the live side-aware quote or the last trade — toward the price
//! the position could actually be exited at.
//!
//! # The board is NOT the account mark slot
//!
//! Every write here is UNCONDITIONAL: slots are source-tagged, so a candle close and a venue mark
//! can both land without either destroying the other. `Account.marks` is the opposite shape — ONE
//! untagged scalar per symbol, read by the pre-trade gate / margin-call law / `LiveBroker.price` —
//! and its precedence rule lives in [`crate::MarkSource`], enforced inside
//! `Account::set_mark_from`, the only door into that map. Do not reimplement it here: a freshness
//! predicate on this board is how a first fix ended up bypassable by every lane not calling it.

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

/// Which source priced a resolution — surfaced to snapshot views for display.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PriceSource {
    Mark,
    Bid,
    Ask,
    LastTrade,
    BarClose,
}

/// Outcome of a read-side price resolution. `Missing` is a first-class answer — callers decide
/// skip/zero and feed it back through [`PriceBoard::note`].
///
/// `Stale` is the last-known floor (rung 5): a real price past its freshness window, returned ONLY
/// when the caller opted into [`PriceCfg::stale_fallback`]. DISTINCT from `Priced` so a consumer
/// can tell fresh from stale, with the same `(px, source, ts)` so "any price beats zero" can use
/// it uniformly.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Resolution {
    Priced { px: f64, source: PriceSource, ts: i64 },
    Stale { px: f64, source: PriceSource, ts: i64 },
    Missing,
}

/// Freshness classification of a (venue, symbol) cell — the ONE decision [`PriceBoard::classify`]
/// makes. Unlike [`Resolution`] it ignores [`PriceCfg::stale_fallback`] and ALWAYS reports fresh,
/// stale or absent, so an operator query sees "priced but stale" even when valuation does not
/// fall back to it.
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
    /// Opt-in last-known floor (rung 5): when `true`, a cell whose every slot aged out resolves
    /// [`Resolution::Stale`] with the freshest surviving price instead of [`Resolution::Missing`].
    /// INERT under the permissive default cfg (no freshness windows ⇒ nothing is ever stale).
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
    /// venue -> symbols that failed to resolve (maintained by `note`).
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
    /// value survives past every window), or missing — the ONE decision site ([`Self::resolve`]
    /// and the missing/stale operator queries project from it). Walks mark -> Bid/Ask -> last
    /// trade -> bar close; the `Fresh` early-return on rung 1 is the common case. Only when NO rung
    /// is fresh does it walk the four slots again with freshness relaxed (highest-priority present
    /// slot wins). Allocation-free; COLD path only.
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
        // The chain in priority order. `use_mark = false` drops the mark rung from BOTH walks.
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
    /// [`Self::classify`]: `Fresh` -> `Priced`, `Stale` -> `Resolution::Stale` when
    /// `stale_fallback`, else `Missing`. Allocation-free on the priced path.
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
            // A Stale resolution is still a PRICE, so it clears the missing set like a fresh Priced;
            // "priced but stale" is the dedicated stale-mark query's job, not this set's.
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

/// Per-position resolver result for the snapshot read-model. NOT journaled.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ResolvedPosition {
    pub unrealized: f64,
    pub mark_source: Option<PriceSource>,
}

/// Mode-aware, resolver-priced equity for one account (cold publish path).
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedEquity {
    pub equity: f64,
    pub unrealized_total: f64,
    pub missing: u32,
    pub per_position: Vec<ResolvedPosition>,
}

#[path = "price_board_tests.rs"]
#[cfg(test)]
mod price_board_tests;
