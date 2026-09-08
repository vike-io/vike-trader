//! The maker's QUOTE-EMISSION unit — the one shared tick step both `Strategy` lanes drive, split
//! out of the crate root (audit F11); every item moved VERBATIM, behavior byte-identical.
//!
//! [`SpreadMaker::requote`] is the pipeline: own-filter the public book → price (A-S layer or the
//! pure [`QuoteStyle`](vike_model::QuoteStyle) placement) → reward-band clamp → inventory-skew the
//! sizes → flow-toxicity widen/cut → per-side breaker suppression → LADDER or SINGLE placement. The
//! STAGE ORDER is semantic (each stage consumes the previous stage's `(price, size)`), and every
//! stage's "off ⇒ byte-identical" contract is stated inline at the stage that owns it. It stays ONE
//! straight-line function rather than a guard-stage pipeline: this crate runs on the p99 core
//! thread, and inline is what makes the neutral-reduction argument checkable by reading.
//!
//! The tail helpers are the per-side arms it dispatches to — `requote_single_side` /
//! `requote_ladder_side` (place-or-diff), `retire_single_order` / `retire_ladder_orders` (the
//! mode-switch retirements), and the two RE-PRICE gates `refresh_skips` / `reward_moas_holds`,
//! which are reachable ONLY from the re-price arm: a place or a pull is never gated.

use vike_model::{FlowToxicity, HftBroker};

use crate::SpreadMaker;
use crate::alpha::ofi_toxicity;
use crate::book::{BookView, filter_own};
use crate::ladder;
use crate::own_book::OwnSide;
use crate::refresh::within_tolerance;
use crate::reward::{clamp_into_band, moas_holds};
use crate::skew::skew_multipliers;

/// The per-side inputs to [`SpreadMaker::requote_ladder_side`], bundled into one struct so the arg
/// count stays sane (and because `half`/`tick`/`ts` are shared across a tick's two sides). All fields
/// come straight from the tick's already-computed single-quote pricing.
struct LadderSide {
    /// This side's level-0 price — the single quote's `bid_px` / `ask_px` (rung 0 rests here).
    level0_px: f64,
    /// The level-0 half-spread `(ask_px − bid_px)/2` — the `HalfSpread` unit scale between rungs.
    half: f64,
    /// The venue tick grid — the `Ticks` unit scale between rungs.
    tick: f64,
    /// This side's already-skewed base (level-0) quote size, which every rung multiplies.
    base_qty: f64,
    /// Whether the fill-rate breaker is suppressing THIS side this tick (⇒ pull all its rungs).
    suppressed: bool,
}

impl SpreadMaker {
    /// The shared quote step, driven by BOTH the L1 quote lane ([`Strategy::on_quote_tick`]) and the
    /// L2 book lane ([`Strategy::on_order_book`]). One pipeline: (1) optionally own-filter the public
    /// book (`filter_own`), (2) derive the two prices via the selected [`QuoteStyle`], (3) shape the
    /// sizes with the inventory skew, (4) apply the breaker's per-side suppression, then place-or-
    /// modify each side in place. `ts` is the triggering event's EVENT ts (quote/book), never
    /// wall-clock. With the default (`Mid`, no filtration, breaker/skew neutral) this reduces to the
    /// original `mid ± half_spread` placement bit-for-bit.
    ///
    /// [`Strategy::on_quote_tick`]: vike_model::Strategy::on_quote_tick
    /// [`Strategy::on_order_book`]: vike_model::Strategy::on_order_book
    /// [`QuoteStyle`]: vike_model::QuoteStyle
    /// Warn on the EDGE into the unpriced-book hold — see [`SpreadMaker::no_quote_holding`] for why
    /// this early return may not be silent, and why it is edge-triggered rather than per-tick.
    ///
    /// ⚠ Deliberately says NOTHING, and deliberately does not latch, when the break-even refusal is
    /// the cause: `AsState::note_fee_floor` owns that episode end-to-end — it has already announced
    /// the hold with the two numbers that make it actionable, and it announces its own recovery. A
    /// generic "no two-sided quote" line on top would bury the specific one and then emit a second,
    /// redundant RESUMING beside it. Leaving the latch alone keeps this warn's episodes disjoint
    /// from the fee floor's, so a mount that goes fee-floor-hold → book-hold reports both edges in
    /// the right order rather than collapsing them.
    fn note_no_quote(&mut self, view: &BookView) {
        if self.as_state.as_ref().is_some_and(|st| st.is_fee_floor_refusing()) {
            return;
        }
        if !self.no_quote_holding {
            self.no_quote_holding = true;
            tracing::warn!(
                bid_levels = view.bids.len(),
                ask_levels = view.asks.len(),
                tick_size = view.tick_size,
                "maker HOLDING: the book priced no two-sided quote, so nothing was placed or \
                 amended. Zero levels on a side means the FEED is not delivering that side — check \
                 the venue subscription before touching the maker's tuning."
            );
        }
    }

