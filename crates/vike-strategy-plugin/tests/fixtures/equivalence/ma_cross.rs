//! `ma_cross` — the fixture strategy `tests/equivalence.rs` drives down BOTH mechanisms.
//!
//! This file is an ORDINARY user strategy: it obeys the entry-file contract
//! (`crates/vike-user-strategies/src/lib.rs`'s module doc — a folder-named file exporting
//! `build<B: HftBroker + 'static>(&toml::Value) -> Box<dyn Strategy<B> + Send>`), it imports only
//! crates the user-facing API surface declares, and it has no idea FFI exists. That is the whole point: the
//! equivalence test compiles THESE EXACT BYTES twice — once as a module of the test binary (the
//! shape `vike-user-strategies`' generated registry produces) and once spliced into the cdylib
//! template by the real builder — and compares what the two produced.
//!
//! ⚠ **It is deliberately NOT a buy-and-hold.** A strategy that submits one order on bar 1 and
//! never reads the broker again would agree across the two mechanisms for reasons that say
//! nothing about them: it exercises one `submit_market` thunk and no arithmetic at all. This one
//! folds a running float sum into two simple moving averages, flips a SIGNED target position on
//! every crossing, and sizes that target from three separate reads back across the broker vtable
//! (`price`, `equity`, `position`) — so the comparison covers four of the fifteen `BrokerVTable`
//! thunks, the `CBar` round trip on every bar, the `warmup` export, and a float fold whose result
//! decides both the DIRECTION and the SIZE of every order.
//!
//! ⚠ **It used to implement `on_bar` and `warmup` AND NOTHING ELSE, and that was a constraint of
//! the PLUGIN rather than of the strategy** — `PluginVTable` carried exactly those two, so a
//! fixture implementing `on_fill` would have diverged for a reason already written down. At
//! `ABI_VERSION` 3 that constraint is gone, and this file now overrides every hook the vtable
//! carries. Each override is written to CHANGE BEHAVIOUR rather than merely to be present,
//! because the proof is a bit-for-bit comparison of what the two mechanisms TRADED: a hook that
//! arrived but changed nothing would be indistinguishable from a hook that never arrived.
//!
//! **What each override puts at stake**, so a reader can tell what a red run is telling them:
//!
//! | hook | how a failure to deliver it shows up |
//! |---|---|
//! | `warmup` | the R2 gate opens at a different bar; every index shifts |
//! | `on_start` | `armed` stays false and the strategy trades NOTHING at all |
//! | `on_bar` | the SMA fold never advances |
//! | `on_fill` | `pct_scale` never shrinks, so every order after the first fill is sized wrong |
//! | `on_schedule` | the periodic re-evaluation never fires, so a rebalance is missing |
//! | `on_quote_tick` | the tick lane's whole fold is empty — no trades at all |
//! | `on_trade_tick` | the print never reaches the fold; the SMA runs on quotes alone |
//! | `on_order_book` | `book_bias` stays 0 and every tick-lane order is sized wrong |
//! | `on_stop` | the parting order is absent from the engine's `pending` list |
//!
//! ⚠ **It also reaches the OTHER TWO crates the user-facing API surface declares**, and that is
//! the whole content of the design's open question 3. A manifest that merely LISTS
//! `vike-strategy` and `vike-indicators` proves nothing — an unnamed crate is dead code the
//! linker drops, so the plugin would build, load and agree while resolving neither. So:
//!
//! | crate | what this file actually reaches | how a failure to deliver it shows up |
//! |---|---|---|
//! | `vike_indicators` | the CATALOG, by name — `registry::make_with("rsi", …)`, advanced on every bar AND every tick | `rsi_scale` stays 1.0 and every order's SIZE differs from the run with it on |
//! | `vike_strategy` | the CONTROLLER framework — a real `MomentumController` built by that crate's OWN `from_params`, invited at every decision point | `controller_scale` stays 1.0; same, and the intent's SIDE and QTY both feed the multiplier |
//!
//! Each is switched OFF by an ordinary params reading (`rsi_period = 0`, no `[controller]`
//! table), which is what lets `tests/equivalence.rs` prove the contributions are non-empty by
//! running the compiled half twice and requiring the two trade lists to DIFFER — rather than by
//! asserting that a `use` line is present.
//!
//! ⚠ The six LIVE-ONLY hooks (`on_feed_status`, `on_mark`, `on_reference_quote`, `on_flow`,
//! `on_order_event`, `on_params_updated`) are implemented too, but the backtest engines never
//! fire them — every one of their doc comments in `vike_model::strategy` says so in as many
//! words. They are therefore written to submit a SELF-DESCRIBING order through the broker
//! (`"<hook>:<payload>"`), which is the only channel a plugin has back to its host, so
//! `tests/equivalence.rs`'s narrower witnesses can dispatch them directly against a real built
//! `.so` and read back exactly which hook fired with exactly which payload. That is a weaker
//! proof than the equivalence comparison and the test says so where it lives.
//!
//! ⚠ Those six SUBMIT ON A FABRICATED SYMBOL, which `SimBroker::idx` would PANIC on (it resolves a
//! symbol by position in the mounted universe and `unwrap_or_else(|| panic!(..))`). That is safe
//! only because no backtest engine ever calls them — the same fact that makes them unprovable by
//! the equivalence comparison is what makes this encoding harmless. A witness that drives one of
//! them must therefore use a broker of its own, never a `SimBroker`, and the ones in
//! `tests/equivalence.rs` do.
//!
//! ⚠ **No `Cargo.toml` sits beside this file, deliberately.** `tests/load_refusals.rs`'s fixture
//! builder walks `tests/fixtures/*` and skips any directory without one, so this fixture costs
//! that test binary nothing.

