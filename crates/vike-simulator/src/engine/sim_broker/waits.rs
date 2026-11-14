//! Fill-time waits and price decisions: stale prints, the session gate, the book, stop release, the
//! trailing activation level.

use vike_marketdata::Bar;
use vike_model::{OrderKind, OrderResting, trailing_activation_reached};

use vike_fills::fill_model::{
    FillModel, FillModelBar, FillModelL2Book, FillModelTick, trailing_path_fill,
};

use super::knobs::FillModelKind;
#[cfg(doc)]
use super::params::StrategyEngineConfig;
use super::state::OrderBrokerSimulator;

impl OrderBrokerSimulator {
    // --- stale-price wait discipline (the opt-in `StrategyEngineConfig::max_price_staleness_ms` lane) ---

    /// Record `event` as this symbol's last fresh print when it carries one. A no-op — a single
    /// `is_none` branch, no indexing — unless the wait discipline is configured.
    ///
    /// Called ONCE per symbol per event, immediately BEFORE that event's fill phase, so an event
    /// that is itself a print has age `0` and fills on the spot. See [`vike_fills::staleness`].
    pub(crate) fn note_print(&mut self, si: usize, event: &Bar) {
        if self.max_price_staleness_ms.is_none() {
            return;
        }
        self.note_print_at(si, event.timestamp, vike_fills::staleness::is_fresh_print(event));
    }

    /// Scalar twin of [`Self::note_print`] for callers that hold a borrow of `self.bars` and so
    /// cannot pass a `&Bar` across the `&mut self` write (the bar-path step loop).
    pub(crate) fn note_print_at(&mut self, si: usize, ts: i64, fresh: bool) {
        if self.max_price_staleness_ms.is_none() {
            return;
        }
        if fresh {
            self.last_print_timestamp[si] = Some(ts);
        }
    }

    /// Must a `kind` order on `symbols[si]` WAIT rather than fill at `ts`? `false` whenever the
    /// discipline is off (the byte-identical default), whenever the kind is price-conditional,
    /// and whenever the symbol's last fresh print is within the bound.
    #[inline]
    pub(crate) fn defer_stale(&self, si: usize, kind: OrderKind, ts: i64) -> bool {
        let Some(bound) = self.max_price_staleness_ms else { return false };
        vike_fills::staleness::defers(kind)
            && vike_fills::staleness::is_stale(self.last_print_timestamp[si], ts, bound)
    }

    /// Is `symbol`'s price currently stale? Read-only strategy surface — a maker can check this
    /// before sending a taker order it knows would be deferred. Always `false` when the
    /// discipline is off; TOTAL on an unknown symbol (answers `false`, same rule as
    /// [`Self::is_resolved`] — a predicate has an honest answer where a write verb would panic).
    ///
    /// CLOCK: measured against **that symbol's own current bar ts** when the bar path is active,
    /// falling back to the engine clock `now` otherwise (the tick path, where `now` IS the
    /// just-routed tick's ts). That matters on a ragged multi-symbol tape: `now` is symbol 0's
    /// bar ts, while the fill lane judges each symbol against `bars[si][step].ts`, so reading the
    /// engine clock here would let this predicate disagree with the engine's actual deferral
    /// decision. Using the symbol's own bar keeps the two in agreement.
    pub fn price_is_stale(&self, symbol: &str) -> bool {
        if self.max_price_staleness_ms.is_none() {
            return false;
        }
        match self.symbols.iter().position(|s| s == symbol) {
            Some(si) => {
                let ts = self
                    .bars
                    .get(si)
                    .and_then(|b| b.get(self.index))
                    .map_or(self.now, |b| b.timestamp);
                self.defer_stale(si, OrderKind::Market, ts)
            }
            None => false,
        }
    }

    // --- session / market-hours gate (the opt-in `StrategyEngineConfig::session_gate` lane) ---

