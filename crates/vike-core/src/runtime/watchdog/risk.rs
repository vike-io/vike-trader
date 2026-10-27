//! Risk sweeps: the margin call, the equity-drawdown latch and the per-mount budget latch.

use super::*;

/// How a risk sweep may price one engine ([`CoreThread::price_only_basis`]).
#[derive(Debug, Clone, Copy)]
pub(crate) enum PriceBasis {
    /// Not judged by the age bound: priced by `config.price_cfg`, as it always was (a mounted engine on the
    /// closed-bar pass, a stopped one on either).
    Mounted,
    /// An engine with usable marks that the age bound judges: priced by this cfg (every rung capped at the
    /// bound, no stale fallback), so a rung past the bound is no price.
    Bounded(PriceCfg),
    /// An engine the sweep must not act on, with the reason said.
    Skip(&'static str),
}

/// Which sweep asks [`CoreThread::price_only_basis`] and the sweeps built on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RiskPass {
    /// A closed bar ([`CoreThread::drive_strategy`]): marks are fresh by construction, and a mounted engine
    /// is priced by `config.price_cfg` exactly as before the clock covered it.
    Bar,
    /// The clock ([`CoreThread::sweep_risk_clock`]), every live engine: no bar vouches for a mark here, so
    /// the age bound does, for a mounted engine as for a price-only one.
    Clock,
}

/// The drawdown curve's terms ([`CoreThread::drawdown_terms_in`]): the group of ENGINE 0's
/// `(capital base, own PnL)` and, for each OTHER group, the same pair under its currency key. Engine 0's
/// group is only where the latch stores its first slot (`pnl_curve_peak`); every group is judged alike, so
/// which one is stored first changes no verdict, and the PUBLISHED primary group is another question
/// ([`CoreThread::live_primary_ccy`], the first LIVE engine's).
#[derive(Debug)]
pub(crate) struct DrawdownTerms {
    pub(crate) base: f64,
    pub(crate) pnl: f64,
    /// `(currency key, capital base, own PnL)` per other group, first-seen order. EMPTY, and never
    /// allocated, when every engine shares the primary's currency.
    pub(crate) foreign: Vec<(vike_exec::Interned, f64, f64)>,
}

/// Why a price-only engine is out of the risk sweeps: a position's mark is older than the bound.
const SKIP_STALE: &str =
    "a marked position's price is older than the price-only bound (a dead pump?)";
/// Why a price-only engine is out of the risk sweeps: its book is another network's.
const SKIP_NETWORK: &str = "its price feed dials another network than the account trades on";

impl<C: ExecutionClient> CoreThread<C> {
    /// How the margin call and the drawdown latch may price engine `eidx` (decision 0038's neighbour: never
    /// act on a number nobody can vouch for). The age bound judges an engine of a venue in
    /// [`CoreConfig::price_only`] on either pass, and EVERY live engine on the clock pass
    /// ([`RiskPass::Clock`]: no closed bar vouches for a mark there, a mounted engine's included). A STOPPED
    /// engine is exempt, of a mounted venue or a price-only one (its board never moves again, so the bound
    /// would read it stale for ever; it is [`PriceBasis::Mounted`], valued at its last known price, and never
    /// said), except that a stopped engine on another network than its feed stays out ([`SKIP_NETWORK`]).
    /// Every other case that is not judged is [`PriceBasis::Mounted`] too (so a mounted venue's closed-bar
    /// sweep is byte-identical to before). For an engine the bound judges:
    ///
    /// * [`PriceBasis::Skip`] when it trades a real network (`tier != Paper`) other than the one its feed
    ///   dials ([`PriceOnlyConfig::price_source`]: the mainnet-vs-testnet basis is phantom PnL that, with
    ///   leverage, can trip the account-wide drawdown latch), or when an open position's price is older than
    ///   [`PriceOnlyConfig::max_price_age`] (a dead pump with the position still marked: a frozen number).
    ///   The two skips mean different things to the callers: the margin call and the mount budgets skip the
    ///   engine on either, while the DRAWDOWN LATCH leaves it out of the curve for the network alone and
    ///   values a merely STALE one at its last known price ([`Self::drawdown_terms_in`]);
    /// * otherwise [`PriceBasis::Bounded`], so an engine whose freshest rung is fine is not priced
    ///   off an older rung that the permissive default `price_cfg` would still pick.
    ///
    /// A skip is said ONCE per episode ([`CoreThread::price_only_skipped`]), not at every 30 s sweep, and
    /// said again if the reason changes or the engine recovers and fails later; a stale one is also a WARN
    /// naming the engine, the age of its oldest marked price and, only when a latch is configured, what the
    /// latch does with it. Cold: per sweep.
    pub(crate) fn price_only_basis(&mut self, eidx: usize, pass: RiskPass) -> PriceBasis {
        let Some(po) = self.config.price_only.as_ref() else { return PriceBasis::Mounted };
        let eng = self.eng(eidx);
        let price_only = po.venues.contains(eng.venue.as_str());
        let retired = self.is_retired(eidx);
        if !price_only && (pass == RiskPass::Bar || retired) {
            return PriceBasis::Mounted;
        }
        let ms = i64::try_from(po.max_price_age.as_millis()).unwrap_or(i64::MAX);
        let base = self.config.price_cfg;
        let cap = |own: Option<i64>| Some(own.map_or(ms, |o| o.min(ms)));
        let cfg = PriceCfg {
            mark_max_age_ms: cap(base.mark_max_age_ms),
            quote_max_age_ms: cap(base.quote_max_age_ms),
            trade_max_age_ms: cap(base.trade_max_age_ms),
            bar_max_age_ms: cap(base.bar_max_age_ms),
            stale_fallback: false,
            ..base
        };
        let other_network = eng.tier != vike_model::AccountTier::Paper
            && po.price_source.get(eng.venue.as_str()).is_some_and(|t| *t != eng.tier);
        // A STOPPED engine (decision 0169) is not a dead pump: its board never moves again, so it reads stale for
        // ever, and it keeps its position (the operator's to close). It is valued at its last known price by
        // everything that still asks (the latch: a loss is never forgiven by stopping the account), without a
        // skip line. A stopped engine of another network than its feed is still out of the curve, below.
        if retired && !other_network {
            self.price_only_skipped.remove(&eidx);
            return PriceBasis::Mounted;
        }
        let stale = if other_network { Vec::new() } else { eng.stale_marks(&cfg) };
        let reason = if other_network {
            Some(SKIP_NETWORK)
        } else if !stale.is_empty() {
            Some(SKIP_STALE)
        } else {
            None
        };
        let Some(reason) = reason else {
            self.price_only_skipped.remove(&eidx);
            return PriceBasis::Bounded(cfg);
        };
        if self.price_only_skipped.insert(eidx, reason) != Some(reason) {
            let (venue, route) = (self.eng(eidx).venue.clone(), self.eng(eidx).route_key.clone());
            let line = if reason == SKIP_NETWORK {
                format!(
                    "price-only risk sweeps SKIP the {venue} engine {route}: {reason}; the margin \
                     call and the drawdown latch do not act on it until that clears"
                )
            } else {
                // THE LATCH DOES NOT SKIP A STALE ENGINE (owner ruling 2026-10-25): a quiet pump must not
                // blind the node-wide latch (an FX weekend, a dead pump in an open market), so the latch
                // values the engine at the last price it had, and this line, said ONCE per episode, is the
                // loud half: which engine, how old the price is, and what the latch does with it. The
                // MARGIN CALL still skips it: it must not liquidate on a stale price.
                let oldest = stale.iter().max_by_key(|m| m.age_ms);
                let (symbol, age_s) =
                    oldest.map_or((String::new(), 0), |m| (m.symbol.clone(), m.age_ms / 1000));
                let (who, what, closed) = if price_only {
                    ("price-only risk sweeps", "the margin call does not act on it", "")
                } else {
                    (
                        "mounted-engine clock sweeps",
                        "the clock's margin call and mount budget do not act on it",
                        " (its closed bars still judge it)",
                    )
                };
                // The latch is only claimed when one is configured (`max_drawdown`): the ring line and the WARN
                // are built from the same fact, so neither tells an operator a loss limit is running when none is.
                let latch_on = self.config.max_drawdown.is_some();
                let latch = if latch_on {
                    ", and the drawdown latch values it at its last known price"
                } else {
                    ""
                };
                let warn_latch = if latch_on {
                    "the drawdown latch values it at its last known price, "
                } else {
                    ""
                };
                tracing::warn!(
                    target: "vike_core::risk_clock",
                    %venue,
                    %route,
                    %symbol,
                    age_s,
                    "the {venue} engine {route} holds a position whose price ({symbol}) is {age_s} s old: \
                     {warn_latch}{what}"
                );
                format!(
                    "{who} SKIP the {venue} engine {route}: {reason}; {what} until that \
                     clears{latch} ({symbol} is {age_s} s old){closed}"
                )
            };
            self.note(line);
            self.dirty = true;
        }
        PriceBasis::Skip(reason)
    }