use vike_indicators::Indicator;
use vike_model::{
    Bar, Broker, FeedStatus, Fill, FlowToxicity, L2Book, MarkTick, OrderEventKind, OrderLifecycle,
    QuoteTick, Strategy, StrategyParams, TradeTick,
};
use vike_strategy::{Controller, MomentumController};

/// A dual-SMA crossover holding a signed, equity-scaled target position.
pub struct MaCross {
    fast: usize,
    slow: usize,
    /// Fraction of equity the target notional is sized at, signed by the crossover.
    pct: f64,
    /// Every close this instance has been shown, in order — the fold input. On the bar lane it is
    /// fed by `on_bar`; on the tick lane by `on_quote_tick` and `on_trade_tick` together.
    closes: Vec<f64>,
    /// `-1` short, `0` flat, `1` long: the position this strategy currently WANTS.
    signal: i32,
    /// Set by `on_start`. Until it is true this strategy submits nothing at all, so a mechanism
    /// that never delivered `on_start` produces an EMPTY trade list rather than a subtly
    /// different one — the loudest failure available for a hook whose payload is nothing.
    armed: bool,
    /// How many fills this instance has been told about, capped where it is consumed.
    fills: usize,
    /// How many of those were BUYS — the fill's `side`, read rather than counted past.
    buy_fills: usize,
    /// The running filled QUANTITY — the fill's `size`, read for its magnitude rather than only
    /// for being positive. Both feed `pct_scale` below.
    filled_units: f64,
    /// The order-size multiplier `on_fill` walks down — the behaviour change that makes a
    /// delivered fill distinguishable from an undelivered one.
    pct_scale: f64,
    /// Top-of-book size bias in `[-1, 1]`, read out of `on_order_book`'s best bid/ask. Scales
    /// tick-lane order size, so a book that never arrived shows up as a different trade list.
    book_bias: f64,
    /// The last symbol a market hook delivered. `on_stop` has no payload of its own and the
    /// portable `Broker` surface has no "what am I mounted on" read, so the parting order can only
    /// be placed on a symbol the strategy was TOLD about — which is also what keeps it from
    /// naming one `SimBroker::idx` would panic on.
    last_symbol: String,
    /// A REAL indicator out of `vike_indicators`' catalog, resolved BY NAME through
    /// [`vike_indicators::registry::make_with`] rather than by naming a struct — because "the
    /// catalog" is the surface the design's open question 3 is about, and a direct `Rsi::new()`
    /// would prove only that one type is reachable.
    ///
    /// `None` when the params document asks for no period. That is the off switch
    /// `tests/equivalence.rs`'s differential probe flips to prove this contribution is not inert.
    rsi: Option<Box<dyn Indicator>>,
    /// The indicator's latest reading, `NAN` until it warms. Read by [`rsi_scale`] on every
    /// decision, so an indicator that never advanced — or a catalog that resolved nothing —
    /// changes the SIZE of every order this strategy sends.
    rsi_last: f64,
    /// A REAL type out of `vike_strategy`'s Controller framework, built by that crate's OWN
    /// params reader (`MomentumController::from_params`) so the surface under test is the
    /// framework's rather than this file's reading of it.
    ///
    /// It is invited at every decision point and its answer scales order size. `None` when the
    /// params document carries no `[controller]` table — the other half of the differential
    /// probe.
    controller: Option<MomentumController>,
}

