//! The guards that refuse a comparison which would agree about nothing.

use vike_model::{Bar, QuoteTick, Strategy, TradeTick};
use vike_sim::SimBroker;

use super::drive::{Lane, Run, drive};
use super::{N_BARS, N_TICKS, PARAMS_TOML, ma_cross};

/// The comparison is only worth anything if the fixture actually did something. Asserted on the
/// COMPILED side, before the two are compared: if this run traded nothing, an equality that holds
/// proves nothing about either mechanism.
pub(super) fn assert_not_vacuous(run: &Run, lane: Lane) {
    let r = &run.result;
    let where_ = lane.label();
    match lane {
        Lane::Bars => assert_eq!(
            r.equity_curve.len(),
            N_BARS,
            "{where_}: the engine must have folded every bar — a short curve means the run \
             stopped early"
        ),
        // One equity sample per PRICED tick under the default `EquitySampling::EveryTick`; a
        // `Tick::Book` bumps neither the index nor the curve, which is what this asserts.
        Lane::Ticks => assert_eq!(
            r.equity_curve.len(),
            N_TICKS,
            "{where_}: the engine must have folded every priced tick and no book event"
        ),
    }
    // A FLOOR, not a golden — it exists so a fixture edit that quietly stopped the run from
    // trading fails here instead of making the comparison below vacuously true, and it is set well
    // BELOW what the run actually produces so it never becomes a number somebody rebaselines.
    // MEASURED on the latency box lane1, debug, 2026-09-22: 17 closed trades on the bar lane, 29 on the tick
    // lane. The bar lane's eight is the number this test has asserted since it was written.
    let floor = match lane {
        Lane::Bars => 8,
        Lane::Ticks => 12,
    };
    assert!(
        r.n_trades >= floor,
        "{where_}: the fixture must trade repeatedly for this comparison to mean anything, got \
         {} closed trades (floor {floor})",
        r.n_trades
    );
    assert!(
        r.trades.iter().any(|t| t.is_long),
        "{where_}: the fixture must open at least one LONG — a one-sided run would not exercise \
         the sign change this comparison is built around"
    );
    assert!(
        r.trades.iter().any(|t| !t.is_long),
        "{where_}: the fixture must open at least one SHORT — see above"
    );
    assert!(
        r.trades.iter().any(|t| t.pnl > 0.0) && r.trades.iter().any(|t| t.pnl < 0.0),
        "{where_}: the fixture must produce both winning and losing trades, so the trade-keyed \
         metrics (profit_factor, avg_win/avg_loss, payoff_ratio) are computed over real inputs \
         rather than over an empty branch"
    );
    // ...and the HOOK-specific half. Each of these is the observable an override was written to
    // move, so a comparison that passed while the hook did nothing is refused HERE rather than
    // reported as agreement.
    assert!(
        r.n_trades > 0,
        "{where_}: `on_start` never armed the strategy — it submits nothing until it fires, so a \
         zero-trade run is what an undelivered `on_start` looks like on BOTH mechanisms at once, \
         which the comparison itself could never catch"
    );
    let parting =
        run.pending.iter().find(|o| o.kind == vike_model::OrderKind::Limit).unwrap_or_else(|| {
            panic!(
                "{where_}: `on_stop` left no resting LIMIT in the engine's pending list. That \
                 order is the ONLY trace `on_stop` can leave — the run is over, so it can never \
                 fill and never becomes a Trade — and without it the pending comparison below is \
                 vacuous."
            )
        });
    assert_eq!(
        parting.price,
        Some(ma_cross::ON_STOP_PRICE),
        "{where_}: the parting order must rest at the price the fixture named"
    );
    assert!(
        parting.size > 1.0,
        "{where_}: the parting order's size is `1.0 + the fill count`, so a size of exactly 1.0 \
         means `on_fill` never reached the strategy even though the run traded — the one \
         observable that separates a delivered fill from an undelivered one on the compiled side \
         alone"
    );
    assert_the_engine_emits_the_state_dependent_hooks(lane);
}