    /// **The CLOCK pass of the margin call and the drawdown latch: EVERY live engine, whatever the interval
    /// of the bars its mount (if any) listens to** (owner ruling 2026-10-25: a mount on 4-hour bars is not
    /// looked at once per 4 hours). [`TimerKind::RiskClockSweep`] calls this at the drain-loop boundary, on
    /// [`PriceOnlyConfig::sweep_interval`]; the closed-bar pass ([`Self::drive_strategy`]) keeps running
    /// beside it, and an engine with no mount (a price-only venue) has no other.
    ///
    /// The marks it reads are the ones the feeds put on each engine's board (`mirror_venue_price`, the quote
    /// and trade lanes), priced by [`Self::price_only_basis`] on [`RiskPass::Clock`]: every rung capped at
    /// [`PriceOnlyConfig::max_price_age`] with no stale fallback, an engine with a position past that bound
    /// SKIPPED (said once per episode) and an engine on another network than its feed kept out of both
    /// sweeps. `config.price_cfg` itself is the permissive default and would accept a quote of any age,
    /// which is why the clock does not use it, except for the drawdown latch's view of a STALE engine of any
    /// kind (its last known price, as a bar always used for a mounted one: see [`Self::drawdown_terms_in`]). A STOPPED engine is skipped by the
    /// margin call ([`Self::sweep_margin_call_engine`]). The per-mount budget latch rides the clock too
    /// ([`Self::sweep_mount_budgets_in`]: fire-once, same staleness rule). Cold, once per interval, never
    /// the fold; the liquidation it can release is journaled write-ahead by
    /// [`Self::sweep_margin_call_engine`], so replay re-applies it rather than re-deciding. A margin-call
    /// WARNING that holds is said once per episode here (a closed bar repeats it per bar).
    /// The drawdown latch is idempotent (it only ever raises the state, and only from `Active`), so the
    /// two passes cannot both trip it; the margin call's own guard against both releasing is in
    /// [`Self::sweep_margin_call_engine`].
    pub(crate) fn sweep_risk_clock(&mut self, now: i64) {
        // ⚠ EACH STAGE under its own guard ([`Self::clock_stage`]): the walk goes from the highest engine down,
        // and one panic (a client that dies on engine k, a journal append that fails there) must not stop the
        // margin call of the engines below it, the primary's included, nor the latch and the budgets after.
        if let Some(cfg) = self.config.margin_call {
            for eidx in (0..=self.extra_engines.len()).rev() {
                self.clock_stage("margin call", Some(eidx), |c| {
                    c.sweep_margin_call_engine(eidx, &cfg, now, RiskPass::Clock);
                });
            }
        }
        if let Some(threshold) = self.config.max_drawdown {
            self.clock_stage("drawdown latch", None, |c| {
                c.sweep_drawdown_latch_in(threshold, RiskPass::Clock);
            });
        }
        if self.any_mount_budget {
            self.clock_stage("mount budgets", None, |c| {
                c.sweep_mount_budgets_in(now, RiskPass::Clock);
            });
        }
    }

    /// Phase B margin-call sweep (LEAN DefaultMarginCallModel over the live Account), the CLOSED-BAR pass.
    /// Warning lands in the recent-events ring; liquidation intents become reduce-only
    /// MARKET orders submitted through the ONE live path (mint → RiskGate → client) — a
    /// margin call can never bypass the gate (a veto surfaces as OrderDenied, LEAN's
    /// user-hook analog).
    pub(crate) fn sweep_margin_call(&mut self, cfg: &vike_model::MarginCallConfig, now: i64) {
        // full LEAN model PER ENGINE — each account is checked at its own seed and its
        // liquidations submit through THAT engine, named by INDEX (`EngineRoute::Engine`) rather
        // than by the position's venue string, which two accounts of one exchange share.
        for eidx in (1..=self.extra_engines.len()).rev() {
            self.sweep_margin_call_engine(eidx, cfg, now, RiskPass::Bar);
        }
        self.sweep_margin_call_engine(0, cfg, now, RiskPass::Bar);
    }