/// The mean of the last `n` closes, or `None` until there are `n` of them.
///
/// An explicit `+=` fold rather than `iter().sum()`: the two are the same operation on `f64`
/// (`Sum for f64` is `fold(0.0, Add::add)`), and spelling it keeps the fold order visible at the
/// site whose bit-for-bit result the equivalence test compares.
fn sma(closes: &[f64], n: usize) -> Option<f64> {
    if n == 0 || closes.len() < n {
        return None;
    }
    let mut sum = 0.0;
    for c in &closes[closes.len() - n..] {
        sum += *c;
    }
    Some(sum / n as f64)
}

impl MaCross {
    /// The ONE decision body, shared by the bar lane and the tick lane so the two cannot drift
    /// about what a crossing means. `size_scale` is 1.0 on the bar lane and carries the book bias
    /// on the tick lane.
    fn decide<B: Broker>(&mut self, broker: &mut B, symbol: &str, size_scale: f64) {
        if !self.armed {
            return;
        }
        // The controller is invited BEFORE any gate below can return early, so its own momentum
        // reference advances on a schedule the crossover does not decide — which is what makes
        // its answer depend on the whole tape rather than on the handful of bars a crossing
        // happens to land on.
        let ctrl_scale = self.controller_scale(broker, symbol);
        let (Some(fast), Some(slow)) = (sma(&self.closes, self.fast), sma(&self.closes, self.slow))
        else {
            return;
        };
        let want = if fast > slow {
            1
        } else if fast < slow {
            -1
        } else {
            0
        };
        if want == self.signal {
            return;
        }
        if symbol.is_empty() {
            return;
        }
        let price = broker.price(symbol);
        if !price.is_finite() || price <= 0.0 {
            return;
        }
        let target = broker.equity()
            * self.pct
            * self.pct_scale
            * size_scale
            * ctrl_scale
            * rsi_scale(self.rsi_last)
            * f64::from(want)
            / price;
        let delta = target - broker.position(symbol);
        if delta.abs() <= f64::EPSILON {
            self.signal = want;
            return;
        }
        broker.submit_market(symbol, if delta > 0.0 { 1 } else { -1 }, delta.abs());
        self.signal = want;
    }

    /// Invite the `vike_strategy` controller and turn its answer into an order-size multiplier.
    ///
    /// The multiplier reads the intent's SIDE and its QTY — not merely that an intent came back —
    /// so a framework that was linked but answered with a default-constructed intent scales
    /// differently from one that read the `[controller]` table. `1.0` when there is no controller
    /// or when it declines, which is what the differential probe in `tests/equivalence.rs` uses
    /// as its baseline.
    fn controller_scale<B: Broker>(&mut self, broker: &B, symbol: &str) -> f64 {
        let Some(c) = self.controller.as_mut() else {
            return 1.0;
        };
        match c.evaluate(broker, CONTROLLER_VENUE, symbol) {
            Some(intent) => 1.0 + CTRL_GAIN * f64::from(intent.side) * intent.qty,
            None => 1.0,
        }
    }