    /// The exit edge of [`SpreadMaker::note_no_quote`]. A no-op unless a hold episode is actually
    /// open, so a healthy maker emits nothing on any tick.
    fn note_quote_resumed(&mut self) {
        if self.no_quote_holding {
            self.no_quote_holding = false;
            tracing::warn!("maker RESUMING: the book priced a two-sided quote again");
        }
    }

    pub(crate) fn requote<B: HftBroker>(&mut self, broker: &mut B, book: &BookView, ts: i64) {
        // (1)+(2) price off the (optionally own-filtered) book. On the default path we borrow the raw
        // book with NO clone; only when filtration is on do we clone+subtract our own resting orders.
        let filtered;
        let view = if self.cfg.filter_own {
            // Two filtration backends behind the ONE `filter_own` gate: the opt-in `OwnOrderBook`
            // ladder (multi-order + accepted-buffer race gate) when it is mounted, else the
            // original single-snapshot pair — the verbatim prior call, so the default path is
            // byte-identical.
            filtered = match self.own_book.as_ref() {
                Some(own) => own.filtered_view(book, ts),
                None => filter_own(book, self.bid.own, self.ask.own),
            };
            &filtered
        } else {
            book
        };
        // Read the signed inventory ONCE — the A-S reservation price and the size-skew both need it,
        // and it can't change during a single requote (single-writer core).
        let position = HftBroker::position(broker);
        // (2) derive the two PRICES. When the A-S layer is enabled it computes an inventory-aware
        // reservation price + optimal spread (and advances its online σ̂² off this tick's mid);
        // otherwise the pure [`QuoteStyle`] prices straight off the (own-filtered) book, unchanged.
        let priced = if let Some(as_state) = self.as_state.as_mut() {
            as_state.price(view, position, ts)
        } else {
            view.priced(self.cfg.style, self.cfg.half_spread, self.cfg.depth_levels)
        };
        let Some((bid_px, ask_px)) = priced else {
            // No valid two-sided quote (book lacks a side / A-S pinned at a wall) — hold the current
            // resting orders untouched this tick, exactly as the one-sided-book path always has.
            // ⚠ …but SAY SO on the edge. This early return was the maker's one wholly silent hold,
            // and it sits ABOVE the fee-floor guard that exists to make holds attributable.
            self.note_no_quote(view);
            return;
        };
        // Priced ⇒ any hold episode is over. Announce the exit edge so an earlier warn cannot
        // outlive its cause and leave a working maker looking permanently broken.
        self.note_quote_resumed();
        // (2b) LIQUIDITY-REWARDS fold (steal/mm-rewards-quoting): the reward band is scored off the
        // BOOK midpoint (not the A-S reservation the candidate is centred on), so compute it from the
        // touch. `priced` being `Some` guarantees both sides are present, so the fallback is
        // unreachable — kept only to stay total.
        let mid = match (view.bids.first(), view.asks.first()) {
            (Some(&(best_bid, _)), Some(&(best_ask, _))) => 0.5 * (best_bid + best_ask),
            _ => 0.5 * (bid_px + ask_px),
        };
        // Reward chasing is active only for a positive weight; `None`/`weight <= 0` ⇒ the whole fold
        // below is skipped and the quote is byte-identical to before.
        let reward = self.cfg.reward.filter(|r| r.weight > 0.0);
        // The A-S near-resolution BLACKOUT is the SAFETY authority: inside it the maker quotes at the
        // walls (A-S widest) and must NOT be reward-clamped back into the band — that would farm
        // rewards straight into the resolution vol spike. So skip the price clamp while blacked out.
        let blackout = self.as_state.as_ref().is_some_and(|st| st.in_blackout(ts));
        let (bid_px, ask_px) = match reward {
            Some(r) if !blackout => clamp_into_band(mid, bid_px, ask_px, r),
            _ => (bid_px, ask_px),
        };
        // (3) shape the two sizes from CURRENT inventory vs target — neutral params yield (1.0, 1.0),
        // so qty is unchanged (skew composes with the style: PRICES from the style/A-S, SIZES here).
        let (bid_mult, ask_mult) = skew_multipliers(
            position,
            self.cfg.target_inventory,
            self.cfg.max_inventory,
            self.cfg.skew,
        );
        let mut bid_qty = self.cfg.qty * bid_mult;
        let mut ask_qty = self.cfg.qty * ask_mult;
        // (3b) REWARD min_size floor: a reward-active side must rest at least `min_size` shares to be
        // eligible, so bump each side UP to it (never down). Skipped when reward is off (or the floor
        // is zero), keeping the sizes byte-identical to before on the default path. Also flows into
        // the ladder below, whose rung 0 multiplies this already-floored base size.
        if let Some(r) = reward.filter(|r| r.min_size > 0.0) {
            bid_qty = bid_qty.max(r.min_size);
            ask_qty = ask_qty.max(r.min_size);
        }
        // (3c) FLOW-TOXICITY guard (RTDS wallet-toxicity, 5c): on the side under toxic taker pressure,
        // WIDEN the level-0 quote away from mid and CUT its size, driven by the latest `on_flow`
        // reading. The widen reuses THIS tick's per-side half-spread `(ask_px − bid_px)/2` (the mid→
        // quote gap), captured BEFORE any widen so both sides scale off the SAME base spread. Applied
        // before the suppression / ladder-vs-single split so a widened level-0 flows through to the
        // rungs, and so a size the cut drives to `0` can route through the per-side suppression PULL
        // path below. `flow_guard` is `Some` ONLY when a non-zero `toxicity` bag is configured AND an
        // `on_flow` reading has arrived; `None` (the default / never-fed / all-zero bag) leaves the
        // quote — and every value below — BYTE-IDENTICAL to before (no float is touched).
        // (3c-i) OFI-TOXICITY synthesis (Group-B, PR-3): feed the tracked Cont–Kukanov–Stoikov OFI into
        // the EXISTING flow-toxicity guard as an INTERNAL FALLBACK reading — but ONLY when a toxicity
        // guard is configured, `ofi_toxicity_scale != 0`, and NO external `on_flow` reading has ever
        // arrived (`!self.flow_external`). PRECEDENCE: an external `on_flow` reading takes precedence
        // permanently (once `flow_external` is set this never runs), so a real reading is never
        // clobbered — the synthesis is the fallback for a maker with no external toxicity feed. SIDE
        // MAPPING: positive OFI = buy pressure ⇒ aggressive BUY takers lift our ASK ⇒ the ASK is the
        // toxic/adverse side (`flow.ask = tox`, matching how the guard below pushes the ASK further
        // from mid); negative OFI (sell pressure) ⇒ the BID. Placed AFTER `as_state.price()` above so it
        // reads the OFI this tick's book update advanced. INERT (byte-identical, no float touched) when
        // the scale is 0, no toxicity guard is set, or an external reading exists.
        if !self.flow_external && self.cfg.toxicity.is_some() {
            let synth = self.as_state.as_ref().and_then(|st| {
                let scale = st.params.ofi_toxicity_scale;
                (scale != 0.0).then(|| {
                    let ofi = st.ofi();
                    let tox = ofi_toxicity(ofi, scale);
                    if ofi >= 0.0 {
                        FlowToxicity { bid: 0.0, ask: tox, ts }
                    } else {
                        FlowToxicity { bid: tox, ask: 0.0, ts }
                    }
                })
            });
            if let Some(flow) = synth {
                self.last_flow = Some(flow);
            }
        }
        let flow_guard =
            self.last_flow.zip(self.cfg.toxicity.filter(|t| t.widen != 0.0 || t.size_cut != 0.0));
        let (bid_px, ask_px, bid_qty, ask_qty) = match flow_guard {
            Some((flow, tox)) => {
                // per-side half-spread this tick — the mid→quote gap the widen scales by.
                let half = 0.5 * (ask_px - bid_px);
                (
                    // BID: toxic SELL takers hit our bid ⇒ push it LOWER (further from mid), cut size.
                    bid_px - flow.bid * tox.widen * half,
                    // ASK: toxic BUY takers lift our ask ⇒ push it HIGHER (further from mid), cut size.
                    ask_px + flow.ask * tox.widen * half,
                    bid_qty * (1.0 - flow.bid * tox.size_cut).max(0.0),
                    ask_qty * (1.0 - flow.ask * tox.size_cut).max(0.0),
                )
            }
            None => (bid_px, ask_px, bid_qty, ask_qty),
        };
        // (4) suppression is decided against the triggering event's EVENT ts (never wall-clock): a
        // side is suppressed while `ts` is still before its cooldown deadline. Disabled breaker →
        // both `*_suppressed_until` stay 0 → never suppressed → the per-side place/modify below is
        // byte-identical to the skew-only maker (submit both first, modify both after). A toxicity
        // size-cut that reached `0` folds in here too — routing that side through the SAME PULL path
        // (never place a zero-size order) — but ONLY while the guard is active (`flow_guard.is_some()`),
        // so the default path, where a side can reach `0` only via an extreme inventory skew whose
        // long-standing place-at-zero behavior is preserved, stays byte-identical.
        let enabled = self.breaker_enabled();
        let tox_active = flow_guard.is_some();
        let bid_suppressed =
            (enabled && ts < self.bid.suppressed_until) || (tox_active && bid_qty <= 0.0);
        let ask_suppressed =
            (enabled && ts < self.ask.suppressed_until) || (tox_active && ask_qty <= 0.0);
        // (5) LADDER vs SINGLE quote. An ACTIVE ladder (`levels >= 2`) rests N rungs per side stepping
        // out from THIS tick's `(bid_px, ask_px)` reservation, diffing against the resting rungs; the
        // single-order tail below is skipped. On the DEFAULT path (no ladder) `is_active()` is `false`
        // ⇒ the two `retire_ladder_orders` calls are guarded no-ops (empty rung vecs) and the tail runs
        // verbatim — byte-identical. Rung 0 is bit-for-bit today's quote; the style/A-S price it, the
        // skew shapes the base size each rung multiplies, the breaker's suppression pulls the WHOLE
        // side, and the refresh tolerance gates each rung's re-price.
        if self.cfg.ladder.is_some_and(|l| l.is_active()) {
            // rung geometry needs the level-0 half-spread and the venue grid, taken from THIS tick.
            let half = 0.5 * (ask_px - bid_px);
            let tick = view.tick_size;
            let bid_side = LadderSide {
                level0_px: bid_px,
                half,
                tick,
                base_qty: bid_qty,
                suppressed: bid_suppressed,
            };
            let ask_side = LadderSide {
                level0_px: ask_px,
                half,
                tick,
                base_qty: ask_qty,
                suppressed: ask_suppressed,
            };
            // if we just switched FROM single mode, retire the leftover `"bid"`/`"ask"` first so it is
            // not stranded (no-op after the first laddered tick — `*_placed` is then false).
            self.retire_single_order(broker, true);
            self.retire_single_order(broker, false);
            self.requote_ladder_side(broker, true, bid_side, ts);
            self.requote_ladder_side(broker, false, ask_side, ts);
            return;
        }
        // Not laddering: retire any rungs left from a previous laddered tick (a guarded no-op on the
        // default path, where the rung vecs are always empty — which is what keeps the tail below
        // byte-identical), then run today's exact single-order tail.
        self.retire_ladder_orders(broker, true);
        self.retire_ladder_orders(broker, false);
        // Per-side single-order tail — ONE parameterized arm run for each side (audit F5: formerly
        // two fully mirrored bid/ask blocks). Bid first, then ask, exactly the original order.
        self.requote_single_side(broker, true, (bid_px, bid_qty), bid_suppressed, mid, ts);
        self.requote_single_side(broker, false, (ask_px, ask_qty), ask_suppressed, mid, ts);
    }

