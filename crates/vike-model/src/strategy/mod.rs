//! The unified strategy seam — ONE `Strategy` trait + the ONE `OrderBroker` handle it trades through
//! (decision 0277: the `Broker` and `HftBroker` traits it replaced are deleted, their reads moved into
//! it and their verbs into its `place`, `amend` and `cancel`).
//!
//! `OrderBroker` is the strategy's window onto whichever execution stack is running it:
//! - backtest: the sim engine core implements `OrderBroker` with DIRECT mutation (fast path);
//! - live/paper: the per-`on_bar` live context implements `OrderBroker` by BUFFERING submissions,
//!   drained through the one live path (mint → RiskGate → client) after the handler returns.
//!
//! Write-once rule: a strategy written as `impl<B: OrderBroker> Strategy<B>` runs UNCHANGED on both
//! stacks (the r7 gate proves the results are bit-identical). A strategy that needs
//! backtest-only power (schedule registration, weighted/bracket submits, sizers, multi-TF
//! reads) is written against the concrete sim broker instead — `impl Strategy<OrderBrokerSimulator>` —
//! and the type system documents that it is not live-portable.
//!
//! Dispatch is GENERIC (monomorphized), never `dyn`, in the backtest hot loop — a vtable per
//! verb would regress the ~70 ns/bar engine. The live side may box the strategy
//! (`Box<dyn Strategy<LiveBroker> + Send>`): one virtual call per closed bar is noise there.
//!
//! # Where the line runs — and why widening [`OrderBroker`] would not move it
//!
//! The recurring proposal is "add the missing accessors to [`OrderBroker`] and every strategy becomes
//! portable". It is wrong, and the reason is worth stating once here rather than re-deriving it:
//! what keeps a backtest-only strategy backtest-only is not a missing READ, it is a CONCEPT the
//! live stack does not have. Five of them, each owned by the simulator alone:
//!
//! * the **position sizer** — the backtest's non-`raw` submit routes the requested size through a
//!   `PositionSizer` that may ignore it entirely (a percent-of-equity sizer returns
//!   `pct·equity/(price·mult)` and never reads the intent). The live path is ALWAYS raw. A verb
//!   added here would have to either always-raw (losing the concept) or invent a live sizer.
//! * the **shared-cash gate `weight`** — the priority order entries are DROPPED in when cash runs
//!   out. Live has no shared-cash gate, so a `weight` argument on a portable verb would name
//!   nothing on one of the two stacks.
//! * the **protective stop attached to the entry** — a field on the backtest order that also feeds
//!   the sizer's risk basis. Live arms a bracket as SEPARATE orders.
//! * the **symbol universe** — a backtest runs a slice of N symbols; a live mount trades ONE
//!   `(venue, symbol)` series and reaches a declared leg by name, so [`OrderBroker::symbols`] answers
//!   none there.
//! * the **bar-count schedule** — `every_n_bars` has no wall-clock meaning, and the live schedule
//!   takes wall-clock rules supplied as mount CONFIG, unreachable from a strategy's `on_start`.
//!
//! So `impl Strategy<OrderBrokerSimulator>` is a deliberate, load-bearing SECOND form, not a backlog item:
//! it is how a strategy says "I drive simulator machinery", and the type system is what stops it
//! being mounted live. The genuinely portable ones are written `impl<B: OrderBroker> Strategy<B>`, and
//! that is pinned by COMPILATION rather than by assertion — see
//! `crates/vike-strategies-check/src/lib.rs`'s `stand_in`: a fn generic over the broker that builds
//! every shipped taker file, its body type-checked at its DEFINITION, so narrowing a file's impl
//! back to one concrete broker stops compiling.
//!
//! ⚠ A second such probe once covered `TickPairMse`, and it was deleted with the portability it
//! claimed. Making that strategy generic silently swapped `position_of(&symbol).size` — the exchange's
//! own state — for [`OrderBroker::position`], which on `OrderBrokerSimulator` returns the response-latency SHADOW
//! whenever the opt-in latency gate is armed. Same code, different number, on a path a profile knob
//! reaches. `crates/vike-simulator/src/test_support/fixtures.rs`'s
//! `the_fixtures_are_deliberately_not_portable` records that, so the next reader does not
//! re-derive it from an impl that merely looks narrower than it needs to be.
//!
//! ⚠ **Adding a verb here whose NAME already exists as an inherent method on a concrete broker
//! creates a silent generic-vs-concrete split.** The inherent method wins at every concrete call
//! site while generic `B: OrderBroker` code gets the trait's — the same source line meaning two
//! different things. The tree dodged this once: `OrderBrokerSimulator`'s 7-argument inherent `submit_limit`
//! shadowed the deleted `Broker` trait's 4-argument one, so that impl spelled the delegation as UFCS.
//! Either pick a non-colliding name, or make the concrete broker's impl delegate explicitly.
//!
//! # What belongs in this module, and what does NOT
//!
//! This module is the SEAM — the traits and the event/lifecycle views every strategy on either
//! stack is written against. It is NOT a home for strategy configuration.
//!
//! A strategy family's tuning parameters enter this crate on exactly ONE criterion: the family
//! supports a LIVE re-tune, so its params ride [`StrategyParams`] — a serde field of the runtime's
//! journaled command lane (`vike_execution::Command::UpdateParams`), which sits BELOW every
//! strategy and must be able to name them. Two families qualify today: the `SpreadMaker` /
//! Avellaneda-Stoikov maker (its params tree is [`maker_params`], with its `AsParams` bag in
//! `maker_params/as_params.rs`, split out of this file so a re-tune diff never reads like a change
//! to the seam above) and the `PositionController`
//! harness ([`crate::strategy::triple_barrier::PositionControllerParams`]).
//!
//! Every OTHER family — `pairs_zscore`, `funding_carry`/`funding_capture`, `grid`/`dca_accumulate`,
//! `trailing_scalper`, `momentum`, the Polymarket takers, … — keeps its knobs as plain fields on
//! its own struct next to its implementation and reads them from the harness TOML params table in
//! its `from_params`. None of them appear here, and a new family should not unless it needs the
//! live-tune lane.