/// The two hooks whose delivery depends on ENGINE STATE rather than on the fixture, witnessed
/// against the engine itself.
///
/// ⚠ **This is the hole the rest of `assert_not_vacuous` did not cover, and it is the one that
/// matters most.** `on_start`, `on_fill` and `on_stop` are probed above through observables the
/// FIXTURE produces. `on_schedule` and `on_order_book` are not like them: whether they arrive at
/// all is decided by state this file sets up and the engine then judges —
/// `StrategyEngine::run` fires `on_schedule` only for tags `Schedule::check_due` returns, and
/// `run_ticks` fires `on_order_book` only when `apply_book_event` reports `applied`, which under
/// `SeqPolicy::Strict` means every delta's `seq` was exactly `last_seq + 1`. A seq gap
/// introduced into [`tick_series`] drops the book and STOPS DELIVERY SILENTLY; a schedule rule
/// registered with the wrong cadence, or a warm-up that swallowed every due bar, does the same
/// for tags.
///
/// **And the bit-for-bit comparison cannot see either.** A hook that never arrives does not arrive
/// on EITHER mechanism, so both fall to identical behaviour and the comparison agrees perfectly —
/// green, with the hook unexercised. That is exactly the family this project has produced five
/// times: a test performing for the system the very step it was meant to witness.
///
/// So the claim is made where it can be MEASURED: the REAL fixture, wrapped in a counting
/// decorator, driven through the SAME [`drive`] the comparison uses — same engine, same params,
/// same tape, same registration — reporting what the engine actually emitted. No cargo, no
/// plugin; it gates in an ordinary run.
///
/// ⚠ **It WRAPS the fixture rather than standing in for it, and the first version did not.** A
/// passive spy that overrides only the hooks it counts submits no orders, so the engine has
/// nothing to fill and `on_fill` never fires — the probe's own fill assertions then measured a
/// strategy that could not trade, and said so: `0 buy / 0 sell` on its first real run. Every hook
/// is forwarded to the inner strategy, `warmup` included, so the run these counters describe is
/// the run the comparison compares.
///
/// The book half asserts a NON-ZERO bias rather than a delivery count, because a delivered book
/// the fixture reads as `0.0` changes no order and is worth exactly as little as no book at all.
fn assert_the_engine_emits_the_state_dependent_hooks(lane: Lane) {
    struct HookSpy {
        inner: Box<dyn Strategy<SimBroker>>,
        schedule_fires: std::rc::Rc<std::cell::Cell<usize>>,
        books_with_a_nonzero_bias: std::rc::Rc<std::cell::Cell<usize>>,
        buy_fills: std::rc::Rc<std::cell::Cell<usize>>,
        sell_fills: std::rc::Rc<std::cell::Cell<usize>>,
        filled_units: std::rc::Rc<std::cell::Cell<f64>>,
        fills_below_the_ladder: std::rc::Rc<std::cell::Cell<usize>>,
        fills_above_the_ladder: std::rc::Rc<std::cell::Cell<usize>>,
    }
    impl Strategy<SimBroker> for HookSpy {
        // ---- forwarded UNCHANGED, so the inner strategy runs exactly as it does in the
        // comparison. `warmup` above all: answering `0` here would open the R2 gate at a
        // different bar and make every count describe a different run.
        fn warmup(&self) -> usize {
            self.inner.warmup()
        }
        fn on_start(&mut self, b: &mut SimBroker) {
            self.inner.on_start(b);
        }
        fn on_bar(&mut self, b: &mut SimBroker, bar: &Bar) {
            self.inner.on_bar(b, bar);
        }
        fn on_quote_tick(&mut self, b: &mut SimBroker, q: &QuoteTick) {
            self.inner.on_quote_tick(b, q);
        }
        fn on_trade_tick(&mut self, b: &mut SimBroker, t: &TradeTick) {
            self.inner.on_trade_tick(b, t);
        }
        fn on_stop(&mut self, b: &mut SimBroker) {
            self.inner.on_stop(b);
        }

        // ---- counted, THEN forwarded ----
        fn on_schedule(&mut self, b: &mut SimBroker, tag: &str) {
            if tag == ma_cross::REBALANCE_TAG {
                self.schedule_fires.set(self.schedule_fires.get() + 1);
            }
            self.inner.on_schedule(b, tag);
        }
        fn on_order_book(&mut self, b: &mut SimBroker, book: &vike_model::L2Book) {
            // The fixture's OWN expression, called rather than re-typed — see its doc.
            if ma_cross::top_of_book_bias(book) != 0.0 {
                self.books_with_a_nonzero_bias.set(self.books_with_a_nonzero_bias.get() + 1);
            }
            self.inner.on_order_book(b, book);
        }
        fn on_fill(&mut self, b: &mut SimBroker, fill: &vike_model::Fill) {
            // ⚠ The SAME guard the fixture applies, applied before counting anything — because
            // the assertions below name the FIXTURE's accumulator as their subject, and counters
            // that accepted a fill the fixture skips would describe a different number under that
            // name. Immaterial against `SimBroker`, which emits no zero-size or side-0 fill; the
            // point is that the message and the measurement have one subject rather than two.
            // The fill is FORWARDED either way — the fixture does its own guarding, and skipping
            // the forward would make the spy's run diverge from the comparison's.
            if fill.size <= 0.0 || fill.side == 0 {
                self.inner.on_fill(b, fill);
                return;
            }
            if fill.side > 0 {
                self.buy_fills.set(self.buy_fills.get() + 1);
            } else {
                self.sell_fills.set(self.sell_fills.get() + 1);
            }
            // The RUNG the fixture's `size_step` takes for this fill, recomputed the way the
            // fixture computes it — the running total AFTER this fill, compared to the ladder.
            let units = self.filled_units.get() + fill.size;
            self.filled_units.set(units);
            if units > ma_cross::FILL_UNITS_LADDER {
                self.fills_above_the_ladder.set(self.fills_above_the_ladder.get() + 1);
            } else {
                self.fills_below_the_ladder.set(self.fills_below_the_ladder.get() + 1);
            }
            self.inner.on_fill(b, fill);
        }
    }

    let params: toml::Value =
        toml::from_str(PARAMS_TOML).expect("fixture params must be valid TOML");
    let spy = HookSpy {
        inner: ma_cross::build::<SimBroker>(&params),
        schedule_fires: std::rc::Rc::default(),
        books_with_a_nonzero_bias: std::rc::Rc::default(),
        buy_fills: std::rc::Rc::default(),
        sell_fills: std::rc::Rc::default(),
        filled_units: std::rc::Rc::default(),
        fills_below_the_ladder: std::rc::Rc::default(),
        fills_above_the_ladder: std::rc::Rc::default(),
    };
    let (fires, biased_books) = (
        std::rc::Rc::clone(&spy.schedule_fires),
        std::rc::Rc::clone(&spy.books_with_a_nonzero_bias),
    );
    let (buys, sells, units) = (
        std::rc::Rc::clone(&spy.buy_fills),
        std::rc::Rc::clone(&spy.sell_fills),
        std::rc::Rc::clone(&spy.filled_units),
    );
    let (below, above) = (
        std::rc::Rc::clone(&spy.fills_below_the_ladder),
        std::rc::Rc::clone(&spy.fills_above_the_ladder),
    );
    drive(spy, lane);
    let where_ = lane.label();

    // ...and the same treatment for `on_fill`'s other two reads, which are otherwise backed only
    // by the fixture's own comment — the exact shape that comment was corrected FOR.
    // `assert_not_vacuous` already proves a fill ARRIVED (the parting order's size exceeds 1.0);
    // these two prove the SIDE and the SIZE reads DISCRIMINATE rather than being constants
    // wearing a field's clothes.
    assert!(
        buys.get() > 0 && sells.get() > 0,
        "{where_}: the run filled on only ONE side ({} buy / {} sell), so the fixture's SIDE read \
         takes the same branch all run and a mirror that crossed `side` inverted would change \
         nothing",
        buys.get(),
        sells.get()
    );
    // ⚠ BOTH directions, because "never crosses" and "crossed on the FIRST fill" are the same
    // always-one-branch vacuity mirrored — and the first version of this assertion checked only
    // the former. A ladder every fill sits past is exactly as constant as one nothing reaches,
    // and a mirror that halved every size would change nothing under either. The PARTWAY property
    // is the whole claim, so it is asserted from the counters rather than computed in a comment:
    // arithmetic in a doc comment is what `16f7e4cdb` corrected one assertion to the left.
    assert!(
        below.get() > 0 && above.get() > 0,
        "{where_}: every fill took the SAME rung of `FILL_UNITS_LADDER` ({}) — {} below, {} \
         above, {} units filled in total. The fixture's size-MAGNITUDE read is then a constant \
         and a mirror that halved every size would change nothing. The ladder must sit PARTWAY \
         through the run: a threshold nothing reaches and a threshold everything passes on its \
         first fill are the same defect.",
        ma_cross::FILL_UNITS_LADDER,
        below.get(),
        above.get(),
        units.get()
    );
    match lane {
        Lane::Bars => {
            assert!(
                fires.get() > 0,
                "{where_}: the engine emitted NO `on_schedule` for `{}`. The fixture's override is \
                 then dead code on both mechanisms and the comparison agrees about nothing. Check \
                 the rule this file registers in `drive` against `StrategyEngine::run`'s warm-up \
                 gate — `check_due` is only consulted once `index >= warmup`.",
                ma_cross::REBALANCE_TAG
            );
            assert_eq!(
                biased_books.get(),
                0,
                "{where_}: the BAR lane emitted an `on_order_book`, which it has no call site for \
                 — this probe's lane split is wrong, or the engine grew one"
            );
        }
        Lane::Ticks => {
            assert!(
                biased_books.get() > 0,
                "{where_}: the engine delivered no book with a NON-ZERO top-of-book bias. Either \
                 `on_order_book` is not being emitted at all — `apply_book_event` drops the book \
                 on any `seq` that is not `last_seq + 1` under `SeqPolicy::Strict`, and stops \
                 delivering SILENTLY — or every delivered book had equal size on both sides, which \
                 the fixture reads as 0.0 and sizes no differently for. Either way its override is \
                 dead code on both mechanisms."
            );
            assert_eq!(
                fires.get(),
                0,
                "{where_}: the TICK lane emitted an `on_schedule`, which `run_ticks` has no call \
                 site for — this probe's lane split is wrong, or the engine grew one"
            );
        }
    }
}