    /// Is `symbols[si]`'s market CLOSED at `ts`? `false` — one `Vec::get` returning `None`, no
    /// calendar lookup — whenever the gate is off (the byte-identical default), the symbol has no
    /// calendar pinned (fail-permissive), or that symbol's market trades 24/7.
    ///
    /// This is the per-symbol counterpart to [`Self::defer_stale`], and it deliberately gates MORE
    /// than staleness does. Staleness exempts price-conditional kinds and protective stops: a
    /// repeated stale price cannot spuriously satisfy a condition, and gating a protective stop
    /// would strand live risk while the venue is perfectly able to fill it. A CLOSED market is a
    /// different fact — no order of any kind can execute, because there is no matching engine
    /// running. A protective stop that "fires" over a weekend is fiction; the real one fills at
    /// the next open, into the gap. So this gate applies to every fill lane uniformly — the coarse,
    /// granular, cash-gated and tick pending passes (`defer_fill`), the tagged maker lane
    /// (`fill_tagged`), protective stops (`check_stop`), AND the opt-in `queue_model` tick twins for
    /// both the queued resting limit and the queued maker tag (`fill_pending_tick_queued` /
    /// `fill_tagged_queued`) — and the stop simply fires on the first in-session event at that
    /// event's (gapped) price, which is exactly the adverse-gap law
    /// [`crate::engine::StrategyEngine::check_stop`] already implements. Keyed PER SYMBOL so a
    /// mixed-venue run gates each instrument by its own market.
    #[inline]
    pub(crate) fn session_closed(&self, si: usize, ts: i64) -> bool {
        match self.session.get(si) {
            Some(Some(cal)) => !cal.is_open(ts),
            _ => false, // gate off (empty vec), si out of range, or no calendar ⇒ open
        }
    }

    /// The price a held order's price condition reads (`vike_model::OrderRelease::holds`): `symbol`'s
    /// last seen price, the mark BEFORE the event being judged, so a condition met on one event
    /// releases the order from the next, as the live core sends it once it has seen the price.
    /// `None` (nothing released) for a symbol outside the run or one that has not printed.
    pub(crate) fn release_price(&self, symbol: &str) -> Option<f64> {
        let si = self.symbols.iter().position(|s| s == symbol)?;
        Some(self.symbol[si].price).filter(|p| *p > 0.0)
    }

    /// Is the bar path's current step the LAST bar of `symbols[si]`'s session, the bar of its
    /// closing auction: a session close falls after the bar opens and no later than the next bar
    /// does? Read off the step's own bar and the next one's ts (the schedule, never a price), so it
    /// is `false` with no calendar and on a series' last bar, where nothing says the session ends.
    /// A walk of the calendar's minutes, asked only for a resting closing-auction order.
    pub(crate) fn closes_session(&self, si: usize) -> bool {
        let Some(Some(cal)) = self.session.get(si) else { return false };
        let bars = &self.bars[si];
        let (Some(bar), Some(next)) = (bars.get(self.step), bars.get(self.step + 1)) else {
            return false;
        };
        cal.next_close(bar.timestamp).is_some_and(|close| close <= next.timestamp)
    }

    /// Is `symbol`'s market currently open? Read-only strategy surface, the sibling of
    /// [`Self::price_is_stale`] — a strategy can check before quoting into a market that cannot
    /// fill. Always `true` when the gate is off (empty session vec), and `true` on an unknown
    /// symbol (a total predicate, same stance as `price_is_stale`), so a strategy reading it is
    /// never misled into thinking a default run is session-aware.
    ///
    /// CLOCK: that symbol's own current-step bar ts when the bar path is active, falling back to
    /// the engine clock `now` (the tick path). Same discipline as [`Self::price_is_stale`]: on a
    /// ragged multi-symbol tape `now` is symbol 0's ts, but the fill lane judges each symbol
    /// against its own bar ts — reading the engine clock here would let this predicate disagree
    /// with the actual gate decision.
    pub fn symbol_is_open(&self, symbol: &str) -> bool {
        if self.session.is_empty() {
            return true; // gate off
        }
        match self.symbols.iter().position(|s| s == symbol) {
            Some(si) => {
                let ts = self
                    .bars
                    .get(si)
                    .and_then(|b| b.get(self.index))
                    .map_or(self.now, |b| b.timestamp);
                !self.session_closed(si, ts)
            }
            None => true,
        }
    }

    /// The replayed L2 book for `symbols[si]`, or `None` on every path that carries none.
    pub(crate) fn book_at(&self, si: usize) -> Option<&vike_marketdata::L2Book> {
        self.books.get(si).and_then(|b| b.as_deref())
    }

