//! Multi-leg combos: the resting combo book, leg pricing off fresh marks and the all-or-none fill.

use vike_fills::broker_sim::adverse_fill_price;
use vike_model::events::{Event, FillEvent, OrderFilled, OrderPartiallyFilled, TradeId};
use vike_model::{ComboLeg, combo_net_cross};

use super::{PaperExecutionClient, PaperFill};

/// One leg's `(ratio, price)` pair as fed to [`vike_model::combo_net_cross`] — named so the
/// per-leg vectors below never spell a bare nested tuple type (clippy `type_complexity`).
type LegQuote = (i32, f64);

/// One resting multi-leg combo order (combo PR-3). Kept in its OWN book, separate from the
/// single-symbol `pending` list, because a combo is not a `WorkingOrder`: it has no single symbol,
/// its `price` is a SIGNED NET per combo unit (negative for a credit structure), and it must fill
/// **all legs or none** — none of which the single-symbol fill primitives model.
///
/// One combo is ONE order downstream: one coid, one event stream, exactly one terminal event —
/// the invariant `vike_model`'s combo vocabulary is built around (see [`vike_model::ComboSpec`]).
///
/// **Downstream requirement:** the fills this combo eventually emits carry each LEG's own symbol,
/// so the consuming `ExecutionEngine` must accept every leg symbol (`extra_symbols`) or the bare
/// fills are dropped while the FSM wraps still land — order Filled, Account flat. See
/// `emit_combo_fills` and `tests/combo_engine_fold.rs`.
///
/// **Contingency links are NOT supported on a combo** — `submit` terminally rejects a combo
/// request carrying `parent_order_id` / `linked_order_ids` / `contingency_type`, because the
/// OTO-hold and expire-cascade machinery enforce against the single-symbol `pending` book only.
/// (The one enforced direction: a PLAIN order may OCO-link a resting combo — `apply_contingency`
/// sweeps the combo book for siblings — and a combo's FILL arms plain OTO children as any fill
/// does.)
#[derive(Debug, Clone)]
pub(super) struct RestingCombo {
    /// +1 buy the combo / -1 sell it. Legs flip with it; NEVER 0 (`ComboSpec::validate`).
    pub(super) side: i32,
    /// combo UNITS (a leg trades `|ratio| × qty` of its own instrument)
    pub(super) qty: f64,
    /// SIGNED net limit per combo unit. `None` = combo MARKET (fills as soon as every leg carries
    /// a FRESH mark — see `combo_mark_staleness_ms`). **Never clamped, never absolute-valued** —
    /// a credit combo's limit is negative.
    pub(super) net_limit: Option<f64>,
    /// 2..=N legs in venue-creation order — the fold order `combo_net` pins.
    pub(super) legs: Vec<ComboLeg>,
}

impl PaperExecutionClient {
    /// Price every leg of `combo` off the recorded marks, or `None` if ANY leg is still unmarked
    /// **or marked staler than [`Self::combo_mark_staleness_ms`] as of `now_ts`** (the triggering
    /// pass's bar ts). A stale mark is treated exactly like no mark at all.
    ///
    /// This all-or-nothing return is the FIRST half of combo atomicity: a combo whose legs are not
    /// all priceable AT THE SAME MOMENT produces no quote at all, so it can neither half-fill on
    /// the legs that happen to have ticked nor trigger off a net whose leg prices never coexisted.
    ///
    /// Side selection mirrors [`vike_model::combo_net_from_legs`] exactly: the combo pays the ASK
    /// on a leg it buys and receives the BID on a leg it sells, i.e. `want_ask = side · ratio > 0`.
    /// A bar without an explicit bid/ask falls back to its close for both sides.
    fn quote_legs(&self, combo: &RestingCombo, now_ts: i64) -> Option<Vec<LegQuote>> {
        let mut quotes: Vec<LegQuote> = Vec::with_capacity(combo.legs.len());
        for leg in &combo.legs {
            let bar = self.leg_marks.get(&leg.symbol)?;
            // The engine's staleness convention verbatim (strict `age > bound`; bound 0 = only a
            // mark stamped at this very pass's ts). The mark's ts IS its bar's ts — bar streams
            // are the clock here, exactly as in the engine's stale-price wait lane.
            if vike_fills::staleness::is_stale(Some(bar.ts), now_ts, self.combo_mark_staleness_ms) {
                return None;
            }
            let want_ask = combo.side.signum() * leg.ratio.signum() > 0;
            let px =
                if want_ask { bar.ask.unwrap_or(bar.close) } else { bar.bid.unwrap_or(bar.close) };
            quotes.push((leg.ratio, px));
        }
        Some(quotes)
    }