use crate::events::Event;
use crate::money::position::Fill;
use crate::orders::order::OrderListLeg;
use crate::orders::placement::OrderPlacement;
use vike_marketdata::{Bar, L2Book, TickQuote, TickTrade};

mod lifecycle;
pub mod triple_barrier;

pub use lifecycle::{
    FeedStatus, FlowToxicity, OrderEvent, OrderEventKind, RESERVED_SOURCE_KEY, TickMark,
    tag_busy_reason,
};

/// The strategy's ONE execution handle (decision 0277): the PORTABLE ORDER seam (decision 0269, item 2),
/// every kind and every group of the order layer placed, amended and cancelled by a strategy-chosen
/// TAG on either stack, plus the account and market reads. A strategy written as
/// `impl<B: OrderBroker> Strategy<B>` runs over the simulator's `OrderBrokerSimulator` and the live core's
/// `LiveBroker` (a paper engine is the live core over the paper book) through one path.
///
/// Every verb NAMES an inherent twin of the two concrete brokers, so the trait adds no order law:
/// each order meets the pre-trade gate and, live, the venue's capability row as any order of its kind
/// does (decision 0219 refuses a group whole, 0237 has the core hold a reduce-only stop where the row
/// cannot rest it). [`OrderBroker::deny`] sends nothing: it is the brokers' own refusal at intake,
/// handed the caller's words.
///
/// **Every verb that sends an order names its SYMBOL, and `""` is the mount's own** (decision 0277,
/// the one broker interface): the meaning both engines already gave an empty symbol, so a strategy
/// that trades only its own symbol may pass `""` and allocate nothing live. The simulator places a
/// seam order on the named symbol's book and refuses by name a symbol its run does not hold; live the
/// symbol rides the submission (`LiveBroker::named`), and the
/// drain resolves it by the mount's declaration (a mount that declared no symbols trades its own
/// whatever is named, as for every live verb). [`OrderBroker::amend`]
/// and [`OrderBroker::cancel`] name no symbol: a tag names ONE order of the mount, whatever its
/// symbol. Placing under a BUSY tag (one naming an order still working that no cancel was sent for)
/// is REFUSED on both brokers, a tagged `Denied` in [`tag_busy_reason`]'s words (P1): the older order
/// keeps its tag, so amend it, or cancel it and place in a later hook. The
/// fill of a tagged order names its tag ([`Fill::tag`]), and its lifecycle events do
/// ([`OrderEvent::tag`]).
///
/// Its verb names are its own (never an inherent method's of a concrete broker, the split this
/// module's doc warns of). Verbs only one engine has (weighted submits, `cancel_all`, target-percent,
/// a sizer) stay inherent methods of the concrete broker, so a strategy that calls one is written
/// against that broker.
pub trait OrderBroker {
    /// Place `order` on `symbol` under `tag`: `side` +1 buy / −1 sell, `qty` in units (raw: no sizer),
    /// and with `reduce_only` an order that may only shrink the position it meets (refused while there
    /// is nothing on its side to reduce, and capped to the position at its fill). Terms the
    /// order-terms rule refuses (`crate::order_terms_refusal`) are refused in its words.
    ///
    /// Tag `""` is an UNTAGGED order, which no later amend or cancel can name and no order event
    /// reports: exactly the deleted `Broker` trait's `submit_market` / `submit_limit` for a plain market
    /// or limit, and the inherent untagged verb of a trigger kind (decision 0277's rule 2), so a strategy
    /// moved onto this seam fills and hears what it did. A kind only a tag can work (a pegged order,
    /// an iceberg) is refused untagged, in words.
    fn place(
        &mut self,
        symbol: &str,
        tag: &str,
        side: i32,
        qty: f64,
        order: OrderPlacement,
        reduce_only: bool,
    );
    /// Place a reduce-only `order` on `symbol` under `tag` as a LEG of the exit `exit`: any name a
    /// strategy gives the stop, target and trailing orders protecting one position. Nothing links the
    /// legs here: one leg's fill cancels no other (the strategy cancels them by tag, or an order
    /// list's OCO does). DEFAULT: [`OrderBroker::place`] with `reduce_only`, the live meaning; a
    /// simulator whose in-bar fill rules read which orders are one exit's overrides it.
    fn place_exit_leg(
        &mut self,
        symbol: &str,
        exit: &str,
        tag: &str,
        side: i32,
        qty: f64,
        order: OrderPlacement,
    ) {
        let _ = exit;
        self.place(symbol, tag, side, qty, order, true);
    }
    /// Place a market order on `symbol` under `tag` that fills at THIS bar's close. DEFAULT: a market
    /// order through [`OrderBroker::place`], the live meaning (a bar strategy runs at the bar's close,
    /// so a market order sent then meets it); a simulator that fills a market order at the NEXT bar's
    /// open overrides it.
    fn place_at_close(&mut self, symbol: &str, tag: &str, side: i32, qty: f64, reduce_only: bool) {
        self.place(symbol, tag, side, qty, OrderPlacement::Market, reduce_only);
    }
    /// Place a reduce-only market order on `symbol` under `tag`, sent in reaction to a fill at `level`
    /// to close what is held (the order language's fills-a-day cap). DEFAULT: a reduce-only market
    /// order through [`OrderBroker::place`], the live meaning; a simulator walking the bar that fill
    /// is inside overrides it with the fill measured for that close: at the first point of the walk
    /// at or past `level` on the order's side.
    fn place_close_from_fill(&mut self, symbol: &str, tag: &str, side: i32, qty: f64, level: f64) {
        let _ = level;
        self.place(symbol, tag, side, qty, OrderPlacement::Market, true);
    }
    /// Whether a reduce-only order placed while nothing is held on its side RESTS, judged only at its
    /// fill. DEFAULT `false`, the live meaning and [`OrderBroker::place`]'s contract: it is refused at
    /// intake, so an exit issued with the entry it protects goes as that entry's OTO child. A
    /// simulator running the reference's backtest-only rule answers `true` (the owner's ruling of
    /// 2026-11-10 on decision 0269's wave 2).
    fn rests_reduce_only_while_flat(&self) -> bool {
        false
    }
    /// Place `legs` as ONE order list on `symbol` (`crate::OrderListLeg`: OTO through `parent`, OCO
    /// or OUO through `contingency` and `linked`, by index), each leg under its tag; the list is
    /// judged and refused WHOLE (`crate::build_order_list`). Live a list still trades the dispatching
    /// series, as before the symbol: it carries the symbol from the live multi-symbol step.
    fn place_group(&mut self, symbol: &str, legs: Vec<(String, OrderListLeg)>);
    /// Amend the order `tag` names IN PLACE: a new size, a new limit price, a new trigger, each `None`
    /// unchanged. Where each price lands on its kind is `crate::modified_prices`' rule (a stop's one
    /// price is its trigger; a trailing order amends its size only). No-op for an unknown tag or an
    /// order already ended.
    fn amend(
        &mut self,
        tag: &str,
        new_qty: Option<f64>,
        new_price: Option<f64>,
        new_trigger: Option<f64>,
    );
    /// Cancel the order `tag` names, of any kind, a group's leg included. No-op for an unknown tag or
    /// an order already ended.
    fn cancel(&mut self, tag: &str);
    /// Refuse, under `tag`, an order on `symbol` its caller judged it cannot send: terms only the
    /// caller can read (`vike_tactics`' `.chase()` and `.iceberg(visible)` combinations). Nothing is
    /// sent; the order ends as a terminal [`OrderEventKind::Denied`] in `reason`'s words, through the
    /// path the broker's own refusals at intake take, so the strategy hears it as it hears any refused
    /// order. `qty` is the order's size, for the refusal's record.
    fn deny(&mut self, symbol: &str, tag: &str, qty: f64, reason: &str);
    /// The AVERAGE ENTRY PRICE of the position in `symbol` (`""`: the mount's own), `0.0` when flat:
    /// the engine's own book of it, read beside [`OrderBroker::position`].
    fn average_price(&self, symbol: &str) -> f64;
    /// The run's symbol UNIVERSE, in the run's order: what a strategy that ranks symbols ranks.
    /// DEFAULT: none, a broker with no universe. The simulator answers its run's symbols. The live
    /// core answers none (a mount trades its own series and reaches a declared leg by name), so a
    /// strategy that ranks a universe trades in the simulator alone; a runtime-loaded Rust strategy's
    /// broker answers what its host's broker answers (since `ABI_VERSION` 8).
    fn symbols(&self) -> &[String] {
        &[]
    }