    /// Place-or-re-quote ONE side's SINGLE resting order (tag `"bid"`/`"ask"`) — the collapsed
    /// per-side tail of [`SpreadMaker::requote`] (audit F5: formerly two fully mirrored arms).
    /// `target` is this side's already-priced/-shaped `(price, size)` (the same pair shape the
    /// [`SideState::own`] snapshot holds); `suppressed` is the breaker/toxicity verdict for this
    /// side this tick; `mid` feeds the reward moas gate.
    ///
    /// - SUPPRESSED: PULL the resting quote (cancel) so it stops getting hit in the adverse run —
    ///   recording a CENSORED own outcome for the OwnFillFit κ MLE BEFORE the resting snapshot /
    ///   placement clock is cleared.
    /// - not resting: SUBMIT it.
    /// - resting: MODIFY in place — additionally gated by the order-refresh TOLERANCE
    ///   (`refresh_skips`) and the reward min-order-age hold (`reward_moas_holds`): a target that
    ///   barely moved leaves the resting order — and its queue position — untouched, sending
    ///   nothing. OFF by default (`refresh_tolerance == None` ⇒ `refresh_skips` is a constant
    ///   `false`), which is what keeps this tail byte-identical. The PLACE and PULL arms are
    ///   deliberately NOT gated: a quote that must go on or come off the book always does.
    ///
    /// Every arm tracks this side's [`SideState`] (`own`, so the NEXT tick's filtration can
    /// subtract the resting order; `placed`/`refresh_stale`/`quoted_ts`) and mirrors its verb into
    /// the own-order LADDER (when mounted) keyed by the SAME tag, so it tracks exactly what the
    /// snapshot pair tracks — plus the lifecycle/race state a snapshot cannot hold. Every
    /// `own_book_*` call is a no-op while the ladder is `None` (the default), which is what keeps
    /// this tail byte-identical.
    ///
    /// [`SideState`]: crate::SideState
    /// [`SideState::own`]: crate::SideState::own
    fn requote_single_side<B: HftBroker>(
        &mut self,
        broker: &mut B,
        is_bid: bool,
        target: (f64, f64),
        suppressed: bool,
        mid: f64,
        ts: i64,
    ) {
        let (px, qty) = target;
        let (tag, side_code, own_side) =
            if is_bid { ("bid", 1, OwnSide::Bid) } else { ("ask", -1, OwnSide::Ask) };
        if suppressed {
            if self.side(is_bid).placed {
                // OwnFillFit κ MLE: a breaker PULL is a CENSORED own outcome (came off the book
                // unfilled). Recorded BEFORE the resting snapshot / placement clock is cleared.
                self.feed_own_outcome(is_bid, false, ts);
                broker.cancel_tagged(tag);
                let side = self.side_mut(is_bid);
                side.placed = false;
                side.own = None;
                side.refresh_stale = false;
                side.quoted_ts = 0;
                self.own_book_pull(tag);
            }
        } else if !self.side(is_bid).placed {
            broker.submit_limit_tagged(tag, side_code, qty, px);
            let side = self.side_mut(is_bid);
            side.placed = true;
            side.own = Some((px, qty));
            side.refresh_stale = false;
            side.quoted_ts = ts;
            self.own_book_place(tag, own_side, px, qty, ts);
        } else if !(self.refresh_skips(
            self.side(is_bid).own,
            px,
            qty,
            self.side(is_bid).refresh_stale,
        ) || self.reward_moas_holds(is_bid, self.side(is_bid).own, mid, ts))
        {
            broker.modify_tagged(tag, Some(qty), Some(px));
            let side = self.side_mut(is_bid);
            side.own = Some((px, qty));
            side.refresh_stale = false;
            side.quoted_ts = ts;
            self.own_book_place(tag, own_side, px, qty, ts);
        }
    }

