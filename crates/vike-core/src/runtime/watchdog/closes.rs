//! What a reduce-only close already OUT means for the next one, and the risk clock's per-stage panic guard.
//!
//! The gate nets in-flight reduce-only orders only against a FLAT book (`ExecutionEngine::cover_position_size`),
//! so on a non-flat book every reduce-only close is admitted whole, an async client has folded none of them, and
//! two of them add up to more than the position. A venue that puts `reduce_only` on the wire trims the second;
//! oanda, ig, fxcm, ibkr, ctrader and alpaca do not, and the position flips. The risk sweeps that release a
//! close of their own (the margin call, the budget latch's flatten) therefore take what is already out from what
//! they are about to send, here, in one place. Cold: per sweep, never the fold.

use super::*;

impl<C: ExecutionClient> CoreThread<C> {
    /// The quantity still to fill of engine `eidx`'s live reduce-only orders on `symbol` and `side` that fill NOW:
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
        eidx: usize,
        symbol: &str,
        side: i32,
        except_mount: Option<usize>,
    ) -> f64 {
        let eng = self.eng(eidx);
        eng.registry
            .iter()
            .filter(|(coid, mo)| {
                let r = &mo.request;
                mo.status.is_live()
                    && r.reduce_only
                    && r.side == side
                    && (r.symbol == symbol || (r.symbol.is_empty() && eng.symbol == symbol))
                    && except_mount.is_none_or(|m| self.coid_mount.get(coid.as_str()) != Some(&m))
                    && match r.order_type {
                        vike_model::OrderType::Market => true,
                        vike_model::OrderType::Limit => self.limit_crosses(eidx, r, symbol),
                        _ => false,
                    }
            })
            .map(|(_, mo)| mo.request.qty - mo.filled_qty)
            .filter(|r| r.is_finite() && *r > 0.0)
            .sum()
    }

    /// Whether the reduce-only LIMIT `r` of engine `eidx` is priced THROUGH the market of `symbol` (the engine's
    /// own symbol when the order names none): a sell at or below the bid it would hit, a buy at or above the ask.
    /// The sell closes a long (so the bid, as the valuation law reads a long) and the buy a short (the ask).
    /// `false` when the order carries no usable price or the board has no price for the symbol: an order we
    /// cannot place against the market is not counted, as before.
    fn limit_crosses(&self, eidx: usize, r: &OrderRequest, symbol: &str) -> bool {
        let Some(limit) = r.price.filter(|p| p.is_finite() && *p > 0.0) else { return false };
        let eng = self.eng(eidx);
        let held = if r.side < 0 { 1.0 } else { -1.0 }; // the sign of the position the close reduces
        let Some(market) = eng.resolved_position_price(
            &eng.venue,
            if r.symbol.is_empty() { symbol } else { &r.symbol },
            held,
            &self.config.price_cfg,
        ) else {
            return false;
        };
        if r.side < 0 { limit <= market } else { limit >= market }
    }

    /// What a reduce-only order on `side` can still take off engine `eidx`'s book of `symbol`, or `None` on a
    /// FLAT local book (the gate nets the venue's own evidence there, so the caller asks for what it wanted).
    /// A sell closes a long and a buy a short, each from ITS bucket: the one-way `Both` bucket when it holds
    /// anything, else the hedge `Long` and `Short` buckets apart (`Long + Short` would read a long 10 beside a
    /// short 6 as a net long 4, clamping the long's close and skipping the short's).
    fn close_room(&self, eidx: usize, symbol: &str, side: i32) -> Option<f64> {
        use vike_model::events::PositionSide::{Both, Long, Short};
        let eng = self.eng(eidx);
        let both = eng.position_size_of(symbol, Both);
        let (long, short) = if both == 0.0 {
            (eng.position_size_of(symbol, Long), eng.position_size_of(symbol, Short))
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
    /// that is not the margin call's.
    pub(crate) fn flatten_qty(&self, i: usize, symbol: &str, side: i32, want: f64) -> f64 {
        let eidx = self.mount_eng(i);
        let Some(room) = self.close_room(eidx, symbol, side) else { return want };
        want.min(room - self.closes_in_flight(eidx, symbol, side, Some(i))).max(0.0)
    }

    /// How much of a LATCHED mount's flatten may be asked for AGAIN ([`Self::reoffer_mount_flatten`]): the
    /// attributed size `want` at most what that side of the ENGINE's book holds ([`Self::close_room`]; `want`
    /// on a flat local book, which the gate nets against the venue's own evidence), less EVERY live reduce-only
    /// MARKET close on that symbol and side. Unlike [`Self::flatten_qty`], which runs right after the latch
    /// canceled the mount's own orders, nothing has canceled them here: the mount's earlier flatten, when it
    /// is still out, is cover and is not stacked on.
    pub(crate) fn reoffer_qty(&self, i: usize, symbol: &str, side: i32, want: f64) -> f64 {
        let eidx = self.mount_eng(i);
        let cap = self.close_room(eidx, symbol, side).map_or(want, |room| want.min(room));
        (cap - self.closes_in_flight(eidx, symbol, side, None)).max(0.0)
    }

    /// The margin call's intents less the closes already in flight: each intent asks for the REMAINING need
    /// (`qty` less the market closes out on that symbol and side), and an intent with nothing left is dropped.
    /// Need-based, not additive: the closes of a second sweep over a breach that has not folded back are the
    /// first's, so they cover it and a third is not stacked on them.
    pub(crate) fn net_intents(
        &self,
        eidx: usize,
        intents: Vec<vike_exec::LiquidationIntent>,
    ) -> Vec<vike_exec::LiquidationIntent> {
        intents
            .into_iter()
            .filter_map(|mut it| {
                it.qty -= self.closes_in_flight(eidx, &it.symbol, it.side, None);
                (it.qty > 1e-12).then_some(it)
            })
            .collect()
    }

    /// Run ONE stage of the risk clock (`stage` of engine `eidx`) under its own `catch_unwind`. The sweep walks
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
        eidx: Option<usize>,
        run: impl FnOnce(&mut Self),
    ) {
        let Err(payload) = catch_unwind(AssertUnwindSafe(|| run(self))) else { return };
        let why = panic_text(payload);
        if self.risk_clock_faulted.insert((stage, eidx)) {
            tracing::error!(
                target: "vike_core::risk_clock",
                stage,
                engine = ?eidx,
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
