//! Entry sizing and the pre-trade gate: the sizer, the live `RiskGate` mount, the clamp.

use vike_exec::{RiskContext, RiskGate};
use vike_model::RiskLimits;
use vike_model::TradingState;
use vike_model::{OrderKind, OrderRequest, OrderType, py_sum};

use vike_analytics::sizing::SizeContext;

use super::params::EngineParams;
use super::state::SimBroker;

impl SimBroker {
    // --- sizing ---

    fn is_opening(&self, si: usize, side_sign: i32) -> bool {
        let pos = &self.sym[si].pos;
        pos.size == 0.0 || (pos.size > 0.0) == (side_sign > 0)
    }

    /// Total $ risk currently open across the book (builtin sum → py_sum, cross-section order).
    fn open_risk(&self) -> f64 {
        py_sum(
            self.cross_section()
                .iter()
                .copied()
                .filter(|&si| self.sym[si].stop.is_some() && self.sym[si].pos.size != 0.0)
                .map(|si| {
                    (self.sym[si].price - self.sym[si].stop.unwrap()).abs()
                        * self.sym[si].pos.size.abs()
                        * self.multiplier_of_idx(si)
                }),
        )
    }

    /// Run the sizer on opening/increasing entries; pass through when raw or reducing.
    pub(super) fn size_entry(
        &self,
        si: usize,
        side_sign: i32,
        size: f64,
        raw: bool,
        stop: Option<f64>,
    ) -> f64 {
        if raw || !self.is_opening(si, side_sign) {
            return size;
        }
        let equity = self.equity_now();
        let peak = self.equity_peak;
        let drawdown = if peak > 0.0 { f64::max(0.0, 1.0 - equity / peak) } else { 0.0 };
        self.sizer.size(&SizeContext {
            symbol: self.symbols[si].clone(),
            side: side_sign,
            intent: size,
            basis_price: self.sym[si].price,
            equity,
            cash: self.cash,
            multiplier: self.multiplier_of_idx(si),
            atr: self.atr(si, 14),
            drawdown,
            risk_stop: stop,
            open_risk: self.open_risk(),
        })
    }

    /// Mount THE ONE JUDGE (deny-vs-clamp phase 2): build the pre-trade `RiskGate` a `SimBroker`
    /// admits market orders through, or `None` when no gate is armed. Called once at engine
    /// construction (`StrategyEngine::new`); one gate per `SimBroker` (per venue-sim), exactly
    /// as live runs one gate per venue/account session.
    ///
    /// * `clamp_to_leverage == true` ⇒ `None` — the escape hatch restores the WHOLE pre-phase-2
    ///   admission pipeline ([`Self::cap_to_leverage`] truncation, verbatim); the gate is never
    ///   consulted and `risk_limits` is inert (documented on the knob).
    /// * `risk_limits: Some` ⇒ those limits verbatim (every live knob, present and future).
    /// * else `leverage: Some(L)` ⇒ `RiskLimits { im_requirement: Some(1/L) }` — **the
    ///   leverage→initial-margin mapping.** Equivalence to the retired leverage-room formula:
    ///   with uniform `im = 1/L` and `margin_used` folded pending-aware
    ///   ([`Self::margin_in_use_pending_aware`]), the gate's
    ///   `order_margin ≤ equity − margin_used` is `qty·px·mult/L ≤ eq − (open+pending)/L`, i.e.
    ///   `qty·px·mult ≤ L·eq − open − pending` — algebraically the exact `cap_to_leverage`
    ///   fits-whole test, `<=` boundary included. Its LIMITS: (a) equality is algebraic, not
    ///   bit-for-bit (the gate divides where the old room test multiplied), so a knife-edge f64
    ///   boundary can in principle flip — the margin lane was never parity-frozen, so no golden
    ///   pins that; (b) the old direction-only "reducing never denied" arm is now the gate's
    ///   stricter covered-reduce bypass plus the LEAN `closing_credit` — an uncovered REVERSAL
    ///   faces buying power for its full size (credited `2·|pos|·mark·mult·im` for the leg it
    ///   closes), so an overshoot beyond the credit now DENIES where the old formula waved it
    ///   through. That is the gate's verdict winning on purpose (live is the judge), pinned in
    ///   `riskgate_simbroker_parity.rs`.
    /// * neither ⇒ `None` — the submit path never builds a gate context: byte-identical for
    ///   every non-margined backtest (the big compat pin).
    ///
    /// The mounted gate always has `max_orders_per_window` DISARMED: the throttle is a
    /// WALL-CLOCK order-rate limit and sim time is not wall time — a backtest replaying a month
    /// in a second would spuriously rate-limit everything, and sim-timing it would gate on the
    /// tape's timestamps, a semantics live never has. Every other check runs as live.
    pub(crate) fn build_risk_gate(p: &EngineParams) -> Option<RiskGate> {
        if p.clamp_to_leverage {
            return None;
        }
        let mut limits = match (&p.risk_limits, p.leverage) {
            (Some(l), _) => l.clone(),
            (None, Some(lev)) => {
                RiskLimits { im_requirement: Some(1.0 / lev), ..RiskLimits::new() }
            }
            (None, None) => return None,
        };
        limits.max_orders_per_window = None; // wall-clock throttle: meaningless in sim time
        Some(RiskGate::new(limits))
    }