    /// Diff ONE side's DESIRED ladder rungs against what is resting and emit the minimal verb set —
    /// the multi-order twin of the single-order arms in `requote`, keyed by `"bid{k}"`/`"ask{k}"`.
    ///
    /// - SUPPRESSED (breaker): pull EVERY resting rung of this side (never strand a rung), and place
    ///   nothing — the whole-side analog of the single path's suppression cancel.
    /// - otherwise: for each desired rung `k`, SUBMIT it if there is no rung `k` resting yet, else
    ///   MODIFY it in place unless the per-rung refresh tolerance says the drift is churn (a fill on
    ///   this side sets `*_refresh_stale`, forcing every rung through the modify arm once so a partial
    ///   fill's size top-up is never read as "no change"); then CANCEL any resting rung BEYOND the
    ///   desired count (the ladder shrank — a re-tune with fewer levels, or a decayed/`unit_scale`
    ///   guard).
    ///
    /// Reached ONLY when the ladder is active (`levels >= 2`), so it never runs on the default path.
    /// The resting-rung vec is taken by value for the duration (so `own_book_*`'s `&mut self` calls do
    /// not alias it) and stored back at the end.
    fn requote_ladder_side<B: HftBroker>(
        &mut self,
        broker: &mut B,
        is_bid: bool,
        side: LadderSide,
        ts: i64,
    ) {
        let Some(lp) = self.cfg.ladder else {
            return; // unreachable: the caller gates on `is_active()`, but keep it total
        };
        let (prefix, side_code, own_side, sign) =
            if is_bid { ("bid", 1, OwnSide::Bid, -1.0) } else { ("ask", -1, OwnSide::Ask, 1.0) };
        let stale = self.side(is_bid).refresh_stale;
        // take the resting-rung vec out so the `&mut self` `own_book_*` calls below don't alias it.
        let mut resting = std::mem::take(&mut self.side_mut(is_bid).rungs);
        if side.suppressed {
            // pull the WHOLE side — every rung comes off the book (the tolerance never gates a pull).
            for k in 0..resting.len() {
                let tag = format!("{prefix}{k}");
                broker.cancel_tagged(&tag);
                self.own_book_pull(&tag);
            }
            resting.clear();
        } else {
            let desired =
                ladder::desired_side(lp, side.level0_px, sign, side.half, side.tick, side.base_qty);
            for (k, &(px, sz)) in desired.iter().enumerate() {
                let tag = format!("{prefix}{k}");
                if k >= resting.len() {
                    // a NEW rung deeper than anything currently resting — submit it.
                    broker.submit_limit_tagged(&tag, side_code, sz, px);
                    self.own_book_place(&tag, own_side, px, sz, ts);
                    resting.push((px, sz));
                } else {
                    // an existing rung — re-price in place unless the refresh tolerance (never while
                    // `stale`) says the drift is pure churn. Mirrors the single path's re-price arm.
                    let skip = !stale
                        && self
                            .cfg
                            .refresh_tolerance
                            .is_some_and(|tol| within_tolerance(resting[k], (px, sz), tol));
                    if !skip {
                        broker.modify_tagged(&tag, Some(sz), Some(px));
                        self.own_book_place(&tag, own_side, px, sz, ts);
                        resting[k] = (px, sz);
                    }
                }
            }
            // the ladder shrank: cancel every rung beyond the new desired count (never strand one).
            for k in desired.len()..resting.len() {
                let tag = format!("{prefix}{k}");
                broker.cancel_tagged(&tag);
                self.own_book_pull(&tag);
            }
            resting.truncate(desired.len());
        }
        // store the resting-rung vec back and clear this side's fill-staleness (the side re-quoted).
        let state = self.side_mut(is_bid);
        state.rungs = resting;
        state.refresh_stale = false;
    }