    // --- reads ---
    /// Signed position size in `symbol` (0.0 when flat).
    fn position(&self, symbol: &str) -> f64;
    /// Last mark for `symbol` (the engine's post-fill price; live: the bar close).
    fn price(&self, symbol: &str) -> f64;
    /// Account equity now (cash + Σ pos·price·mult / live: resolver-priced via `resolve_equity`).
    fn equity(&self) -> f64;
    /// Closed bars of `symbol` up to and INCLUDING the current step (no look-ahead).
    /// Single-symbol live brokers may ignore `symbol`.
    fn bars(&self, symbol: &str) -> &[Bar];
    /// Current step index (bars processed so far minus one).
    fn index(&self) -> usize;
    /// Current engine time, epoch milliseconds.
    fn now(&self) -> i64;

    // --- pre-trade liquidity reads (defaulted: an engine with no book answers "I don't know") ---

    /// **What would `qty` units actually cost me right now?** The volume-weighted price of
    /// consuming the resting book best-level-outward — `side` +1 buy (walks asks) / −1 sell
    /// (walks bids).
    ///
    /// This exists because a strategy cannot gate on a price it has no way to see. Without it the
    /// only price surface is [`OrderBroker::price`], a single scalar, and every strategy that sizes off
    /// it is implicitly assuming infinite depth at the top of book. Reading this before ordering
    /// is what lets a strategy refuse a trade whose edge does not survive the liquidity actually
    /// on offer.
    ///
    /// `None` means **the answer is unavailable or the book cannot honour it** — no L2 data for
    /// that symbol, or the displayed depth does not cover `qty`. It is never a price. A strategy
    /// must treat `None` as "do not trade this size", not as "assume the last price": that
    /// distinction is the whole value of the verb. Pair it with [`OrderBroker::depth_within_price`] to
    /// size DOWN to what is available instead of giving up.
    ///
    /// Displayed depth upper-bounds real fill quality (it ignores queue position, hidden size and
    /// the taker's own impact), so this is a bound, not a guarantee. The backtest answers it from
    /// the replayed L2 book with exactly the law its own fill path uses, so a strategy that gates
    /// on this read and then submits gets a fill consistent with what it was told.
    ///
    /// DEFAULT `None`: every existing broker — live, paper, bar-only backtest — is unchanged and
    /// needs no edit. A book-less engine truthfully says it does not know.
    fn quote_vwap(&self, symbol: &str, side: i32, qty: f64) -> Option<f64> {
        let _ = (symbol, side, qty);
        None
    }

    /// **How much could I get without paying worse than `limit_px`?** Total displayed size at
    /// `limit_px` or better (buy: asks at or below; sell: bids at or above).
    ///
    /// The sizing half of [`OrderBroker::quote_vwap`]: a strategy that knows the worst price it can
    /// accept asks this for the quantity that price supports, and submits THAT rather than a size
    /// the market cannot fill. `0.0` — the default, and the honest answer for a book-less engine —
    /// means "nothing available", which correctly sizes such a strategy to no trade.
    fn depth_within_price(&self, symbol: &str, side: i32, limit_px: f64) -> f64 {
        let _ = (symbol, side, limit_px);
        0.0
    }

    /// **How far behind the market is the data I am pricing `symbol` off?** The venue's delay
    /// label for that symbol's stream (decision 0140): real-time, delayed (a stock on an account
    /// without a real-time entitlement), or frozen.
    ///
    /// INFORMATION, never a gate: a strategy may trade off a delayed price, and nothing below it
    /// refuses or holds an order because of the label (`crate::StreamDelay`'s module doc). The label
    /// is recorded with every order the live core passes, so the strategy reading it is about
    /// knowing, not about permission.
    ///
    /// `None` means **no label was stated**: the venue does not label its streams, or the stream has
    /// not yet sent one. DEFAULT `None`, so every existing broker is unchanged; the live broker
    /// answers for its mount's own symbol.
    fn data_delay(&self, symbol: &str) -> Option<crate::StreamDelay> {
        let _ = symbol;
        None
    }