    /// Σ committed initial margin the gate's buying-power lane judges against — the backtest's
    /// `RiskContext::margin_used`, mirroring live's `Account::margin_in_use_priced` fold (each open
    /// position priced `|size|·mark·mult·im`, per-symbol `im_for` falling back to the ORDER
    /// symbol's rate; every SimBroker position is CROSS, so the live fold's isolated/cash pool
    /// partition (#497) is vacuously satisfied — the mode-partitioned law flows through the gate
    /// unchanged the day the sim grows margin modes) **plus the resting/in-flight market-order
    /// notional margin the retired `cap_to_leverage` counted**. Live's `Account` cannot see
    /// resting orders, but a pre-trade cap must model the LOCAL view — without this term every
    /// submit inside one entry-latency window would be granted the full budget
    /// (`latency_model.rs` pins it), so the pending fold is kept, valued at current marks with
    /// the same `py_sum` order as the formula it replaces.
    fn margin_in_use_pending_aware(&self, order_im: f64) -> f64 {
        let lim = &self.risk_gate.as_ref().expect("armed").limits;
        py_sum(self.cross_section().iter().map(|&i| {
            let im = lim.im_for(&self.symbols[i]).unwrap_or(order_im);
            let px_mult = self.sym[i].price * self.multiplier_of_idx(i);
            let mut signed = py_sum(
                self.sym[i]
                    .pending
                    .iter()
                    .filter(|o| o.kind == OrderKind::Market)
                    .map(|o| o.side as f64 * o.size),
            );
            if let Some(g) = self.latency.as_ref() {
                signed += g.in_flight_market_signed(i);
            }
            (self.sym[i].pos.size.abs() * px_mult + signed.abs() * px_mult) * im
        }))
    }