    /// Retire the SINGLE-order quote on one side (`"bid"`/`"ask"`) when switching INTO ladder mode, so
    /// the pre-ladder order is not stranded. A no-op unless that side currently has a single quote
    /// resting (`*_placed`) — so after the first laddered tick, and on a maker that starts laddered,
    /// it does nothing.
    fn retire_single_order<B: HftBroker>(&mut self, broker: &mut B, is_bid: bool) {
        if !self.side(is_bid).placed {
            return;
        }
        let tag = if is_bid { "bid" } else { "ask" };
        broker.cancel_tagged(tag);
        self.own_book_pull(tag);
        let side = self.side_mut(is_bid);
        side.placed = false;
        side.own = None;
        side.refresh_stale = false;
    }

    /// Retire any resting LADDER rungs on one side when switching OUT of ladder mode (back to a single
    /// quote, or the ladder turned off), so no rung is stranded. A GUARDED no-op while that side's rung
    /// vec is empty — which is ALWAYS the case on the default (never-laddered) path, so it adds no
    /// verbs and touches no state there, keeping the single-order tail byte-identical.
    fn retire_ladder_orders<B: HftBroker>(&mut self, broker: &mut B, is_bid: bool) {
        let n = self.side(is_bid).rungs.len();
        if n == 0 {
            return;
        }
        let prefix = if is_bid { "bid" } else { "ask" };
        for k in 0..n {
            let tag = format!("{prefix}{k}");
            broker.cancel_tagged(&tag);
            self.own_book_pull(&tag);
        }
        let side = self.side_mut(is_bid);
        side.rungs.clear();
        side.refresh_stale = false;
    }