    /// How many TRADE PRINTS the tick lane's impact context currently holds for `symbol` — or
    /// `None` when no context is armed, which is every path except a `run_ticks` replay with an
    /// [`StrategyEngineConfig::impact`] model configured.
    ///
    /// A diagnostic in the same spirit as [`Self::slippage_saturations`], and it answers a
    /// question this lane can otherwise only answer silently: a quote-only tape records no prints,
    /// so an armed model measures nothing and charges nothing. "The model was off" and "the model
    /// had nothing to measure" produce identical fills, and an operator must be able to tell them
    /// apart. Reads `0` during the run's warmup and `None` once the replay has torn its window
    /// down; an unknown symbol is `None` too.
    pub fn impact_prints(&self, symbol: &str) -> Option<usize> {
        let si = self.symbols.iter().position(|s| s == symbol)?;
        self.impact_ticks.get(si).map(crate::impact::ImpactModelTickWindow::len)
    }

    /// The fill-price decision. `si` selects the symbol whose replayed book the
    /// [`FillModelKind::L2Book`] tier prices from; the other two tiers ignore it entirely, so the
    /// frozen paths are byte-identical.
    ///
    /// A trailing order with an activation level ([`OrderResting::activation`], set by
    /// [`Self::submit_trailing_activated`] and [`Self::submit_trailing_limit`]) is decided here
    /// first; with none it takes the tier's own trailing law, one failed `kind` test later. A bar
    /// goes to the path walk the paper book shares (`vike_fills::fill_model::trailing_path_fill`,
    /// a trailing limit's limit part included). A tick has no path to walk, so it only activates
    /// the order, on the side of the book the tick tier trails, and the tier then owns its trigger
    /// and its price (a walk of the book on `L2Book`), a trailing limit's limit part included.
    pub(crate) fn fill_price_for(&self, si: usize, o: &mut OrderResting, bar: &Bar) -> Option<f64> {
        if matches!(o.kind, OrderKind::Trailing | OrderKind::TrailingLimit)
            && let Some(level) = o.activation
        {
            if self.fill_model == FillModelKind::Bar || bar.high != bar.low {
                return trailing_path_fill(o, bar);
            }
            if o.extreme.is_none() {
                let quote = if o.side < 0 { bar.bid } else { bar.ask };
                let px = quote.unwrap_or(bar.close);
                if !trailing_activation_reached(o.side, px, level) {
                    return None;
                }
                o.extreme = Some(px);
            }
        }
        match self.fill_model {
            FillModelKind::Bar => FillModelBar.fill_price(o, bar),
            FillModelKind::Tick => FillModelTick.fill_price(o, bar),
            FillModelKind::L2Book => FillModelL2Book.fill_price_book_valued(
                o,
                bar,
                self.book_at(si),
                self.value_of_index(si),
            ),
        }
    }

    /// The opt-in emulator-mirroring stop release (see
    /// [`StrategyEngineConfig::emulator_release_stops`]): when armed and `o` is a resting
    /// [`OrderKind::Stop`] whose trigger the SAME oracle fill check says has crossed on `event`,
    /// CONVERT it in place to a resting MARKET child — the live `OrderEmulationBook` release law —
    /// and return `true`; the caller keeps it resting, so it fills at the NEXT event's price.
    /// `false` (the knob off, a non-stop kind, or no trigger) leaves `o` untouched — the
    /// byte-identical default path. The Stop fill check mutates nothing but an ADJUSTABLE stop's
    /// terms (`vike_model::apply_stop_adjust`), so such a stop is asked on a copy: the caller's own
    /// check of this event adjusts it once, and its new terms wait for the next event.
    pub(crate) fn stop_released(&self, si: usize, o: &mut OrderResting, event: &Bar) -> bool {
        if !self.emulator_release_stops || o.kind != OrderKind::Stop {
            return false;
        }
        let fired = if o.adjust.is_some() {
            self.fill_price_for(si, &mut o.clone(), event)
        } else {
            self.fill_price_for(si, o, event)
        };
        if fired.is_none() {
            return false;
        }
        o.kind = OrderKind::Market;
        o.price = None;
        true
    }
}