    /// One engine's margin call, on either pass. `#[inline(never)]`: cold, and two passes call it now, so it
    /// must not be folded into the strategy drive that `dispatch` reaches.
    #[inline(never)]
    fn sweep_margin_call_engine(
        &mut self,
        eidx: usize,
        cfg: &vike_model::MarginCallConfig,
        now: i64,
        pass: RiskPass,
    ) {
        // A STOPPED account (decision 0169) has a detaching client: a liquidation sent to it could only
        // be refused. Its open position is the operator's to close, and was said when it stopped.
        if self.is_retired(eidx) {
            return;
        }
        // **ONE RELEASE PER ENGINE PER CLOCK INTERVAL, whichever pass releases it.** A close released at T has
        // not folded back (the position, the equity) when the other pass looks at T + a few seconds, and it
        // would read the same breach and release a second close of the same size. So an engine that
        // released one is quiet until a whole interval has passed ([`CoreThread::margin_call_quiet_until`]).
        // Past the window a close still unfilled is NETTED from the next one ([`CoreThread::net_intents`], in
        // the `Liquidate` arm below), which is what actually bounds the closes: this window is the cheap
        // first guard and no longer the only one. The clock is the engine's own `now_ms` as the message or the timer
        // stamped it, read BEFORE the release moves it (`lower_submit` stamps the sweep's `now`, a bar's
        // timestamp on the bar pass). Without a clock (`price_only_tick_ms` 0) nothing is recorded, so a
        // core with no `price_only` is byte-identical.
        let wall = self.eng(eidx).now_ms;
        if self.margin_call_quiet_until.get(&eidx).is_some_and(|&until| wall < until) {
            return;
        }
        // One-price law: resolver-priced equity (the SAME `config.price_cfg` source the
        // snapshot/sampler read), so the liquidation decision acts on the number the operator
        // sees — never a stale/absent `Account.marks` scalar. The margin-used side of the
        // comparison is priced through the SAME resolver (`check_margin_call_priced` with the
        // engine's board), closing the #518 asymmetry where maintenance folded off stale
        // `Account.marks` while equity read fresh quotes — numerator and denominator of the
        // breach test now share one price basis.
        //
        // ⚠ **`resolved_equity`, NOT `sizing_equity`, and this line must never be "aligned" with
        // the gate's.** `vike_config::Policy::max_sizing_equity` caps the equity the SPENDING lanes
        // see, which is conservative there and destructive here: this sweep reads equity as the
        // COLLATERAL backing open positions, so a capped figure would make a healthy account look
        // under-margined and LIQUIDATE it. A liquidation decision judges the account as it really
        // is. `vike_exec::ExecutionEngine::sizing_equity`'s doc is the authority for the split and
        // names which consumer sits on which side.
        // ⚠ An engine the age bound judges (a price-only one; every one on the clock pass) is priced by
        // `price_only_basis` (bounded age) or not at all; a mounted one on the bar pass keeps
        // `config.price_cfg` exactly.
        let pcfg = match self.price_only_basis(eidx, pass) {
            PriceBasis::Mounted => self.config.price_cfg,
            PriceBasis::Bounded(cfg) => cfg,
            PriceBasis::Skip(_) => return,
        };
        let eng = self.eng(eidx);
        let equity = eng.resolved_equity(self.seed_of(eidx), &pcfg);
        let verdict =
            vike_exec::check_margin_call_priced(&eng.account, equity, cfg, |(v, s, _side), p| {
                eng.resolved_position_price(v, s, p.size, &pcfg)
            });
        match verdict {
            vike_exec::MarginCall::Healthy => {
                self.margin_warning_held.remove(&eidx);
            }
            vike_exec::MarginCall::Warning { margin_used, margin_remaining } => {
                // A closed bar says a holding band at every bar, as it always did. The CLOCK says it
                // once per EPISODE (until a sweep finds the engine out of the band): the recent ring is
                // shared with fills and rejects, and a band that holds for an hour would put 120 lines
                // of one warning into it.
                let first = self.margin_warning_held.insert(eidx);
                if pass == RiskPass::Bar || first {
                    self.note(format!(
                        "margin-call WARNING: used={margin_used:.2} remaining={margin_remaining:.2}"
                    ));
                    self.dirty = true;
                }
            }
            vike_exec::MarginCall::Liquidate(intents) => {
                // ⚠ **NET THE CLOSES ALREADY IN FLIGHT** (third review, 1). The quiet window below only
                // guards against this engine's OWN last release; a close it did not send (a manual Flatten,
                // a MarketExit, a stuck pre-ack close) arms none, and a close of its own still unfilled after
                // one interval was sent AGAIN. The gate nets in-flight reduce-only orders only against a FLAT
                // book, and oanda, ig, fxcm, ibkr, ctrader and alpaca put no reduce-only flag on the wire,
                // so a second full-size sell flips the position. Each intent now asks for what is STILL
                // missing: its quantity less the market closes out on that symbol and side; the CLAMPED
                // quantity is what is journaled write-ahead and sent, and an intent with nothing left is
                // dropped. Need-based, so a close that stays unfilled for an hour is not stacked on.
                let asked = intents.len();
                let intents = self.net_intents(eidx, intents);
                if intents.is_empty() {
                    // The breach stands and every close it asks for is already out: nothing to add, no quiet
                    // window to arm (the closes in flight ARE the guard). Said once per episode on the clock
                    // (the warning's own marker: an episode ends at the first sweep that finds the engine
                    // out of the breach), at every closed bar on the bar pass.
                    if pass == RiskPass::Bar || self.margin_warning_held.insert(eidx) {
                        self.note(format!(
                            "margin-call breach COVERED: {asked} close(s) already in flight, none added"
                        ));
                        self.dirty = true;
                    }
                    return;
                }
                self.margin_warning_held.remove(&eidx);
                self.note(format!("MARGIN CALL: liquidating {} position(s)", intents.len()));
                for it in intents {
                    let req = OrderRequest {
                        client_order_id: String::new(),
                        venue: it.venue,
                        symbol: it.symbol,
                        side: it.side,
                        qty: it.qty,
                        order_type: vike_model::OrderType::Market,
                        reduce_only: true,
                        ts: now,
                        ..Default::default()
                    };
                    // WRITE-AHEAD (emulator PR-4): journal the released reduce-only MARKET BEFORE
                    // `apply_intent`, exactly as `submit_fired` journals a conditional FIRE. The
                    // sweep is a runtime reaction to the account's marks + equity on a CLOSED BAR or
                    // the CLOCK — neither is journaled — so without this record a replay could not
                    // reproduce the liquidation and the determinism fence caught it as a
                    // `HashMismatch`. The record carries the request with its coid still EMPTY (the
                    // mint happens inside `apply_intent`, which then writes its own `MintedSubmit`);
                    // replay re-applies it as `Command::Order(Submit(req))` through the SAME
                    // one-write path and re-mints the identical coid, never re-evaluating the breach.
                    // Off-fold (per closed bar or per clock interval, never the p99 per-message
                    // path) and gated on journaling being on, so the no-journal path stays
                    // byte-identical.
                    // §9 item 12: the RESOLVED route key of the BREACHING engine — this
                    // liquidation closes THAT book, and `req.venue` names an exchange rather than
                    // an account. Resolved BEFORE the journal borrow (the `&self` engine read and
                    // the `&mut self.journal` append cannot overlap), and `None` on every
                    // single-account box, so its journal bytes do not move.
                    let rk = self.journal_route_key(eidx, &req.venue);
                    if let Some(j) = self.journal.as_mut() {
                        // `mount_id: None` — this is the ACCOUNT-wide sweep. It liquidates a
                        // POSITION, not a strategy's book, so no mount owns the released order and
                        // its fill belongs in the unattributed residual row. (The per-mount budget
                        // latch is the `Some` case; see `latch_mount`.)
                        j.append_margin_call_liquidate(now, &req, None, rk.as_deref())
                            .expect("journal append");
                        self.journaled_since_snap += 1;
                    }
                    // ⚠ `EngineRoute::Engine(eidx)` — THE BREACHING ENGINE, not the venue's default
                    // account. The sweep above is per-ENGINE and the intents come out of THIS
                    // engine's account, but the release lowered through `apply_intent`, i.e. by the
                    // payload's venue string — which resolves the venue's DEFAULT engine. With two
                    // accounts of one exchange that submitted a reduce-only close of account B's
                    // position to account A: on a flat A it is refused and B is never liquidated
                    // (a protection that silently does nothing), and on the spread configuration
                    // this file's sibling tests describe — long on A, short on B — it acts on the
                    // wrong book entirely. The comment on `sweep_margin_call` claiming these
                    // "submit through ITS engine" was true only while one engine per venue made the
                    // venue lookup and the engine index the same answer.
                    self.apply_intent_routed(
                        OrderIntent::Submit(Box::new(req)),
                        now,
                        CancelIntent::Unspecified,
                        EngineRoute::Engine(eidx),
                    );
                }
                if self.price_only_tick_ms > 0 {
                    self.margin_call_quiet_until
                        .insert(eidx, wall.saturating_add(self.price_only_tick_ms));
                }
                self.dirty = true;
            }
        }
    }

