//! Resolver-priced valuation: equity, the sizing ceiling, own PnL, margin in use, exposure.

use super::{ExecutionClient, ExecutionEngine};

#[cfg(doc)]
use crate::Account;

impl<C: ExecutionClient> ExecutionEngine<C> {
    /// Mode-aware equity with every open position priced through the resolver (mark ->
    /// side-aware quote -> last -> bar-close -> Missing). Cold publish path only
    /// (`CoreSnapshot::build`); NOT the per-message fold. A `Missing` resolution contributes 0.0
    /// unrealized (`equity_all`'s silent zero for an unmarked position) and is counted in
    /// `missing` for the GUI badge. Pure `&self`: warn-once tracking (`PriceBoard::note`) is the
    /// cold sampler's.
    ///
    /// The fold law and the mode expression are `Account::equity_all`'s, so an all-`Missing` run
    /// is bit-identical to `equity_all`.
    pub fn resolve_equity(
        &self,
        seed: f64,
        cfg: &crate::price_board::PriceCfg,
    ) -> crate::price_board::ResolvedEquity {
        use crate::price_board::{ResolvedEquity, ResolvedPosition};
        let mut per_position = Vec::with_capacity(self.account.positions.len());
        let mut missing = 0u32;
        for ((venue, symbol, position_side), entry) in self.account.positions.iter() {
            let (unrealized, mark_source) =
                self.resolve_position_unrealized(venue, symbol, *position_side, entry, cfg);
            if mark_source.is_none() {
                missing += 1;
            }
            per_position.push(ResolvedPosition { unrealized, mark_source });
        }
        // SAME fold law as Account::equity_all: Neumaier (py_sum), in insertion order.
        let unrealized_total = vike_model::py_sum(per_position.iter().map(|p| p.unrealized));
        let equity = self.mode_equity(seed, unrealized_total);
        ResolvedEquity { equity, unrealized_total, missing, per_position }
    }

    /// Scalar twin of [`Self::resolve_equity`] for the DECISION paths (the one-price law): the
    /// liquidation watchdog and the portfolio-snap journal read equity HERE, so an
    /// auto-liquidation acts on exactly the number the snapshot/sampler display. Same resolver
    /// chain, per-position law (`Self::resolve_position_unrealized`), `py_sum` fold in
    /// `positions` insertion order (bit-identical to `resolve_equity`'s) and mode expression
    /// (`Self::mode_equity`), without the display Vec. Cold/per-order/per-bar cadence only:
    /// no logging, no allocation. With an empty board it is bit-identical to
    /// `Account::equity_all(seed)`.
    ///
    /// ⚠ **THIS IS THE UNCAPPED FIGURE, deliberately — see [`Self::sizing_equity`].** Its callers
    /// must judge the account as it really is: `vike_core`'s `sweep_margin_call_engine`, where a
    /// smaller number LIQUIDATES, and the portfolio-snap journal, a record. Strategy `ctx.equity`
    /// and the armed margin gate read `sizing_equity`.
    pub fn resolved_equity(&self, seed: f64, cfg: &crate::price_board::PriceCfg) -> f64 {
        let unrealized_total =
            vike_model::py_sum(self.account.positions.iter().map(|((v, s, side), entry)| {
                self.resolve_position_unrealized(v, s, *side, entry, cfg).0
            }));
        self.mode_equity(seed, unrealized_total)
    }

