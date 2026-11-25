//! Resolver-priced valuation: equity, the sizing ceiling, own PnL, margin in use, exposure.

use super::{Engine, ExecutionClient};

#[cfg(doc)]
use crate::Account;

impl<C: ExecutionClient> Engine<C> {
    /// Mode-aware equity with every open position priced through the resolver (mark ->
    /// side-aware quote -> last -> bar-close -> Missing). Cold publish path only
    /// (`vike_core::snapshot::build`); NOT the per-message fold. A `Missing` resolution contributes
    /// 0.0 unrealized (`equity_all`'s silent zero for an unmarked position) and is counted in
    /// `missing` for the GUI badge and `trade health`, when the position is OPEN: a FLAT row (size 0,
    /// what a closed position leaves behind) has nothing to value, so it is not counted, as in
    /// [`Self::missing_marks`]; its `per_position` entry, the unrealized fold and the equity are
    /// the same either way. Pure `&self`: warn-once tracking (`PriceBoard::note`) is the cold
    /// sampler's.
    ///
    /// The fold law and the mode expression are `Account::equity_all`'s, so an all-`Missing` run
    /// is bit-identical to `equity_all`.
    pub fn resolve_equity(
        &self,
        seed: f64,
        config: &crate::price_board::PriceBoardConfig,
    ) -> crate::price_board::PriceBoardEquity {
        use crate::price_board::{PriceBoardEquity, PriceBoardPosition};
        let mut per_position = Vec::with_capacity(self.account.positions.len());
        let mut missing = 0u32;
        for ((venue, symbol, position_side), entry) in self.account.positions.iter() {
            let (unrealized, mark_source) =
                self.resolve_position_unrealized(venue, symbol, *position_side, entry, config);
            if mark_source.is_none() && entry.size != 0.0 {
                missing += 1;
            }
            per_position.push(PriceBoardPosition { unrealized, mark_source });
        }
        // SAME fold law as Account::equity_all: Neumaier (compensated_sum), in insertion order.
        let unrealized_total =
            vike_math::summation::compensated_sum(per_position.iter().map(|p| p.unrealized));
        let equity = self.mode_equity(seed, unrealized_total, config);
        PriceBoardEquity { equity, unrealized_total, missing, per_position }
    }

    /// **The factor that turns a COIN LEDGER's money into the account's currency** (decision 0258, 0183's one
    /// factor `r`): the account-currency value of ONE settle coin, which is the mounted inverse contract's own
    /// price. `None` for every engine that is not a coin ledger (one `Option` test), so its readers keep the
    /// figure as it is. For one: the resolver's price of the mounted contract, else the account's last mark of
    /// it (a restore brings that back), else `0.0`: an unpriced coin counts NOTHING, so the margin lane denies
    /// an opening (fail closed) and the margin call finds no priced position to act on. Nothing hashed reads
    /// it (the coin books come from fill prices), so no rate is journaled. Cold: per publish, order and sweep.
    #[inline]
    pub fn coin_rate(&self, config: &crate::price_board::PriceBoardConfig) -> Option<f64> {
        let key = self.account.coin_key()?;
        let px = self
            .resolved_position_price(&key.0, &key.1, 1.0, config)
            .or_else(|| self.account.mark_of_key(&key));
        Some(px.filter(|p| p.is_finite() && *p > 0.0).unwrap_or(0.0))
    }

    /// **The one product that turns this engine's booked money into the account's currency** (decision 0258): a COIN
    /// LEDGER's amount times [`Self::coin_rate`], any other engine's amount as it is. The rate is read ONCE, here, so a
    /// caller valuing several fields of one engine (the publish, the sampler) pays one board read, not one per field.
    #[inline]
    pub fn account_money(
        &self,
        config: &crate::price_board::PriceBoardConfig,
    ) -> impl Fn(f64) -> f64 + Copy + use<C> {
        let coin = self.coin_rate(config);
        move |amount| coin.map_or(amount, |r| amount * r)
    }