    /// Drawdown latch (audit exec#4): fold a high-water-mark of **the daemon's own equity curve**
    /// across ALL engines on the per-CLOSED-bar sweep (marks fresh here — the SAME cadence and
    /// resolver as [`Self::sweep_margin_call`], never the event fold). When the drop from the HWM
    /// exceeds `threshold`, LATCH into liquidate-only by setting every engine's
    /// `trading_state = Reducing` (the existing RiskGate then denies risk-increasing orders and
    /// permits only reduce-only — the same enforcement the manual
    /// `Command::SetTradingState(Reducing)` arms). A warning naming the HWM, the current curve and
    /// the drawdown % is pushed to the recent-events ring (the margin-call watchdog's channel).
    /// This mints NO orders and adds NO new Event/Command variant.
    ///
    /// # The curve is `capital base + own PnL`, and neither half is an account balance level
    ///
    /// ```text
    /// curve = Σ_engines seed_of(e)                     (frozen: config, never observed)
    ///       + Σ_engines ExecutionEngine::resolved_own_pnl(e)   (realized − fees + funding + unrealized)
    /// ```
    ///
    /// ⚠ **It used to be `Σ ExecutionEngine::resolved_equity`, and that is a live-risk defect on
    /// any engine whose `Account` has flipped to `BalanceMode::Authoritative`.** That block's
    /// equity is `venue wallet + unrealized`, and the wallet is the venue's number for the WHOLE
    /// ACCOUNT the credentials open — nothing in it is scoped to this daemon. Two failures, both
    /// measured on the CI box 2026-08-17 (nine paper `Delta` blocks at 1000 seed each plus one
    /// `Authoritative` bybit block carrying 53647.10600813 of a SHARED demo wallet, HWM latched at
    /// ~62647):
    /// 1. a THIRD PARTY withdrawing from that shared wallet drops the sum with no trading activity
    ///    at all, and trips this daemon into liquidate-only;
    /// 2. a genuine 25% loss on the daemon's own ~9000 of book is 3.6% of 62647 — invisible.
    ///
    /// [`vike_exec::ExecutionEngine::resolved_own_pnl`] carries the argument for why P&L is the
    /// right movement term (every one of its four fields is written only by this engine's own
    /// fills/funding, and `Account::apply_account_state` writes none of them) and the one declared
    /// residual. The base is `seed_of(e)` — the operator's CONFIGURED capital for the mount, a
    /// constant this process never re-reads from a venue, so it is identical across a restart and
    /// cannot be moved by anyone but the operator editing the profile. (One exception, a NON-dollar
    /// engine that has adopted a venue wallet: its seed slot holds that wallet from the first sweep that
    /// sees it, once; [`Self::adopt_wallet_bases`].)
    ///
    /// ⚠ On an all-`Delta` core (every paper mount, every backtest-shaped run, every test in this
    /// tree) `seed + own_pnl` IS `resolved_equity` — `balance` there is exactly
    /// `−fees_paid + funding_paid` accumulated from the same fills — so this change is a NO-OP
    /// wherever there is no venue wallet to contaminate the measure, and bites exactly where one
    /// exists. The residual (a liquidation fee, which moves `balance` but not `fees_paid`) is
    /// declared on `resolved_own_pnl`.
    ///
    /// # Startup, restart, and the disarm
    ///
    /// Before any P&L exists `own_pnl` is 0, so the curve IS the capital base and the latch is
    /// armed against the operator's configured capital from the very first sweep — where the old
    /// code seeded its HWM from the first OBSERVED equity, i.e. from whatever the wallet happened
    /// to hold at that instant.
    ///
    /// Across a RESTART the peak seeds to `max(capital_base, curve)`, not to the curve. `Account`'s
    /// four PnL terms and its positions ARE in `AccountSnapshot`, so the curve RESUMES underwater
    /// rather than resetting; seeding the peak at the base therefore keeps the drawdown measured
    /// from configured capital instead of forgiving every loss booked before the restart. On a
    /// fresh start the two are equal, so this is only ever the more protective of the two. (This
    /// matches the existing manual-reset doctrine: a `Command::SetTradingState(Active)` re-arms
    /// against the same running HWM, so a still-underwater account re-latches next bar.) The HWM
    /// itself stays core-local and is NOT serialized — putting it in the journal snapshot is a
    /// schema change, and `docs/decisions/` records schema versioning as deferred.
    ///
    /// LATCH semantics are unchanged: once tripped it STAYS Reducing even if the curve later
    /// recovers — this method NEVER un-latches (it only ever flips Active → Reducing).
    ///
    /// ⚠ A non-positive peak cannot express a FRACTION, so the latch cannot arm — reachable only
    /// with a zero/negative `seed_cash` under an opted-in `max_drawdown`. That is the one failure
    /// direction worse than the bug being fixed (protection silently off), so it is refused at
    /// mount-row load (`vike_tradehub::config::MountCfg`'s `seed_cash` refuses a non-positive value)
    /// AND announced ONCE into the ring here, for a `CoreConfig`
    /// assembled directly rather than from a profile.
    ///
    /// # Currency groups
    ///
    /// A curve adds figures of ONE currency, so the engines are folded per currency group
    /// ([`ExecutionEngine::ledger_ccy_key`]): each group has its own `(base, pnl, peak)` and the latch
    /// trips when ANY group breaches (the latch is node-wide, so one breaching group puts every engine
    /// in liquidate-only). Engine 0's group keeps `pnl_curve_peak` (a storage slot: every group is judged
    /// alike, and the published primary group is [`CoreThread::live_primary_ccy`]'s); every other group has a
    /// [`ForeignCurve`]. A node whose engines share a currency, which is every node today, has ONE
    /// group and folds exactly the terms, in exactly the order, described above; its notes read as they
    /// always did. A node with several names the group in each note. The base of a NON-dollar group is
    /// the wallets its engines adopted ([`Self::adopt_wallet_bases`]), the dollar group's the configured
    /// seeds.
    pub(crate) fn sweep_drawdown_latch(&mut self, threshold: f64) {
        self.sweep_drawdown_latch_in(threshold, RiskPass::Bar);
    }

    /// [`Self::sweep_drawdown_latch`] on either pass: the clock ([`Self::sweep_risk_clock`]) judges every
    /// engine by the age bound, the closed bar leaves a mounted engine on `config.price_cfg`. Kept out of
    /// line like the rest of the latch (see [`Self::latch_group`]).
    #[inline(never)]
    fn sweep_drawdown_latch_in(&mut self, threshold: f64, pass: RiskPass) {
        // `<= 0.0` (and NaN) is the disabled sentinel: never latch, fold nothing — byte-identical.
        if threshold <= 0.0 || threshold.is_nan() {
            return;
        }
        self.adopt_wallet_bases();
        let terms = self.drawdown_terms_in(pass);
        // Only an Active engine can trip. Already Reducing (the latch fired, or a manual set) or
        // Halted ⇒ leave it (no re-latch, no ring spam, and crucially no auto-un-latch on recovery).
        // The NODE's state (`node_floor`), not any engine's own: a primary the link dead-man halted for
        // its own venue's dead link, or an account-scoped `halt`/`reduce` of the primary (`trade
        // account-state`), must not switch the account-wide latch off for every other venue. Read once,
        // before any group trips: a group that trips does not silence the groups judged after it.
        let active = self.node_floor == TradingState::Active;
        let mixed = !terms.foreign.is_empty();
        let primary_ccy = mixed.then(|| self.engine.ledger_ccy_key());
        self.latch_group(None, primary_ccy, (terms.base, terms.pnl), active, threshold);
        for (ccy, base, pnl) in terms.foreign {
            let k = match self.foreign_curves.iter().position(|f| f.ccy == ccy) {
                Some(k) => k,
                None => {
                    self.foreign_curves.push(ForeignCurve {
                        ccy,
                        peak: None,
                        disarmed_noted: false,
                        curve: 0.0,
                    });
                    self.foreign_curves.len() - 1
                }
            };
            self.latch_group(Some(k), Some(ccy), (base, pnl), active, threshold);
        }
    }