    /// The order-refresh TOLERANCE decision for ONE side: `true` ⇒ SKIP this side's modify and
    /// leave the resting order (and its queue position) alone.
    ///
    /// `resting` is that side's tracked `(price, size)` — the [`SideState::own`] snapshot the
    /// requote tail already maintains on every place/re-price. THREE ways to answer `false`
    /// immediately, the first two of which are the DEFAULT path: no tolerance configured (`None`),
    /// no tracked resting order to compare against, or `stale` — that side's snapshot invalidated
    /// by a fill ([`SideState::refresh_stale`]), where the snapshot is the maker's
    /// INTENDED quote but the venue-side remainder is smaller, so the comparison would wrongly read
    /// "no change" and the partial-fill size top-up would be lost. The arithmetic itself is the pure
    /// [`refresh::within_tolerance`].
    ///
    /// NOTE it is deliberately only reachable from the re-price arm of `requote`: the place and
    /// pull arms must never consult it.
    ///
    /// [`SideState::own`]: crate::SideState::own
    /// [`SideState::refresh_stale`]: crate::SideState::refresh_stale
    /// [`refresh::within_tolerance`]: crate::refresh::within_tolerance
    fn refresh_skips(
        &self,
        resting: Option<(f64, f64)>,
        price: f64,
        qty: f64,
        stale: bool,
    ) -> bool {
        if stale {
            return false;
        }
        match (self.cfg.refresh_tolerance, resting) {
            (Some(tol), Some(resting)) => within_tolerance(resting, (price, qty), tol),
            _ => false,
        }
    }