    /// Scalar twin of [`Self::resolve_equity`] for the DECISION paths (the one-price law): the
    /// liquidation watchdog and the portfolio-snap journal read equity HERE, so an
    /// auto-liquidation acts on exactly the number the snapshot/sampler display. Same resolver
    /// chain, per-position law (`Self::resolve_position_unrealized`), `compensated_sum` fold in
    /// `positions` insertion order (bit-identical to `resolve_equity`'s) and mode expression
    /// (`Self::mode_equity`), without the display Vec. Cold/per-order/per-bar cadence only:
    /// no logging, no allocation. With an empty board it is bit-identical to
    /// `Account::equity_all(seed)`.
    ///
    /// ⚠ **THIS IS THE UNCAPPED FIGURE, deliberately — see [`Self::sizing_equity`].** Its callers
    /// must judge the account as it really is: `vike_core`'s `sweep_margin_call_engine`, where a
    /// smaller number LIQUIDATES, and the portfolio-snap journal, a record. Strategy `ctx.equity`
    /// and the armed margin gate read `sizing_equity`.
    pub fn resolved_equity(&self, seed: f64, config: &crate::price_board::PriceBoardConfig) -> f64 {
        let unrealized_total = vike_math::summation::compensated_sum(
            self.account.positions.iter().map(|((v, s, side), entry)| {
                self.resolve_position_unrealized(v, s, *side, entry, config).0
            }),
        );
        self.mode_equity(seed, unrealized_total, config)
    }

    /// **THE one answer to "what equity may this engine size and admit against"** —
    /// [`Self::resolved_equity`] capped by [`vike_model::RiskLimits::max_sizing_equity`]. With no
    /// ceiling armed (`None`, the default: no `policy.max_sizing_equity` row) it is BIT-IDENTICAL
    /// to `resolved_equity`, except on a COIN LEDGER that has adopted its wallet: that one is first capped at
    /// its seed held in coin plus its own PnL ([`Self::cap_at_coin_seed`]), and the ceiling then applies to
    /// what is left, so the lower of the two wins.
    ///
    /// # Why a second resolver rather than a cap inside the first
    ///
    /// The cap is not uniformly safe. Under [`crate::BalanceMode::Authoritative`] resolved equity
    /// is the venue wallet plus PnL, and the wallet is the venue's number for the WHOLE account
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
    /// * the pre-trade gate's margin lane — `risk_context`'s `equity` field, which
    ///   `crate::RiskGate`'s `check_inner` buying-power comparison judges against;
    /// * the COMBO gate's per-leg equity — `vike_core`'s `apply/combo.rs`;
    /// * the published `free_buying_power` — `vike_core`'s `snapshot::build`, through [`Self::cap_at_coin_seed`] and
    ///   [`Self::cap_sizing_equity`] on the equity it already resolved, so the number shown is the one the gate
    ///   spends against;
    /// * every strategy context — `vike_core`'s `strategy_drive.rs` builds `LiveBroker::equity`,
    ///   which `LiveBroker::order_target_percent` multiplies through
    ///   `vike_model::ContractValue::units_worth` and `vike-rhai` hands Rhai strategies as
    ///   `ctx.equity()`;
    /// * the fold's per-fill `AppliedFill::equity_after` (`Strategy::on_fill`'s `ctx.equity`),
    ///   [`Self::resolved_equity`] since the owner's ruling of 2026-11-15 (it was `Account::equity_all`, the
    ///   account slot's mid), capped through the same two (`Engine::equity_after_fill`, which is this method at
    ///   the engine's own seed and price config). ⚠ No count is written here on purpose:
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
    pub fn sizing_equity(&self, seed: f64, config: &crate::price_board::PriceBoardConfig) -> f64 {
        let equity = self.resolved_equity(seed, config);
        self.cap_sizing_equity(self.cap_at_coin_seed(equity, seed, self.coin_rate(config)))
    }