    /// **Which grid does the pre-trade gate hold `symbol`'s orders to?** The tick, lot and minimum
    /// size / notional the engine's own gate applies ([`crate::Grid`]): live, the gate's limits
    /// (the venue's instrument properties at a demo or live tier, the run profile's `[risk]` on paper);
    /// in a backtest, the instrument data's point-in-time properties. A strategy that would otherwise
    /// send an order the gate rounds or refuses reads it to snap, size or refuse in words first.
    ///
    /// `None` means no grid is known (no instrument data, or a symbol this broker cannot answer for).
    /// DEFAULT `None`, so every existing broker is unchanged.
    fn instrument_grid(&self, symbol: &str) -> Option<crate::Grid> {
        let _ = symbol;
        None
    }

    /// **What is one unit of `symbol`'s quantity worth?** The contract's kind and per-unit factor
    /// ([`crate::ContractValue`], decision 0170) the engine's own ledger books it at: live, the
    /// account's value of the mount's own symbol; in a backtest, the run's per-symbol value. A strategy
    /// that folds its own PnL (`vike_strategy`'s position executor) reads it so a 50-point future
    /// realizes 50 times the price move, and an inverse contract books its PnL in the settle coin.
    ///
    /// `None` means this broker cannot answer for `symbol`. DEFAULT `None`, so every existing broker is
    /// unchanged; a caller that needs a value takes `unwrap_or_default()`, the linear 1.0 unit.
    fn contract_value(&self, symbol: &str) -> Option<crate::ContractValue> {
        let _ = symbol;
        None
    }

    /// **The last bar of the input `name` that had CLOSED by now** (decision 0271): `name` is one of
    /// the strategy's [`Strategy::inputs`], as the strategy spells it. A bar of interval `I` opened at
    /// `ts` closes at `ts + I` and is seen by an event at `t` once `ts + I <= t`, so a bar closing
    /// exactly at `t` is seen and the bar in progress at `t` never is. A bar lane's event is the bar's
    /// CLOSE, where the strategy decides (a fill's hook inside the bar sees the inputs as of its open);
    /// a tick lane's is the tick.
    ///
    /// `None` before the input's first bar closes, and for a name the strategy did not declare. The last
    /// of [`OrderBroker::input_bars`], which is what a broker answers.
    fn input(&self, name: &str) -> Option<&Bar> {
        self.input_bars(name).last()
    }

    /// **Every bar of the input `name` that had CLOSED by now, oldest first** (decision 0271): the
    /// history [`OrderBroker::input`] is the last of, under the same as-of rule. It holds what the run loaded
    /// of the input before its window (a backtest reads a higher timeframe's whole stored history, and
    /// a lower or equal one from one of its bars before the window: `vike_backtest`'s input loader),
    /// so a strategy can warm an indicator of the input on its first bar.
    ///
    /// Empty before the input's first bar closes, and for a name the strategy did not declare. DEFAULT
    /// empty: a broker that loads no input. The simulator answers it from its as-of view, and a paper or
    /// live mount from the inputs its tradehub reads from the datahub, by the same rule and to the same
    /// depth (`vike_core`'s input view).
    fn input_bars(&self, name: &str) -> &[Bar] {
        let _ = name;
        &[]
    }

    /// **The strategy's closed trades so far, in the order the fills closed them**: the run's
    /// closed-trade ledger (`vike_analytics::closed_trades`: one lot per entry fill, first in first
    /// out, each row net of its share of both commissions), as the engine booked it, never a second
    /// booking. A fill's rows are in it before the strategy hears that fill.
    ///
    /// `None` means this broker keeps no closed-trade ledger, which is NOT "no trade closed": the
    /// simulator and a paper, demo or live mount (`vike_core::LiveBroker`) always keep one (an empty
    /// slice before the first close). DEFAULT `None`.
    fn closed_trades(&self) -> Option<&[crate::TradeClosedRow]> {
        None
    }
}