    /// One currency group's turn in [`Self::sweep_drawdown_latch`]: fold its high-water mark, then judge
    /// its drop from it. `slot` is `None` for the primary group (its state is `pnl_curve_peak` and
    /// `pnl_curve_disarmed_noted`) and the index into `foreign_curves` otherwise; `ccy` is `Some` only
    /// on a node with several groups, and then names this one in the notes; `(base, pnl)` is the group's
    /// curve terms.
    ///
    /// `#[inline(never)]` (here, on [`Self::drawdown_terms_in`] and on `curve_terms`): the sweep runs from the
    /// strategy drive, and `nm -S` showed `dispatch` changing by 12 KB when these were free to inline into
    /// it, from a change that touched none of its source. They stay out of line, as the other cold helpers
    /// of the publish and fan-out code do.
    #[inline(never)]
    fn latch_group(
        &mut self,
        slot: Option<usize>,
        ccy: Option<vike_exec::Interned>,
        (base, pnl): (f64, f64),
        active: bool,
        threshold: f64,
    ) {
        let curve = base + pnl;
        // Kept for the publish site's group view ([`Self::state_groups`]): the curve this sweep measured.
        match slot {
            None => self.pnl_curve_last = curve,
            Some(k) => self.foreign_curves[k].curve = curve,
        }
        let peak_slot = match slot {
            None => &mut self.pnl_curve_peak,
            Some(k) => &mut self.foreign_curves[k].peak,
        };
        // Seed the HWM at `max(base, curve)` — see "Startup, restart, and the disarm" above.
        let peak = *peak_slot.get_or_insert(if curve > base { curve } else { base });
        if curve > peak {
            *peak_slot = Some(curve);
            return; // a fresh peak ⇒ zero drawdown, and (if already latched) never un-latch.
        }
        if !active {
            return;
        }
        // ` (EUR group)` on a node with several currencies, nothing on one with a single currency.
        let group_label = || {
            ccy.map_or_else(String::new, |c| {
                format!(" ({} group)", vike_exec::LedgerTotals::label_of(c))
            })
        };
        if peak <= 0.0 {
            // LOUD, not silent: a non-positive HWM means no fraction exists to compare against, so
            // an armed `max_drawdown` protects nothing. Said ONCE per core and group (the ring is a
            // bounded per-bar channel and this condition holds every sweep).
            let noted = match slot {
                None => &mut self.pnl_curve_disarmed_noted,
                Some(k) => &mut self.foreign_curves[k].disarmed_noted,
            };
            if !*noted {
                *noted = true;
                let group = group_label();
                self.note(format!(
                    "DRAWDOWN LATCH DISARMED{group}: capital base {base:.2} is not positive, so a \
                     {threshold:.4} drawdown fraction cannot be measured — set a positive \
                     `seed_cash` on the mount"
                ));
                self.dirty = true;
            }
            return;
        }
        if (peak - curve) / peak > threshold {
            let drawdown_pct = (peak - curve) / peak * 100.0;
            self.raise_trading_state(TradingState::Reducing);
            let group = group_label();
            self.note(format!(
                "DRAWDOWN LATCH{group}: liquidate-only (peak={peak:.2} curve={curve:.2} \
                 capital_base={base:.2} own_pnl={pnl:.2} drawdown={drawdown_pct:.2}%)"
            ));
            self.dirty = true;
        }
    }

    /// **A NON-dollar engine's capital base is the wallet it adopted, not the configured seed** (decision 0183,
    /// item 13, amended): the group of one account in its own currency (FXCM's EUR demo) would otherwise be
    /// judged against ONE `seed_cash` (default 1000), which is no statement about that account's money, and a
    /// 250 EUR move on a 100,000-unit position, 0.5% of its 50,000 EUR wallet, read as 25% and put the WHOLE
    /// node in liquidate-only. So the first sweep that finds such an engine [`BalanceMode::Authoritative`]
    /// writes its `balance` into its seed slot ([`CoreThread::seed_of`]'s: every reader of the capital base,
    /// the published `capital_base` included, then agrees) and moves its group's high-water mark by the
    /// difference (an addition is not a new high, as [`Self::shift_curve_peak`] says). Once per engine
    /// ([`CoreThread::base_adopted`]): a later third-party deposit or withdrawal moves neither the base nor the
    /// curve, which is why the wallet could leave this number in the first place (the sweep's doc).
    ///
    /// ⚠ **The dollar group is untouched**: its engines keep the configured seeds, so a shared demo wallet
    /// (the CI box's bybit 53,647 beside nine paper blocks) can never swamp the daemon's own book, and a core with no
    /// non-dollar engine runs a loop of key compares and changes nothing. Neither does a `Delta` engine (a
    /// paper or never-reconciled one: its seed IS its capital), a stopped one (its successor states the
    /// capital; adopting the tombstone's wallet too would count two), or a wallet that is not a positive
    /// number (it stays on the seed and is looked at again next sweep). Residual, fail-safe: after a restart the
    /// engine's own PnL resumes from its snapshot on top of a wallet that already holds it, which moves the
    /// base and the peak by that PnL (a fraction of a percent of a wallet) and trips earlier, never later.
    ///
    /// `#[inline(never)]`, cold: the latch's own sweep, like [`Self::latch_group`].
    #[inline(never)]
    fn adopt_wallet_bases(&mut self) {
        for i in 0..=self.extra_engines.len() {
            let e = self.eng(i);
            let ccy = e.ledger_ccy_key();
            let wallet = e.account.balance;
            if ccy == vike_exec::Interned::default()
                || e.account.balance_mode != BalanceMode::Authoritative
                || !wallet.is_finite()
                || wallet <= 0.0
                || self.base_adopted.contains(&i)
                || self.is_retired(i)
            {
                continue;
            }
            let seed = self.seed_of(i);
            if i == 0 {
                self.config.seed_cash = wallet;
            } else {
                self.extra_engines[i - 1].0 = wallet;
            }
            self.base_adopted.insert(i);
            self.shift_curve_peak(ccy, wallet - seed);
        }
    }

    /// Move the high-water mark of the currency group `ccy` by `delta`, when the group has one: a seed that
    /// joins or leaves the capital base moves the peak with it (an addition is not a new high, a restart is
    /// not a drawdown). The primary's group is `pnl_curve_peak`; a group the latch has not yet seen has none.
    pub(crate) fn shift_curve_peak(&mut self, ccy: vike_exec::Interned, delta: f64) {
        let slot = if ccy == self.engine.ledger_ccy_key() {
            &mut self.pnl_curve_peak
        } else if let Some(f) = self.foreign_curves.iter_mut().find(|f| f.ccy == ccy) {
            &mut f.peak
        } else {
            return;
        };
        if let Some(peak) = slot.as_mut() {
            *peak += delta;
        }
    }