    /// Advance the catalog indicator by one bar and remember its reading.
    ///
    /// Takes a `Bar` because that is what [`vike_indicators::Indicator::on_bar`] takes; the tick
    /// lanes synthesize one through [`flat_bar`] rather than skipping the indicator, so the
    /// contribution is live on BOTH lanes the equivalence comparison drives.
    fn feed_indicator(&mut self, bar: &Bar) {
        if let Some(ind) = self.rsi.as_mut() {
            self.rsi_last = ind.on_bar(bar).first().copied().unwrap_or(f64::NAN);
        }
    }
}

impl<B: Broker> Strategy<B> for MaCross {
    /// Declared so the equivalence comparison covers the `warmup` export too — the engine folds
    /// this into its own R2 gate (`StrategyEngine::new`), so a plugin answering differently would
    /// shift every bar index and be visible in the trade list rather than nowhere.
    fn warmup(&self) -> usize {
        self.slow
    }

    /// The arming latch. Cheap to deliver, catastrophic to miss — which is exactly why the rest
    /// of the strategy is gated on it.
    fn on_start(&mut self, _broker: &mut B) {
        self.armed = true;
    }

    fn on_bar(&mut self, broker: &mut B, bar: &Bar) {
        self.closes.push(bar.close);
        self.feed_indicator(bar);
        let symbol = bar.symbol.as_deref().unwrap_or("");
        self.last_symbol = symbol.to_string();
        self.decide(broker, symbol, 1.0);
    }

    /// Each fill walks the size multiplier down. A mechanism that never delivered a fill keeps
    /// sizing at 1.0 and the trade SIZES diverge from the second trade onward — a difference the
    /// bit-for-bit comparison reports on `trade[N].size` rather than one a reader has to infer.
    ///
    /// ⚠ **All THREE of `side`, `size` and the arrival itself decide the multiplier, and the
    /// comment here used to claim that while the body read only `fill.size <= 0.0`.** A count
    /// alone is satisfied by a mirror that crossed every field wrong; what makes a wrong field
    /// visible is a field being READ into behaviour:
    ///
    /// * `fills` — the arrival. A hook never delivered leaves it at 0.
    /// * `buy_fills` — the SIDE. A mirror that crossed `side` inverted walks a different ladder.
    /// * `filled_units` vs [`FILL_UNITS_LADDER`] — the SIZE's MAGNITUDE, not merely its sign. A
    ///   mirror that halved every size crosses the ladder later in the run, or never; one that
    ///   zeroed them is refused by the guard below and leaves `fills` at 0 as well.
    fn on_fill(&mut self, _broker: &mut B, fill: &Fill) {
        if fill.size <= 0.0 || fill.side == 0 {
            return;
        }
        self.fills += 1;
        if fill.side > 0 {
            self.buy_fills += 1;
        }
        self.filled_units += fill.size;
        let size_step = usize::from(self.filled_units > FILL_UNITS_LADDER);
        let steps = self.fills.min(5) + self.buy_fills.min(3) + size_step;
        self.pct_scale = 1.0 - 0.01 * steps as f64;
    }

    /// The periodic re-evaluation: clearing `signal` makes the next bar re-price the target
    /// against the equity and price of that moment, so a schedule tag that never arrived leaves a
    /// rebalance out of the trade list.
    fn on_schedule(&mut self, _broker: &mut B, tag: &str) {
        if tag == REBALANCE_TAG {
            self.signal = 0;
        }
    }

    fn on_quote_tick(&mut self, broker: &mut B, q: &QuoteTick) {
        self.closes.push(q.mid());
        self.feed_indicator(&flat_bar(q.ts, q.mid(), &q.symbol));
        self.last_symbol = q.symbol.clone();
        // The book bias moves size by at most 10%, so a book that never arrived changes every
        // tick-lane order's size without changing the direction of a single one.
        let scale = 1.0 + 0.1 * self.book_bias;
        self.decide(broker, &q.symbol, scale);
    }