/// The ONE strategy trait, generic over its broker (monomorphized per engine).
///
/// All handlers default to no-ops; implement only what the strategy needs. Handlers are
/// skipped until `index >= warmup()` (the R2 gate) — enforced by the engines, not here.
#[expect(unused_variables)]
pub trait Strategy<B: OrderBroker> {
    fn warmup(&self) -> usize {
        0
    }
    /// The stored series this strategy READS beside its own bars, each by the name it passes to
    /// [`OrderBroker::input`] (decision 0271): `VENUE:SYMBOL:INTERVAL`, or `LANE:VENUE:SYMBOL:INTERVAL`
    /// for a `source=` lane such as a user's CSV import (`user-<label>`). A backtest plans and loads
    /// each one before it starts and refuses one the store does not hold; a profile's `[inputs]` may
    /// point a name at another series. A live mount reads each from the datahub and is refused by name
    /// when one cannot be read.
    /// Read once, before the run or the mount. Default: none, so every existing strategy is unchanged.
    fn inputs(&self) -> Vec<String> {
        Vec::new()
    }
    /// The symbols this strategy TRADES beside the one it is mounted on, on the mount's own venue: a
    /// live mount declares each as a leg (`vike_core::Mount::symbols`), so its bars reach the
    /// strategy and an order naming it is routed there, while an order naming any other symbol is
    /// refused. A backtest trades the run's symbols and reads nothing here. Read once, before the mount.
    /// Default: none, so every existing strategy trades its own symbol alone.
    fn symbols(&self) -> Vec<String> {
        Vec::new()
    }
    fn on_start(&mut self, broker: &mut B) {}
    /// Backtest: fires once per symbol per step (bar carries its symbol tag).
    /// Live: fires once per CLOSED bar of the mounted series.
    fn on_bar(&mut self, broker: &mut B, bar: &Bar) {}
    fn on_quote_tick(&mut self, broker: &mut B, q: &TickQuote) {}
    fn on_trade_tick(&mut self, broker: &mut B, t: &TickTrade) {}
    /// L2 book update for the mounted symbol (R8 HFT track). Net-new Rust surface — no Python
    /// twin — so default no-op keeps every existing strategy compiling unchanged.
    fn on_order_book(&mut self, broker: &mut B, book: &L2Book) {}
    fn on_schedule(&mut self, broker: &mut B, tag: &str) {}
    /// An order THIS strategy caused FILLED (fully or partly): it is the mount that minted the order's
    /// order client id that hears it, with the position and equity as they stood after THAT fill.
    /// Live, a fill nobody on this mount asked for — an operator ticket, a reconcile-synthesised
    /// fill, a liquidation, a Polymarket settlement — is NOT delivered here (decision 0116): the
    /// account takes it and `OrderBroker::position` shows it on the next hook. The simulator still fires
    /// this hook for settlement and expiry (the accepted parity gap in that record).
    fn on_fill(&mut self, broker: &mut B, fill: &Fill) {}
    /// A feed-health transition for the mounted stream (net-hardening §B): the feed disconnected,
    /// went stale, or recovered (see [`FeedStatus`]). This is the seam a strategy uses to "pull my
    /// quotes when my feed dies" (and resume on recovery). Fires ONLY on a status CHANGE (feed
    /// up↔down) — the data layer's stream-health machine already debounces to one signal per
    /// transition, so this is an OCCASIONAL control event, never a per-market-message call, and
    /// stays off the live core's per-tick hot path. Default no-op keeps every existing strategy
    /// (backtest and live) compiling and behaving unchanged; net-new Rust surface, no Python twin.
    /// The backtest engines never fire it (there is no live feed to disconnect); it is a live-only
    /// signal, harmless to implement in a portable `impl<B: OrderBroker> Strategy<B>`.
    fn on_feed_status(&mut self, broker: &mut B, status: FeedStatus) {}
    /// A REFERENCE/underlying MARK for a series this strategy WATCHES but is NOT mounted on (see
    /// [`TickMark`]) — the cross-symbol routing seam ("Option B"). The runtime routes a mark of the
    /// strategy's declared underlying symbol (a DIFFERENT symbol than the mounted one — e.g. the
    /// `btcusdt` RTDS spot a Polymarket BTC up/down maker anchors its fair mid on) here. Delivered off
    /// the OCCASIONAL mark-drain lane — an underlying mark drains WITH the venue marks (`drain_market`),
    /// well off the per-tick / per-market-message `p99 < 10µs` event fold, and touches no OMS fold —
    /// only the mounted strategy's hook + any orders it buffers, drained through the one live path. The
    /// broker ctx is built on the strategy's OWN (venue, symbol) — the underlying rides only in `mark`
    /// — so a strategy reads its own position/price and buffers orders exactly as on any other hook.
    /// NOT warmup-gated (an underlying observation is a market fact regardless of bar count, mirroring
    /// `on_feed_status`/`on_fill`).
    ///
    /// Default no-op: a strategy that ignores this hook is entirely UNAFFECTED — every existing
    /// strategy behaves identically, and a mount that declares no underlying is never routed one. Net-new
    /// Rust surface, no Python twin; a live-only signal the backtest engines never fire, harmless in a
    /// portable `impl<B: OrderBroker> Strategy<B>`.
    fn on_mark(&mut self, broker: &mut B, mark: &TickMark) {}
    /// An L1 touch from a DECLARED leg on a DIFFERENT VENUE — the cross-EXCHANGE reference-price
    /// seam, and the input a cross-exchange market maker (xEMM) prices its maker quotes off.
    ///
    /// The runtime routes a `TickQuote` here when the mount declared a leg
    /// `MountLeg::at(symbol, venue)` whose venue is NOT the mount's own and the tick matches that
    /// `(venue, symbol)` pair. `venue` names WHICH venue the touch came from — carried as an
    /// argument, not in the tick, because **no tick type carries a venue**: [`TickQuote`] and
    /// [`TickTrade`] carry `symbol` only and `L2Book` carries neither, and stamping a venue onto a
    /// journaled serde payload would be a wire change. Two venues can also legitimately use the
    /// SAME symbol string, so venue attribution cannot be inferred from the payload.
    ///
    /// WHY IT IS A DISTINCT HOOK, NOT `on_quote_tick`. Overloading `on_quote_tick` would make
    /// "which book am I resting in" depend on an operator not typing the same ticker for both legs
    /// — a maker that mistakes the reference venue's touch for its own quotes off its own quotes
    /// and self-references. A distinct hook makes the two lanes structurally disjoint: the runtime
    /// dispatches a tick to EITHER `on_quote_tick` (the mount's OWN venue) or here (a declared
    /// foreign venue), never both.
    ///
    /// THE BROKER IS THE MOUNT'S OWN. The `ctx` is built on the mount's `(venue, symbol)` — its own
    /// engine, its own bars, its own mark — and any order buffered here drains against the mount's
    /// OWN series, exactly as on `on_bar`/`on_quote_tick`. That is load-bearing rather than
    /// incidental: [`OrderBroker::amend`] and [`OrderBroker::cancel`] name no symbol, so their tag
    /// registry key is derived from the DRAIN's `(venue, symbol)`. Draining a reference-venue tick
    /// on the reference venue's series would key a maker quote under the WRONG venue AND route it
    /// there — placing the maker's own quote on the venue it meant to hedge on. So a strategy may
    /// safely place/re-price/pull its maker quotes from this hook; it must NOT assume the ctx
    /// describes the venue named by `venue`.
    ///
    /// Delivered off the same per-tick lane `on_quote_tick` rides, so it IS a per-market-message
    /// hook — but only for a mount that declared a foreign-venue leg. A runtime where no mount
    /// declared one never reaches the dispatch at all (one bool read), so the `p99 < 10µs` fold is
    /// untouched for every existing runtime. NOT warmup-gated: a reference touch is a market fact
    /// regardless of the mount's own bar count (mirroring `on_feed_status`/`on_mark`).
    ///
    /// Default no-op: a strategy that ignores this hook is entirely UNAFFECTED, and a mount that
    /// declares no foreign-venue leg is never routed one. Net-new Rust surface, no Python twin; a
    /// live-only signal the backtest engines never fire (an `OrderBrokerSimulator` slice is single-venue),
    /// harmless in a portable `impl<B: OrderBroker> Strategy<B>`.
    fn on_reference_quote(&mut self, broker: &mut B, venue: &str, q: &TickQuote) {}
    /// A per-side FLOW-TOXICITY update for the mounted stream (RTDS wallet-toxicity guard, see
    /// [`FlowToxicity`]): the current normalized toxic-flow intensity on each side of the book. This
    /// is the seam a market maker uses to WIDEN and CUT SIZE on the side toxic (edge-carrying /
    /// size-moving) flow is hitting. Delivered off the OCCASIONAL control lane — the runtime routes a
    /// toxicity reading for the mount's EXACT `(venue, symbol)` here (the toxic tape's asset IS the
    /// mounted token), NOT per market message, so an update folds at toxicity cadence and the
    /// `p99 < 10µs` market/tick fold is untouched (this hook touches NO OMS fold — only the mounted
    /// strategy's reaction + any orders it buffers, drained through the one live path). NOT
    /// warmup-gated (toxic flow is a market fact regardless of bar count, mirroring
    /// `on_feed_status`/`on_mark`).
    ///
    /// Default no-op: a strategy that ignores this hook is entirely UNAFFECTED — every existing
    /// strategy behaves identically, and a mount fed no toxicity is never routed one. Net-new Rust
    /// surface, no Python twin; a live-only signal the backtest engines never fire, harmless in a
    /// portable `impl<B: OrderBroker> Strategy<B>`.
    fn on_flow(&mut self, broker: &mut B, flow: FlowToxicity) {}
    /// An order-lifecycle transition for one of THIS strategy's orders — the venue ACCEPTED /
    /// REJECTED / CANCELED / EXPIRED it, the `RiskGate` DENIED it, or it FILLED COMPLETELY (see
    /// [`OrderEvent`]). This is the seam a position/order manager (e.g. a `PositionExecutor`
    /// harness) uses to drive retry / refresh / early-abort off a reject or cancel it would
    /// otherwise only infer from a fill timeout — and to detect a venue cancel it could not see at
    /// all — and the seam a TAGGED-order strategy uses to free the slot its tag names, by that same
    /// tag ([`OrderEvent::tag`]).
    ///
    /// ⚠ The FILL ITSELF is not delivered here: the executed quantity, price and the resulting
    /// position/equity stay on the richer [`Strategy::on_fill`] path (post-dedup, per-fill
    /// snapshot), which fires FIRST and is unchanged. What [`OrderEventKind::Filled`] adds is the
    /// ORDER's death — the fact `on_fill` structurally cannot carry, and the one that makes all
    /// three ways a resting order dies arrive through ONE hook with ONE shape.
    ///
    /// Delivered off the OCCASIONAL order-outcome lane: the runtime routes each transition to the
    /// mount that MINTED the order (by its order client id) — the SAME routing `on_fill` uses; an order
    /// no mount minted reaches none (decision 0116) — so this is
    /// OFF the per-tick / per-market-message hot path. An order event folds at ORDER cadence (a
    /// submit reply / a cancel), never per quote or bar, so the `p99 < 10µs` market/tick fold is
    /// untouched. NOT warmup-gated (an order outcome is an account fact regardless of bar count,
    /// mirroring `on_fill`). Because the live core is single-writer, this hook runs on the SAME core
    /// thread as the tick handlers, so any order it buffers drains through the one live path exactly
    /// like `on_fill`'s.
    ///
    /// Default no-op: a strategy that ignores this hook is entirely UNAFFECTED — SpreadMaker and
    /// every existing strategy behave identically. Net-new Rust surface, no Python twin; harmless in
    /// a portable `impl<B: OrderBroker> Strategy<B>`. The backtest engines fire it for the orders placed
    /// through [`OrderBroker`] alone (their end, delivered before the next bar: `OrderBrokerSimulator`'s bar
    /// lane), so a strategy that places none never hears it there.
    fn on_order_event(&mut self, broker: &mut B, event: &OrderEvent) {}
    /// A LIVE PARAMETER update for the mounted strategy (the live-parameter plane). Delivered off
    /// an OCCASIONAL control lane (a GUI/operator re-tune, not a market message — the runtime's
    /// `Command::UpdateParams`), so it is off the per-tick hot path and touches NO OMS fold. The
    /// strategy hot-swaps its own tunables from `params` — typically by matching its own
    /// [`StrategyParams`] variant and ignoring any other — and the new values take effect on its
    /// NEXT tick (a re-price rides the existing modify-in-place path, so resting orders / queue
    /// position are preserved; nothing is canceled just because a knob changed).
    ///
    /// Default no-op: a strategy that ignores this hook is entirely UNAFFECTED — there is no
    /// behavior change until an update is actually sent. Because the live core is single-writer
    /// (this hook and the tick handlers run on the SAME core thread, serialized through the one
    /// ingest queue), the swap is a plain `&mut self` field set — no lock, no arc-swap, and so NO
    /// per-tick read cost is added on the hot path. Net-new Rust surface, no Python twin; a
    /// live-only signal the backtest engines never fire, harmless in a portable `impl<B: OrderBroker>`.
    fn on_params_updated(&mut self, broker: &mut B, params: &StrategyParams) {}
    /// The READ side of the live-parameter plane: this strategy's CURRENT live tunables, or `None`
    /// when it publishes none. The exact inverse of [`Strategy::on_params_updated`] — what comes
    /// back is the bag that hook would consume, so an operator surface can read-modify-write one
    /// knob without re-typing (and thereby silently reverting) the rest.
    ///
    /// The AUTHORITY must be the strategy's own live fields, never an echo of the last bag it was
    /// handed: an implementor that transforms what it was given (as the `SpreadMaker` file does —
    /// it strips the A-S sub-bag into a live estimator and re-attaches it on read, which may have
    /// CLAMPED it) would otherwise report a value nobody holds. A clamp showing up in the read-back
    /// is information; a cached input reporting it away is the declared-but-unread failure this
    /// workspace engineers against.
    ///
    /// Default `None`, the [`Strategy::on_params_updated`] precedent verbatim: a strategy that
    /// ignores this hook is entirely UNAFFECTED. `None` is a FIRST-CLASS answer rather than a gap —
    /// the mounts that can answer this read are exactly the mounts `Command::UpdateParams` can
    /// address, so a strategy with no [`StrategyParams`] variant (a registry strategy mounted by
    /// name off a free-form TOML table) is honestly unreadable AND unwritable by this plane.
    ///
    /// Called off the COLD path only — the runtime fills it at the coalesced snapshot publish
    /// (`vike_core`'s `mount_views`), never on the per-message fold. It takes `&self` and returns an
    /// OWNED value, so an implementor whose params are expensive to materialise would pay that at
    /// every publish; every implementor today is a plain struct copy.
    fn params(&self) -> Option<StrategyParams> {
        None
    }
    fn on_stop(&mut self, broker: &mut B) {}
    /// Serialize this strategy's durable state (e.g. a market-maker's circuit-breaker counters +
    /// estimator) so it can survive a process restart, or `None` if it has nothing to persist —
    /// the default, so every existing strategy is unaffected. The runtime calls this at a
    /// checkpoint cadence it owns; the strategy owns the shape of the returned `Value` entirely.
    fn save_state(&self) -> Option<serde_json::Value> {
        None
    }
    /// Restore durable state previously returned by [`Strategy::save_state`] (e.g. on mount after
    /// a restart). Default no-op, so a strategy that never saves state is unaffected.
    fn load_state(&mut self, state: &serde_json::Value) {}
}