    /// **The node's PRIMARY currency group: the group of the first LIVE engine** (decision 0183, item 13): the
    /// currency the published totals, the headline equity and the push drawdown alert are in. "Live" is not a
    /// tombstone (`retired_engines`: a stopped account's engine stays in the list for ever), so a primary
    /// account that stopped and started again in ANOTHER currency does not leave the headline on its dead
    /// engine's group (which would read 0 while every live account sat under "other ledgers"). With no live
    /// engine at all it is engine 0's. On a node of one currency, every node today, it is that currency.
    pub(crate) fn live_primary_ccy(&self) -> vike_exec::Interned {
        (0..=self.extra_engines.len())
            .find(|&i| !self.is_retired(i))
            .map_or_else(|| self.engine.ledger_ccy_key(), |i| self.eng(i).ledger_ccy_key())
    }

    /// The publish site's view of a node with SEVERAL currency groups (called only when
    /// [`CoreThread::ledger_mixed`]): (1) restate the three primary-group totals for the first LIVE
    /// engine's group when `snapshot::build`, which cannot see tombstones, folded the first engine's; (2)
    /// publish the drawdown curve and high-water mark the latch last measured for every group but the
    /// primary's, so a push alert can watch them ([`vike_exec::Portfolio::group_curves`]). Cold.
    #[inline(never)]
    pub(crate) fn state_groups(&self, pf: &mut vike_exec::Portfolio) {
        let live = self.live_primary_ccy();
        if live != self.engine.ledger_ccy_key() {
            pf.primary_ccy_stated = Some(live);
            pf.equity_total = vike_model::py_sum(
                pf.venues.iter().filter(|v| v.ledger_ccy == live).map(|v| v.equity),
            );
            pf.capital_base = vike_model::py_sum(
                std::iter::once(self.config.seed_cash)
                    .chain(self.extra_engines.iter().map(|(s, _)| *s))
                    .zip(pf.venues.iter())
                    .filter(|(_, v)| v.ledger_ccy == live)
                    .map(|(s, _)| s),
            );
            pf.margin_used_total =
                pf.venues.iter().filter(|v| v.ledger_ccy == live).map(|v| v.margin_used).sum();
        }
        let first = self.engine.ledger_ccy_key();
        let slot0 = self.pnl_curve_peak.map(|peak| (first, self.pnl_curve_last, peak));
        let others = self.foreign_curves.iter().filter_map(|f| f.peak.map(|p| (f.ccy, f.curve, p)));
        pf.group_curves = slot0
            .into_iter()
            .chain(others)
            .filter(|(ccy, _, _)| *ccy != live)
            .map(|(ccy, curve, peak)| vike_exec::GroupCurve { ccy, curve, peak })
            .collect();
    }

    /// Say, at `warn` and once per boot and per hot add, that this node's engines are in more than one
    /// currency and which: the totals it publishes are the first live engine's currency, every other group
    /// is listed beside them and judged by the drawdown latch on its own (any breach puts the whole node in
    /// liquidate-only). Silent for a node of one currency. Cold; the line [`CoreThread::ledger_mixed`] gates.
    pub(crate) fn report_currency_groups(&self) {
        if !self.ledger_mixed {
            return;
        }
        let mut groups: Vec<&'static str> = Vec::new();
        let mut rows: Vec<String> = Vec::new();
        for i in 0..=self.extra_engines.len() {
            let e = self.eng(i);
            let group = vike_exec::LedgerTotals::label_of(e.ledger_ccy_key());
            if !groups.contains(&group) {
                groups.push(group);
            }
            rows.push(format!("{}={group}", e.route_key));
        }
        tracing::warn!(
            target: "vike_core::core",
            groups = %groups.join(","),
            "this node's engines are in MORE THAN ONE currency ({}): the published totals, the headline \
             equity and the drawdown alert are the first live engine's currency only, each other currency \
             is listed beside them, and the drawdown latch judges every currency on its own (a breach of \
             any one puts the whole node in liquidate-only) (decision 0183, item 13)",
            rows.join(", ")
        );
    }

    /// [`Self::drawdown_terms_in`] on the closed-bar pass: the reader the tests use.
    #[cfg(test)]
    pub(crate) fn drawdown_terms(&mut self) -> DrawdownTerms {
        self.drawdown_terms_in(RiskPass::Bar)
    }

    /// The drawdown curve's terms, exactly as [`Self::sweep_drawdown_latch`] folds them: `(capital base, own
    /// PnL)` of engine 0's currency group, and the same pair for every other group. ALWAYS measured: no
    /// engine's staleness makes the curve unmeasurable (see below). Split out so the number the latch acts
    /// on can be read by a test without a threshold, and by nothing else.
    #[inline(never)]
    fn drawdown_terms_in(&mut self, pass: RiskPass) -> DrawdownTerms {
        let cfg = self.config.price_cfg;
        // THE ENGINES THE AGE BOUND JUDGES (only with `CoreConfig::price_only`; otherwise `None` and nothing
        // below differs; the price-only engines on either pass, every live engine on the clock pass): each
        // is priced by its own bounded cfg while its marks are fresh, and a SKIP-for-another-network engine
        // is out of the curve altogether (its seed and its PnL: a constant property, so the high-water mark
        // stays coherent).
        //
        // ⚠ **A STALE ENGINE IS VALUED AT ITS LAST KNOWN PRICE, never skipped** (owner ruling 2026-10-25;
        // before it a stale price-only engine made the whole curve "unmeasurable" and the latch acted for
        // NO account while one pump was quiet). The cases that blinded the node: an FX weekend (Friday's
        // EURUSD beside a Bybit loss on Saturday), a dead pump in an open market (Dukascopy's USDJPY with no
        // tick for two minutes), a mount whose feed is silent for a restart or a deploy, a mount on bar
        // closes alone (stale a minute after each close) and an event-driven book (stale always). The
        // engine is priced permissively (`config.price_cfg`): the last price it had, which is what a closed
        // bar always used for a mounted engine. A frozen price can only repeat the number it already gave,
        // and the other engines keep moving the curve, so measuring beats not measuring. It is not silent:
        // `price_only_basis` says it once per episode (a WARN naming the engine, the age and this stance).
        // Only the MARGIN CALL and the mount budgets skip a stale engine (they act on orders, so they act
        // on fresh numbers only). A STOPPED engine is the same case without the noise (it is always stale).
        let per_engine: Option<Vec<Option<PriceCfg>>> = if self.config.price_only.is_some() {
            let mut out = Vec::with_capacity(self.extra_engines.len() + 1);
            for i in 0..=self.extra_engines.len() {
                out.push(match self.price_only_basis(i, pass) {
                    PriceBasis::Mounted | PriceBasis::Skip(SKIP_STALE) => Some(cfg),
                    PriceBasis::Bounded(c) => Some(c),
                    PriceBasis::Skip(_) => None,
                });
            }
            Some(out)
        } else {
            None
        };
        // `py_sum` over (primary, then extras) for BOTH halves — the identical fold law and the
        // identical order `snapshot::build` uses for `Portfolio::capital_base` /
        // `Portfolio::pnl_total`, so this decision number and the number a `RuleTrigger::Drawdown`
        // alert reads out of the published snapshot are bit-identical rather than merely close.
        // `crates/vike-core/tests/drawdown_measures_own_pnl.rs`'s
        // `the_latch_curve_and_the_published_curve_are_bit_identical` is the pin.
        //
        // A curve adds figures of ONE currency (decision 0183, item 13): the engines are folded per
        // currency group, the PRIMARY engine's first. With every engine in one group (every node today)
        // `member` is true for all of them and these are the folds, over the engines, in the order, that
        // this method always had.
        let primary = self.engine.ledger_ccy_key();
        let mixed = self.extra_engines.iter().any(|(_, e)| e.ledger_ccy_key() != primary);
        let per_engine = per_engine.as_deref();
        let (base, pnl) = self
            .curve_terms(per_engine, &cfg, |i| !mixed || self.eng(i).ledger_ccy_key() == primary);
        let mut foreign: Vec<(vike_exec::Interned, f64, f64)> = Vec::new();
        if mixed {
            let mut seen = vec![primary];
            for (_, e) in &self.extra_engines {
                let ccy = e.ledger_ccy_key();
                if seen.contains(&ccy) {
                    continue;
                }
                seen.push(ccy);
                let (base, pnl) =
                    self.curve_terms(per_engine, &cfg, |i| self.eng(i).ledger_ccy_key() == ccy);
                foreign.push((ccy, base, pnl));
            }
        }
        DrawdownTerms { base, pnl, foreign }
    }