    /// The pre-trade judge at market-order intake — ONE dispatch point over the two outcome
    /// shapes (deny-vs-clamp phases 1+2):
    ///
    /// * DEFAULT (`clamp_to_leverage == false`): **the literal live gate.** The order crosses
    ///   `vike_exec::RiskGate::check` — the same code path `ExecutionEngine::gate_and_register`
    ///   runs live — over a [`RiskContext`] built from this engine's own state, mirroring the
    ///   live construction field for field: `position_size`/`mark_price` from the symbol state,
    ///   `equity` = [`Self::equity_now`] (the sim account is continuously marked to market, the
    ///   `equity_all` twin), `margin_used` = [`Self::margin_in_use_pending_aware`],
    ///   `closing_credit` = live's `2·|pos|·mark·mult·im` reversing credit, `multiplier` = the
    ///   symbol's real contract multiplier (unconditionally — live currently wires `1.0` when
    ///   the margin lane is unarmed, a known ctx-construction bug this side deliberately does
    ///   not reproduce; the gate CODE is shared, the ctx is each caller's to get right). A
    ///   refusal is whole (zero fill) and recorded in [`Self::dropped`] under the gate's OWN
    ///   reason string — a backtest denial IS the live denial. When a
    ///   [`EngineParams::properties`] source is configured, the gate's four instrument-grid
    ///   fields are refreshed from the point-in-time grid at `self.now` before the check, so
    ///   venue floors judge pre-trade exactly as live's `from_properties` limits do (on top of
    ///   `apply_fill`'s venue-side fill-time check, which stays — the venue re-checks at
    ///   execution, live and here).
    /// * OPT-IN (`clamp_to_leverage == true`): the historical TRUNCATE shape, verbatim —
    ///   [`Self::cap_to_leverage`] shrinks the order to the remaining room and the shrunken
    ///   size executes silently (including its quirks, e.g. the `equity <= 0` early-zero that
    ///   predates the reducing check). The phase-1 deny arm that lived here (the
    ///   `"insufficient-leverage-room"` room test) is GONE — `cap_to_leverage` survives only as
    ///   this clamp-knob path.
    ///
    /// A COVERED reducing order is never denied (anti-stranding — the gate's own
    /// `is_covered_reduce` bypass, the same rule that keeps `apply_fill`'s floors off closing
    /// fills), including on a bankrupt account. An exactly-fitting order passes whole
    /// (`has_sufficient_margin` is `<=`).
    ///
    /// The IMPACT knobs (`max_slippage_bps`/`require_fillable`) are NOT armed here: this site
    /// calls plain [`RiskGate::check`], never `check_with_book` — deliberately, because the
    /// live runtime ALSO has no `check_with_book` caller yet ("no such site exists today",
    /// `risk.rs` module doc: neither the engine nor the runtime retains a per-symbol book), and
    /// the replay books live in `run_ticks` locals, not on `SimBroker`. Arming the veto here
    /// FIRST would create a fresh backtest-vs-live divergence in the opposite direction; when a
    /// live site opts in, the tick/book lane grows the same call.
    /// ⚠ **Every order-submitting verb goes through here now, not just the market ones.**
    ///
    /// This was `gate_market_order`, called from `submit_market` and `submit_market_close` ALONE.
    /// `submit_limit`, `submit_stop`, `submit_trailing`, `submit_limit_close` and
    /// `HftBroker::submit_limit_tagged` faced NO pre-trade gate at all, so a backtest of a limit
    /// strategy ran with the operator's `[risk]` budget effectively switched off — no per-order
    /// notional cap, no projected-exposure cap, no buying-power / `max_leverage` check — while LIVE
    /// gates all of them. The backtest was the PERMISSIVE side, so a strategy could validate here
    /// and be refused in production. `vike-mm` quotes limits exclusively.
    ///
    /// ⚠ **`price`/`trigger` go on the REQUEST; `ctx.mark_price` stays the MARK.** That split is
    /// deliberate and is not a detail. `RiskGate::check_inner` already computes
    /// `ref_price = req.price.or(req.trigger_price).unwrap_or(ctx.mark_price)` for the notional
    /// lanes, so a limit order is judged on its own price for min/max-notional while the
    /// projected-exposure cap keeps valuing at the mark. Folding the order price into `mark_price`
    /// instead would judge EXPOSURE on a price the position is not worth — exposure is a valuation
    /// question, not an order-price one, and doing it that way would import the live gate's own
    /// `mark_price`-basis bug into the backtest rather than fixing anything.
    pub(super) fn gate_order(
        &mut self,
        si: usize,
        side_sign: i32,
        size: f64,
        weight: f64,
        order_type: OrderType,
        price: Option<f64>,
        trigger: Option<f64>,
    ) -> f64 {
        if self.clamp_to_leverage {
            return self.cap_to_leverage(si, side_sign, size);
        }
        if self.risk_gate.is_none() || size <= 0.0 {
            return size; // no gate armed / nothing to judge — byte-identical pass-through
        }
        // ---- RiskContext, mirroring `ExecutionEngine::gate_and_register` field for field ----
        let mark = self.sym[si].price;
        let mult = self.multiplier_of_idx(si);
        let im = {
            let lim = &self.risk_gate.as_ref().expect("checked Some").limits;
            lim.im_for(&self.symbols[si])
        };
        // The margin fields are computed ONLY when the buying-power lane is armed for this
        // symbol — the live construction's exact gating (they are read by no other check).
        let (equity, margin_used, closing_credit) = match im {
            Some(im_req) => {
                let pos = self.sym[si].pos.size;
                let credit = if pos != 0.0 && side_sign as f64 * pos < 0.0 {
                    // direction-reversing order: margin the close frees + the LEAN re-open
                    // credit (single-requirement model: mm == im in the gate) — live verbatim
                    2.0 * pos.abs() * mark * mult * im_req
                } else {
                    0.0
                };
                (self.equity_now(), self.margin_in_use_pending_aware(im_req), credit)
            }
            None => (0.0, 0.0, 0.0),
        };
        // The ACCOUNT-aggregate ceiling's other half (`RiskLimits::max_account_exposure`), folded
        // only when that lane is armed — the same gating the margin fields above use, and for the
        // same reason: it is O(symbols) and an unarmed backtest must pay nothing for it.
        //
        // ⚠ **It is folded HERE rather than left at `0.0`, and that is the whole standing rule of
        // this function**: the backtest must never be the PERMISSIVE side (see this fn's doc — the
        // gap it was written to close was exactly a limit strategy validating here against a budget
        // live enforces). A `0.0` would vacate the account ceiling for every backtest while live
        // refused, so a strategy could pass validation and be denied in production.
        //
        // Same shape as the live producer (`ExecutionEngine::resolved_account_exposure_excluding`):
        // GROSS (`|size|`, never netting), the ORDER's own symbol's POSITION excluded because the
        // gate re-adds it projected, everything valued at the current mark and the symbol's own
        // multiplier. The sim account is one venue's one account by construction, so there is no
        // foreign-venue row to skip here — the live fold's venue skip has no counterpart. `py_sum`
        // in `symbols` order, matching `margin_in_use_pending_aware` and `equity_now` beside it.
        //
        // ⚠ **RESTING ORDERS COUNT, on EVERY symbol including the order's own** — the live
        // producer's second half, and the reason that one exists: without it N orders submitted
        // inside one fill window are each judged as though the others committed nothing, and the
        // ceiling is exceeded by an arbitrary multiple. A backtest that skipped this term would be
        // the PERMISSIVE side for exactly the strategy shape that trips it live (a maker resting
        // quotes on many symbols), which is the failure this whole function's doc is about.
        //
        // ⚠ Two deliberate differences from the MARGIN term directly above, both of which move
        // this fold TOWARD the live producer rather than away from it: every pending KIND is
        // counted (that fold takes `OrderKind::Market` alone, while live's `is_live()` registry
        // walk counts a resting limit too), and each order contributes its own `|size|` instead of
        // the per-symbol SIGNED sum's magnitude (exposure is gross by construction — see
        // `vike_model::RiskLimits::max_account_exposure`; netting two opposed resting orders to
        // zero is precisely the reading that axis refuses). A COVERED reduce contributes nothing,
        // the live fold's own skip, through the same shared predicate.
        let account_exposure_excl_order =
            if self.risk_gate.as_ref().expect("checked Some").limits.max_account_exposure.is_some()
            {
                py_sum(self.cross_section().iter().map(|&i| {
                    let px_mult = self.sym[i].price * self.multiplier_of_idx(i);
                    let position = if i == si {
                        0.0
                    } else {
                        vike_model::gross_notional(
                            self.sym[i].pos.size,
                            self.sym[i].price,
                            self.multiplier_of_idx(i),
                        )
                    };
                    let resting = py_sum(self.sym[i].pending.iter().filter_map(|o| {
                        let covered = vike_model::is_covered_reduce(
                            false, // the sim's `WorkingOrder` carries no reduce_only flag; the
                            // predicate's IMPLICIT arm (opposite direction, fully covered) is the
                            // one that can fire here, and it is the same arm live relies on for an
                            // untagged exit.
                            o.side,
                            self.sym[i].pos.size,
                            o.size,
                        );
                        (!covered).then(|| o.size.abs() * px_mult)
                    }));
                    // …and the ORDERS ALREADY ON THE WIRE under the opt-in latency model, which
                    // have left `pending` and not yet filled. Taken exactly as the margin term
                    // above takes them (per-symbol SIGNED sum, magnitude); `None` — every default
                    // run — contributes nothing and this line is inert.
                    let in_flight = self
                        .latency
                        .as_ref()
                        .map_or(0.0, |g| g.in_flight_market_signed(i).abs() * px_mult);
                    position + resting + in_flight
                }))
            } else {
                0.0
            };
        let ctx = RiskContext {
            position_size: self.sym[si].pos.size,
            mark_price: mark,
            trading_state: TradingState::Active,
            now_ms: self.now,
            equity,
            margin_used,
            closing_credit,
            contract_value: vike_model::ContractValue::linear(mult),
            account_exposure_excl_order,
        };
        let req = OrderRequest {
            client_order_id: String::new(), // sim orders have no coid; the gate never reads it
            venue: self.venue.clone().unwrap_or_default(),
            symbol: self.symbols[si].clone(),
            order_type,
            side: side_sign,
            qty: size,
            price,
            trigger_price: trigger,
            ..Default::default()
        };
        // BEHAVIOR NOTE (leverage+properties runs only — review M1): with the gate armed, the
        // venue floors now judge at PRE-TRADE time through this refresh, not only at apply_fill:
        // a below-min OPENING order is denied at submit with the gate's reason ("below-min-qty")
        // instead of dropped at fill ("min_qty"), and a below-min REVERSAL that previously flipped
        // whole at apply_fill is now DENIED — agreeing with live, which denies. Pure-leverage runs
        // without a properties source are unaffected (no grid to refresh).
        // Refresh the gate's instrument-grid fields from the PIT properties grid (the venue's
        // own time-varying facts; the operator knobs in `risk_limits` are never touched). Only
        // when a grid source is configured — otherwise explicit `risk_limits` floors stand.
        let grid = if self.venue.is_some() && self.properties.is_some() {
            Some(self.grid_for(si, self.now).as_ref().map(RiskLimits::from_properties))
        } else {
            None
        };
        let gate = self.risk_gate.as_mut().expect("checked Some");
        if let Some(g) = grid {
            let g = g.unwrap_or_else(RiskLimits::new); // no PIT record = unconstrained
            let lim = &mut gate.limits;
            lim.tick_size = g.tick_size;
            lim.lot_size = g.lot_size;
            lim.min_qty = g.min_qty;
            lim.min_notional = g.min_notional;
        }
        let verdict = gate.check(&req, &ctx);
        if verdict.ok {
            // The gate's normalized (lot-rounded) request is deliberately NOT substituted:
            // `apply_fill` re-snaps to the PIT grid at fill time, the sim's venue-side site.
            return size;
        }
        let sym = self.symbols[si].clone();
        self.dropped.push((sym, verdict.reason, size, weight));
        0.0
    }