/// A boxed strategy is a strategy: every hook forwards to the inner `dyn Strategy<B>` through
/// one deref. This is what lets the live side box a strategy (`Box<dyn Strategy<LiveBroker> +
/// Send>`, per the module doc) and the backtest harness (`vike-backtest`'s
/// `harness::strategy_builder`) hand back a `Box<dyn Strategy<OrderBrokerSimulator>>` from a profile — neither
/// caller needs to know or care that the strategy underneath is boxed. NOT feature-gated: this
/// is a general blanket utility, not an HFT-only or live-only seam, and it does not touch the
/// generic monomorphized backtest hot loop (nothing calls it there unless a strategy is
/// deliberately boxed).
impl<B: OrderBroker> Strategy<B> for Box<dyn Strategy<B>> {
    fn warmup(&self) -> usize {
        (**self).warmup()
    }
    fn inputs(&self) -> Vec<String> {
        (**self).inputs()
    }
    fn symbols(&self) -> Vec<String> {
        (**self).symbols()
    }
    fn on_start(&mut self, broker: &mut B) {
        (**self).on_start(broker)
    }
    fn on_bar(&mut self, broker: &mut B, bar: &Bar) {
        (**self).on_bar(broker, bar)
    }
    fn on_quote_tick(&mut self, broker: &mut B, q: &TickQuote) {
        (**self).on_quote_tick(broker, q)
    }
    fn on_trade_tick(&mut self, broker: &mut B, t: &TickTrade) {
        (**self).on_trade_tick(broker, t)
    }
    fn on_order_book(&mut self, broker: &mut B, book: &L2Book) {
        (**self).on_order_book(broker, book)
    }
    fn on_schedule(&mut self, broker: &mut B, tag: &str) {
        (**self).on_schedule(broker, tag)
    }
    fn on_fill(&mut self, broker: &mut B, fill: &Fill) {
        (**self).on_fill(broker, fill)
    }
    fn on_feed_status(&mut self, broker: &mut B, status: FeedStatus) {
        (**self).on_feed_status(broker, status)
    }
    fn on_mark(&mut self, broker: &mut B, mark: &TickMark) {
        (**self).on_mark(broker, mark)
    }
    fn on_reference_quote(&mut self, broker: &mut B, venue: &str, q: &TickQuote) {
        (**self).on_reference_quote(broker, venue, q)
    }
    fn on_flow(&mut self, broker: &mut B, flow: FlowToxicity) {
        (**self).on_flow(broker, flow)
    }
    fn on_order_event(&mut self, broker: &mut B, event: &OrderEvent) {
        (**self).on_order_event(broker, event)
    }
    fn on_params_updated(&mut self, broker: &mut B, params: &StrategyParams) {
        (**self).on_params_updated(broker, params)
    }
    fn on_stop(&mut self, broker: &mut B) {
        (**self).on_stop(broker)
    }
    fn params(&self) -> Option<StrategyParams> {
        (**self).params()
    }
    fn save_state(&self) -> Option<serde_json::Value> {
        (**self).save_state()
    }
    fn load_state(&mut self, state: &serde_json::Value) {
        (**self).load_state(state)
    }
}

