//! The adverse-move fraction per fill: flat slippage plus the opt-in impact model's terms.

use super::knobs::FillModelKind;
#[cfg(doc)]
use super::params::EngineParams;
use super::state::SimBroker;

impl SimBroker {
    // --- slippage (flat, plus the opt-in size-dependent impact model) ---

    /// The adverse-move fraction to charge one fill of `size` on `symbols[si]` at `ts`.
    ///
    /// BYTE-IDENTICAL DEFAULT: with no [`EngineParams::impact`] model this returns the
    /// `slippage` field itself — the same `f64`, with no arithmetic performed on it — so every
    /// frozen fill path is bit-for-bit what it was. That is why the `None` arm is an early
    /// return rather than `self.slippage + 0.0`: adding a zero is not the identity on `-0.0`,
    /// and a parity gate is not the place to find that out.
    ///
    /// A FILL THAT SUPPLIED LIQUIDITY IS NOT CHARGED IMPACT, and which fills those are is
    /// [`Self::charges_impact`]'s question, not `is_maker`'s. The flat `slippage` applies to every
    /// fill exactly as it did on the frozen path; only the size-dependent addend is conditional.
    ///
    /// With a model configured this is TWO lane-shaped decisions, each with its own function so
    /// neither can be read off the other's comment: WHICH SERIES supplies the market context
    /// ([`Self::impact_context`] — the tick window when `run_ticks` armed one, the bar window
    /// otherwise, both free of lookahead) and WHICH TERMS of the model this lane has not already
    /// paid ([`Self::impact_terms`] — everything, unless the fill was priced by walking a
    /// replayed L2 book, which already charged the temporary half from real depth).
    ///
    /// A context too short or degenerate to measure (either window returning `None` — the first
    /// bars of a run, the first prints of a replay, a volume-less resampled series, a quote-only
    /// tape) charges the flat slippage alone rather than guessing.
    ///
    /// UNBOUNDED ABOVE, deliberately, and that is not the omission it looks like. The sum returned
    /// here can exceed 1.0 — `impact_frac` is bounded below (a cost is never negative) but never
    /// above — which on a SELL used to invert the fill price. The bound is NOT applied here: this
    /// function does not know the side, and clamping the sum would silently change the `None`/
    /// `is_maker` early returns this doc promises are the `slippage` field itself with no
    /// arithmetic performed on it. It is applied one layer down, at
    /// [`vike_fills::broker_sim::adverse_fill_price`], which holds both the fraction and the side and is
    /// the single site every other producer of a slippage number also funnels through — so the
    /// cash pre-check, the vector kernel and the paper exchange are bounded by the same floor. See
    /// that module's docs, and [`Self::slippage_saturations`] for the diagnostic.
    /// Returns the slippage fraction AND whether an impact charge was SKIPPED for want of a market
    /// context — the second half exists because that skip is otherwise invisible, and a caller that
    /// can count ([`Self::apply_fill`] holds `&mut self`) must be able to see it without respelling
    /// the condition. The `bool` is `true` ONLY for "configured, and could not be priced": a run
    /// with no impact model at all reports `false`, because nothing was skipped.
    pub(crate) fn slippage_for(
        &self,
        si: usize,
        size: f64,
        ts: i64,
        is_maker: bool,
    ) -> (f64, bool) {
        if !self.charges_impact(is_maker) {
            return (self.slippage, false);
        }
        let Some(model) = self.impact.as_ref() else {
            return (self.slippage, false);
        };
        let Some(stats) = self.impact_context(si, ts) else {
            // CONFIGURED but unmeasurable — the one branch worth counting.
            return (self.slippage, true);
        };
        let extra = model.impact_frac_for(
            &crate::impact::ImpactInputs {
                qty: size.abs(),
                avg_volume: stats.avg_volume,
                sigma: stats.sigma,
            },
            self.impact_terms(si),
        );
        (self.slippage + extra, false)
    }

    /// The market context [`Self::slippage_for`] prices one fill against — measured off whichever
    /// series this lane actually has.
    ///
    /// TICK LANE FIRST, and only because `run_ticks` armed it: `impact_ticks` is EMPTY everywhere
    /// else, so the `Vec::get` misses and the bar read below is the frozen line, character for
    /// character. The tick window carries no `ts` argument because it cannot need one — it holds
    /// the prints that have ALREADY been folded, and `run_ticks` records each print after every
    /// fill site that tick reaches (`fill_pending_tick` AND `check_liquidation_tick`), so the
    /// lookahead discipline is enforced by WHEN the window is written rather than by a cutoff
    /// applied when it is read.
    ///
    /// ⚠ **That first branch is a SHORT-CIRCUIT and not a fallback, deliberately.** A tick replay
    /// CAN carry a real per-symbol bar series — `TickReplayConfig::seed_bar_interval_ms` (exposed
    /// as `EngineCfg::seed_bar_interval_ms`) builds one for exactly this lane — and once a window
    /// is armed that series is ignored even while the window is still warming up and measuring
    /// `None`. Falling back to it would be strictly worse than charging nothing: `sigma` and
    /// `avg_volume` are PER-PERIOD quantities, so the bar read and the print read answer in
    /// different units by a factor of the tape's print rate, and a fallback would price a run's
    /// first fills in bar units and its later ones in print units with nothing in the output
    /// saying which. One lane, one unit; [`Self::impact_prints`] is how an operator sees that the
    /// window is warming rather than that the model is off.
    ///
    /// BAR LANE otherwise, unchanged: the last `impact_window` bars of this symbol that CLOSED
    /// BEFORE `ts` — the bar `ts` falls INSIDE is excluded, because its own close and volume are
    /// not knowable at the moment the order prices, and its volume would in part be this very
    /// order. The window end is `partition_point(|b| b.ts <= ts) - 1` (the index of the bar
    /// containing `ts`), NOT `partition_point(|b| b.ts < ts)`: the two agree on the coarse bar
    /// lanes, where `ts` is exactly the landing bar's own `ts`, but only the former holds on the
    /// granular sub-bar lane (`dispatch_fill` is called with `sub.ts`, strictly INSIDE the coarse
    /// bar).
    fn impact_context(&self, si: usize, ts: i64) -> Option<crate::impact::MarketStats> {
        if let Some(window) = self.impact_ticks.get(si) {
            return window.stats();
        }
        let bars = &self.bars[si];
        let hi = bars.partition_point(|b| b.ts <= ts).saturating_sub(1);
        crate::impact::window_stats(&bars[..hi], self.impact_window)
    }

