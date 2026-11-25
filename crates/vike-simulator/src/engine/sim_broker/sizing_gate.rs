//! Entry sizing and the pre-trade gate: the sizer, the live `RiskGate` mount, the clamp.

use vike_execution::risk::{MountLeverageLeg, MountLeverageRow};
use vike_execution::{RiskContext, RiskGate, RiskVerdict};
use vike_math::summation::compensated_sum;
use vike_model::RiskLimits;
use vike_model::TradingState;
use vike_model::{NotionalKind, OrderDrop, OrderKind, OrderRequest, OrderResting, OrderType};

use crate::latency::LatencyModelInFlightAction;

use vike_portfolio::sizing::PositionSizerContext;

use super::params::StrategyEngineConfig;
use super::state::{OrderBrokerSimulator, SizingGateTerms};

/// The reason a reduce-only order is refused (`OrderBrokerSimulator::dropped`): the live core's own words
/// (`vike-core`'s `core_held_fire_quantity`) for a reduce-only stop that meets no position on its side.
pub(crate) const REDUCE_ONLY_REFUSED: &str =
    "reduce-only: there is no position on its side to reduce";

impl OrderBrokerSimulator {
    // --- sizing ---

    /// The size a REDUCE-ONLY order may take now: `min(size, |position|)` while `side_sign` pushes
    /// against the symbol's position, else `0.0` with the refusal recorded in [`Self::dropped`] under
    /// the words the live core says for the same thing (`core_held_fire_quantity`'s "there is no position
    /// on its side to reduce"). The ONE copy of the rule: the submit verb asks it (a flat book or the
    /// position's own side is refused at once) and every fill lane asks it again at the fill, so a
    /// stop whose position was closed another way in between drops instead of OPENING a new one.
    pub(crate) fn reduce_only_size(&mut self, si: usize, side_sign: i32, size: f64) -> f64 {
        let pos = self.symbol[si].pos.size;
        if !vike_model::is_reducing_direction(side_sign, pos) {
            let symbol_name = self.symbols[si].clone();
            self.dropped.push(OrderDrop {
                symbol: symbol_name,
                reason: REDUCE_ONLY_REFUSED.into(),
                size,
                weight: 0.0,
            });
            return 0.0;
        }
        size.min(pos.abs())
    }