pub mod maker_params;
pub mod xemm_params;

pub use xemm_params::XemmParams;

// Re-exported so the split moved no public path: `vike_model::strategy::SpreadMakerParams` and
// `vike_model::SpreadMakerParams` (via lib.rs) both still resolve, as they did when these types
// were inline in this file.
pub use maker_params::{
    AsParams, HorizonMode, KappaMode, LadderLevel, LadderOffsetUnit, LadderParams,
    LadderSizeProfile, PriceDomain, QuoteStyle, RefreshTolerance, ReservationModel, RewardParams,
    SpreadMakerParams, SpreadModel, SpreadSource, ToxicityParams, VarianceMode,
};
/// The typed, serde-friendly payload of a live-parameter update — the runtime routes one of these
/// to a mounted strategy and calls [`Strategy::on_params_updated`]. A tagged union (one variant per
/// tunable strategy family) that a strategy MATCHES its own arm of and ignores the rest: a typed
/// alternative to a stringly-typed blob, and additive — a new strategy adds a variant without
/// disturbing the others (an externally-tagged enum, so a journal predating a variant simply never
/// carries it; old payloads still decode). It is journaled with its `Command`, so every variant is
/// serde.
// A live-parameter update is constructed one-at-a-time on a manual tune and journaled with its
// Command — never held in bulk on any hot path — so the size disparity between the maker-params
// variant and the smaller controller variant is immaterial; boxing would add an allocation per
// param update for no real benefit.
#[expect(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum StrategyParams {
    /// New tunables for the `SpreadMaker` market maker.
    SpreadMaker(SpreadMakerParams),
    /// New tunables for a `vike_strategy::ControllerHarness` mount (the controller → `PositionExecutor`
    /// live-params plane, position-executor stage 6): the harness post-exit cooldown + the
    /// controller's intent template (size / triple barrier) + the reference controller's decision
    /// knob. Consumed by `vike_strategy::ControllerHarness::on_params_updated`, which hot-swaps the
    /// controller's tunables and the harness cooldown atomically and bumps its epoch. Additive —
    /// mirrors [`Self::SpreadMaker`]; a journal predating it never carries this variant.
    PositionController(crate::strategy::triple_barrier::PositionControllerParams),
    /// New tunables for the CROSS-EXCHANGE maker (the `XemmMaker` file). Additive, exactly like the
    /// two above: this enum is externally tagged, so a journal predating this variant simply never
    /// carries it and old payloads still decode.
    ///
    /// The two SYMBOLS an xEMM trades are deliberately NOT in the payload — they are mount
    /// IDENTITY, held on the maker outside its config bag, so a live re-tune can widen the edge or
    /// cut size but can never repoint a leg while an unhedged position is open.
    Xemm(XemmParams),
    /// New tunables for ANY strategy, as the key/value bag its `from_params` reads (the same
    /// [`crate::params::Params`] a profile's `[strategy.params]` table is, serde-identical to its
    /// TOML, decision 0145). A strategy outside the three typed families is live-tunable by
    /// re-reading its knobs from this bag in `on_params_updated` and returning its current bag from
    /// `params()`. Additive like the arms above: a journal predating it never carries it.
    Params(crate::params::Params),
}