    /// **THE one answer to "what equity may this engine size and admit against"** —
    /// [`Self::resolved_equity`] capped by [`crate::RiskLimits::max_sizing_equity`]. With no
    /// ceiling armed (`None`, the default: no `policy.max_sizing_equity` row) it is BIT-IDENTICAL
    /// to `resolved_equity`.
    ///
    /// # Why a second resolver rather than a cap inside the first
    ///
    /// The cap is not uniformly safe. Under [`crate::BalanceMode::Authoritative`] resolved equity
    /// is `venue wallet + unrealized`, and the wallet is the venue's number for the WHOLE account
    /// the credentials open, so a third party's deposit or withdrawal on a shared account moves it.
    /// Capping it is:
    ///
    /// * **conservative** for anything that SPENDS against equity — a percent-of-equity sizer buys
    ///   fewer units, the pre-trade margin lane admits less — and
    /// * **destructive** for anything that judges SOLVENCY against it: the margin-call sweep reads
    ///   equity as the collateral backing open positions, so a capped figure makes a healthy
    ///   account look under-margined and it LIQUIDATES.
    ///
    /// Two named resolvers make each call site state which side it is on.
    ///
    /// # The consumers, and which side each is on
    ///
    /// **ACTING (they read this method, capped):**
    ///
    /// * the pre-trade gate's margin lane — `risk_ctx`'s `equity` field, which
    ///   `crate::RiskGate`'s `check_inner` buying-power comparison judges against;
    /// * the COMBO gate's per-leg equity — `vike_core`'s `apply/combo.rs`;
    /// * every strategy context — `vike_core`'s `strategy_drive.rs` builds `LiveBroker::equity`,
    ///   which `LiveBroker::order_target_percent` multiplies through
    ///   `vike_model::units_from_percent` and `vike-script` hands Rhai strategies as
    ///   `ctx.equity()`;
    /// * the fold's per-fill `AppliedFill::equity_after` (`Strategy::on_fill`'s `ctx.equity`),
    ///   computed from `Account::equity_all`, so a grep for `resolved_equity` does NOT find it;
    ///   it is capped through [`Self::cap_sizing_equity`]. ⚠ No count is written here on purpose:
    ///   `git grep -n 'equity:' -- crates/vike-core/src/runtime` plus this site is the derivation.
    ///
    /// **REPORT-ONLY or SOLVENCY (they read [`Self::resolved_equity`], uncapped, deliberately):**
    ///
    /// * the margin-call sweep — it must see the real collateral;
    /// * the equity SAMPLER and the portfolio-snap journal (`vike_core`'s `timers.rs`),
    ///   `CoreSnapshot`/`Portfolio` display and `vike-tradehub`'s book summary — observations of
    ///   the account, which must not read the operator's ceiling back as the account;
    /// * the DRAWDOWN latch reads neither: it is on `capital_base + resolved_own_pnl`
    ///   (`vike_core`'s `sweep_drawdown_latch` argues why), untouched by this ceiling.
    ///
    /// Cold/per-order/per-bar cadence only, like the method it wraps.
    pub fn sizing_equity(&self, seed: f64, cfg: &crate::price_board::PriceCfg) -> f64 {
        self.cap_sizing_equity(self.resolved_equity(seed, cfg))
    }

    /// Apply [`crate::RiskLimits::max_sizing_equity`] to an already-resolved equity — the ONE
    /// place the ceiling is applied, so [`Self::sizing_equity`] and the `equity_all`-derived
    /// `AppliedFill::equity_after` cannot disagree. `None` returns the figure verbatim
    /// (bit-identical, not `min`'d against an infinity).
    #[inline]
    #[must_use]
    pub fn cap_sizing_equity(&self, equity: f64) -> f64 {
        // ⚠ **A comparison, deliberately NOT `f64::min`.** `min` returns the OTHER operand for a
        // NaN, so a poisoned equity (`Account::equity_all` goes NaN off one NaN mark slot) would
        // come out as the operator's ceiling — a plausible number the gate ADMITS against, where
        // the NaN would have failed every comparison and denied. A cap may only LOWER a real
        // figure; `NaN > cap` is false, so the NaN passes through to every downstream guard.
        match self.gate.limits.max_sizing_equity {
            Some(cap) if equity > cap => cap,
            _ => equity,
        }
    }

    /// **This engine's OWN profit and loss** — resolver-priced, and the one equity-shaped scalar
    /// with NO account balance level in it. The sibling of [`Self::resolved_equity`] (same
    /// resolver chain, per-position law, `py_sum` fold order and cold-path cadence), differing
    /// only in what it is denominated against.
    ///
    /// ```text
    /// resolved_equity  = seed + balance + realized − ... + unrealized   (Delta)
    ///                  = balance + unrealized                           (Authoritative)
    /// resolved_own_pnl = realized − fees_paid + funding_paid + unrealized  (BOTH modes)
    /// ```
    ///
    /// ⚠ **`balance` is the term that makes equity unusable as a risk measure on a live venue.**
    /// Under [`crate::BalanceMode::Authoritative`] it is the venue's wallet for the WHOLE ACCOUNT
    /// the credentials open ([`crate::Account::apply_account_state`] sets it ABSOLUTELY), so a
    /// third party's deposit or withdrawal on a SHARED account moves `resolved_equity` by the full
    /// amount while this daemon did nothing.
    ///
    /// Every term here is written ONLY by this engine's own activity — `Account::apply_fill` /
    /// `apply_liquidation` (via `Account::fold`) write `realized_pnl` and `fees_paid`,
    /// `apply_funding` writes `funding_paid`, and the positions are this engine's;
    /// `apply_account_state` (`balance` / `balances_by_asset` / `balance_mode` /
    /// `realized_pnl_at_balance_sync`) touches NONE of them. So this scalar is mode-BLIND: it does
    /// not step when a venue frame flips the account from `Delta` to `Authoritative` mid-session.
    ///
    /// SIGNS: `fees_paid` is a signed cost (`> 0` paid, `< 0` maker rebate) and is SUBTRACTED;
    /// `funding_paid` is a signed cashflow (`> 0` received, `< 0` paid — see
    /// [`crate::Account::apply_funding`]) and is ADDED. On a `Delta` account this makes
    /// `seed + resolved_own_pnl` equal `resolved_equity` exactly, because `balance` there is
    /// precisely `−fees_paid + funding_paid` accumulated from the same events.
    ///
    /// ⚠ ONE DECLARED RESIDUAL: a forced-close fee (`Account::apply_liquidation`'s
    /// `balance -= ev.fee`) is netted into `balance` but NOT accrued into `fees_paid`, so it is
    /// invisible here while it moves a `Delta` account's `resolved_equity` — the whole `Delta`-mode
    /// difference, bounded by the liquidation fee and beside a realized loss that IS counted.
    /// Closing it means widening `fees_paid`'s meaning, which the GUI and every report read.
    pub fn resolved_own_pnl(&self, cfg: &crate::price_board::PriceCfg) -> f64 {
        let unrealized_total =
            vike_model::py_sum(self.account.positions.iter().map(|((v, s, side), entry)| {
                self.resolve_position_unrealized(v, s, *side, entry, cfg).0
            }));
        self.own_pnl(unrealized_total)
    }

