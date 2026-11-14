//! What a reduce-only close already OUT means for the next one, and the risk clock's per-stage panic guard.
//!
//! The gate nets in-flight reduce-only orders only against a FLAT book (`Engine::cover_position_size`),
//! so on a non-flat book every reduce-only close is admitted whole, an async client has folded none of them, and
//! two of them add up to more than the position. A venue that puts `reduce_only` on the wire trims the second;
//! oanda, ig, fxcm, ibkr, ctrader and alpaca do not, and the position flips. The risk sweeps that release a
//! close of their own (the margin call, the budget latch's flatten) therefore take what is already out from what
//! they are about to send, here, in one place. Cold: per sweep, never the fold.

use super::*;

impl<C: ExecutionClient> CoreThread<C> {
    /// The quantity still to fill of engine `engine_index`'s live reduce-only orders on `symbol` and `side` that fill NOW:
    /// its MARKET orders and its LIMIT orders priced through the market ([`Self::limit_crosses`]), except the
    /// orders attributed to mount `except_mount` (the budget latch has just canceled its own).
    ///
    /// ⚠ Only an order that fills now counts. A take-profit or a stop resting far from the market is not cover,
    /// and counting it left a position open behind it. A reduce-only LIMIT sell at or below the bid (a buy at or
    /// above the ask) IS cover: it crosses the book on arrival, exactly like a market close, and left uncounted it
    /// let a second close go out beside it on a venue that sends no reduce-only flag. The market it is judged
    /// against is the engine's board under `config.price_cfg`, and an unpriced symbol counts no limit.
    /// An order that names no symbol is the engine's own (`apply/exit.rs`, the adapters read it so).
    pub(crate) fn closes_in_flight(
        &self,
        engine_index: usize,
        symbol: &str,
        side: i32,
        except_mount: Option<usize>,
    ) -> f64 {
        self.closes_out(engine_index, symbol, side, except_mount, false)
    }

    /// [`Self::closes_in_flight`] for a conditional FIRE (`submit_fired`: a core-held arm's cap, and what a plain arm
    /// still has to close, the rest of it opening) and a core-held re-arm (`core_held.rs`): it
    /// also counts every live reduce-only LIMIT a conditional fire released ([`CoreThread::fired_limit_closes`]),
    /// whether or not it crosses the market. Such a limit is the close of a stop that already fired (a
    /// stop-limit's gapped limit rests above a falling market), so a second fire beside it is capped to what is
    /// left, or dropped in words, and two of them never both go out and flip the position on a venue that sends
    /// no reduce-only flag. Not for the risk sweeps: a resting limit that may never fill must not stop a margin
    /// call. ⚠ Core memory: after a restart a limit released before it counts only when it crosses.
    pub(crate) fn core_held_closes_in_flight(
        &self,
        engine_index: usize,
        symbol: &str,
        side: i32,
    ) -> f64 {
        self.closes_out(engine_index, symbol, side, None, true)
    }

    /// The one walk of the two above; `fired_limits` adds the limits a fire released.
    fn closes_out(
        &self,
        engine_index: usize,
        symbol: &str,
        side: i32,
        except_mount: Option<usize>,
        fired_limits: bool,
    ) -> f64 {
        let engine = self.engine(engine_index);
        engine
            .registry
            .iter()
            .filter(|(order_client_id, mo)| {
                let r = &mo.request;
                mo.status.is_live()
                    && r.reduce_only
                    && r.side == side
                    && (r.symbol == symbol || (r.symbol.is_empty() && engine.symbol == symbol))
                    && except_mount.is_none_or(|m| {
                        self.order_client_id_mount.get(order_client_id.as_str()) != Some(&m)
                    })
                    && match r.order_type {
                        vike_model::OrderType::Market => true,
                        vike_model::OrderType::Limit => {
                            (fired_limits
                                && self.fired_limit_closes.iter().any(|(e, c)| {
                                    *e == engine_index && c.as_str() == order_client_id.as_str()
                                }))
                                || self.limit_crosses(engine_index, r, symbol)
                        }
                        _ => false,
                    }
            })
            .map(|(_, mo)| mo.request.qty - mo.filled_qty)
            .filter(|r| r.is_finite() && *r > 0.0)
            .sum()
    }

    /// Whether the reduce-only LIMIT `r` of engine `engine_index` is priced THROUGH the market of `symbol` (the engine's
    /// own symbol when the order names none): a sell at or below the bid it would hit, a buy at or above the ask.
    /// The sell closes a long (so the bid, as the valuation law reads a long) and the buy a short (the ask).
    /// `false` when the order carries no usable price or the board has no price for the symbol: an order we
    /// cannot place against the market is not counted, as before.
    fn limit_crosses(&self, engine_index: usize, r: &OrderRequest, symbol: &str) -> bool {
        let Some(limit) = r.price.filter(|p| p.is_finite() && *p > 0.0) else { return false };
        let engine = self.engine(engine_index);
        let held = if r.side < 0 { 1.0 } else { -1.0 }; // the sign of the position the close reduces
        let Some(market) = engine.resolved_position_price(
            &engine.venue,
            if r.symbol.is_empty() { symbol } else { &r.symbol },
            held,
            &self.config.price_config,
        ) else {
            return false;
        };
        if r.side < 0 { limit <= market } else { limit >= market }
    }