    /// The reward MIN-ORDER-AGE (moas) hold for one side (`is_bid` picks bid/ask): `true` ⇒ leave the
    /// resting quote to keep ageing toward the reward floor instead of re-pricing (churning) it.
    /// `false` on the DEFAULT path — reward chasing off, that side fill-stale (which must always top
    /// up, mirroring `refresh_skips`' `stale` short-circuit), or no tracked resting order. Delegates
    /// the actual age + still-in-band test to the pure [`reward::moas_holds`], so a safety re-price (a
    /// mid that ran the quote out of the band) always fires. Composed with `refresh_skips` (either may
    /// skip) and, like it, reachable ONLY from the re-price arm — never a place / pull.
    ///
    /// [`reward::moas_holds`]: crate::reward::moas_holds
    fn reward_moas_holds(
        &self,
        is_bid: bool,
        resting: Option<(f64, f64)>,
        mid: f64,
        now: i64,
    ) -> bool {
        let side = self.side(is_bid);
        if side.refresh_stale {
            return false;
        }
        let quoted_ts = side.quoted_ts;
        match (self.cfg.reward.filter(|r| r.weight > 0.0), resting) {
            (Some(r), Some((rpx, _))) => moas_holds(r, rpx, quoted_ts, mid, now, is_bid),
            _ => false,
        }
    }
}