    fn on_trade_tick(&mut self, broker: &mut B, t: &TradeTick) {
        self.closes.push(t.price);
        self.feed_indicator(&flat_bar(t.ts, t.price, &t.symbol));
        self.last_symbol = t.symbol.clone();
        let scale = 1.0 + 0.1 * self.book_bias;
        self.decide(broker, &t.symbol, scale);
    }

    /// Reads TWO real levels — the best bid's and the best ask's resting size — rather than only
    /// asking whether a book arrived. A cursor that delivered the levels in the wrong order, or
    /// dropped one side, or lost the quantities, changes this number and therefore every
    /// subsequent order's size.
    fn on_order_book(&mut self, _broker: &mut B, book: &L2Book) {
        self.book_bias = top_of_book_bias(book);
    }

    /// A parting order, sized from the fill count. It never fills (the run is over), so it lands
    /// in the engine's `pending` list — which is where `tests/equivalence.rs` reads it, because a
    /// `BacktestResult` carries no trace of `on_stop` at all.
    ///
    /// A LIMIT far below any price this fixture's series reaches, deliberately: `pending` may also
    /// hold a market order from the final bar, and `OrderKind::Limit` is what lets the test say
    /// "the parting order is HERE" rather than "something is here". `ON_STOP_PRICE` is public so
    /// the test asserts the price the strategy actually used.
    fn on_stop(&mut self, broker: &mut B) {
        if self.last_symbol.is_empty() {
            return;
        }
        broker.submit_limit(&self.last_symbol, 1, 1.0 + self.fills as f64, ON_STOP_PRICE);
    }

    // ---- the six LIVE-ONLY hooks ----------------------------------------------------------
    //
    // No backtest engine fires any of these (each one's own doc in `vike_model::strategy` says
    // so), so they cannot change a trade list and cannot be proven by the equivalence
    // comparison. What they CAN do is describe themselves through the one channel a plugin has
    // back to its host — an order — so a direct dispatch against a real built `.so` reads back
    // which hook fired and what payload reached it. The symbol carries the evidence; the qty
    // carries whatever scalar the payload has.

    fn on_feed_status(&mut self, broker: &mut B, status: FeedStatus) {
        let (name, code) = match status {
            FeedStatus::Disconnected => ("disconnected", 1.0),
            FeedStatus::Stale => ("stale", 2.0),
            FeedStatus::Live => ("live", 3.0),
        };
        broker.submit_market(&format!("feed:{name}"), 1, code);
    }

    fn on_mark(&mut self, broker: &mut B, mark: &MarkTick) {
        broker.submit_market(&format!("mark:{}:{}", mark.symbol, mark.ts), 1, mark.price);
    }

    fn on_reference_quote(&mut self, broker: &mut B, venue: &str, q: &QuoteTick) {
        broker.submit_market(&format!("refq:{venue}:{}", q.symbol), 1, q.mid());
    }

    fn on_flow(&mut self, broker: &mut B, flow: FlowToxicity) {
        broker.submit_market(&format!("flow:{}", flow.ts), 1, flow.bid);
        broker.submit_market(&format!("flow:{}", flow.ts), -1, flow.ask);
    }

    fn on_order_event(&mut self, broker: &mut B, event: &OrderLifecycle) {
        let (kind, reason) = match &event.kind {
            OrderEventKind::Accepted => ("accepted", String::new()),
            OrderEventKind::Rejected { reason } => ("rejected", reason.clone()),
            OrderEventKind::Denied { reason } => ("denied", reason.clone()),
            OrderEventKind::Canceled { reason } => ("canceled", reason.clone()),
            OrderEventKind::Expired => ("expired", String::new()),
            OrderEventKind::Filled => ("filled", String::new()),
        };
        // The tag is rendered as `-` when ABSENT, which is a different string from an EMPTY tag —
        // the distinction `COptStrRef` exists to carry, asserted here rather than assumed.
        let tag = match &event.tag {
            Some(t) => format!("some({t})"),
            None => "none".to_string(),
        };
        broker.submit_market(
            &format!("ord:{}:{tag}:{kind}:{reason}", event.client_order_id),
            1,
            1.0,
        );
    }