    /// What a reduce-only order on `side` can still take off engine `engine_index`'s book of `symbol`, or `None` on a
    /// FLAT local book (the gate nets the venue's own evidence there, so the caller asks for what it wanted).
    /// A sell closes a long and a buy a short, each from ITS bucket: the one-way `Both` bucket when it holds
    /// anything, else the hedge `Long` and `Short` buckets apart (`Long + Short` would read a long 10 beside a
    /// short 6 as a net long 4, clamping the long's close and skipping the short's).
    fn close_room(&self, engine_index: usize, symbol: &str, side: i32) -> Option<f64> {
        use vike_model::events::PositionSide::{Both, Long, Short};
        let engine = self.engine(engine_index);
        let both = engine.position_size_of(symbol, Both);
        let (long, short) = if both == 0.0 {
            (engine.position_size_of(symbol, Long), engine.position_size_of(symbol, Short))
        } else {
            (0.0, 0.0)
        };
        if both == 0.0 && long == 0.0 && short == 0.0 {
            return None;
        }
        // signed sizes: a long is positive and a short negative in every bucket
        let held = if side < 0 {
            both.max(0.0) + long.max(0.0)
        } else {
            (-both).max(0.0) + (-short).max(0.0)
        };
        Some(held)
    }

    /// How much of `want` a reduce-only close of mount `i`'s `symbol` on `side` may still ask for: at most what
    /// that side of its ENGINE's book holds ([`Self::close_room`]), less the market closes already out on that
    /// symbol and side ([`Self::closes_in_flight`]: the margin call's close of this very sweep, a bar's, a
    /// strategy's own exit), except the mount's own, which the latch has just canceled. `want` unchanged on a
    /// flat local book, and 0 when the book is on the side the mount closes TOWARD (nothing to reduce). The
    /// simple alternative, skipping the flatten while the engine's margin call is quiet, would miss every close
    /// that is not the margin call's. The engine is the one the row lives on ([`Self::mount_row_engine`]).
    pub(crate) fn flatten_quantity(
        &self,
        i: usize,
        venue: &str,
        symbol: &str,
        side: i32,
        want: f64,
    ) -> f64 {
        let engine_index = self.mount_row_engine(i, venue);
        let Some(room) = self.close_room(engine_index, symbol, side) else { return want };
        want.min(room - self.closes_in_flight(engine_index, symbol, side, Some(i))).max(0.0)
    }

    /// How much of a LATCHED mount's flatten may be asked for AGAIN ([`Self::reoffer_mount_flatten`]): the
    /// attributed size `want` at most what that side of the ENGINE's book holds ([`Self::close_room`]; `want`
    /// on a flat local book, which the gate nets against the venue's own evidence), less EVERY live reduce-only
    /// MARKET close on that symbol and side. Unlike [`Self::flatten_quantity`], which runs right after the latch
    /// canceled the mount's own orders, nothing has canceled them here: the mount's earlier flatten, when it
    /// is still out, is cover and is not stacked on.
    pub(crate) fn reoffer_quantity(
        &self,
        i: usize,
        venue: &str,
        symbol: &str,
        side: i32,
        want: f64,
    ) -> f64 {
        let engine_index = self.mount_row_engine(i, venue);
        let cap = self.close_room(engine_index, symbol, side).map_or(want, |room| want.min(room));
        (cap - self.closes_in_flight(engine_index, symbol, side, None)).max(0.0)
    }

    /// The margin call's intents less the closes already in flight: each intent asks for the REMAINING need
    /// (`qty` less the market closes out on that symbol and side), and an intent with nothing left is dropped.
    /// Need-based, not additive: the closes of a second sweep over a breach that has not folded back are the
    /// first's, so they cover it and a third is not stacked on them.
    pub(crate) fn net_intents(
        &self,
        engine_index: usize,
        intents: Vec<vike_execution::LiquidationIntent>,
    ) -> Vec<vike_execution::LiquidationIntent> {
        intents
            .into_iter()
            .filter_map(|mut it| {
                it.qty -= self.closes_in_flight(engine_index, &it.symbol, it.side, None);
                (it.qty > 1e-12).then_some(it)
            })
            .collect()
    }

    /// Run ONE stage of the risk clock (`stage` of engine `engine_index`) under its own `catch_unwind`. The sweep walks
    /// the engines from the highest index down and one guard around the whole walk ended it at the first
    /// panic for ever: a client that panics on engine k (or a journal append that fails there) kept engine 0,
    /// the primary, from ever being margin-called. A caught panic enters the safe state exactly as the outer
    /// guard does, and the walk goes on.
    ///
    /// Once per `(stage, engine)` is the whole fault handling: the first panic logs, enters the safe state
    /// (`enter_safe_state` keeps the FIRST fault and pulls every working order) and drains the bus; a stage
    /// that panics AGAIN at the next sweep only re-asserts `Halted` on every engine (an operator may have lifted
    /// it), without a second cancel-all, which would pull the closes the engines below it have sent in the
    /// meantime and have them re-sent, and cancelled, at every 30 s sweep for as long as the stage stays broken.
    pub(crate) fn clock_stage(
        &mut self,
        stage: &'static str,
        engine_index: Option<usize>,
        run: impl FnOnce(&mut Self),
    ) {
        let Err(payload) = catch_unwind(AssertUnwindSafe(|| run(self))) else { return };
        let why = panic_text(payload);
        if self.risk_clock_faulted.insert((stage, engine_index)) {
            tracing::error!(
                target: "vike_core::risk_clock",
                stage,
                engine = ?engine_index,
                reason = %why,
                "a risk-clock stage panicked; the sweep goes on with the next engine"
            );
            self.enter_safe_state(why);
        } else {
            self.engine.trading_state = TradingState::Halted;
            for (_, e) in self.extra_engines.iter_mut() {
                e.trading_state = TradingState::Halted;
            }
        }
        self.drain_poisoned();
    }
}