    /// Combo-book hygiene: `leg_marks` exists ONLY to price a resting combo's legs, so the moment
    /// the last combo leaves the book (filled / canceled / expired / OCO-canceled) every recorded
    /// mark is dropped. This keeps the field's contract literal — marks live only while a combo is
    /// resting — and makes mark RESURRECTION structurally impossible: a combo submitted later can
    /// never price off a mark recorded during an earlier combo's life, no matter how permissive
    /// `combo_mark_staleness_ms` is. (The freshness bound alone already refuses old marks; this is
    /// the belt to that suspenders, and it also frees the map on every combo-free run.)
    ///
    /// Never called from inside `emit_combo_fills`: `fill_combos` drains `self.combos` via
    /// `mem::take` while iterating, so the book is transiently empty there and clearing marks
    /// mid-pass would starve the other combos in the same pass.
    pub(super) fn drop_marks_if_idle(&mut self) {
        if self.combos.is_empty() && !self.leg_marks.is_empty() {
            self.leg_marks.clear();
        }
    }

    /// The combo twin of the single-symbol fill pass: for each resting combo, price its legs, test
    /// the COMBO's net against its net limit, and fill **every leg or none**.
    ///
    /// The trigger is the aggregate net only ([`vike_model::combo_net_cross`], which owns both the
    /// `Σ(ratio · px)` sign law and the buy-`net ≤ limit` / sell-`net ≥ limit` crossing law). The
    /// net is SIGNED and is never compared as a magnitude: a credit spread's net and limit are both
    /// negative and cross exactly like a debit's, which is why nothing here takes an `abs()`.
    ///
    /// A combo MARKET (`net_limit == None`) has no trigger — it fills as soon as every leg carries
    /// a FRESH mark (see `quote_legs`; freshness is the same gate the limit trigger runs behind).
    ///
    /// Each leg then fills at its OWN price; the net is only the trigger, never a fill price. This
    /// is deliberate: downstream accounting must see real per-instrument fills, not one synthetic
    /// blended fill on a symbol that does not exist.
    ///
    /// **The net limit binds PRE-slippage.** The trigger compares the RAW leg quotes; per-leg
    /// adverse slippage is applied afterwards in `emit_combo_fills`, so with `slippage > 0` the
    /// EXECUTED net lands through the limit by slippage × gross(Σ|ratio·px|). This is not an
    /// oversight — it is the same law as this book's single-symbol path (`BarFillModel` triggers
    /// off raw bar prices; `emit_fill` then applies `adverse_fill_price`) and as LEAN's
    /// `ComboLimitFill`, which also crosses on raw quotes. Folding slippage into the trigger here
    /// would make a combo limit STRICTER than a plain limit under the identical cost model.
    /// Pinned by `slippage_is_adverse_per_leg_side`.
    pub(super) fn fill_combos(&mut self, ts: i64) {
        if self.combos.is_empty() {
            // the book may have emptied since the last pass (cancel / OCO) — drop dead marks
            self.drop_marks_if_idle();
            return;
        }
        let mut still: Vec<(String, RestingCombo)> = Vec::new();
        for (coid, combo) in std::mem::take(&mut self.combos) {
            let Some(quotes) = self.quote_legs(&combo, ts) else {
                still.push((coid, combo)); // a leg is unmarked or stale — cannot fill any leg
                continue;
            };
            // `None` limit = combo market: no crossing test at all. `Some` = LEAN's
            // `ComboLimitFill` aggregate crossing law, sign-preserving.
            if let Some(limit) = combo.net_limit
                && combo_net_cross(combo.side, limit, &quotes).is_none()
            {
                still.push((coid, combo)); // net has not crossed — the whole combo rests on
                continue;
            }
            self.emit_combo_fills(&coid, &combo, &quotes, ts);
        }
        self.combos = still;
        self.drop_marks_if_idle();
    }