    /// Total $ risk currently open across the book (builtin sum → compensated_sum, cross-section order). Each
    /// holding's risk is `ContractValue::stop_risk` of the move to its stop: linear the price distance
    /// times the size and the unit, inverse the contract's PnL of the move in dollars at its price
    /// (`ContractValue::move_risk`), the money the sizer's context is in.
    fn open_risk(&self) -> f64 {
        compensated_sum(
            self.cross_section()
                .iter()
                .copied()
                .filter(|&si| self.symbol[si].stop.is_some() && self.symbol[si].pos.size != 0.0)
                .map(|si| {
                    let st = &self.symbol[si];
                    self.value_of_index(si).stop_risk(st.pos.size, st.price, st.stop.unwrap())
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
        if raw || vike_model::is_reducing_direction(side_sign, self.symbol[si].pos.size) {
            return size;
        }
        let equity = self.equity_now();
        let peak = self.equity_peak;
        // `1.0 - equity / peak`, not `vike_math::time_series::drawdown_fraction`: the two round apart,
        // and the frozen r4 `caps_sizers_mask` run replays these bits through a drawdown throttle
        // (decision 0021).
        let drawdown = if peak > 0.0 { f64::max(0.0, 1.0 - equity / peak) } else { 0.0 };
        // The context's money is the ledger's (a converted symbol's value carries its rate): a coin
        // ledger's coin at the basis price.
        self.sizer.size(&PositionSizerContext {
            symbol: self.symbols[si].clone(),
            side: side_sign,
            intent: size,
            basis_price: self.symbol[si].price,
            equity: self.in_quote(si, equity),
            cash: self.in_quote(si, self.cash),
            contract_value: self.value_of_index(si),
            atr: self.atr(si, 14),
            drawdown,
            risk_stop: stop,
            open_risk: self.open_risk(),
        })
    }

    /// Mount THE ONE JUDGE (deny-vs-clamp phase 2): build the pre-trade `RiskGate` an `OrderBrokerSimulator`
    /// admits market orders through, or `None` when no gate is armed. Called once at engine
    /// construction (`StrategyEngine::new`); one gate per `OrderBrokerSimulator` (per venue-sim), exactly
    /// as live runs one gate per venue/account session.
    ///
    /// * `clamp_to_leverage == true` ⇒ `None` — the escape hatch restores the WHOLE pre-phase-2
    ///   admission pipeline ([`Self::cap_to_leverage`] truncation, verbatim); the gate is never
    ///   consulted and `risk_limits` is inert (documented on the knob).
    /// * `risk_limits: Some` ⇒ those limits verbatim (every live knob, present and future).
    /// * else `leverage: Some(L)` ⇒ `RiskLimits { initial_margin_requirement: Some(1/L) }` — **the
    ///   leverage→initial-margin mapping.** Equivalence to the retired leverage-room formula:
    ///   with uniform `initial_margin = 1/L` and `margin_used` folded pending-aware
    ///   ([`Self::margin_in_use_pending_aware`]), the gate's
    ///   `order_margin ≤ equity − margin_used` is `qty·px·mult/L ≤ eq − (open+pending)/L`, i.e.
    ///   `qty·px·mult ≤ L·eq − open − pending` — algebraically the exact `cap_to_leverage`
    ///   fits-whole test, `<=` boundary included. Its LIMITS: (a) equality is algebraic, not
    ///   bit-for-bit (the gate divides where the old room test multiplied), so a knife-edge f64
    ///   boundary can in principle flip — the margin lane was never parity-frozen, so no golden
    ///   pins that; (b) the old direction-only "reducing never denied" arm is now the gate's
    ///   stricter covered-reduce bypass plus the LEAN `closing_credit` — an uncovered REVERSAL
    ///   faces buying power for its full size (credited `2·|pos|·mark·mult·initial_margin` for the leg it
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
    pub(crate) fn build_risk_gate(p: &StrategyEngineConfig) -> Option<RiskGate> {
        if p.clamp_to_leverage {
            return None;
        }
        let mut limits = match (&p.risk_limits, p.leverage) {
            (Some(l), _) => l.clone(),
            (None, Some(leverage)) => {
                RiskLimits { initial_margin_requirement: Some(1.0 / leverage), ..RiskLimits::new() }
            }
            (None, None) => return None,
        };
        limits.max_orders_per_window = None; // wall-clock throttle: meaningless in sim time
        Some(RiskGate::new(limits))
    }

    /// Σ committed initial margin the gate's buying-power lane judges against — the backtest's
    /// `RiskContext::margin_used`, mirroring live's `Account::margin_in_use_priced` fold (each open
    /// position priced `|size|·mark·mult·initial_margin`, per-symbol `initial_margin_for` falling back to the ORDER
    /// symbol's rate; every OrderBrokerSimulator position is CROSS, so the live fold's isolated/cash pool
    /// partition (#497) is vacuously satisfied — the mode-partitioned law flows through the gate
    /// unchanged the day the sim grows margin modes) **plus the margin of every working order,
    /// live's `Engine::live_order_margin`**: every kind, gross, valued at the current mark,
    /// a covered reduce skipped ([`Self::committed_order_quantities`]). The backtest must never be the
    /// PERMISSIVE side, and without this term every submit inside one entry-latency window would be
    /// granted the full budget (`latency_model.rs` pins it). The order an amend judges is left out
    /// (`skip_queue_id`: [`Self::gate_verdict`]).
    fn margin_in_use_pending_aware(
        &self,
        order_initial_margin: f64,
        skip_queue_id: Option<u64>,
    ) -> f64 {
        let limit = &self.risk_gate.as_ref().expect("armed").limits;
        compensated_sum(self.cross_section().iter().map(|&i| {
            let initial_margin =
                limit.initial_margin_for(&self.symbols[i]).unwrap_or(order_initial_margin);
            // `ContractValue::gross_by_unit`: the unit's worth first, `|q|·(p·u)`, the order this fold
            // pins (not `gross_notional`'s); an inverse unit is its face, the price never entering.
            let (value, price) = (self.value_of_index(i), self.symbol[i].price);
            let committed = compensated_sum(self.committed_order_quantities(i, skip_queue_id));
            (value.gross_by_unit(self.symbol[i].pos.size, price)
                + value.gross_by_unit(committed, price))
                * initial_margin
        }))
    }

    /// **The cash a spot buy may spend** (`OrderBrokerSimulator::fill_terms`' spot rule): `cash`, with every symbol on
    /// margin (not in [`StrategyEngineConfig::cash_symbols`]) valued back at its price and the margin it holds taken
    /// out, its position and its working orders ([`Self::committed_order_quantities`], as
    /// [`Self::margin_in_use_pending_aware`] counts them), at its initial-margin rate: the pre-trade gate's, else
    /// `1 / leverage`, else the full price. A run of spot symbols only is `cash` exactly.
    pub(super) fn free_cash(&self) -> f64 {
        let limits = self.risk_gate.as_ref().map(|g| &g.limits);
        let margined = self.cross_section().iter().filter(|&&i| !self.cash_by_si[i]).map(|&i| {
            let rate = limits
                .and_then(|l| l.initial_margin_for(&self.symbols[i]))
                .or(self.leverage.map(|leverage| 1.0 / leverage))
                .unwrap_or(1.0);
            let (value, price, size) =
                (self.value_of_index(i), self.symbol[i].price, self.symbol[i].pos.size);
            let committed = compensated_sum(self.committed_order_quantities(i, None));
            value.cash_leg(size, price)
                - (value.gross_by_unit(size, price) + value.gross_by_unit(committed, price)) * rate
        });
        self.cash + compensated_sum(margined)
    }

    /// Why a SPOT (cash) symbol refuses a SELL of `qty` on `symbols[si]` at its submit, or `None`:
    /// the live engine's `cash_oversell_denial`, through the ONE rule
    /// [`vike_execution::risk::cash_oversell_refusal`] and in its words. `None` at once for a buy and for a
    /// symbol on margin ([`StrategyEngineConfig::cash_symbols`] does not list it), so such a run is unchanged.
    /// The sells already out are every live sell of the symbol: resting, and in flight under the
    /// opt-in latency model, an amend or a cancel still in flight counting as sent; the order-list
    /// leg being placed is judged with its group's live legs as one claim (`Self::placing_sets`), and
    /// `skip_queue_id` leaves out the order being raised. A held order (a release, an OTO child) is not
    /// out: no venue holds it.
    pub(super) fn cash_refusal(
        &self,
        si: usize,
        side: i32,
        qty: f64,
        skip_queue_id: Option<u64>,
    ) -> Option<String> {
        if side >= 0 || !self.cash_by_si[si] {
            return None;
        }
        let group = self.placing_sets();
        let in_group = |qid: u64| {
            group.as_ref().is_some_and(|(sets, _)| sets.iter().flatten().any(|q| *q == qid))
        };
        let st = &self.symbol[si];
        let pushed = self.latency.iter().flat_map(|g| &g.queue).filter_map(|f| match &f.action {
            LatencyModelInFlightAction::Push { si: s, order } if *s == si => Some(order),
            _ => None,
        });
        let out: Vec<&OrderResting> = st
            .pending
            .iter()
            .chain(pushed)
            .filter(|o| o.side < 0 && Some(o.qid) != skip_queue_id)
            .collect();
        // `(qid, remaining)` of each sell, the maker law's quotes after the others (the order the
        // compensated sum below has always folded them in); a quote is never an order-list leg
        let quote = |o: &&&OrderResting| self.is_quote(si, o.qid);
        let (quotes, others): (Vec<&&OrderResting>, Vec<&&OrderResting>) =
            out.iter().partition(quote);
        let mut sells: Vec<(u64, f64)> =
            others.into_iter().chain(quotes).map(|o| (o.qid, o.size)).collect();
        for f in self.latency.iter().flat_map(|g| &g.queue) {
            match &f.action {
                LatencyModelInFlightAction::AmendQueueId {
                    si: s, qid, new_qty: Some(q), ..
                } if *s == si => {
                    if let Some(sell) = sells.iter_mut().find(|(k, _)| k == qid) {
                        sell.1 = *q;
                    }
                }
                LatencyModelInFlightAction::CancelQueueId { si: s, qid, .. } if *s == si => {
                    sells.retain(|(k, _)| k != qid);
                }
                _ => {}
            }
        }
        // `compensated_sum`, as the live engine's twin: vike-simulator keeps no naive float fold (`parity_fold_gate`).
        let out = vike_math::summation::compensated_sum(
            sells.iter().filter(|(q, _)| !in_group(*q)).map(|(_, size)| *size),
        );
        let live: Option<Vec<f64>> = group.as_ref().map(|(sets, _)| {
            let sum = |set: &Vec<u64>| {
                vike_math::summation::compensated_sum(
                    sells.iter().filter(|(q, _)| set.contains(q)).map(|(_, size)| *size),
                )
            };
            sets.iter().map(sum).collect()
        });
        let group = live.as_deref().zip(group.as_ref().map(|(_, own)| *own));
        vike_execution::risk::cash_oversell_refusal(st.pos.size, out, group, qty)
    }

    /// What each working order of `symbols[i]` commits, by live's one rule
    /// ([`vike_execution::risk::committed_quantity`]): the resting orders (the maker law's quotes after the
    /// others, the order this fold has always taken them in) and whatever is still in flight under
    /// the opt-in latency model, which has left `pending` and not yet filled, and then each resting
    /// combo's legs on it (`OrderBrokerSimulator::combo_committed`). The order an amend judges is left out by
    /// `skip_queue_id` ([`Self::gate_verdict`]).
    fn committed_order_quantities(
        &self,
        i: usize,
        skip_queue_id: Option<u64>,
    ) -> impl Iterator<Item = f64> {
        let st = &self.symbol[i];
        let position = st.pos.size;
        let in_flight = self.latency.iter().flat_map(move |g| g.in_flight_orders(i, skip_queue_id));
        let resting = move |quotes: bool| {
            st.pending
                .iter()
                .filter(move |o| skip_queue_id != Some(o.qid) && self.is_quote(i, o.qid) == quotes)
        };
        resting(false)
            .chain(resting(true))
            .chain(in_flight)
            .filter_map(move |o| vike_execution::risk::committed_quantity(o.size, o.side, position))
            .chain(self.combo_committed(i, position))
    }

    /// The pre-trade judge at market-order intake — ONE dispatch point over the two outcome
    /// shapes (deny-vs-clamp phases 1+2):
    ///
    /// * DEFAULT (`clamp_to_leverage == false`): **the literal live gate.** The order crosses
    ///   `vike_execution::RiskGate::check` — the same code path `Engine::gate_and_register`
    ///   runs live — over a [`RiskContext`] built from this engine's own state, mirroring the
    ///   live construction field for field: `position_size`/`mark_price` from the symbol state,
    ///   `equity` = [`Self::equity_now`] (the sim account is continuously marked to market, the
    ///   `equity_all` twin) under `RiskLimits::max_sizing_equity`, `margin_used` =
    ///   [`Self::margin_in_use_pending_aware`],
    ///   `closing_credit` = live's `2·|pos|·mark·mult·initial_margin` reversing credit, `multiplier` = the
    ///   symbol's real contract multiplier (unconditionally — live currently wires `1.0` when
    ///   the margin lane is unarmed, a known ctx-construction bug this side deliberately does
    ///   not reproduce; the gate CODE is shared, the ctx is each caller's to get right). A
    ///   refusal is whole (zero fill) and recorded in [`Self::dropped`] under the gate's OWN
    ///   reason string — a backtest denial IS the live denial. When a
    ///   [`StrategyEngineConfig::properties`] source is configured, the gate's four instrument-grid
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
    /// the replay books live in `run_ticks` locals, not on `OrderBrokerSimulator`. Arming the veto here
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
    /// and be refused in production. The market-making strategy files quote limits exclusively.
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
        // An order sent inside a release is judged AT its release, against the account as it is
        // then (`Self::release_held`), as the live core judges a held order: its terms wait.
        if self.releasing.is_some() {
            if size > 0.0 {
                self.held_terms = Some(SizingGateTerms { order_type, price, trigger });
            }
            return size;
        }
        // The mount's leverage cap refuses an OPENING order past it, in the live drain's words, FIRST: live
        // judges it in the drain, before the engine's own refusals below (`StrategyEngineConfig::max_mount_leverage`,
        // a portfolio mount's budget); an order-list leg was judged with its list (`judge_group`), a combo leg
        // with its combo (`judge_combo`, `leverage_judged`).
        if size > 0.0
            && !self.placing_a_list_leg()
            && !self.leverage_judged
            && self.leverage_drop(si, (side_sign, size, weight), price.or(trigger))
        {
            return 0.0;
        }
        // A spot (cash) symbol never sells more than it holds: the live engine's rule, before its gate.
        if let Some(why) = self.cash_refusal(si, side_sign, size, None) {
            let symbol_name = self.symbols[si].clone();
            self.dropped.push(OrderDrop { symbol: symbol_name, reason: why, size, weight });
            return 0.0;
        }
        // A converted symbol whose rate is stale opens nothing, gate armed or not (the live gate's
        // `quote-rate-stale`; `crates/vike-simulator/src/engine/sim_broker/conversion.rs`'s module doc).
        if size > 0.0
            && let Some(why) = self.quote_stale_refusal(si, side_sign, size)
        {
            let sym = self.symbols[si].clone();
            self.dropped.push(OrderDrop { symbol: sym, reason: why.into(), size, weight });
            return 0.0;
        }
        if self.clamp_to_leverage {
            return self.cap_to_leverage(si, side_sign, size);
        }
        if self.risk_gate.is_none() || size <= 0.0 {
            return size; // no gate armed / nothing to judge — byte-identical pass-through
        }
        let terms = SizingGateTerms { order_type, price, trigger };
        let verdict = self.gate_verdict(si, side_sign, size, terms, None);
        if verdict.ok {
            // The gate's normalized (lot-rounded) request is deliberately NOT substituted:
            // `apply_fill` re-snaps to the PIT grid at fill time, the sim's venue-side site.
            return size;
        }
        let symbol_name = self.symbols[si].clone();
        self.dropped.push(OrderDrop { symbol: symbol_name, reason: verdict.reason, size, weight });
        0.0
    }

    /// Whether the mount leverage cap refuses ONE order of `side_sign` · `size` (of sizing `weight`) on
    /// `symbols[si]` at its own `price` ([`Self::leverage_refusal`]), the refusal recorded in [`Self::dropped`];
    /// never for an order sent inside a release, judged at its release instead (`Self::release_held`).
    /// The reduce-only verbs ask it on the STATED size before the reduce-only clamp, as live's drain judges
    /// it before its engine clamps the order to the account's position (review #8); `gate_order` then
    /// judges the clamped size, which passes wherever the stated one did (a covered reduce, or a smaller
    /// reversal on the same row).
    pub(super) fn leverage_drop(
        &mut self,
        si: usize,
        (side_sign, size, weight): (i32, f64, f64),
        price: Option<f64>,
    ) -> bool {
        if self.releasing.is_some() {
            return false;
        }
        let leg =
            [MountLeverageLeg { side: side_sign, quantity: size, price, ..Default::default() }];
        let Some(why) = self.leverage_refusal(&[(si, &leg[..])]) else { return false };
        let sym = self.symbols[si].clone();
        self.dropped.push(OrderDrop { symbol: sym, reason: why, size, weight });
        true
    }

    /// What the current mount holds on `symbols[si]`, as [`Self::leverage_refusal`] reads its row: its own
    /// attributed row in a portfolio run, the account's position in a plain one (its one mount).
    pub(super) fn leverage_row_held(&self, si: usize) -> f64 {
        match self.mounts.get(self.mount) {
            Some(m) => m.rows[si].size,
            None => self.symbol[si].pos.size,
        }
    }

    /// Why the MOUNT leverage cap refuses ONE verb, or `None`: `order` its legs on each slot it trades
    /// (`(slot, legs)`: one for an order, an order list or an amend, one per leg for a COMBO, all of it or
    /// none), through the ONE judge live's drain asks ([`vike_execution::risk::judge_mount_leverage`]):
    /// judged ALONE over the current mount's OWN rows in cross-section order, each slot at its price and
    /// contract value (a portfolio run's mount its attributed rows of the slots it trades,
    /// `OrderBrokerSimulatorMount::rows`, so a sibling's holding counts for nothing, as live judges it; a plain
    /// run, its one mount, the account's positions), against the equity the gate spends against
    /// ([`Self::sizing_equity`] of its first slot under `RiskLimits::max_sizing_equity`). A refusal is counted
    /// for the mount's report. `None` at once with no cap.
    pub(super) fn leverage_refusal(
        &mut self,
        order: &[(usize, &[MountLeverageLeg])],
    ) -> Option<String> {
        let cap = match self.mounts.get(self.mount) {
            Some(m) => m.budget.and_then(|b| b.max_leverage),
            None => self.max_mount_leverage,
        }?;
        let mount = self.mounts.get(self.mount);
        let slots = self.cross_section().iter().filter(|&&i| mount.is_none_or(|m| m.hears[i]));
        let rows: Vec<MountLeverageRow<usize>> = slots
            .map(|&i| {
                let (size, average_price) = match mount {
                    Some(m) => (m.rows[i].size, m.rows[i].average_price),
                    None => (self.symbol[i].pos.size, self.symbol[i].pos.average_price),
                };
                let (price, value) = (Some(self.symbol[i].price), self.value_of_index(i));
                MountLeverageRow { key: i, size, average_price, price, value }
            })
            .collect();
        let ceiling = self.risk_gate.as_ref().and_then(|g| g.limits.max_sizing_equity);
        let first = order.first().map_or(0, |o| o.0);
        let equity = vike_execution::risk::capped_equity(ceiling, self.sizing_equity(first));
        let order: Vec<_> =
            order.iter().map(|(si, legs)| (si, self.value_of_index(*si), *legs)).collect();
        let why = vike_execution::risk::judge_mount_leverage(Some(cap), equity, &rows, &order);
        if why.is_some()
            && let Some(mount) = self.mounts.get_mut(self.mount)
        {
            mount.leverage_refusals += 1;
        }
        why
    }

    /// Why the pre-trade gate refuses an AMEND of a working order on `symbols[si]` to `side_sign` ·
    /// `size` at `terms` (the order as amended), or `None`: live's `Engine::gate_modify`,
    /// which judges every amend, up, down or a price alone, through `RiskGate::check_modify`. The
    /// order is named by `skip_queue_id` and left out of what the working orders commit, as live leaves
    /// the judged order client id out. A refused amend keeps its terms, recorded in [`Self::dropped`] by the caller,
    /// and the strategy hears nothing: live's refusal is the non-terminal `OrderModifyRejected`,
    /// which no `OrderEvent` carries. `None` at once with no gate armed (and so under
    /// `clamp_to_leverage`), so such a run is unchanged, but for an amend that would open on a
    /// converted symbol whose rate is stale, refused `quote-rate-stale` as [`Self::gate_order`] refuses it.
    pub(super) fn amend_refusal(
        &mut self,
        si: usize,
        side_sign: i32,
        size: f64,
        terms: SizingGateTerms,
        skip_queue_id: Option<u64>,
    ) -> Option<String> {
        if let Some(why) = self.quote_stale_refusal(si, side_sign, size) {
            return Some(why.into());
        }
        self.risk_gate.as_ref()?;
        let verdict = self.gate_verdict(si, side_sign, size, terms, skip_queue_id);
        (!verdict.ok).then_some(verdict.reason)
    }

    /// The live gate's verdict on `side_sign` · `size` at `terms` on `symbols[si]`, over a
    /// [`RiskContext`] built from this engine's state ([`Self::gate_order`]'s doc has every field):
    /// the ONE body a submit ([`Self::gate_order`], `RiskGate::check`) and an amend
    /// ([`Self::amend_refusal`], `RiskGate::check_modify`) are judged by, as live's
    /// `Engine::risk_context` is shared by its submit and its modify. An amend names its order
    /// (`skip_queue_id`), which the margin and exposure folds leave out: it is judged at its
    /// new terms, and counting its old ones too would refuse an amend at a ceiling it was admitted
    /// under. Nothing of it has executed for the gate to net (`0.0`): the simulator amends the
    /// REMAINING size, the paper book's `InPlaceRemaining`. The gate must be armed.
    fn gate_verdict(
        &mut self,
        si: usize,
        side_sign: i32,
        size: f64,
        terms: SizingGateTerms,
        skip_queue_id: Option<u64>,
    ) -> RiskVerdict {
        let amend = skip_queue_id.is_some();
        let SizingGateTerms { order_type, price, trigger } = terms;
        // ---- RiskContext, mirroring `Engine::gate_and_register` field for field ----
        let mark = self.symbol[si].price;
        let value = self.value_of_index(si);
        let initial_margin = {
            let limit = &self.risk_gate.as_ref().expect("checked Some").limits;
            limit.initial_margin_for(&self.symbols[si])
        };
        // The margin fields are computed ONLY when the buying-power lane is armed for this
        // symbol — the live construction's exact gating (they are read by no other check).
        let (equity, margin_used, closing_credit) = match initial_margin {
            Some(initial_margin_requirement) => {
                let pos = self.symbol[si].pos.size;
                let credit = if vike_model::is_reducing_direction(side_sign, pos) {
                    // direction-reversing order: margin the close frees + the LEAN re-open
                    // credit (single-requirement model: mm == im in the gate) — live verbatim
                    value.margin_credit(pos, mark, initial_margin_requirement)
                } else {
                    0.0
                };
                // This lane SPENDS against equity, so the operator's ceiling applies, as in live's
                // `Engine::sizing_equity`. A coin ledger's equity is valued in the quote
                // currency at the contract's own price, live's `coin_rate` (decision 0258): every
                // margin and ceiling of the gate is in dollars.
                let ceiling =
                    self.risk_gate.as_ref().expect("checked Some").limits.max_sizing_equity;
                let equity = vike_execution::risk::capped_equity(ceiling, self.sizing_equity(si));
                let used =
                    self.margin_in_use_pending_aware(initial_margin_requirement, skip_queue_id);
                (equity, used, credit)
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
        // Same shape as the live producer (`Engine::resolved_account_exposure_excluding`):
        // GROSS (`|size|`, never netting), the ORDER's own symbol's POSITION excluded because the
        // gate re-adds it projected, everything valued at the current mark and the symbol's own
        // multiplier. The sim account is one venue's one account by construction, so there is no
        // foreign-venue row to skip here — the live fold's venue skip has no counterpart. `compensated_sum`
        // in `symbols` order, matching `margin_in_use_pending_aware` and `equity_now` beside it.
        //
        // ⚠ **RESTING ORDERS COUNT, on EVERY symbol including the order's own** — the live
        // producer's second half, and the reason that one exists: without it N orders submitted
        // inside one fill window are each judged as though the others committed nothing, and the
        // ceiling is exceeded by an arbitrary multiple. A backtest that skipped this term would be
        // the PERMISSIVE side for exactly the strategy shape that trips it live (a maker resting
        // quotes on many symbols), which is the failure this whole function's doc is about.
        //
        // Each order contributes its own remainder through the rule the margin term above reads
        // (exposure is gross by construction — see `vike_model::RiskLimits::max_account_exposure`;
        // netting two opposed resting orders to zero is precisely the reading that axis refuses).
        let account_exposure_excl_order =
            if self.risk_gate.as_ref().expect("checked Some").limits.max_account_exposure.is_some()
            {
                compensated_sum(self.cross_section().iter().map(|&i| {
                    let (value, price) = (self.value_of_index(i), self.symbol[i].price);
                    let position = if i == si {
                        0.0
                    } else {
                        value.gross_notional(self.symbol[i].pos.size, price)
                    };
                    let orders = self.committed_order_quantities(i, skip_queue_id);
                    position + compensated_sum(orders.map(|qty| value.gross_by_unit(qty, price)))
                }))
            } else {
                0.0
            };
        let ctx = RiskContext {
            position_size: self.symbol[si].pos.size,
            mark_price: mark,
            trading_state: TradingState::Active,
            now_ms: self.now,
            equity,
            margin_used,
            closing_credit,
            contract_value: value,
            // A converted symbol's value carries its rate already; the gate converts the VENUE's
            // numbers (`min_notional`) by it on their side. Every other symbol is in the ledger's
            // own currency, at 1.0.
            quote_rate: self.quote_rate_of(si).map_or(1.0, vike_model::AccountPerQuote::get),
            quote_stale: self.quote_rate_stale(si),
            account_exposure_excl_order,
        };
        let req = OrderRequest {
            order_client_id: String::new(), // sim orders have no order client id; the gate never reads it
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
        // instead of dropped at fill ("min_qty"), and a below-min REVERSAL that apply_fill would
        // split is DENIED whole — agreeing with live, which denies. Pure-leverage runs without a
        // properties source are unaffected (no grid to refresh).
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
            let limit = &mut gate.limits;
            limit.tick_size = g.tick_size;
            limit.lot_size = g.lot_size;
            limit.min_qty = g.min_qty;
            limit.min_notional = g.min_notional;
        }
        if amend { gate.check_modify(&req, &ctx, 0.0) } else { gate.check(&req, &ctx) }
    }

    /// Shrink an opening/adding market order so projected TOTAL account notional
    /// <= leverage*equity (account-level; pending-aware; reducing never shrunk). The frozen
    /// pre-phase-1 TRUNCATE formula — since deny-vs-clamp phase 2 reached ONLY via
    /// [`Self::gate_order`] with `clamp_to_leverage == true` (the escape hatch); the
    /// default path now judges through the live `RiskGate` and never consults this.
    ///
    /// Every notional is the symbol's own (`ContractValue::gross_notional`, the linear product unless
    /// the run listed another value), the equity `Self::sizing_equity` (a coin ledger's coin in dollars
    /// at the order's contract price) and the room in units the one sizing law's
    /// (`ContractValue::units_worth`) on an inverse symbol, so a coin ledger is capped in dollars.
    fn cap_to_leverage(&self, si: usize, side_sign: i32, size: f64) -> f64 {
        let Some(leverage) = self.leverage else {
            return size;
        };
        let eq = self.sizing_equity(si);
        if eq <= 0.0 {
            return 0.0;
        }
        if vike_model::is_reducing_direction(side_sign, self.symbol[si].pos.size) {
            return size; // reducing/closing: never capped
        }
        let max_notional = leverage * eq;
        let current = compensated_sum(self.cross_section().iter().map(|&i| {
            self.value_of_index(i).gross_notional(self.symbol[i].pos.size, self.symbol[i].price)
        }));
        let pending = compensated_sum(self.cross_section().iter().map(|&i| {
            let mut signed = compensated_sum(
                self.symbol[i]
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
            self.value_of_index(i).gross_notional(signed, self.symbol[i].price)
        }));
        let room_notional = max_notional - current - pending;
        if room_notional <= 0.0 {
            return 0.0;
        }
        let value = self.value_of_index(si);
        let room = if value.kind == NotionalKind::Inverse {
            value.units_worth(room_notional, self.symbol[si].price, 1.0).unwrap_or(0.0)
        } else {
            // `p·u` exactly as this cap always divided by it (no `units_worth` guard: a linear unit
            // worth nothing or less is divided by as it was)
            room_notional / value.gross_by_unit(1.0, self.symbol[si].price)
        };
        if size <= room { size } else { room }
    }
}