    fn on_params_updated(&mut self, broker: &mut B, params: &StrategyParams) {
        match params {
            StrategyParams::PositionController(p) => {
                // `take_profit` is the field a TOML hop could not have carried at all when it is
                // `None` — reporting it is what makes this witness say something about the
                // ENCODING and not only about the dispatch.
                let tp = match p.barriers.take_profit {
                    Some(v) => format!("some({v})"),
                    None => "none".to_string(),
                };
                broker.submit_market(
                    &format!("params:controller:{}:{}:{tp}", p.cooldown_ms, p.threshold),
                    1,
                    p.qty,
                );
            }
            StrategyParams::SpreadMaker(_) => {
                broker.submit_market("params:spread_maker", 1, 1.0);
            }
            StrategyParams::Xemm(_) => {
                broker.submit_market("params:xemm", 1, 1.0);
            }
        }
    }
}

/// Top-of-book size bias in `[-1, 1]` — `(bidQty - askQty) / (bidQty + askQty)` over the best
/// level of each side, `0.0` when either side is empty.
///
/// ⚠ **Public and a free function so `tests/equivalence.rs`'s engine-emission probe can call THIS
/// expression rather than a second copy of it.** The probe's whole job is to assert that a real
/// run produces a NON-ZERO bias — a claim about this arithmetic over the engine's real book — and
/// a re-typed copy would let the two drift until the probe asserted something the strategy does
/// not compute.
pub fn top_of_book_bias(book: &L2Book) -> f64 {
    match (book.best_bid(), book.best_ask()) {
        (Some(b), Some(a)) if b.qty + a.qty > 0.0 => (b.qty - a.qty) / (b.qty + a.qty),
        _ => 0.0,
    }
}

/// The catalog name this fixture resolves an indicator by. A registry LOOKUP rather than a struct
/// name, because the surface under test is the catalog: a `use vike_indicators::indicators::Rsi`
/// would prove one type reachable and say nothing about `registry()`.
pub const RSI_NAME: &str = "rsi";

/// How hard the indicator's reading moves order size: `1 + gain * (rsi - 50)`. At the fixture's
/// period a reading wanders roughly 25..75, so the multiplier spans about ±5% — small enough that
/// the crossover still decides direction, large enough that every order's SIZE differs from the
/// no-indicator run in a way the bit-for-bit comparison reports on `trade[N].size`.
pub const RSI_SCALE_GAIN: f64 = 0.002;

/// Order-size multiplier from the catalog indicator's latest reading — `1.0` while it is NaN (it
/// has not warmed, or there is no indicator at all).
///
/// ⚠ **Public and a free function for the same reason as [`top_of_book_bias`]**: the probe in
/// `tests/equivalence.rs` calls THIS expression rather than a second copy of it, so the two
/// cannot drift until the probe is asserting arithmetic the strategy does not perform.
pub fn rsi_scale(rsi: f64) -> f64 {
    if rsi.is_finite() { 1.0 + RSI_SCALE_GAIN * (rsi - 50.0) } else { 1.0 }
}

/// Resolve the fixture's indicator out of the real catalog, or `None` for a period of zero.
///
/// Public so the probe builds the SAME instance the strategy builds, through the same registry
/// call with the same parameter.
pub fn make_rsi(period: usize) -> Option<Box<dyn Indicator>> {
    if period == 0 {
        return None;
    }
    Some(vike_indicators::registry::make_with(RSI_NAME, &[period as f64]).unwrap_or_else(|| {
        panic!(
            "the indicator catalog must resolve `{RSI_NAME}` — a None here means the registry \
             linked but answered nothing, which would make this fixture's indicator silently \
             inert on BOTH mechanisms and let the comparison agree for the wrong reason"
        )
    }))
}