    /// `(capital base, own PnL)` of the engines `member` accepts (by routing index, primary 0), `py_sum` over
    /// them in registration order, and in a price-only run over the engines whose cfg is `Some`.
    #[inline(never)]
    fn curve_terms(
        &self,
        per_engine: Option<&[Option<PriceCfg>]>,
        cfg: &PriceCfg,
        member: impl Fn(usize) -> bool,
    ) -> (f64, f64) {
        let seeds = std::iter::once(self.config.seed_cash)
            .chain(self.extra_engines.iter().map(|(s, _)| *s));
        match per_engine {
            None => (
                vike_model::py_sum(seeds.enumerate().filter(|(i, _)| member(*i)).map(|(_, s)| s)),
                vike_model::py_sum(
                    (0..=self.extra_engines.len())
                        .filter(|i| member(*i))
                        .map(|i| self.curve_pnl_of(i, cfg)),
                ),
            ),
            Some(cfgs) => (
                vike_model::py_sum(
                    seeds
                        .zip(cfgs)
                        .enumerate()
                        .filter(|(i, (_, c))| member(*i) && c.is_some())
                        .map(|(_, (s, _))| s),
                ),
                vike_model::py_sum((0..).zip(cfgs).filter_map(|(i, c)| {
                    c.as_ref().filter(|_| member(i)).map(|c| self.curve_pnl_of(i, c))
                })),
            ),
        }
    }

    /// Engine `i`'s own-PnL term of the drawdown curve: `resolved_own_pnl`, a stopped engine's included (it
    /// stays in the list, so its booked PnL and its unrealized count for the process life, in ITS currency
    /// group).
    fn curve_pnl_of(&self, i: usize, cfg: &PriceCfg) -> f64 {
        self.eng(i).resolved_own_pnl(cfg)
    }

    /// Resolver-priced per-mount valuation (steal/core-per-mount-budget): `(net position, realized
    /// PnL net of fees, marked unrealized PnL, gross notional)` for mount `i` from its attribution
    /// ledger, priced through the SAME `config.price_cfg` resolver the drawdown latch + snapshot use
    /// (the one-price law). A FLAT mount short-circuits before touching the resolver; an unpriceable
    /// position contributes 0 unrealized / 0 notional (like every other resolver caller). Two
    /// callers: the per-closed-bar budget sweep, and every publish's mount view (`mount_views`) —
    /// and publish runs per event on a sporadic feed, so the second is NOT off the gated hop.
    #[inline]
    pub(crate) fn mount_valuation(&self, i: usize) -> (f64, f64, f64, f64) {
        self.mount_valuation_at(i, &self.config.price_cfg)
    }

    /// [`Self::mount_valuation`] priced by `cfg`: the permissive `config.price_cfg` for the views and the
    /// closed-bar sweep, the age-bounded cfg of [`Self::price_only_basis`] for the clock's sweep.
    fn mount_valuation_at(&self, i: usize, cfg: &PriceCfg) -> (f64, f64, f64, f64) {
        let attr = self.mount_attr[i];
        let realized_net = attr.realized_pnl - attr.fees_paid;
        if attr.size == 0.0 {
            return (0.0, realized_net, 0.0, 0.0);
        }
        let (venue, symbol) = &self.mount_vs[i];
        // THE MOUNT'S OWN ENGINE ([`CoreThread::mount_eng`]), not the venue's default account: this
        // prices the LATCH's own decision, and the contract multiplier it reads is a per-ENGINE
        // grid (a venue-fetched grid on a live mount, empty — so 1.0 — on one that fell back to
        // paper). A labelled mount whose venue's default account is capped to paper was having its
        // gross notional folded at the default account's multiplier, so its `max_notional` arm
        // latched at the wrong threshold in whichever direction the real multiplier ran. Identical
        // on a single-account core, where `mount_eng` IS this venue lookup.
        let eidx = self.mount_eng(i);
        let value = self.eng(eidx).account.contract_value_of(symbol);
        match self.eng(eidx).resolved_position_price(venue, symbol, attr.size, cfg) {
            Some(px) => {
                let unrealized = value.unrealized_pnl(attr.avg_px, px, attr.size);
                let notional = value.gross_notional(attr.size, px);
                (attr.size, realized_net, unrealized, notional)
            }
            None => (attr.size, realized_net, 0.0, 0.0),
        }
    }

    /// Per-mount loss/notional BUDGET sweep (steal/core-per-mount-budget) — the SCOPED sibling of
    /// [`Self::sweep_drawdown_latch`]: same CLOSED-bar pass + resolver-priced source (marks
    /// fresh, off the event fold), but it latches ONE mount at a time. For each not-yet-latched
    /// mount with an active [`MountBudget`], it prices the mount's attributed ledger
    /// ([`Self::mount_valuation`]) and — if its LOSS (realized net of fees + marked unrealized)
    /// exceeds `max_loss` OR its gross NOTIONAL exceeds `max_notional` — latches it liquidate-only
    /// via [`Self::latch_mount`], leaving every other mount trading. Gated by `any_mount_budget` at
    /// the call site, so a budget-free runtime never reaches here. The risk clock runs it too
    /// ([`Self::sweep_risk_clock`]), between bars.
    pub(crate) fn sweep_mount_budgets(&mut self, now: i64) {
        self.sweep_mount_budgets_in(now, RiskPass::Bar);
    }