#[cfg(test)]
mod order_event_tests;

/// A minimal recording [`OrderBroker`] test double — the shared mock the strategy-layer
/// tests drive (`vike-strategy`'s position-executor + controller machine tests, `vike_execution::emulated`'s
/// law, and this crate's own boxed-forwarding test below). It captures every order verb, and lets a
/// test script `now`/`price`/`position` and the instrument's grid and contract value; the rest of the
/// reads are inert. Consolidated here — the [`OrderBroker`]
/// trait's home — from the near-identical per-crate copies (Phase 4 test-support consolidation) so a
/// new strategy-layer test inherits it instead of re-declaring a broker. Gated behind `test-support`
/// (compiled only under `cfg(test)` or when a consumer enables `vike-model/test-support` as a
/// dev-dependency), so a default build never sees it.
#[cfg(any(test, feature = "test-support"))]
#[derive(Default)]
pub struct OrderBrokerMock {
    /// Engine clock returned by [`OrderBroker::now`], epoch ms.
    pub now: i64,
    /// Last mark returned by [`OrderBroker::price`].
    pub price: f64,
    /// Signed position returned by [`OrderBroker::position`] of every symbol.
    pub pos: f64,
    /// Every [`OrderBroker::place`] captured as `(symbol, tag, side, qty, order, reduce_only)`, the
    /// three verbs whose default goes through it included, an untagged one (`""`) too.
    pub placed: Vec<(String, String, i32, f64, OrderPlacement, bool)>,
    /// Every [`OrderBroker::deny`] captured as `(tag, reason)`.
    pub denied: Vec<(String, String)>,
    /// Every [`OrderBroker::place_group`] captured whole.
    pub groups: Vec<Vec<(String, OrderListLeg)>>,
    /// Every [`OrderBroker::amend`], as `(tag, new_qty, new_price)`.
    pub modified: Vec<(String, Option<f64>, Option<f64>)>,
    /// Every [`OrderBroker::cancel`], by tag.
    pub cancelled: Vec<String>,
    /// [`OrderBroker::instrument_grid`] of every symbol.
    pub grid: Option<crate::Grid>,
    /// [`OrderBroker::contract_value`] of every symbol (`None`: the broker does not say).
    pub value: Option<crate::ContractValue>,
}

#[cfg(any(test, feature = "test-support"))]
impl OrderBrokerMock {
    /// A broker whose clock starts at `now` — for tests that must stamp a submit time BEFORE the
    /// first `start` (so refresh/backoff timers key off it). Avoids the `field_reassign_with_default`
    /// lint an immediate `b.now = ..` after `default()` would trip.
    pub fn at(now: i64) -> Self {
        OrderBrokerMock { now, ..Default::default() }
    }

    /// Every submission, of any kind: "did the strategy route ANY order".
    pub fn submits(&self) -> usize {
        self.placed.len()
    }
}

/// The seam over an inert book, so any strategy written over [`OrderBroker`] (an order-language
/// rule, a Rhai script whose `on_bar` takes `strategy`, `vike_execution::emulated`'s law) mounts on it.
#[cfg(any(test, feature = "test-support"))]
impl OrderBroker for OrderBrokerMock {
    fn place(
        &mut self,
        symbol: &str,
        tag: &str,
        side: i32,
        qty: f64,
        order: OrderPlacement,
        reduce_only: bool,
    ) {
        self.placed.push((symbol.to_string(), tag.to_string(), side, qty, order, reduce_only));
    }
    fn place_group(&mut self, _symbol: &str, legs: Vec<(String, OrderListLeg)>) {
        self.groups.push(legs);
    }
    // shortcut: an amend's trigger is not recorded; widen `modified` when a test amends one
    fn amend(&mut self, tag: &str, qty: Option<f64>, price: Option<f64>, _trigger: Option<f64>) {
        self.modified.push((tag.to_string(), qty, price));
    }
    fn cancel(&mut self, tag: &str) {
        self.cancelled.push(tag.to_string());
    }
    fn deny(&mut self, _symbol: &str, tag: &str, _quantity: f64, reason: &str) {
        self.denied.push((tag.to_string(), reason.to_string()));
    }
    fn average_price(&self, _symbol: &str) -> f64 {
        0.0
    }
    fn position(&self, _symbol: &str) -> f64 {
        self.pos
    }
    fn price(&self, _symbol: &str) -> f64 {
        self.price
    }
    fn equity(&self) -> f64 {
        0.0
    }
    fn bars(&self, _symbol: &str) -> &[Bar] {
        &[]
    }
    fn index(&self) -> usize {
        0
    }
    fn now(&self) -> i64 {
        self.now
    }
    fn instrument_grid(&self, _symbol: &str) -> Option<crate::Grid> {
        self.grid
    }
    fn contract_value(&self, _symbol: &str) -> Option<crate::ContractValue> {
        self.value
    }
}

#[cfg(test)]
mod boxed_strategy_tests;