/// The venue string the controller is asked about. It is echoed into the `PositionIntent` and
/// never reaches a broker, so it names nothing `SimBroker::idx` must resolve.
pub const CONTROLLER_VENUE: &str = "sim";

/// How hard the controller's answer moves order size: `1 + gain * side * qty`. With the params
/// document's `qty` this is a few percent either way, signed by the intent's DIRECTION — so a
/// framework that crossed the side wrong scales the opposite way rather than not at all.
pub const CTRL_GAIN: f64 = 0.01;

/// An OHLC bar with no range at all, at `price`. The tick lanes hand this to the catalog
/// indicator, which takes a `Bar` and nothing else — feeding it is what keeps the indicator's
/// contribution live on the `run_ticks` lane instead of leaving it to the bar lane alone.
fn flat_bar(ts: i64, price: f64, symbol: &str) -> Bar {
    Bar {
        ts,
        open: price,
        high: price,
        low: price,
        close: price,
        volume: 0.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: Some(symbol.to_string()),
    }
}

/// The running filled quantity at which `on_fill`'s multiplier takes its extra step down — the
/// threshold that makes the fill's SIZE decide behaviour rather than only its presence.
///
/// Chosen to sit PARTWAY through a run rather than before or after it: at the fixture's sizing
/// (roughly a third of ~100k equity at ~100 a unit, so a few hundred units a fill) both lanes
/// cross it, and cross it at a bar the tape decides. A threshold nothing reaches, or one
/// everything passes on its first fill, would be a constant wearing a magnitude's clothes.
pub const FILL_UNITS_LADDER: f64 = 5_000.0;

/// The schedule tag this strategy reacts to. Public so `tests/equivalence.rs` registers exactly
/// the tag the strategy listens for, rather than two copies of one string that can drift apart.
pub const REBALANCE_TAG: &str = "ma-cross-rebalance";

/// The price `on_stop`'s parting LIMIT rests at — far below anything this fixture's series
/// reaches, so it can never fill and can never be confused with a working order from the run.
/// Public for the same reason as the tag above.
pub const ON_STOP_PRICE: f64 = 1.0;

/// The entry-file contract: lenient param reading, same idiom as the built-in `from_params` arms.
pub fn build<B: vike_model::HftBroker + 'static>(
    params: &toml::Value,
) -> Box<dyn vike_model::Strategy<B> + Send> {
    let window = |key: &str, default: usize| -> usize {
        params
            .get(key)
            .and_then(toml::Value::as_integer)
            .map(|i| i.max(1) as usize)
            .unwrap_or(default)
    };
    let pct = params
        .get("pct")
        .and_then(|v| v.as_float().or_else(|| v.as_integer().map(|i| i as f64)))
        .unwrap_or(0.25);
    // ⚠ Both default to OFF, and that is the same rule the rest of this reader follows: every
    // value the equivalence params document carries DIFFERS from the fallback, so a params table
    // that failed to cross runs a visibly different strategy rather than the same one. Off is
    // also the spelling `tests/equivalence.rs`'s differential probes flip to, which is why the
    // switches are ordinary param readings rather than a knob invented for a test.
    let rsi_period = params
        .get("rsi_period")
        .and_then(toml::Value::as_integer)
        .map(|i| i.max(0) as usize)
        .unwrap_or(0);
    let controller = params.get("controller").map(MomentumController::from_params);
    Box::new(MaCross {
        fast: window("fast", 5),
        slow: window("slow", 20),
        pct,
        closes: Vec::new(),
        signal: 0,
        armed: false,
        fills: 0,
        buy_fills: 0,
        filled_units: 0.0,
        pct_scale: 1.0,
        book_bias: 0.0,
        last_symbol: String::new(),
        rsi: make_rsi(rsi_period),
        rsi_last: f64::NAN,
        controller,
    })
}