    /// Emit the per-leg fills of ONE crossed combo — the SECOND half of atomicity: this runs only
    /// after `fill_combos` has proved every leg priceable and the net crossed, and it emits all
    /// `n` legs unconditionally, so there is no path on which a subset of legs fills.
    ///
    /// Event shape follows `emit_fill`'s dual-publish rule per leg (bare `Fill` first, then the FSM
    /// wrap) and keeps the ONE-order invariant: legs `0..n-1` wrap as `OrderPartiallyFilled` and
    /// the LAST leg as `OrderFilled`, so the combo's single coid reaches exactly one terminal
    /// event, exactly as a single-symbol order does.
    ///
    /// **The consuming engine must admit every LEG symbol** — via `ExecutionEngine::extra_symbols`
    /// (the mount-side lowering wires this at combo registration; it also carries leg
    /// Funding/PositionLiquidated events, which fill-routing cannot), AND, since combo gate 4,
    /// the engine's own `owns_fill_symbol` coid-ownership route folds a bare leg fill whose
    /// `client_order_id` names the registered combo even when the leg was never admitted — the
    /// safety net that retired the old failure shape (order Filled via the coid-routed wraps,
    /// Account flat, strategy blind). Both are proven by `tests/combo_engine_fold.rs`.
    ///
    /// **`ManagedOrder` accounting is a cross-instrument AGGREGATE for a combo.** The FSM wraps
    /// accumulate per-leg fills under the one coid, so `filled_qty` ends at `Σ|ratio| × qty`
    /// across legs — e.g. 9 for a 1×2 ratio spread with `request.qty` 3 — and `avg_fill_px` is a
    /// qty-weighted VWAP across DIFFERENT instruments: display-only, a price on no tradable
    /// instrument, and NOT the per-unit net (which `combo_net` computes from the leg fills).
    /// Surfaces as-is in `OrderView`, the journal's `exec_order` rows and any order table; per-leg
    /// truth lives in the bare `Fill`s (Account) — pinned by `tests/combo_engine_fold.rs`.
    /// Making the wraps count combo units instead would require the wrap's embedded `FillEvent`
    /// to disagree with the bare `Fill` the Account folds (same struct, dual-published), rippling
    /// into vike-exec's accumulate/journal semantics — documented instead, deliberately.
    ///
    /// **Fill prices are the raw quotes worsened by per-leg adverse slippage** — the net limit the
    /// trigger enforced binds pre-slippage (see `fill_combos`).
    ///
    /// `self.position` is deliberately NOT moved: it tracks this book's own single `symbol` for
    /// `resolve_intrabar_fills`' bracket guard, while combo legs are other instruments. Folding
    /// foreign-instrument quantities into it would corrupt the single-symbol fill pass.
    fn emit_combo_fills(&mut self, coid: &str, combo: &RestingCombo, quotes: &[LegQuote], ts: i64) {
        // a filled combo has left the resting book, so it can never expire afterwards
        self.expiry.shift_remove(coid);
        // maker/taker by ORDER KIND, matching `emit_fill`: a net-limit combo books maker.
        let is_maker = combo.net_limit.is_some();
        let n = combo.legs.len();
        for (i, leg) in combo.legs.iter().enumerate() {
            let (ratio, raw_px) = quotes[i];
            // selling the combo flips every leg (the venue convention `ComboLeg` documents)
            let leg_side = combo.side.signum() * ratio.signum();
            let leg_qty = f64::from(ratio.abs()) * combo.qty;
            let px = adverse_fill_price(raw_px, leg_side, self.slippage);
            // Each leg fills on its OWN instrument, so the underlying source (if wired) is queried
            // per LEG symbol; a non-`PercentOfUnderlying`/no-source combo is byte-identical.
            let commission = self.commission_for(is_maker, leg_qty, px, &leg.symbol, ts);
            self.fill_n += 1;
            let fill = FillEvent {
                // Same minted namespace as the single-symbol path above — `fill_n` is shared, so
                // combo legs and plain fills draw from one sequence and stay byte-identical.
                trade_id: TradeId::prefixed("paper-", self.fill_n),
                client_order_id: coid.to_string(),
                venue: self.venue.clone().into(),
                // the LEG's own instrument — never the (empty) combo symbol
                symbol: leg.symbol.clone().into(),
                side: leg_side,
                last_qty: leg_qty,
                last_px: px,
                commission,
                commission_asset: String::new().into(),
                liquidity_side: if is_maker { "maker" } else { "taker" }.to_string().into(),
                ts,
                mark_price: None,
                position_side: vike_model::events::PositionSide::Both,
            };
            self.fills.lock().unwrap().push(PaperFill {
                ts,
                side: leg_side,
                qty: leg_qty,
                px,
                fee: commission,
                is_maker,
            });
            self.events.push_back(Event::Fill(fill.clone()));
            let wrap = if i + 1 == n {
                Event::OrderFilled(OrderFilled { client_order_id: coid.to_string(), fill, ts })
            } else {
                Event::OrderPartiallyFilled(OrderPartiallyFilled {
                    client_order_id: coid.to_string(),
                    fill,
                    ts,
                })
            };
            self.events.push_back(wrap);
        }
        self.apply_contingency(coid, ts);
    }
}