    /// **A COIN LEDGER sizes from its starting capital, never from the venue's wallet** (decision 0258, the
    /// owner's ruling of 2026-11-15 on its second question). Once the wallet is adopted (`Authoritative`) the
    /// equity is the whole coin wallet in dollars (the CI box's deribit demo: 100 BTC), so it is capped at the seed
    /// held in coin ([`Self::coin_seed`] at `rate`; `seed` itself until the core's first priced risk pass converts
    /// it) plus this engine's own PnL: `min(equity, seed + own PnL)`, the figure a `Delta` coin ledger already
    /// reads. A wallet below that keeps `equity`. `rate` is `None` for an engine that is no coin ledger, which,
    /// like a `Delta` coin ledger, gets `equity` verbatim. Applied before
    /// [`vike_model::RiskLimits::max_sizing_equity`] at every acting site ([`Self::sizing_equity`]'s list).
    #[inline]
    #[must_use]
    pub fn cap_at_coin_seed(&self, equity: f64, seed: f64, rate: Option<f64>) -> f64 {
        let Some(rate) = rate else { return equity };
        let a = &self.account;
        if a.balance_mode != crate::BalanceMode::Authoritative {
            return equity;
        }
        let base = self.coin_seed.map_or(seed, |coin| coin * rate);
        // `equity` less `seed + own PnL`: the coin the wallet holds beyond this engine's own realized, fees
        // and funding (the venue's money, not this engine's), valued at `rate`, less the seed.
        let own_coin = a.realized_pnl - a.fees_paid + a.funding_paid;
        let over = (a.balance + a.realized_since_balance_sync() - own_coin) * rate - base;
        if over > 0.0 { equity - over } else { equity }
    }

    /// The CAPPED per-fill `AppliedFill::equity_after` (`Strategy::on_fill`'s `ctx.equity`): [`Self::sizing_equity`]
    /// at the engine's own seed and price config, so it is RESOLVER-priced (owner, 2026-11-15: a long at the bid, a
    /// short at the ask, never the account slot's mid) and capped by the coin seed and the ceiling like every other
    /// acting site.
    #[inline]
    pub(super) fn equity_after_fill(&self) -> f64 {
        self.sizing_equity(self.equity_seed, &self.price_config)
    }

    /// **Hold `seed` in this COIN LEDGER's settle coin and say what it is worth now** (decision 0258): the first
    /// call that can price the coin ([`Self::coin_rate`]) converts it into [`Self::coin_seed`], and every call
    /// values that coin at today's rate. `None` (nothing converted) for an engine that is no coin ledger and for
    /// an unpriced coin. Cold: `vike_core`'s risk passes (`hold_coin_seeds`, latch or not, and the latch's
    /// `revalue_coin_seeds`).
    pub fn hold_seed_in_coin(
        &mut self,
        seed: f64,
        config: &crate::price_board::PriceBoardConfig,
    ) -> Option<f64> {
        let rate = self.coin_rate(config).filter(|r| *r > 0.0)?;
        Some(*self.coin_seed.get_or_insert(seed / rate) * rate)
    }

    /// Apply [`vike_model::RiskLimits::max_sizing_equity`] to an already-resolved equity — the ONE
    /// place the ceiling is applied, so [`Self::sizing_equity`] and the fold's
    /// `AppliedFill::equity_after` cannot disagree. `None` returns the figure verbatim
    /// (bit-identical, not `min`'d against an infinity).
    #[inline]
    #[must_use]
    pub fn cap_sizing_equity(&self, equity: f64) -> f64 {
        crate::risk::capped_equity(self.gate.limits.max_sizing_equity, equity)
    }