    /// Shrink an opening/adding market order so projected TOTAL account notional
    /// <= leverage*equity (account-level; pending-aware; reducing never shrunk). The frozen
    /// pre-phase-1 TRUNCATE formula — since deny-vs-clamp phase 2 reached ONLY via
    /// [`Self::gate_order`] with `clamp_to_leverage == true` (the escape hatch); the
    /// default path now judges through the live `RiskGate` and never consults this.
    fn cap_to_leverage(&self, si: usize, side_sign: i32, size: f64) -> f64 {
        let Some(leverage) = self.leverage else {
            return size;
        };
        let eq = self.equity_now();
        if eq <= 0.0 {
            return 0.0;
        }
        let pos = &self.sym[si].pos;
        if pos.size != 0.0 && (pos.size > 0.0) != (side_sign > 0) {
            return size; // reducing/closing: never capped
        }
        let max_notional = leverage * eq;
        let cur = py_sum(self.cross_section().iter().map(|&i| {
            vike_model::gross_notional(
                self.sym[i].pos.size,
                self.sym[i].price,
                self.multiplier_of_idx(i),
            )
        }));
        let pending = py_sum(self.cross_section().iter().map(|&i| {
            let mut signed = py_sum(
                self.sym[i]
                    .pending
                    .iter()
                    .filter(|o| o.kind == OrderKind::Market)
                    .map(|o| o.side as f64 * o.size),
            );
            // ...plus the market orders this account has already SENT but the matching engine has
            // not yet seen (opt-in latency gate only; `None` leaves the fold byte-identical). A
            // pre-trade cap must model the LOCAL view — without this, every submit made inside one
            // entry-latency window would be granted the full leverage room and the cap would be
            // silently unenforced for the whole window. See `LatencyGate::in_flight_market_signed`.
            if let Some(g) = self.latency.as_ref() {
                signed += g.in_flight_market_signed(i);
            }
            vike_model::gross_notional(signed, self.sym[i].price, self.multiplier_of_idx(i))
        }));
        let room_notional = max_notional - cur - pending;
        if room_notional <= 0.0 {
            return 0.0;
        }
        let room = room_notional / (self.sym[si].price * self.multiplier_of_idx(si));
        if size <= room { size } else { room }
    }
}