    /// [`Self::sweep_mount_budgets`] on either pass. On the clock a mount is priced by the age bound of its
    /// ENGINE and a stale one is SKIPPED (said once, with the margin call's note): the latch cancels orders
    /// and may flatten, so it acts on fresh numbers only, and the mount's closed bars still judge it. Fire-once
    /// as ever (`mount_latched`), so the two passes cannot both latch a mount; the flatten it releases is
    /// journaled write-ahead by [`Self::latch_mount`], whichever pass fires it.
    #[inline(never)]
    fn sweep_mount_budgets_in(&mut self, now: i64, pass: RiskPass) {
        for i in 0..self.mounts.len() {
            if self.mount_latched[i] {
                continue;
            }
            let Some(budget) = self.mount_budget[i] else { continue };
            if !budget.is_active() {
                continue;
            }
            // A STOPPED account's mount (decision 0169) is skipped, as the margin call skips its engine:
            // the flatten would be refused (its client is detaching) and the fire-once latch spent on it,
            // leaving the position unjudged for good once the account starts again.
            if self.is_retired(self.mount_eng(i)) {
                continue;
            }
            let cfg = if pass == RiskPass::Bar {
                self.config.price_cfg
            } else {
                match self.price_only_basis(self.mount_eng(i), pass) {
                    PriceBasis::Mounted => self.config.price_cfg,
                    PriceBasis::Bounded(c) => c,
                    PriceBasis::Skip(_) => continue,
                }
            };
            let (_size, realized_net, unrealized, notional) = self.mount_valuation_at(i, &cfg);
            let loss = -(realized_net + unrealized);
            let loss_breach = budget.max_loss.is_some_and(|ml| ml > 0.0 && loss > ml);
            let notional_breach = budget.max_notional.is_some_and(|mn| mn > 0.0 && notional > mn);
            if loss_breach || notional_breach {
                self.latch_mount(i, now, loss, notional);
            }
        }
    }

    /// Latch ONE mount liquidate-only after a budget breach (steal/core-per-mount-budget): set its
    /// fire-once `mount_latched` flag, cancel EXACTLY its own resting orders (its attributed coids —
    /// so a sibling mount on the same symbol is untouched), and — when `flatten_on_breach` — submit
    /// a reduce-only MARKET to close its attributed net position, journaled write-ahead exactly like
    /// a margin-call liquidation (same [`vike_journal::JournalRecord::MarginCallLiquidate`] shape +
    /// replay contract: replay re-applies it as a `Submit`, re-minting the same coid — so a
    /// budget-flatten session replays deterministically like a margin-call one). This NEVER touches
    /// `engine.trading_state` — that is the ACCOUNT-wide drawdown/HALT latch; this is scoped to one
    /// mount so every other mount keeps trading. Cold path (per breach, off the fold).
    fn latch_mount(&mut self, i: usize, now: i64, loss: f64, notional: f64) {
        self.mount_latched[i] = true;
        let (venue, symbol) = self.mount_vs[i].clone();
        let budget = self.mount_budget[i];
        let attr = self.mount_attr[i];
        self.note(format!(
            "MOUNT BUDGET LATCH: mount {i} ({venue}/{symbol}) liquidate-only \
             (loss={loss:.2} notional={notional:.2})"
        ));
        // (1) cancel THIS mount's resting orders, scoped by its attributed coids (NOT a
        // (venue,symbol) MassCancel, which would also cancel a sibling mount's orders on the same
        // symbol). Terminal/unknown coids are no-ops in the engine's cancel path, so passing every
        // attributed coid is safe. Replay-neutral like the GTD sweep's cancel: the venue's
        // authoritative `OrderCanceled` is journaled as its own `Ingest::Event` and replays
        // independently, so no cancel record is needed here.
        let coids: Vec<String> =
            self.coid_mount.iter().filter(|&(_, &m)| m == i).map(|(c, _)| c.clone()).collect();
        if !coids.is_empty() {
            // RISK-OFF, declared: the latch is fire-once, so a cancel a venue held back is never
            // re-offered and the mount stays liquidate-only with live orders out.
            self.apply_intent_with_cancel_intent(
                OrderIntent::CancelBatch(coids),
                now,
                CancelIntent::RiskOff,
            );
        }
        // (2) optional flatten of the mount's attributed net position, a reduce-only MARKET journaled
        // write-ahead like the margin-call sweep (same shape + replay contract). Skipped when flat.
        //
        // ⚠ CLAMPED to what the book still holds after the reduce-only orders already in flight
        // ([`Self::flatten_qty`]): one clock sweep runs the margin call first, which has just released a
        // reduce-only close of its excess, and the gate nets in-flight reduce-only orders only against a
        // FLAT book (`ExecutionEngine::cover_position_size`), so on a non-flat book it admits this one
        // too and the two add up to more than the position. A venue that sends `reduce_only` on the wire
        // trims the second; oanda, ig, fxcm, ibkr, ctrader and alpaca do not, and the position flips.
        let flatten = budget.is_some_and(|b| b.flatten_on_breach);
        let want = if flatten { attr.size.abs() } else { 0.0 };
        let qty = if want > 1e-12 {
            self.flatten_qty(i, &symbol, vike_model::closing_side(attr.size), want)
        } else {
            0.0
        };
        if want > 1e-12 && qty < want {
            let what = if qty > 1e-12 { "clamped" } else { "skipped" };
            self.note(format!(
                "MOUNT BUDGET LATCH: mount {i} ({venue}/{symbol}) flatten {what}: it wanted {want:.8}, \
                 the book can still take {qty:.8} beside the closes already in flight"
            ));
        }
        if qty > 1e-12 {
            let req = OrderRequest {
                client_order_id: String::new(),
                venue: venue.clone(),
                symbol: symbol.clone(),
                side: vike_model::closing_side(attr.size),
                qty,
                order_type: vike_model::OrderType::Market,
                reduce_only: true,
                ts: now,
                ..Default::default()
            };
            // …and the ACCOUNT (journal v16, §9 item 12): the latched mount's own engine, which is
            // the book this flatten closes. `mount_id` says WHOSE loss it is; this says WHICH of an
            // exchange's accounts holds it — two different questions, and a multi-account venue is
            // where they stop having one answer. Resolved before the journal borrow.
            let rk = self.journal_route_key(self.mount_eng(i), &req.venue);
            if let Some(j) = self.journal.as_mut() {
                // Stamp the OWNING mount id (journal v14). Without it this record is
                // indistinguishable on disk from the account-wide margin-call sweep's, and
                // `replay::fold_coid_mounts` — which rebuilds attribution FROM the journal — had no
                // owner to credit: after a restart this flatten's fill booked into the RESIDUAL row
                // instead of the mount whose budget breach caused it, so that mount's ledger came
                // back under-reporting the very loss the latch exists to bound. The in-memory
                // `own_coid` write below was always right; only the durable half was missing.
                let mid = self.mount_ids[i].clone();
                j.append_margin_call_liquidate(now, &req, Some(&mid), rk.as_deref())
                    .expect("journal append");
                self.journaled_since_snap += 1;
            }
            // Attribute the flatten back to the mount so its own fill folds the ledger flat (and
            // realizes the loss). `apply_intent` mints + submits; its follow-on `MintedSubmit` ties
            // the coid for the materializer, exactly as the margin-call path does.
            // ⚠ `EngineRoute::Mount(i)` — the LATCHED MOUNT's own engine. Everything else in this
            // function is already mount-scoped (the attributed coids it cancels, the attributed
            // size it closes, the mount id it stamps on the journal record), but the flatten itself
            // lowered through `apply_intent` and was routed by the payload's venue: a labelled
            // mount's budget breach submitted its reduce-only close to the venue's DEFAULT account,
            // leaving the position the latch exists to bound wide open and opening an unwanted one
            // beside it. Routing it by the mount is also what keeps the `coid_mount` attribution
            // below honest — the fill has to come back through the engine that holds the position.
            let flat_coids = self.apply_intent_routed(
                OrderIntent::Submit(Box::new(req)),
                now,
                CancelIntent::Unspecified,
                EngineRoute::Mount(i),
            );
            for c in flat_coids {
                self.own_coid(c, i, None);
            }
        }
        self.dirty = true;
    }
}