    /// Resolver-priced unrealized PnL for ONE open position — the per-position law shared by
    /// [`Self::resolve_equity`] (display/publish) and [`Self::resolved_equity`] (decision
    /// paths). `Missing` -> `(0.0, None)`: the same silent-zero `equity_all` gives an unmarked
    /// position, counted by the callers that surface it.
    #[inline]
    fn resolve_position_unrealized(
        &self,
        venue: &str,
        symbol: &str,
        position_side: vike_model::events::PositionSide,
        entry: &crate::account::PositionEntry,
        cfg: &crate::price_board::PriceCfg,
    ) -> (f64, Option<crate::price_board::PriceSource>) {
        // Long/short for the side-aware quote comes from the SIGN of the folded size (as in
        // `margin_call.rs`); `position_side` is only the bucket label (`Both`/`Long`/`Short`).
        let is_long = entry.size >= 0.0;
        match self.price_board.resolve(venue, symbol, is_long, self.now_ms, cfg) {
            // Stale (a last-known price, returned ONLY under `PriceCfg::stale_fallback`) is valued
            // exactly like Priced — a stale mark beats a silent zero — and its `source` still rides
            // through, so the position is NOT counted as missing.
            crate::price_board::Resolution::Priced { px, source, .. }
            | crate::price_board::Resolution::Stale { px, source, .. } => (
                self.account.unrealized_at(symbol, position_side, entry.size, entry.avg_px, px),
                Some(source),
            ),
            crate::price_board::Resolution::Missing => (0.0, None),
        }
    }

    /// Resolver-priced valuation price for ONE open position — the scalar the margin lane and
    /// the margin-call sweep share with `Self::resolve_position_unrealized`: the SAME chain
    /// (mark -> side-aware quote -> last -> bar-close) with the SAME long/short convention (the
    /// SIGN of the folded size picks Bid/Ask — conservative liquidation-side valuation).
    /// `None` = `Missing`, which every caller treats as unpriceable. Cold / per-order path only.
    pub fn resolved_position_price(
        &self,
        venue: &str,
        symbol: &str,
        size: f64,
        cfg: &crate::price_board::PriceCfg,
    ) -> Option<f64> {
        match self.price_board.resolve(venue, symbol, size >= 0.0, self.now_ms, cfg) {
            crate::price_board::Resolution::Priced { px, .. }
            | crate::price_board::Resolution::Stale { px, .. } => Some(px),
            crate::price_board::Resolution::Missing => None,
        }
    }

    /// Resolver-priced margin-in-use — [`Account::margin_in_use_priced`] with the resolver as the
    /// price input, so the margin side of every ratio/free-BP/liquidation comparison shares
    /// [`Self::resolved_equity`]'s price basis (the one-price law). Callers pass the rate closure.
    /// A `Missing` resolution excludes the position. Cold / per-order path only.
    pub fn resolved_margin_in_use_by(
        &self,
        cfg: &crate::price_board::PriceCfg,
        rate_of: impl Fn(&crate::account::PositionKey, &crate::account::PositionEntry) -> Option<f64>,
    ) -> f64 {
        self.account.margin_in_use_priced(
            |(v, s, _side), p| self.resolved_position_price(v, s, p.size, cfg),
            rate_of,
        )
    }