    /// Which HALVES of the impact model this lane has NOT already charged (see
    /// [`crate::impact::ImpactTerms`]).
    ///
    /// The one case that is not [`crate::impact::ImpactTerms::Both`] is a fill priced by WALKING a
    /// replayed book: [`FillModelKind::L2Book`] with a book actually present for this symbol. That
    /// walk consumed real displayed depth for the order's own size, which IS the temporary
    /// concession — asking the model for it again would charge the same liquidity twice. What the
    /// walk cannot supply is the permanent footprint, because the replayed book is a recording
    /// that never saw the order.
    ///
    /// The predicate is deliberately the SAME pair [`Self::fill_price_for`] dispatches on, read
    /// off broker state rather than passed in: `L2BookFillModel` degrades to the L1 tier exactly
    /// when `book_at` is `None`, so a bookless symbol on an `L2Book` run is charged `Both` — which
    /// is correct, because on that event nothing walked anything.
    ///
    /// ⚠ **It is a per-EVENT book-presence test, not a per-FILL "was this price a walk" test, and
    /// on one path those differ.** Every taker fill funnels through [`Self::apply_fill`], including
    /// the FORCED closes `StrategyEngine::check_liquidation_tick` books at the adverse print or the
    /// last mark — prices that never touched [`Self::fill_price_for`] and walked nothing. On an
    /// `L2Book` replay with a book present those are charged `PermanentOnly` and are therefore
    /// UNDER-charged, which is the wrong direction on exactly the fills where flattering a
    /// backtest matters most. Closing it means carrying the terms down from the site that produced
    /// the price rather than re-deriving them from broker state, which is a signature change
    /// across every `apply_fill` caller; it is declared here rather than assumed away, and it is
    /// bounded — the miss is the temporary term of a liquidation on a book-bearing L2 tape.
    fn impact_terms(&self, si: usize) -> crate::impact::ImpactTerms {
        if self.fill_model == FillModelKind::L2Book && self.book_at(si).is_some() {
            crate::impact::ImpactTerms::PermanentOnly
        } else {
            crate::impact::ImpactTerms::Both
        }
    }

    /// Whether this fill pays the size-dependent addend at all — the LANE's answer to "did this
    /// fill supply liquidity or demand it".
    ///
    /// A taker always pays. A `is_maker` fill pays on every lane EXCEPT the bar lane, and that
    /// asymmetry is the whole point: `is_maker` is set from the order KIND
    /// (`crates/vike-sim/src/engine.rs`'s `dispatch_fill` — "a MARKETABLE limit books as a
    /// maker fill"), which the FEE side needs, and which says nothing about aggressiveness. What
    /// a `Limit` fill is actually priced at is a per-lane fact:
    ///
    /// - **bar** — `vike_model::order_fill_price`'s `Limit` arm returns `price.min(bar.open)` for
    ///   a buy: at-or-better than the limit, reached because the market came to a resting order.
    ///   That IS a supplied-liquidity fill, and a haircut on top would execute it through its own
    ///   price. EXEMPT, and byte-identical to the frozen path.
    /// - **tick** — [`vike_fills::fill_model::TickFillModel`] fills a buy limit only when `ask <=
    ///   price`, AT THE ASK, for ANY size. It crossed the spread and took the touch. CHARGED.
    /// - **L2** — `vike_model::book_taker_price` capped at the limit: a walk down real
    ///   resting levels for the order's own size. CHARGED.
    ///
    /// Exempting the replay lanes made the entire charge AVOIDABLE — a taker respelt as a
    /// marketable limit one tick through the touch fills at the same price for the same size and
    /// paid nothing. The accepted cost of charging it (a fill can land past its own limit price)
    /// and why the two alternatives are worse are argued in `crate::impact`'s module docs.
    ///
    /// ⚠ Keyed on the FILL MODEL rather than on which loop is running, because that is the thing
    /// that chose the price law. The one configuration where the two disagree is a hand-built
    /// `EngineParams` running the BAR loop with `FillModelKind::Tick` over CONSOLIDATED bars
    /// (`high != low`), where `TickFillModel` delegates back to the bar law and a limit can fill
    /// passively yet be charged here. `crates/vike-backtest/src/harness/run.rs`'s `run_backtest`
    /// sets `fill_model` on its TICK arm alone, so no profile can reach that configuration, and
    /// the error runs pessimistic.
    fn charges_impact(&self, is_maker: bool) -> bool {
        !is_maker || self.fill_model != FillModelKind::Bar
    }
}