    /// **This engine's OWN profit and loss** — resolver-priced, and the one equity-shaped scalar
    /// with NO account balance level in it. The sibling of [`Self::resolved_equity`] (same
    /// resolver chain, per-position law, `compensated_sum` fold order and cold-path cadence), differing
    /// only in what it is denominated against.
    ///
    /// ```text
    /// resolved_equity  = seed + balance + realized − ... + unrealized   (Delta)
    ///                  = balance + unrealized + realized since the sync (Authoritative)
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
    /// A forced-close fee (`Account::apply_liquidation`) is accrued into `fees_paid` with the same
    /// sign and conversion as a trade commission, so it is counted here and the `Delta` identity
    /// above holds across a liquidation (it was a declared residual until 0.1.97, when `fees_paid`
    /// took the fee; `docs/decisions/0021-python-oracle-retired-vike-is-the-reference.md`'s third
    /// instance carries the parity fence).
    ///
    /// ⚠ **A STOPPED account is a tombstone and this counts it, for the life of the process.**
    /// `crates/vike-core/src/runtime/account_change.rs`'s `stop_engine_hot` keeps the engine in the list,
    /// so a stop is a ZERO step for `crates/vike-core/src/runtime/watchdog/risk.rs`'s
    /// `sweep_drawdown_latch` and for the published curve (a loss is never forgiven by stopping the
    /// account). The price: if the account starts again and its new engine ADOPTS the venue position
    /// (`hybrid`/`synthesize`/an operator confirm; the default `quarantine` adopts nothing), the position
    /// is counted by BOTH engines. That over-count is the known, FAIL-SAFE direction (the latch fires
    /// earlier, never later);
    /// `crates/vike-core/src/runtime/tests/safe_state/drawdown_latch.rs`'s
    /// `a_stopped_engine_stays_in_the_curve_and_an_adoption_counts_twice` pins it. A design that removes the over-count must meet three requirements, each learned from a
    /// reviewed attempt that was withdrawn because it failed them:
    ///
    /// 1. **Never count a position ZERO times.** Deciding "the running engine holds it" by KEY alone is
    ///    wrong: a restarted engine that starts flat and then buys one lot under the same key hides the
    ///    tombstone's real loss (partial size, or the opposite sign, drops a position the venue still
    ///    holds). The tombstone's copy may be dropped only for the SAME sign and a size at least as
    ///    large, and only when the position was ADOPTED from the venue (an explicit "adopted by
    ///    reconcile" flag on the position), never because the key merely exists.
    /// 2. **A stop of an engine that had adopted a position is not a zero step** unless the adoption is
    ///    rebased: dropping the copy at the stop moves the curve with no market move.
    /// 3. **The successor's copy must be COUNTED and PRICED** before the tombstone's is dropped: a
    ///    `Missing` price counts zero, and a price-only engine or one on another network is excluded
    ///    from the sum. Any later engine of the key covers an earlier tombstone, not only the nearest.
    pub fn resolved_own_pnl(&self, config: &crate::price_board::PriceBoardConfig) -> f64 {
        let unrealized_total = vike_math::summation::compensated_sum(
            self.account.positions.iter().map(|((v, s, side), entry)| {
                self.resolve_position_unrealized(v, s, *side, entry, config).0
            }),
        );
        self.own_pnl(unrealized_total, config)
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
        config: &crate::price_board::PriceBoardConfig,
    ) -> (f64, Option<crate::price_board::PriceSource>) {
        // Long/short for the side-aware quote comes from the SIGN of the folded size (as in
        // `margin_call.rs`); `position_side` is only the bucket label (`Both`/`Long`/`Short`).
        let is_long = entry.size >= 0.0;
        match self.price_board.resolve(venue, symbol, is_long, self.now_ms, config) {
            // Stale (a last-known price, returned ONLY under `PriceBoardConfig::stale_fallback`) is valued
            // exactly like Priced — a stale mark beats a silent zero — and its `source` still rides
            // through, so the position is NOT counted as missing.
            crate::price_board::PriceBoardResolution::Priced { price: px, source, .. }
            | crate::price_board::PriceBoardResolution::Stale { price: px, source, .. } => (
                self.account.unrealized_at(
                    symbol,
                    position_side,
                    entry.size,
                    entry.average_price,
                    px,
                ),
                Some(source),
            ),
            crate::price_board::PriceBoardResolution::Missing => (0.0, None),
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
        config: &crate::price_board::PriceBoardConfig,
    ) -> Option<f64> {
        match self.price_board.resolve(venue, symbol, size >= 0.0, self.now_ms, config) {
            crate::price_board::PriceBoardResolution::Priced { price: px, .. }
            | crate::price_board::PriceBoardResolution::Stale { price: px, .. } => Some(px),
            crate::price_board::PriceBoardResolution::Missing => None,
        }
    }

    /// Resolver-priced margin-in-use — [`Account::margin_in_use_priced`] with the resolver as the
    /// price input, so the margin side of every ratio/free-BP/liquidation comparison shares
    /// [`Self::resolved_equity`]'s price basis (the one-price law). Callers pass the rate closure.
    /// A `Missing` resolution excludes the position. Cold / per-order path only.
    pub fn resolved_margin_in_use_by(
        &self,
        config: &crate::price_board::PriceBoardConfig,
        rate_of: impl Fn(&crate::account::PositionKey, &crate::account::PositionEntry) -> Option<f64>,
    ) -> f64 {
        self.account.margin_in_use_priced(
            |(v, s, _side), p| self.resolved_position_price(v, s, p.size, config),
            rate_of,
        )
    }

    /// **THE margin a gate judges an order against**: [`Self::cross_margin_in_use`] plus
    /// [`Self::live_order_margin`], for the single order (`risk_context`) and each leg of a combo
    /// (`vike_core`'s `apply/combo.rs`), so a combo cannot pass buying power its naked legs are refused.
    /// `initial_margin_requirement` is the rate a symbol with no rate of its own falls back to (the judged order's symbol's),
    /// `judging` the order client id of the order being judged. Cold path: per order.
    pub fn gate_margin_used(
        &self,
        config: &crate::price_board::PriceBoardConfig,
        initial_margin_requirement: f64,
        judging: &str,
    ) -> f64 {
        self.cross_margin_in_use(config, initial_margin_requirement)
            + self.live_order_margin(config, initial_margin_requirement, judging)
    }

    /// The POSITION half of [`Self::gate_margin_used`]: the fold of [`Self::resolved_margin_in_use_by`], each
    /// position at its OWN per-symbol initial margin, falling back to `fallback_initial_margin` (never skipped). POOL (the liquidation
    /// law's partition, mirroring `check_margin_call_priced`): only CROSS positions consume the shared equity a
    /// gate admits against. An Isolated position has its own wallet and a Cash one is fully funded, so counting
    /// either would DOUBLE-CHARGE a mixed account. `vike_core`'s `snapshot::build` publishes it as `margin_used`.
    pub fn cross_margin_in_use(
        &self,
        config: &crate::price_board::PriceBoardConfig,
        fallback_initial_margin: f64,
    ) -> f64 {
        self.resolved_margin_in_use_by(config, |(_v, s, _side), p| {
            p.margin_mode
                .is_cross()
                .then(|| self.gate.limits.initial_margin_for(s).unwrap_or(fallback_initial_margin))
        })
    }

    /// **What this ACCOUNT already has on that the gate is not about to re-project** — the producer
    /// of [`crate::RiskContext::account_exposure_excl_order`], so all that
    /// [`vike_model::RiskLimits::max_account_exposure`] knows about the book it caps. TWO halves:
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
    ///   `crates/vike-execution/tests/risk/risk_lane_pricing.rs`'s
    ///   `max_total_exposure_is_scoped_to_one_venue_and_one_symbol` shows the shape): positions
    ///   that do NOT share a wallet, so summing them would refuse orders on collateral the venue
    ///   never sees.
    /// * **the order being JUDGED** (`judging`, by order-client-id). On a submit it is not in the
    ///   registry; on an amend ([`Self::modify_order`] judges the PROJECTED request under the
    ///   resting order's order client id) it is, and counting it as well would refuse an amend at a ceiling
    ///   the same order was admitted under — the double count `still_executable` was written for.
    /// * **a COVERED REDUCE** (`vike_model::is_covered_reduce`, the predicate the floor and margin
    ///   bypasses read): a resting order that shrinks a position adds no exposure.
    ///
    /// ⚠ **The judged SYMBOL's resting orders ARE counted while its POSITION is not**: the position
    /// is re-added projected and resting orders are projected by nothing, so this is the only way
    /// to neither double-count the one nor drop the other.
    ///
    /// ⚠ **An UNPRICEABLE position or order makes the answer [`f64::INFINITY`] — FAIL-CLOSED.** A
    /// book we cannot value is not a small book: counting it as 0.0 let an order pass the ceiling by
    /// any multiple while a restart's positions on an ADMITTED symbol had no mark yet (marks are not
    /// restored, and nothing re-seeds one until the first quote). The gate reads a non-finite figure
    /// as `account-exposure-unpriced` and denies every order that is not a covered reduce; a closing
    /// order is never held back, and the denial lifts by itself on the first quote. (The margin fold
    /// keeps its own unpriceable-is-0 rule: that is the buying-power lane's, not this ceiling's.)
    /// [`Self::missing_marks`] names the positions that lack a price.
    ///
    /// **A resting LIMIT, STOP, STOP-LIMIT or TAKE-PROFIT order is priceable while its symbol has no
    /// market price: it counts at its OWN limit or trigger** ([`resting_own_price`]). A closed market
    /// quotes nothing, so the first resting order used to pass against an empty book and the second saw
    /// the first as unbounded: one resting order per account. The market's price is always read first,
    /// so an open market is unchanged; a MARKET order (no price of its own) and a position with no mark
    /// stay unpriced and the sum stays infinite for them.
    ///
    /// Cold path: per-order, called ONLY when the ceiling is armed — no logging, no allocation.
    pub fn resolved_account_exposure_excluding(
        &self,
        exclude_symbol: &str,
        judging: &str,
        config: &crate::price_board::PriceBoardConfig,
    ) -> f64 {
        let unpriced = std::cell::Cell::new(false);
        let positions = self.account.gross_notional_priced(|(v, s, _side), p| {
            if s.as_str() == exclude_symbol || v.as_str() != self.venue.as_str() {
                return None;
            }
            let px = self.resolved_position_price(v, s, p.size, config);
            unpriced.set(unpriced.get() || px.is_none());
            px
        });
        if unpriced.get() {
            return f64::INFINITY;
        }
        positions + self.live_order_notional(judging, config)
    }

    /// Σ gross notional of this engine's OWN **live, un-filled** orders — the in-flight half of
    /// [`Self::resolved_account_exposure_excluding`], and the exposure twin of
    /// [`Self::live_order_margin`]: the same skips (the judged order client id, a non-live status, a
    /// non-finite or non-positive remainder, a covered reduce) without the
    /// initial-margin rate, each survivor contributing `remaining × price × unit` (an inverse contract: `remaining × unit`)
    /// (`vike_model::gross_notional`). WHICH orders the two walks see is ONE rule,
    /// [`Self::counted_remaining`]: a divergence there would make the margin and exposure lanes
    /// disagree about one book. The ONE difference is
    /// on purpose: a counted order with no price is [`f64::INFINITY`] here (fail-closed, see
    /// [`Self::resolved_account_exposure_excluding`]), where the margin walk skips it. "No price" means
    /// no market price AND no price of its own ([`resting_own_price`]): a resting limit or stop is
    /// counted at its limit or trigger while its symbol is unquoted. A live COMBO counts each leg at that
    /// leg's own price and contract value ([`Self::counted_legs`]); a leg has no price of its own, so an
    /// unpriced leg is infinite.
    fn live_order_notional(
        &self,
        judging: &str,
        config: &crate::price_board::PriceBoardConfig,
    ) -> f64 {
        let mut open = 0.0;
        for (order_client_id, mo) in self.registry.live() {
            let Some(remaining) = self.counted_remaining(order_client_id, mo, judging) else {
                continue;
            };
            if !mo.request.combo_legs.is_empty() {
                for (symbol, signed, qty) in self.counted_legs(&mo.request, remaining) {
                    // A leg has no price of its own (the net limit is the combo's): unpriced is unbounded.
                    let Some(px) =
                        self.resolved_position_price(&mo.request.venue, symbol, signed, config)
                    else {
                        return f64::INFINITY;
                    };
                    open += self.account.contract_value_of(symbol).gross_notional(qty, px);
                }
                continue;
            }
            let symbol = mo.request.symbol.as_str();
            let signed = mo.request.side as f64 * remaining;
            // The market's price, else (a closed market: nothing quoted since boot) the order's OWN
            // limit or trigger. Only the unpriced fall through to fail-closed.
            let Some(px) = self
                .resolved_position_price(&mo.request.venue, symbol, signed, config)
                .or_else(|| resting_own_price(&mo.request))
            else {
                return f64::INFINITY;
            };
            open += self.account.contract_value_of(symbol).gross_notional(remaining, px);
        }
        open
    }

    /// Σ initial margin of this engine's OWN **live, un-filled** order exposure — the commitment
    /// term of `RiskContext::margin_used`. ⚠ Counting open POSITIONS only overstates free buying
    /// power by every order in flight: the second of two orders submitted before the first fills
    /// is judged as though the first committed nothing. The backtest counts its pending orders
    /// (`OrderBrokerSimulator::margin_in_use_pending_aware`); this mirrors `RiskGate::check_combo`'s
    /// `committed_margin` rather than adding a second commitment model.
    ///
    /// Every term matches the POSITION fold in `Account::margin_in_use_priced`
    /// (`|qty| * px * unit * initial_margin`), through the SAME `resolved_position_price` resolver and
    /// the SAME per-symbol `initial_margin_for` fallback, so the two halves of `margin_used` cannot drift.
    ///
    /// ⚠ Skips reduce-shaped orders via `vike_model::is_covered_reduce` (the floor bypass's
    /// predicate): an order that shrinks a position commits no NEW margin, and charging it would
    /// deny flattens during a drawdown. Unpriceable orders contribute 0, like the position fold.
    /// A live COMBO commits each leg at that leg's own price, contract value and initial margin (`Self::counted_legs`),
    /// an unpriceable leg 0.
    ///
    /// ⚠ `judging` is the order-client-id of the order being GATED, and excluding it is
    /// load-bearing: on an AMEND it IS in the registry, and charging its existing commitment
    /// while judging its new size double-counts it (`partial_fill_amend_accounting.rs`'s
    /// `the_buying_power_lane_charges_only_what_can_still_execute` pins it). Inert on a SUBMIT.
    ///
    /// Per order on the gate, off the `p99 < 10µs` per-message fold. ⚠ ALSO PER PUBLISH with the margin lane
    /// armed (`vike_core`'s `snapshot::build`, the published `free_buying_power`, `judging` empty), inside the gated hop:
    /// no allocation and no logging here, and a book with no live order is zero steps of this loop: the walk
    /// reads the registry's live index ([`crate::execution_engine::OrderRegistry::live`]), never the ended orders.
    pub fn live_order_margin(
        &self,
        config: &crate::price_board::PriceBoardConfig,
        initial_margin_requirement: f64,
        judging: &str,
    ) -> f64 {
        self.live_order_margin_on(config, initial_margin_requirement, judging, |_| true)
    }

    /// [`Self::live_order_margin`] over the orders, and a combo's legs, on the symbols `on` keeps: a paper spot buy's
    /// free cash ([`Self::cash_overbuy_denial`]) counts its derivatives' orders at their margin and its spot buys at
    /// their cost, never both. The same fold in the same order, so `on` keeping every symbol is that function.
    pub(super) fn live_order_margin_on(
        &self,
        config: &crate::price_board::PriceBoardConfig,
        initial_margin_requirement: f64,
        judging: &str,
        on: impl Fn(&str) -> bool,
    ) -> f64 {
        let mut used = 0.0;
        for (order_client_id, mo) in self.registry.live() {
            let Some(remaining) = self.counted_remaining(order_client_id, mo, judging) else {
                continue;
            };
            if !mo.request.combo_legs.is_empty() {
                for (symbol, signed, qty) in self.counted_legs(&mo.request, remaining) {
                    if !on(symbol) {
                        continue;
                    }
                    if let Some(px) =
                        self.resolved_position_price(&mo.request.venue, symbol, signed, config)
                    {
                        let initial_margin = self
                            .gate
                            .limits
                            .initial_margin_for(symbol)
                            .unwrap_or(initial_margin_requirement);
                        used += self.account.contract_value_of(symbol).gross_notional(qty, px)
                            * initial_margin;
                    }
                }
                continue;
            }
            let symbol = mo.request.symbol.as_str();
            if !on(symbol) {
                continue;
            }
            let signed = mo.request.side as f64 * remaining;
            let Some(px) = self.resolved_position_price(&mo.request.venue, symbol, signed, config)
            else {
                continue;
            };
            let initial_margin =
                self.gate.limits.initial_margin_for(symbol).unwrap_or(initial_margin_requirement);
            used += self.account.contract_value_of(symbol).gross_notional(remaining, px)
                * initial_margin;
        }
        used
    }

    /// WHICH orders [`Self::live_order_notional`] and [`Self::live_order_margin`] count, ONE rule so
    /// the exposure and margin lanes cannot disagree about one book: every order but the judged order client id
    /// whose status is live, then [`crate::risk::committed_quantity`], the rule of how much of it counts,
    /// which the backtest's `OrderBrokerSimulator` folds ask too; `None` skips it.
    ///
    /// A live COMBO is counted leg by leg after this test ([`Self::counted_legs`]): its own `symbol` is empty.
    ///
    /// `#[inline(always)]`: the gate calls both walks per order, and each held this test inline
    /// before it was shared.
    #[inline(always)]
    fn counted_remaining(
        &self,
        order_client_id: &str,
        mo: &vike_model::OrderRecord,
        judging: &str,
    ) -> Option<f64> {
        if order_client_id == judging || !mo.status.is_live() {
            return None;
        }
        crate::risk::committed_quantity(
            mo.request.qty - mo.filled_qty,
            mo.request.side,
            self.cover_position_size(
                mo.request.symbol.as_str(),
                mo.request.reduce_only,
                mo.request.side,
                order_client_id,
            ),
        )
    }

    /// A live COMBO's legs as the plain orders both walks count, since the combo's own `symbol` is empty
    /// (`vike_model::build_combo`): `(symbol, signed qty, qty)` per leg, `|ratio| × remaining` on the side
    /// `side × sign(ratio)` (`RiskGate::check_combo`'s law), a covered-reduce leg left out as
    /// [`crate::risk::committed_quantity`] leaves out a plain one (a leg carries no `reduce_only`). No allocation.
    fn counted_legs<'a>(
        &'a self,
        request: &'a vike_model::OrderRequest,
        remaining: f64,
    ) -> impl Iterator<Item = (&'a str, f64, f64)> + 'a {
        request.combo_legs.iter().filter_map(move |leg| {
            let side = request.side.signum() * leg.ratio.signum();
            let qty = crate::risk::committed_quantity(
                f64::from(leg.ratio.unsigned_abs()) * remaining,
                side,
                self.gate_position_size(&leg.symbol),
            )?;
            Some((leg.symbol.as_str(), side as f64 * qty, qty))
        })
    }

    /// `Account::equity_all`'s mode-aware seed expression, verbatim:
    /// delta = seed + balance + realized + unrealized;
    /// authoritative = balance + unrealized + `Account::realized_since_balance_sync`.
    /// A COIN LEDGER's coin is valued at [`Self::coin_rate`] (`Account::coin_equity`, decision 0258).
    #[inline]
    fn mode_equity(
        &self,
        seed: f64,
        unrealized_total: f64,
        config: &crate::price_board::PriceBoardConfig,
    ) -> f64 {
        if let Some(rate) = self.coin_rate(config) {
            return self.account.coin_equity(seed, unrealized_total, rate);
        }
        if self.account.balance_mode == crate::BalanceMode::Authoritative {
            self.account.balance + unrealized_total + self.account.realized_since_balance_sync()
        } else {
            seed + self.account.balance + self.account.realized_pnl + unrealized_total
        }
    }

    /// The MODE-BLIND own-PnL expression [`Self::resolved_own_pnl`] documents (no term in it is
    /// written by an authoritative balance sync). Beside [`Self::mode_equity`] so the two are read
    /// together: the omitted `seed + balance` is the account LEVEL. The term order is fixed so
    /// [`crate::Portfolio::pnl_total`] (the same four published `VenueBlock` fields) is
    /// bit-identical; a COIN LEDGER's three coin terms are each valued by [`Self::account_money`] first, the
    /// products the published fields carry (decision 0258), so the identity holds for it too.
    #[inline]
    fn own_pnl(&self, unrealized_total: f64, config: &crate::price_board::PriceBoardConfig) -> f64 {
        let usd = self.account_money(config);
        let a = &self.account;
        usd(a.realized_pnl) - usd(a.fees_paid) + usd(a.funding_paid) + unrealized_total
    }
}

/// **A resting order's OWN price, for the exposure sum when its symbol has no market price**
/// ([`Engine::resolved_account_exposure_excluding`]): the limit (or, with none, the trigger) of a
/// LIMIT, STOP, STOP-LIMIT or TAKE-PROFIT order, the same `price.or(trigger_price)` the gate sizes the
/// judged order with. `None` for a MARKET order (it has no price of its own), for a kind this build does not
/// recognise, and for a price that is not a finite positive number: those stay unpriced, and the sum fails
/// closed for them. The market's price is always read first; this is the fallback of the closed market.
pub(super) fn resting_own_price(request: &vike_model::OrderRequest) -> Option<f64> {
    use vike_model::OrderType::{Limit, Stop, StopLimit, TakeProfit};
    matches!(request.order_type, Limit | Stop | StopLimit | TakeProfit)
        .then(|| request.price.or(request.trigger_price))
        .flatten()
        .filter(|p| p.is_finite() && *p > 0.0)
}