    /// **What this ACCOUNT already has on that the gate is not about to re-project** — the producer
    /// of [`crate::RiskContext::account_exposure_excl_order`], so all that
    /// [`crate::RiskLimits::max_account_exposure`] knows about the book it caps. TWO halves:
    ///
    /// 1. **OPEN POSITIONS**, gross, every symbol of this account except `exclude_symbol` — the
    ///    order's own, which the gate re-adds PROJECTED (see the ctx field's doc).
    /// 2. **LIVE, UN-FILLED ORDERS**, gross over each one's REMAINING quantity, every order of this
    ///    engine except `judging` — the one being judged, which the gate projects itself.
    ///
    /// ⚠ **Without the second half the ceiling is bypassable by an arbitrary multiple** — the hole
    /// `Self::live_order_margin` closes for buying power: a maker resting quotes on ten symbols
    /// would see the same pre-order exposure ten times and pass every check until the fills landed.
    ///
    /// The position fold is `Account::gross_notional_priced` fed [`Self::resolved_position_price`]
    /// (the one price basis of the equity, margin and per-symbol exposure lanes). GROSS: a
    /// hedge-mode LONG/SHORT pair sums both legs, because exposure is what the account holds, not
    /// what it nets to.
    ///
    /// **Four skips, each a classification rather than an omission:**
    ///
    /// * `exclude_symbol`'s POSITION — re-added PROJECTED by the gate; a `None` from the price
    ///   closure (the "caller declined to price" arm `gross_notional_priced` documents).
    /// * **a FOREIGN-VENUE position row** (it reached the map through a reconcile report;
    ///   `crates/vike-exec/tests/risk/risk_lane_pricing.rs`'s
    ///   `max_total_exposure_is_scoped_to_one_venue_and_one_symbol` shows the shape): positions
    ///   that do NOT share a wallet, so summing them would refuse orders on collateral the venue
    ///   never sees.
    /// * **the order being JUDGED** (`judging`, by client-order-id). On a submit it is not in the
    ///   registry; on an amend ([`Self::modify_order`] judges the PROJECTED request under the
    ///   resting order's coid) it is, and counting it as well would refuse an amend at a ceiling
    ///   the same order was admitted under — the double count `still_executable` was written for.
    /// * **a COVERED REDUCE** (`vike_model::is_covered_reduce`, the predicate the floor and margin
    ///   bypasses read): a resting order that shrinks a position adds no exposure.
    ///
    /// ⚠ **The judged SYMBOL's resting orders ARE counted while its POSITION is not**: the position
    /// is re-added projected and resting orders are projected by nothing, so this is the only way
    /// to neither double-count the one nor drop the other.
    ///
    /// ⚠ **An UNPRICEABLE position or order contributes 0.0 — the ANTI-conservative direction**
    /// (it understates the account). Kept consistent with the margin fold deliberately, so the two
    /// cannot answer differently about one book; a DECLARED residual, not a safe default.
    /// [`Self::missing_marks`] names the positions that lack a price.
    ///
    /// Cold path: per-order, called ONLY when the ceiling is armed — no logging, no allocation.
    pub fn resolved_account_exposure_excluding(
        &self,
        exclude_symbol: &str,
        judging: &str,
        cfg: &crate::price_board::PriceCfg,
    ) -> f64 {
        let positions = self.account.gross_notional_priced(|(v, s, _side), p| {
            if s.as_str() == exclude_symbol || v.as_str() != self.venue.as_str() {
                return None;
            }
            self.resolved_position_price(v, s, p.size, cfg)
        });
        positions + self.live_order_notional(judging, cfg)
    }

    /// Σ gross notional of this engine's OWN **live, un-filled** orders — the in-flight half of
    /// [`Self::resolved_account_exposure_excluding`], and the exposure twin of
    /// [`Self::live_order_margin`]: the same skips (the judged coid, a non-live status, a
    /// non-finite or non-positive remainder, a covered reduce, an unpriceable order) without the
    /// initial-margin rate, each survivor contributing `remaining × price × multiplier`
    /// (`vike_model::gross_notional`). Keep the two walks alike: a divergence in WHICH orders they
    /// see would make the margin and exposure lanes disagree about one book.
    fn live_order_notional(&self, judging: &str, cfg: &crate::price_board::PriceCfg) -> f64 {
        let mut open = 0.0;
        for (coid, mo) in self.registry.iter() {
            if coid.as_str() == judging || !mo.status.is_live() {
                continue;
            }
            let remaining = mo.request.qty - mo.filled_qty;
            // Non-finite or non-positive remainder commits nothing (spelled so NaN is visible).
            if !remaining.is_finite() || remaining <= 0.0 {
                continue;
            }
            let sym = mo.request.symbol.as_str();
            if vike_model::is_covered_reduce(
                mo.request.reduce_only,
                mo.request.side,
                self.gate_position_size(sym),
                remaining,
            ) {
                continue;
            }
            let signed = mo.request.side as f64 * remaining;
            let Some(px) = self.resolved_position_price(&mo.request.venue, sym, signed, cfg) else {
                continue;
            };
            open += vike_model::gross_notional(remaining, px, self.account.multiplier_of(sym));
        }
        open
    }

    /// Σ initial margin of this engine's OWN **live, un-filled** order exposure — the commitment
    /// term of `RiskContext::margin_used`. ⚠ Counting open POSITIONS only overstates free buying
    /// power by every order in flight: the second of two orders submitted before the first fills
    /// is judged as though the first committed nothing. The backtest counts its pending orders
    /// (`SimBroker::margin_in_use_pending_aware`); this mirrors `RiskGate::check_combo`'s
    /// `committed_margin` rather than adding a second commitment model.
    ///
    /// Every term matches the POSITION fold in `Account::margin_in_use_priced`
    /// (`|qty| * px * multiplier * im`), through the SAME `resolved_position_price` resolver and
    /// the SAME per-symbol `im_for` fallback, so the two halves of `margin_used` cannot drift.
    ///
    /// ⚠ Skips reduce-shaped orders via `vike_model::is_covered_reduce` (the floor bypass's
    /// predicate): an order that shrinks a position commits no NEW margin, and charging it would
    /// deny flattens during a drawdown. Unpriceable orders contribute 0, like the position fold.
    ///
    /// ⚠ `judging` is the client-order-id of the order being GATED, and excluding it is
    /// load-bearing: on an AMEND it IS in the registry, and charging its existing commitment
    /// while judging its new size double-counts it (`partial_fill_amend_accounting.rs`'s
    /// `the_buying_power_lane_charges_only_what_can_still_execute` pins it). Inert on a SUBMIT.
    ///
    /// Cold path: per-order, off the `p99 < 10µs` per-message fold.
    pub(super) fn live_order_margin(
        &self,
        cfg: &crate::price_board::PriceCfg,
        im_req: f64,
        judging: &str,
    ) -> f64 {
        let mut used = 0.0;
        for (coid, mo) in self.registry.iter() {
            if coid.as_str() == judging {
                continue;
            }
            if !mo.status.is_live() {
                continue;
            }
            let remaining = mo.request.qty - mo.filled_qty;
            // Non-finite or non-positive remainder commits nothing (spelled so NaN is visible).
            if !remaining.is_finite() || remaining <= 0.0 {
                continue;
            }
            let sym = mo.request.symbol.as_str();
            if vike_model::is_covered_reduce(
                mo.request.reduce_only,
                mo.request.side,
                self.gate_position_size(sym),
                remaining,
            ) {
                continue;
            }
            let signed = mo.request.side as f64 * remaining;
            let Some(px) = self.resolved_position_price(&mo.request.venue, sym, signed, cfg) else {
                continue;
            };
            let im = self.gate.limits.im_for(sym).unwrap_or(im_req);
            used += remaining * px * self.account.multiplier_of(sym) * im;
        }
        used
    }

    /// `Account::equity_all`'s mode-aware seed expression, verbatim:
    /// delta = seed + balance + realized + unrealized; authoritative = balance + unrealized.
    #[inline]
    fn mode_equity(&self, seed: f64, unrealized_total: f64) -> f64 {
        if self.account.balance_mode == crate::BalanceMode::Authoritative {
            self.account.balance + unrealized_total
        } else {
            seed + self.account.balance + self.account.realized_pnl + unrealized_total
        }
    }

    /// The MODE-BLIND own-PnL expression [`Self::resolved_own_pnl`] documents (no term in it is
    /// written by an authoritative balance sync). Beside [`Self::mode_equity`] so the two are read
    /// together: the omitted `seed + balance` is the account LEVEL. The term order is fixed so
    /// `vike_core`'s `Portfolio::pnl_total` (the same four published `VenueBlock` fields) is
    /// bit-identical.
    #[inline]
    fn own_pnl(&self, unrealized_total: f64) -> f64 {
        self.account.realized_pnl - self.account.fees_paid
            + self.account.funding_paid
            + unrealized_total
    }
}
