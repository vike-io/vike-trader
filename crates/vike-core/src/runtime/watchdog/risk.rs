//! Risk sweeps: the margin call, the equity-drawdown latch and the per-mount budget latch.

use super::*;

/// One open row of a mount's ledger ([`CoreThread::mount_open_rows`]): `(venue, account, symbol, signed size)`,
/// the account `Some` only for a row of another account of the venue (decision 0293).
pub(crate) type MountOpenRow =
    (String, Option<vike_model::accounts::account_keys::AccountLabel>, String, f64);

/// How a risk sweep may price one engine ([`CoreThread::price_only_basis`]).
#[derive(Debug, Clone, Copy)]
pub(crate) enum PriceBasis {
    /// Not judged by the age bound: priced by `config.price_cfg`, as it always was (a mounted engine on the
    /// closed-bar pass, a stopped one on either).
    Mounted,
    /// An engine with usable marks that the age bound judges: priced by this cfg (every rung capped at the
    /// bound, no stale fallback), so a rung past the bound is no price.
    Bounded(PriceBoardConfig),
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

/// What [`CoreThread::reoffer_mount_flatten`] did for a latched mount. The sweeps ignore it; the operator's
/// un-latch ([`CoreThread::unlatch_mount`]) says it in the refusal of a mount that still holds a position.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Reoffer {
    /// The mount is flat: nothing to offer.
    Flat,
    /// Not offered now: the mount's engine is halted (the gate refuses even a close there) or its trade link is
    /// down (the close would be rejected at once). It goes out at a sweep after the operator's `resume`.
    Waits,
    /// Not offered again: a live reduce-only close already covers the position (the first flatten, the margin
    /// call's, an operator's), and one is never stacked on another.
    InFlight,
    /// A reduce-only market close of this quantity was sent.
    Sent(f64),
}

/// The drawdown curve's terms ([`CoreThread::drawdown_terms_in`]): the group of ENGINE 0's
/// `(capital base, own PnL)` and, for each OTHER group, the same pair under its currency key. Engine 0's
/// group is only where the latch stores its first slot (`pnl_curve_peak`); every group is judged alike, so
/// which one is stored first changes no verdict, and the PUBLISHED primary group is another question
/// ([`CoreThread::live_primary_currency`], the first LIVE engine's).
#[derive(Debug)]
pub(crate) struct DrawdownTerms {
    pub(crate) base: f64,
    pub(crate) pnl: f64,
    /// `(currency key, capital base, own PnL)` per other group, first-seen order. EMPTY, and never
    /// allocated, when every engine shares the primary's currency.
    pub(crate) foreign: Vec<(vike_execution::Interned, f64, f64)>,
}

/// Whether `e` states the `paper` tier (`Engine::stated_tier`): the engines that start on the one paper balance
/// (`vike_tradehub_core::engine_seed` reads the same statement), and the only ones a group of real engines leaves out of
/// its drawdown curve (`CoreThread::counts_in_curve`). The `Paper` SEED of an engine nobody told is no statement.
fn states_paper<C: vike_execution::ExecutionClient>(e: &vike_execution::Engine<C>) -> bool {
    e.stated_tier() == Some(vike_model::AccountTier::Paper)
}

/// Why a price-only engine is out of the risk sweeps: a position's mark is older than the bound.
const SKIP_STALE: &str =
    "a marked position's price is older than the price-only bound (a dead pump?)";
/// Why a price-only engine is out of the risk sweeps: its book is another network's.
const SKIP_NETWORK: &str = "its price feed dials another network than the account trades on";
/// The risk clock's age-bounded price cfg: every rung of `base` capped at `max_age` (a rung past the bound is
/// no price), no stale fallback, and the last-known walk on RECENCY, so the age a skip line names is the age of
/// the NEWEST price the board holds for the position, which is the price [`CoreThread::last_known_config`] values a
/// stale engine at. A held order's price condition reads through it too (`held_release.rs`'s `release_price`).
pub(crate) fn bounded_config(
    base: PriceBoardConfig,
    max_age: std::time::Duration,
) -> PriceBoardConfig {
    let ms = i64::try_from(max_age.as_millis()).unwrap_or(i64::MAX);
    let cap = |own: Option<i64>| Some(own.map_or(ms, |o| o.min(ms)));
    PriceBoardConfig {
        mark_max_age_ms: cap(base.mark_max_age_ms),
        quote_max_age_ms: cap(base.quote_max_age_ms),
        trade_max_age_ms: cap(base.trade_max_age_ms),
        bar_max_age_ms: cap(base.bar_max_age_ms),
        stale_fallback: false,
        last_known_by_recency: true,
        ..base
    }
}

/// How many clock intervals a skipped engine must stay fresh before its skip EPISODE ends
/// ([`CoreThread::price_only_recovering`]): a feed that gaps around the age bound is one episode, not one per
/// gap. Three keeps a gap every other sweep, or every third, in one episode, and costs nothing else: the hold
/// only delays the SAYING of the next outage, never the pricing of a fresh engine.
const RECOVERY_SWEEPS: i64 = 3;

impl<C: ExecutionClient> CoreThread<C> {
    /// How the margin call and the drawdown latch may price engine `engine_index` (decision 0038's neighbour: never
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
    ///   off an older rung that the permissive default `price_config` would still pick.
    ///
    /// A skip is said ONCE per episode ([`CoreThread::price_only_skipped`]), not at every 30 s sweep, and
    /// said again if the reason changes or the engine recovers and fails later; an episode ends only after the
    /// engine has stayed fresh for [`RECOVERY_SWEEPS`] intervals, so a feed that gaps at the age bound is one
    /// episode ([`CoreThread::price_only_recovering`]); a stale one is also a WARN
    /// naming the engine, the age of its oldest marked price and, only when a latch is configured, what the
    /// latch does with it. Cold: per sweep.
    pub(crate) fn price_only_basis(&mut self, engine_index: usize, pass: RiskPass) -> PriceBasis {
        let Some(po) = self.config.price_only.as_ref() else { return PriceBasis::Mounted };
        let engine = self.engine(engine_index);
        let price_only = po.venues.contains(engine.venue.as_str());
        let retired = self.is_retired(engine_index);
        if !price_only && (pass == RiskPass::Bar || retired) {
            return PriceBasis::Mounted;
        }
        let config = bounded_config(self.config.price_config, po.max_price_age);
        let other_network = self.off_network(engine_index);
        // A STOPPED engine (decision 0169) is not a dead pump: its board never moves again, so it reads stale for
        // ever, and it keeps its position (the operator's to close). It is valued at its last known price by
        // everything that still asks (the latch: a loss is never forgiven by stopping the account), without a
        // skip line. A stopped engine of another network than its feed is still out of the curve, below.
        if retired && !other_network {
            self.price_only_skipped.remove(&engine_index);
            self.price_only_recovering.remove(&engine_index);
            return PriceBasis::Mounted;
        }
        let stale = if other_network { Vec::new() } else { engine.stale_marks(&config) };
        let reason = if other_network {
            Some(SKIP_NETWORK)
        } else if !stale.is_empty() {
            Some(SKIP_STALE)
        } else {
            None
        };
        // **THE HOLD-DOWN on the recovered side of an episode.** A feed that gaps for about as long as the age
        // bound flips the engine between stale and fresh at the clock's sweeps, and every flip used to end the
        // episode and start a new one: a WARN and a ring line at every other sweep. The episode in
        // `price_only_skipped` now ends only after the engine has stayed fresh for `RECOVERY_SWEEPS` clock
        // intervals (the instant of the first fresh sweep is in `price_only_recovering`), counted on the
        // engine's own clock so the several callers of one sweep (the margin call, the latch, the budgets) add
        // up to one observation; it is checked on both sides, so a quiet spell with no sweep between a fresh
        // observation and a later stale one is judged by the time that passed. Only the SAYING waits: a fresh
        // engine is priced and acted on at once, below.
        let now = self.engine(engine_index).now_ms;
        let hold = self.price_only_tick_ms.saturating_mul(RECOVERY_SWEEPS);
        if let Some(&since) = self.price_only_recovering.get(&engine_index)
            && now.saturating_sub(since) >= hold
        {
            self.price_only_recovering.remove(&engine_index);
            self.price_only_skipped.remove(&engine_index);
        }
        let Some(reason) = reason else {
            if self.price_only_skipped.contains_key(&engine_index) {
                let since = *self.price_only_recovering.entry(engine_index).or_insert(now);
                if now.saturating_sub(since) >= hold {
                    self.price_only_recovering.remove(&engine_index);
                    self.price_only_skipped.remove(&engine_index);
                }
            }
            return PriceBasis::Bounded(config);
        };
        self.price_only_recovering.remove(&engine_index); // stale again: the hold-down starts over
        if self.price_only_skipped.insert(engine_index, reason) != Some(reason) {
            let (venue, route) = (
                self.engine(engine_index).venue.clone(),
                self.engine(engine_index).route_key.clone(),
            );
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
    /// ([`Self::sweep_mount_budgets_in`]: fire-once, same staleness rule), and so does the delivery-window cancel of
    /// our resting opening orders on a dated contract ([`Self::sweep_delivery_window`]). Cold, once per interval, never
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
        self.hold_coin_seeds();
        if let Some(config) = self.config.margin_call {
            for engine_index in (0..=self.extra_engines.len()).rev() {
                self.clock_stage("margin call", Some(engine_index), |c| {
                    c.sweep_margin_call_engine(engine_index, &config, now, RiskPass::Clock);
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
        // Our resting OPENING orders on a dated contract whose delivery window has begun (`delivery_window.rs`).
        self.clock_stage("delivery window", None, |c| c.sweep_delivery_window(now));
    }

    /// Phase B margin-call sweep (LEAN DefaultMarginCallModel over the live Account), the CLOSED-BAR pass.
    /// Warning lands in the recent-events ring; liquidation intents become reduce-only
    /// MARKET orders submitted through the ONE live path (mint → RiskGate → client) — a
    /// margin call can never bypass the gate (a veto surfaces as OrderDenied, LEAN's
    /// user-hook analog).
    pub(crate) fn sweep_margin_call(&mut self, config: &vike_model::MarginCallConfig, now: i64) {
        // full LEAN model PER ENGINE — each account is checked at its own seed and its
        // liquidations submit through THAT engine, named by INDEX (`EngineRoute::Engine`) rather
        // than by the position's venue string, which two accounts of one exchange share.
        for engine_index in (1..=self.extra_engines.len()).rev() {
            self.sweep_margin_call_engine(engine_index, config, now, RiskPass::Bar);
        }
        self.sweep_margin_call_engine(0, config, now, RiskPass::Bar);
    }

    /// One engine's margin call, on either pass. `#[inline(never)]`: cold, and two passes call it now, so it
    /// must not be folded into the strategy drive that `dispatch` reaches.
    #[inline(never)]
    fn sweep_margin_call_engine(
        &mut self,
        engine_index: usize,
        config: &vike_model::MarginCallConfig,
        now: i64,
        pass: RiskPass,
    ) {
        // A STOPPED account (decision 0169) has a detaching client: a liquidation sent to it could only
        // be refused. Its open position is the operator's to close, and was said when it stopped.
        if self.is_retired(engine_index) {
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
        let wall = self.engine(engine_index).now_ms;
        if self.margin_call_quiet_until.get(&engine_index).is_some_and(|&until| wall < until) {
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
        // is. `vike_execution::Engine::sizing_equity`'s doc is the authority for the split and
        // names which consumer sits on which side.
        // ⚠ An engine the age bound judges (a price-only one; every one on the clock pass) is priced by
        // `price_only_basis` (bounded age) or not at all; a mounted one on the bar pass keeps
        // `config.price_cfg` exactly.
        let pcfg = match self.price_only_basis(engine_index, pass) {
            PriceBasis::Mounted => self.config.price_config,
            PriceBasis::Bounded(config) => config,
            PriceBasis::Skip(_) => return,
        };
        let engine = self.engine(engine_index);
        let equity = engine.resolved_equity(self.seed_of(engine_index), &pcfg);
        let verdict = vike_execution::check_margin_call_priced(
            &engine.account,
            equity,
            config,
            |(v, s, _side), p| engine.resolved_position_price(v, s, p.size, &pcfg),
        );
        match verdict {
            vike_execution::MarginCall::Healthy => {
                self.margin_warning_held.remove(&engine_index);
            }
            vike_execution::MarginCall::Warning { margin_used, margin_remaining } => {
                // A closed bar says a holding band at every bar, as it always did. The CLOCK says it
                // once per EPISODE (until a sweep finds the engine out of the band): the recent ring is
                // shared with fills and rejects, and a band that holds for an hour would put 120 lines
                // of one warning into it.
                let first = self.margin_warning_held.insert(engine_index);
                if pass == RiskPass::Bar || first {
                    self.note(format!(
                        "margin-call WARNING: used={margin_used:.2} remaining={margin_remaining:.2}"
                    ));
                    self.dirty = true;
                }
            }
            vike_execution::MarginCall::Liquidate(intents) => {
                // ⚠ **NET THE CLOSES ALREADY IN FLIGHT** (third review, 1). The quiet window below only
                // guards against this engine's OWN last release; a close it did not send (a manual Flatten,
                // a MarketExit, a stuck pre-ack close) arms none, and a close of its own still unfilled after
                // one interval was sent AGAIN. The gate nets in-flight reduce-only orders only against a FLAT
                // book, and ig, fxcm, ibkr, ctrader and alpaca put no reduce-only flag on the wire (oanda
                // says `positionFill: REDUCE_ONLY` since 2026-11-09), so a second full-size sell flips the
                // position. Each intent now asks for what is STILL
                // missing: its quantity less the market closes out on that symbol and side; the CLAMPED
                // quantity is what is journaled write-ahead and sent, and an intent with nothing left is
                // dropped. Need-based, so a close that stays unfilled for an hour is not stacked on.
                let asked = intents.len();
                let intents = self.net_intents(engine_index, intents);
                if intents.is_empty() {
                    // The breach stands and every close it asks for is already out: nothing to add, no quiet
                    // window to arm (the closes in flight ARE the guard). Said once per episode on the clock
                    // (the warning's own marker: an episode ends at the first sweep that finds the engine
                    // out of the breach), at every closed bar on the bar pass.
                    if pass == RiskPass::Bar || self.margin_warning_held.insert(engine_index) {
                        self.note(format!(
                            "margin-call breach COVERED: {asked} close(s) already in flight, none added"
                        ));
                        self.dirty = true;
                    }
                    return;
                }
                self.margin_warning_held.remove(&engine_index);
                self.note(format!("MARGIN CALL: liquidating {} position(s)", intents.len()));
                for it in intents {
                    let req = OrderRequest {
                        order_client_id: String::new(),
                        venue: it.venue,
                        symbol: it.symbol,
                        side: it.side,
                        qty: it.qty,
                        order_type: vike_model::OrderType::Market,
                        reduce_only: true,
                        timestamp: now,
                        ..Default::default()
                    };
                    // WRITE-AHEAD (emulator PR-4): journal the released reduce-only MARKET BEFORE
                    // `apply_intent`, exactly as `submit_fired` journals a conditional FIRE. The
                    // sweep is a runtime reaction to the account's marks + equity on a CLOSED BAR or
                    // the CLOCK — neither is journaled — so without this record a replay could not
                    // reproduce the liquidation and the determinism fence caught it as a
                    // `HashMismatch`. The record carries the request with its order client id still EMPTY (the
                    // mint happens inside `apply_intent`, which then writes its own `MintedSubmit`);
                    // replay re-applies it as `Command::Order(Submit(req))` through the SAME
                    // one-write path and re-mints the identical order client id, never re-evaluating the breach.
                    // Off-fold (per closed bar or per clock interval, never the p99 per-message
                    // path) and gated on journaling being on, so the no-journal path stays
                    // byte-identical.
                    // §9 item 12: the RESOLVED route key of the BREACHING engine — this
                    // liquidation closes THAT book, and `req.venue` names an exchange rather than
                    // an account. Resolved BEFORE the journal borrow (the `&self` engine read and
                    // the `&mut self.journal` append cannot overlap), and `None` on every
                    // single-account box, so its journal bytes do not move.
                    let rk = self.journal_route_key(engine_index, &req.venue);
                    if let Some(j) = self.journal.as_mut() {
                        // `mount_id: None` — this is the ACCOUNT-wide sweep. It liquidates a
                        // POSITION, not a strategy's book, so no mount owns the released order and
                        // its fill belongs in the unattributed residual row. (The per-mount budget
                        // latch is the `Some` case; see `latch_mount`.)
                        j.append_margin_call_liquidate(now, &req, None, rk.as_deref())
                            .expect("journal append");
                        self.journaled_since_snap += 1;
                    }
                    // ⚠ `EngineRoute::Engine(engine_index)` — THE BREACHING ENGINE, not the venue's default
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
                    let order_client_ids = self.apply_intent_routed(
                        OrderIntent::Submit(Box::new(req)),
                        now,
                        OrderCancelIntent::Unspecified,
                        EngineRoute::Engine(engine_index),
                    );
                    // The close LIQUIDATES what the mounts hold: their closed trades book its fills
                    // (`closed_trades.rs`'s "What is booked"). Nothing to remember on a mount-less core.
                    // shortcut: a close that ends UNFILLED stays in the set (a few ids per margin call, for
                    // the process's life); drop it at its terminal event if a node ever shows it growing.
                    if !self.mounts.is_empty() {
                        self.liquidation_order_client_ids.extend(order_client_ids);
                    }
                }
                if self.price_only_tick_ms > 0 {
                    // Nine tenths of an interval, not a whole one: the clock is phase-locked to its schedule
                    // (`CoreThread::rearm_periodic`), so two sweeps are one interval apart PLUS the difference
                    // of their lateness, which can be negative. A whole-interval window would let a sweep that
                    // runs a few milliseconds early skip the engine's retry for a second interval.
                    let window = self.price_only_tick_ms - self.price_only_tick_ms / 10;
                    self.margin_call_quiet_until.insert(engine_index, wall.saturating_add(window));
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
    ///       + Σ_engines Engine::resolved_own_pnl(e)   (realized − fees + funding + unrealized)
    /// ```
    ///
    /// ⚠ **It used to be `Σ Engine::resolved_equity`, and that is a live-risk defect on
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
    /// [`vike_execution::Engine::resolved_own_pnl`] carries the argument for why P&L is the
    /// right movement term (every one of its four fields is written only by this engine's own
    /// fills/funding, and `Account::apply_account_state` writes none of them) and the one declared
    /// residual. The base is `seed_of(e)` — the operator's CONFIGURED capital for the mount, a
    /// constant this process never re-reads from a venue, so it is identical across a restart and
    /// cannot be moved by anyone but the operator editing the profile. (Two exceptions: a NON-dollar
    /// engine that has adopted a venue wallet, whose seed slot holds that wallet from the first sweep that
    /// sees it, once ([`Self::adopt_wallet_bases`]); and an engine mounted on an inverse contract, whose
    /// configured seed is held in its settle coin and revalued at every sweep ([`Self::revalue_coin_seeds`]).)
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
    /// four PnL terms and its positions ARE in `EngineSnapshotAccount`, so the curve RESUMES underwater
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
    /// mount-row load (`vike_tradehub::config::MountConfig`'s `seed_cash` refuses a non-positive value)
    /// AND announced ONCE into the ring here, for a `CoreConfig`
    /// assembled directly rather than from a profile.
    ///
    /// # Currency groups
    ///
    /// A curve adds figures of ONE currency, so the engines are folded per currency group
    /// ([`Engine::ledger_currency_key`]): each group has its own `(base, pnl, peak)` and the latch
    /// trips when ANY group breaches (the latch is node-wide, so one breaching group puts every engine
    /// in liquidate-only). Engine 0's group keeps `pnl_curve_peak` (a storage slot: every group is judged
    /// alike, and the published primary group is [`CoreThread::live_primary_currency`]'s); every other group has a
    /// [`ForeignCurve`]. A node whose engines share a currency, which is every node today, has ONE
    /// group and folds exactly the terms, in exactly the order, described above; its notes read as they
    /// always did. A node with several names the group in each note. The base of a NON-dollar group is
    /// the wallets its engines adopted ([`Self::adopt_wallet_bases`]), the dollar group's the configured
    /// seeds. ⚠ **A non-dollar group is not judged while a demo or live engine of it has not adopted its wallet
    /// yet** ([`Self::group_awaits_wallet`]: at most [`ADOPT_WAIT_MS`], said in the ring, and a group that never
    /// gets a wallet is judged on its seed after that, as before): its base is the placeholder seed until the
    /// first balance lands, and judging a 250 EUR loss on a 50,000 EUR wallet against 1000 trips the node.
    ///
    /// ⚠ **A PAPER engine is out of the curve of a group that holds a demo or live engine** ([`Self::counts_in_curve`],
    /// decision 0285's amendment of 2026-11-16): every paper account starts on 100,000, and four of them beside a
    /// 1,000 demo mount moved the base from 5,000 to 401,000, so a demo or live account could lose its whole wallet
    /// without the latch firing. Such a group is judged on its demo and live engines alone; a group of paper engines
    /// only (a paper-only node, every test core) is judged on them, exactly as before.
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
        // A core no account has joined yet (`spawn_core_without_accounts`) has no capital to judge: its zero base
        // would ring "DISARMED" once and park the primary group's peak at 0 for the first account to shift.
        if self.extra_engines.is_empty() && self.engine.route_key == NO_ACCOUNT_ROUTE_KEY {
            return;
        }
        self.adopt_wallet_bases();
        self.revalue_coin_seeds();
        let terms = self.drawdown_terms_in(pass);
        // Only an Active engine can trip. Already Reducing (the latch fired, or a manual set) or
        // Halted ⇒ leave it (no re-latch, no ring spam, and crucially no auto-un-latch on recovery).
        // The NODE's state (`node_floor`), not any engine's own: a primary the link dead-man halted for
        // its own venue's dead link, or an account-scoped `halt`/`reduce` of the primary (`trade
        // account`), must not switch the account-wide latch off for every other venue. Read once,
        // before any group trips: a group that trips does not silence the groups judged after it.
        let active = self.node_floor == TradingState::Active;
        let mixed = !terms.foreign.is_empty();
        let primary_currency = mixed.then(|| self.engine.ledger_currency_key());
        self.latch_group(None, primary_currency, (terms.base, terms.pnl), active, threshold);
        for (currency_value, base, pnl) in terms.foreign {
            let k = match self.foreign_curves.iter().position(|f| f.currency == currency_value) {
                Some(k) => k,
                None => {
                    self.foreign_curves.push(ForeignCurve {
                        currency: currency_value,
                        peak: None,
                        disarmed_noted: false,
                        curve: 0.0,
                        base: 0.0,
                    });
                    self.foreign_curves.len() - 1
                }
            };
            self.latch_group(Some(k), Some(currency_value), (base, pnl), active, threshold);
        }
    }

    /// One currency group's turn in [`Self::sweep_drawdown_latch`]: fold its high-water mark, then judge
    /// its drop from it. `slot` is `None` for the primary group (its state is `pnl_curve_peak` and
    /// `pnl_curve_disarmed_noted`) and the index into `foreign_curves` otherwise; `currency_value` is `Some` only
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
        currency: Option<vike_execution::Interned>,
        (base, pnl): (f64, f64),
        active: bool,
        threshold: f64,
    ) {
        let curve = base + pnl;
        // Kept for the publish site's group view ([`Self::state_groups`]): the base and curve this sweep measured.
        match slot {
            None => (self.pnl_curve_base, self.pnl_curve_last) = (base, curve),
            Some(k) => (self.foreign_curves[k].base, self.foreign_curves[k].curve) = (base, curve),
        }
        // ⚠ A group whose base is not its engine's money yet is NOT JUDGED (no peak folded, nothing tripped): see
        // [`Self::group_awaits_wallet`]. The key is the group's own, whether or not the node has several.
        let key = match slot {
            None => self.engine.ledger_currency_key(),
            Some(k) => self.foreign_curves[k].currency,
        };
        if self.group_awaits_wallet(key) {
            return;
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
            currency.map_or_else(String::new, |c| {
                format!(" ({} group)", vike_model::ledger_label(c.as_str()))
            })
        };
        let Some(drawdown) = vike_math::time_series::drawdown_fraction(peak, curve) else {
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
        };
        if drawdown > threshold {
            let drawdown_percent = drawdown * 100.0;
            self.raise_trading_state(TradingState::Reducing);
            let group = group_label();
            self.note(format!(
                "DRAWDOWN LATCH{group}: liquidate-only (peak={peak:.2} curve={curve:.2} \
                 capital_base={base:.2} own_pnl={pnl:.2} drawdown={drawdown_percent:.2}%)"
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
    ///
    /// **"Adopted" is exactly membership in [`CoreThread::base_adopted`]**: this function wrote the engine's
    /// `Authoritative` wallet into its seed slot. Until then a demo or live engine in a non-dollar group is
    /// WAITING for its wallet, and [`Self::group_awaits_wallet`] keeps the latch from judging its group on the
    /// configured seed.
    #[inline(never)]
    fn adopt_wallet_bases(&mut self) {
        for i in 0..=self.extra_engines.len() {
            let e = self.engine(i);
            let currency = e.ledger_currency_key();
            let wallet = e.account.balance;
            if currency == vike_execution::Interned::default()
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
            self.shift_curve_peak(currency, wallet - seed);
        }
    }

    /// **A COIN-LEDGER engine's capital base is its configured seed HELD IN ITS SETTLE COIN** (decision 0258): the
    /// first sweep that can price the coin ([`vike_execution::Engine::coin_rate`]) converts the seed into coin
    /// ([`vike_execution::Engine::hold_seed_in_coin`], into the engine's `coin_seed`, which the sizing cap
    /// `Engine::cap_at_coin_seed` reads too), and every sweep writes that coin at today's price into the engine's seed slot,
    /// so the capital base, the curve and the published `capital_base` move with the coin an inverse account
    /// holds: a short of the same dollars is flat at every price. The conversion moves nothing (the slot holds
    /// the same figure that sweep), so the peak is not shifted; a later move of the price is a real move of the
    /// curve. It is the dollar group's rule (configured seeds, never wallets: [`Self::adopt_wallet_bases`]), so a
    /// venue wallet of 100 demo coins never enters the node-wide curve. A stopped engine is not revalued (its
    /// successor states the capital, and a restart took its seed out); an unpriced coin leaves the slot as it is.
    /// A node with no coin ledger pays one `Option` test per engine. `#[inline(never)]`, cold.
    #[inline(never)]
    fn revalue_coin_seeds(&mut self) {
        for i in 0..=self.extra_engines.len() {
            if self.is_retired(i) {
                continue;
            }
            let (seed, config) = (self.seed_of(i), self.config.price_config);
            let Some(usd) = self.engine_mut(i).hold_seed_in_coin(seed, &config) else {
                continue;
            };
            if i == 0 {
                self.config.seed_cash = usd;
            } else {
                self.extra_engines[i - 1].0 = usd;
            }
        }
    }

    /// **Convert each live COIN-LEDGER engine's seed into its settle coin at the first pass that can price it, whether
    /// or not the drawdown latch is armed** (decision 0258, the owner's ruling of 2026-11-15): the sizing cap
    /// (`Engine::cap_at_coin_seed`) reads that coin at today's price, so it moves with the coin on a node whose profile
    /// turns the latch off too. It writes only the engine's `coin_seed` (once): the seed slot, the latch's base, is
    /// still revalued by [`Self::revalue_coin_seeds`] alone, so a disarmed latch stays off and its screens unchanged.
    /// Called by both risk passes ([`Self::sweep_risk_clock`] and the closed-bar drive) before the latch, which then
    /// finds the coin already held. A node with no coin ledger pays one `Option` test per engine. `#[inline(never)]`, cold.
    #[inline(never)]
    pub(crate) fn hold_coin_seeds(&mut self) {
        for i in 0..=self.extra_engines.len() {
            if !self.is_retired(i) {
                let (seed, config) = (self.seed_of(i), self.config.price_config);
                self.engine_mut(i).hold_seed_in_coin(seed, &config);
            }
        }
    }

    /// **Whether the latch must NOT judge the currency group `currency` yet, because the base it would judge it on is
    /// a placeholder.** A demo or live engine in a non-dollar group starts on the configured seed (`seed_cash`,
    /// default 1000) and takes its account's wallet only when its first balance arrives
    /// ([`Self::adopt_wallet_bases`]), 70 to 90 s after a hot activation on the CI box (2026-10-27). In that window a
    /// 250 EUR loss on a 50,000 EUR wallet read as 25% of 1000 and would have put the whole node in
    /// liquidate-only. So a group is skipped by [`Self::latch_group`] (no peak folded, nothing tripped, no
    /// DISARMED note) while ANY of its engines is waiting, and the group's numbers stay unpublished until it is
    /// judged ([`Self::state_groups`] lists a group only once it has a peak).
    ///
    /// An engine WAITS when it is: in a non-dollar group, not a tombstone, not `Paper` (a simulated book has no
    /// wallet to wait for: its seed IS its capital, which is why every paper engine of every existing test keeps
    /// its behaviour), and not in [`CoreThread::base_adopted`]; for at most [`ADOPT_WAIT_MS`] from the first
    /// sweep that found it so. Past that it is read as an engine that never gets a balance (a venue with no
    /// reconcile, a pass that never lands), said ONCE in the ring and at WARN, and its group is judged on the
    /// configured seed, exactly as it was before this wait existed; a wallet that arrives later is still adopted.
    /// The dollar group never waits (its seeds are its capital, by design), so a node of dollar engines pays one
    /// key compare per group per sweep.
    ///
    /// The clock is the engine's own `now_ms` as the message or timer stamped it ([`Self::sweep_risk_clock`]'s
    /// timer stamps every engine; `dispatch` does at the top of every message), the margin call's own clock
    /// (`margin_call_quiet_until`). The ring lines are worded without "DRAWDOWN LATCH": that prefix means a TRIP
    /// to the operator's kit and to the tests that read the ring.
    ///
    /// `#[inline(never)]`, cold: the latch's own sweep, like [`Self::latch_group`].
    #[inline(never)]
    fn group_awaits_wallet(&mut self, currency: vike_execution::Interned) -> bool {
        if currency == vike_execution::Interned::default() {
            return false;
        }
        let now = self.engine.now_ms;
        let label = vike_model::ledger_label(currency.as_str());
        let mut waiting = false;
        for i in 0..=self.extra_engines.len() {
            let e = self.engine(i);
            if e.ledger_currency_key() != currency
                || e.tier == vike_model::AccountTier::Paper
                || self.base_adopted.contains(&i)
                || self.is_retired(i)
            {
                continue;
            }
            let first = !self.adopt_wait.contains_key(&i);
            let wait =
                self.adopt_wait.entry(i).or_insert(AdoptWait { since_ms: now, gave_up: false });
            let over = now.saturating_sub(wait.since_ms) >= ADOPT_WAIT_MS;
            let newly_over = over && !wait.gave_up;
            wait.gave_up |= over;
            waiting |= !over;
            if !(first || newly_over) {
                continue;
            }
            let e = self.engine(i);
            let who = if e.route_key.is_empty() { e.venue.clone() } else { e.route_key.clone() };
            let seed = self.seed_of(i);
            let secs = ADOPT_WAIT_MS / 1000;
            if over {
                tracing::warn!(
                    target: "vike_core::risk_clock",
                    %who,
                    secs,
                    "no wallet balance from the {label} account {who} in {secs} s: its group is judged \
                     against the configured seed {seed:.2} from now on"
                );
                self.note(format!(
                    "LATCH WAIT OVER ({label} group): {who} reported no wallet in {secs} s, so the group is \
                     judged on its configured seed {seed:.2}, as before; a wallet that arrives later is still \
                     adopted"
                ));
            } else {
                self.note(format!(
                    "LATCH WAITS ({label} group): {who} has not reported its wallet yet, so the group's base \
                     is still the configured seed {seed:.2} and the group is not judged until the wallet is \
                     adopted, for at most {secs} s"
                ));
            }
            self.dirty = true;
        }
        waiting
    }

    /// Move the high-water mark of the currency group `currency` by `delta`, when the group has one: a seed that
    /// joins or leaves the capital base moves the peak with it (an addition is not a new high, a restart is
    /// not a drawdown). The primary's group is `pnl_curve_peak`; a group the latch has not yet seen has none.
    pub(crate) fn shift_curve_peak(&mut self, currency: vike_execution::Interned, delta: f64) {
        let slot = if currency == self.engine.ledger_currency_key() {
            &mut self.pnl_curve_peak
        } else if let Some(f) = self.foreign_curves.iter_mut().find(|f| f.currency == currency) {
            &mut f.peak
        } else {
            return;
        };
        if let Some(peak) = slot.as_mut() {
            *peak += delta;
        }
    }

    /// Forget the high-water mark of the currency group `currency`: the next sweep takes it again at `max(base, curve)`.
    /// For a group whose curve becomes another set of engines (the first demo or live engine joining a group of paper
    /// engines alone, `account_change.rs`), whose old peak is no peak of the new curve.
    pub(crate) fn forget_curve_peak(&mut self, currency: vike_execution::Interned) {
        if currency == self.engine.ledger_currency_key() {
            self.pnl_curve_peak = None;
        } else if let Some(f) = self.foreign_curves.iter_mut().find(|f| f.currency == currency) {
            f.peak = None;
        }
    }

    /// **The node's PRIMARY currency group: the group of the first LIVE engine** (decision 0183, item 13): the
    /// currency the published totals, the headline equity and the push drawdown alert are in. "Live" is not a
    /// tombstone (`retired_engines`: a stopped account's engine stays in the list for ever), so a primary
    /// account that stopped and started again in ANOTHER currency does not leave the headline on its dead
    /// engine's group (which would read 0 while every live account sat under "other ledgers"). With no live
    /// engine at all it is engine 0's. On a node of one currency, every node today, it is that currency.
    pub(crate) fn live_primary_currency(&self) -> vike_execution::Interned {
        (0..=self.extra_engines.len()).find(|&i| !self.is_retired(i)).map_or_else(
            || self.engine.ledger_currency_key(),
            |i| self.engine(i).ledger_currency_key(),
        )
    }

    /// The publish site's view of the currency groups (called when [`CoreThread::ledger_mixed`], or on a node
    /// of one currency once the latch has swept): (1) restate the three primary-group totals for the first LIVE
    /// engine's group when `snapshot::build`, which cannot see tombstones, folded the first engine's (a node of
    /// one currency never differs here); (2) publish the base, curve and high-water mark the latch last
    /// measured for EVERY group it has judged, the primary's included, so a report can read them and a push
    /// alert can measure the primary from the latch's peak and watch the others
    /// ([`vike_execution::Portfolio::group_curves`]). Cold.
    #[inline(never)]
    pub(crate) fn state_groups(&self, pf: &mut vike_execution::Portfolio) {
        let live = self.live_primary_currency();
        if live != self.engine.ledger_currency_key() {
            pf.primary_currency_stated = Some(live);
            pf.equity_total = vike_math::summation::compensated_sum(
                pf.venues.iter().filter(|v| v.ledger_currency == live).map(|v| v.equity),
            );
            pf.capital_base = vike_math::summation::compensated_sum(
                std::iter::once(self.config.seed_cash)
                    .chain(self.extra_engines.iter().map(|(s, _)| *s))
                    .zip(pf.venues.iter())
                    .filter(|(_, v)| v.ledger_currency == live)
                    .map(|(s, _)| s),
            );
            pf.margin_used_total =
                pf.venues.iter().filter(|v| v.ledger_currency == live).map(|v| v.margin_used).sum();
        }
        let first = self.engine.ledger_currency_key();
        let slot0 =
            self.pnl_curve_peak.map(|peak| (first, self.pnl_curve_base, self.pnl_curve_last, peak));
        let others = self
            .foreign_curves
            .iter()
            .filter_map(|f| f.peak.map(|p| (f.currency, f.base, f.curve, p)));
        pf.group_curves = slot0
            .into_iter()
            .chain(others)
            .map(|(currency_value, base, curve, peak)| vike_execution::GroupCurve {
                currency: currency_value,
                base,
                curve,
                peak,
            })
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
            let e = self.engine(i);
            let group = vike_model::ledger_label(e.ledger_currency_key().as_str());
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

    /// **How the latch values an engine the age bound calls stale, and a stopped one: at the NEWEST price the
    /// board holds.** [`bounded_config`] with the last-known floor ON: a rung inside the age bound is still taken
    /// first, as for a fresh engine, and a position with none is valued at the slot with the newest timestamp
    /// (`PriceBoardConfig::last_known_by_recency`). It used to be `config.price_cfg`, the permissive default, whose first
    /// present slot wins at any age, so an old venue mark (a fill's or a reconcile pass's, written last night)
    /// outranked the quote that ticked a minute ago and a weekend drawdown was measured off the older number.
    /// Without `price_only` there is no age bound to be stale against and the latch keeps `config.price_cfg`.
    #[inline(never)]
    fn last_known_config(&self) -> PriceBoardConfig {
        match self.config.price_only.as_ref() {
            Some(po) => PriceBoardConfig {
                stale_fallback: true,
                ..bounded_config(self.config.price_config, po.max_price_age)
            },
            None => self.config.price_config,
        }
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
        let config = self.config.price_config;
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
        // engine is priced at the NEWEST price its board holds (`Self::last_known_config`; a mounted engine on a
        // closed bar keeps `config.price_cfg`, the bar vouches for its marks). A frozen price can only repeat the number it already gave,
        // and the other engines keep moving the curve, so measuring beats not measuring. It is not silent:
        // `price_only_basis` says it once per episode (a WARN naming the engine, the age and this stance).
        // Only the MARGIN CALL and the mount budgets skip a stale engine (they act on orders, so they act
        // on fresh numbers only). A STOPPED engine is the same case without the noise (it is always stale).
        let per_engine: Option<Vec<Option<PriceBoardConfig>>> = if self.config.price_only.is_some()
        {
            let last_known = self.last_known_config();
            let mut out = Vec::with_capacity(self.extra_engines.len() + 1);
            for i in 0..=self.extra_engines.len() {
                out.push(match self.price_only_basis(i, pass) {
                    // a mounted engine on a closed bar: the bar vouches for its marks
                    PriceBasis::Mounted if !self.is_retired(i) => Some(config),
                    // stale or stopped: its newest price (`Self::last_known_config`)
                    PriceBasis::Mounted | PriceBasis::Skip(SKIP_STALE) => Some(last_known),
                    PriceBasis::Bounded(c) => Some(c),
                    PriceBasis::Skip(_) => None,
                });
            }
            Some(out)
        } else {
            None
        };
        // `compensated_sum` over (primary, then extras) for BOTH halves — the identical fold law and the
        // identical order `snapshot::build` uses for `Portfolio::capital_base` /
        // `Portfolio::pnl_total`, so this decision number and the number a `RuleTrigger::Drawdown`
        // alert reads out of the published snapshot are bit-identical rather than merely close.
        // `crates/vike-core/tests/integration/drawdown_measures_own_pnl.rs`'s
        // `the_latch_curve_and_the_published_curve_are_bit_identical` is the pin.
        //
        // A curve adds figures of ONE currency (decision 0183, item 13): the engines are folded per
        // currency group, the PRIMARY engine's first. With every engine in one group (every node today)
        // `member` is true for all of them and these are the folds, over the engines, in the order, that
        // this method always had.
        let primary = self.engine.ledger_currency_key();
        let mixed = self.extra_engines.iter().any(|(_, e)| e.ledger_currency_key() != primary);
        let per_engine = per_engine.as_deref();
        // A group holding a demo or live engine is judged on those alone ([`Self::counts_in_curve`]).
        let primary_real = self.group_has_real(primary);
        let (base, pnl) = self.curve_terms(per_engine, &config, |i| {
            (!mixed || self.engine(i).ledger_currency_key() == primary)
                && (!primary_real || !states_paper(self.engine(i)))
        });
        let mut foreign: Vec<(vike_execution::Interned, f64, f64)> = Vec::new();
        if mixed {
            let mut seen = vec![primary];
            for (_, e) in &self.extra_engines {
                let currency = e.ledger_currency_key();
                if seen.contains(&currency) {
                    continue;
                }
                seen.push(currency);
                let real = self.group_has_real(currency);
                let (base, pnl) = self.curve_terms(per_engine, &config, |i| {
                    self.engine(i).ledger_currency_key() == currency
                        && (!real || !states_paper(self.engine(i)))
                });
                foreign.push((currency, base, pnl));
            }
        }
        DrawdownTerms { base, pnl, foreign }
    }

    /// **Whether the currency group `currency` holds an engine in its curve that is not a stated PAPER one** (a demo or
    /// live engine, a stopped one included: its booked PnL stays in the curve; an engine nobody told its tier counts as
    /// real, the protective side; one priced off another network is out of the curve, [`Self::off_network`]): the
    /// group's drawdown curve is then theirs alone, its paper engines out of it ([`Self::counts_in_curve`]).
    pub(crate) fn group_has_real(&self, currency: vike_execution::Interned) -> bool {
        (0..=self.extra_engines.len()).any(|i| {
            let e = self.engine(i);
            e.ledger_currency_key() == currency && !states_paper(e) && !self.off_network(i)
        })
    }

    /// Whether engine `i` is a demo or live engine whose price feed dials another network than it trades on (a
    /// `demo` deribit account beside deribit's mainnet feed): its marks are a phantom basis, so the risk sweeps leave it
    /// out ([`Self::price_only_basis`]'s `SKIP_NETWORK`). A constant of the engine and the price-only config.
    fn off_network(&self, i: usize) -> bool {
        let e = self.engine(i);
        e.tier != vike_model::AccountTier::Paper
            && self.config.price_only.as_ref().is_some_and(|po| {
                po.price_source.get(e.venue.as_str()).is_some_and(|t| *t != e.tier)
            })
    }

    /// **Whether engine `i`'s seed and own PnL are in its currency group's drawdown curve**: a demo or live engine's
    /// always, a paper engine's only in a group of paper engines alone (decision 0285's amendment of 2026-11-16: the
    /// one paper balance of 100,000 must never be the base a real account is judged against). What
    /// [`Self::drawdown_terms_in`] folds and what a hot start moves the peak by (`account_change.rs`).
    pub(crate) fn counts_in_curve(&self, i: usize) -> bool {
        let e = self.engine(i);
        !states_paper(e) || !self.group_has_real(e.ledger_currency_key())
    }

    /// `(capital base, own PnL)` of the engines `member` accepts (by routing index, primary 0), `compensated_sum` over
    /// them in registration order, and in a price-only run over the engines whose cfg is `Some`.
    #[inline(never)]
    fn curve_terms(
        &self,
        per_engine: Option<&[Option<PriceBoardConfig>]>,
        config: &PriceBoardConfig,
        member: impl Fn(usize) -> bool,
    ) -> (f64, f64) {
        let seeds = std::iter::once(self.config.seed_cash)
            .chain(self.extra_engines.iter().map(|(s, _)| *s));
        match per_engine {
            None => (
                vike_math::summation::compensated_sum(
                    seeds.enumerate().filter(|(i, _)| member(*i)).map(|(_, s)| s),
                ),
                vike_math::summation::compensated_sum(
                    (0..=self.extra_engines.len())
                        .filter(|i| member(*i))
                        .map(|i| self.curve_pnl_of(i, config)),
                ),
            ),
            Some(configs) => (
                vike_math::summation::compensated_sum(
                    seeds
                        .zip(configs)
                        .enumerate()
                        .filter(|(i, (_, c))| member(*i) && c.is_some())
                        .map(|(_, (s, _))| s),
                ),
                vike_math::summation::compensated_sum((0..).zip(configs).filter_map(|(i, c)| {
                    c.as_ref().filter(|_| member(i)).map(|c| self.curve_pnl_of(i, c))
                })),
            ),
        }
    }

    /// Engine `i`'s own-PnL term of the drawdown curve: `resolved_own_pnl`, a stopped engine's included (it
    /// stays in the list, so its booked PnL and its unrealized count for the process life, in ITS currency
    /// group).
    fn curve_pnl_of(&self, i: usize, config: &PriceBoardConfig) -> f64 {
        self.engine(i).resolved_own_pnl(config)
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
        self.mount_valuation_at(i, &self.config.price_config)
    }

    /// [`Self::mount_valuation`] priced by `config`: the permissive `config.price_cfg` for the views and the
    /// closed-bar sweep, the age-bounded cfg of [`Self::price_only_basis`] for the clock's sweep. Every money
    /// figure adds over the ledger's rows, each on its own engine at its own price and contract value; the
    /// position is the mount's own symbol's (`MountAttribution`). With no leg the loop adds nothing, so the
    /// figures are the own row's to the bit.
    fn mount_valuation_at(&self, i: usize, config: &PriceBoardConfig) -> (f64, f64, f64, f64) {
        let attr = &self.mount_attribution[i];
        let (venue, symbol) = &self.mount_vs[i];
        // THE MOUNT'S OWN ENGINE ([`CoreThread::mount_engine`]), not the venue's default account: this
        // prices the LATCH's own decision, and the contract multiplier it reads is a per-ENGINE
        // grid (a venue-fetched grid on a live mount, empty — so 1.0 — on one that fell back to
        // paper). A labelled mount whose venue's default account is capped to paper was having its
        // gross notional folded at the default account's multiplier, so its `max_notional` arm
        // latched at the wrong threshold in whichever direction the real multiplier ran. Identical
        // on a single-account core, where `mount_engine` IS this venue lookup.
        let (size, mut realized_net, mut unrealized, mut notional) =
            self.ledger_row_valuation(attr, self.mount_engine(i), venue, symbol, config);
        for leg in &attr.legs {
            let engine_index = self.mount_row_engine(i, &leg.venue, leg.account.as_ref());
            let (_, r, u, n) =
                self.ledger_row_valuation(&leg.attr, engine_index, &leg.venue, &leg.symbol, config);
            realized_net += r;
            unrealized += u;
            notional += n;
        }
        (size, realized_net, unrealized, notional)
    }

    /// `(size, realized net of fees, unrealized, gross notional)` of ONE row of a mount's ledger, on engine
    /// `engine_index`: the resolver's price and the engine's contract value, valued by the row's own law
    /// (`MountAttribution::valued_at`, which the backtest values a mount's rows by too). A flat row
    /// short-circuits before touching the resolver.
    fn ledger_row_valuation(
        &self,
        row: &MountAttribution,
        engine_index: usize,
        venue: &str,
        symbol: &str,
        config: &PriceBoardConfig,
    ) -> (f64, f64, f64, f64) {
        let realized_net = row.realized_pnl - row.fees_paid;
        if row.size == 0.0 {
            return (0.0, realized_net, 0.0, 0.0);
        }
        let value = self.engine(engine_index).account.contract_value_of(symbol);
        match self.engine(engine_index).resolved_position_price(venue, symbol, row.size, config) {
            Some(px) => {
                let (realized_net, unrealized, notional) = row.valued_at(px, value);
                (row.size, realized_net, unrealized, notional)
            }
            None => (row.size, realized_net, 0.0, 0.0),
        }
    }

    /// The engine a row of mount `i`'s ledger on `venue` lives on, the route its orders take: the engine of
    /// the row's own ACCOUNT when it names one (a leg on another account, decision 0293), else the mount's own
    /// engine on its own exchange and that venue's otherwise.
    pub(crate) fn mount_row_engine(
        &self,
        i: usize,
        venue: &str,
        account: Option<&vike_model::accounts::account_keys::AccountLabel>,
    ) -> usize {
        account
            .and_then(|label| {
                let key = vike_model::accounts::account_keys::route_key_of(venue, label);
                self.engine_index_for_route_key(RouteKey::declared(&key))
            })
            .or_else(|| self.route_of(EngineRoute::Mount(i), venue))
            .unwrap_or(0)
    }

    /// The ACCOUNT a row of mount `i`'s ledger on `venue` is keyed by when a fill of it booked on engine
    /// `engine_index` (decision 0293): `None` when that is the engine the row's route resolves
    /// ([`Self::mount_row_engine`] with no account), which is every fill of a mount whose legs name no other
    /// account, so their rows stay as they were; else that engine's account.
    pub(crate) fn mount_row_account(
        &self,
        i: usize,
        venue: &str,
        engine_index: usize,
    ) -> Option<vike_model::accounts::account_keys::AccountLabel> {
        if engine_index == self.mount_row_engine(i, venue, None) {
            return None;
        }
        vike_model::accounts::account_keys::label_of_route_key(
            venue,
            &self.engine(engine_index).route_key,
        )
    }

    /// Every row of mount `i`'s ledger that holds a position, its own first: `(venue, account, symbol, signed
    /// size)`, the account a row's own (`MountAttributionRow::account`). Cold: a latched mount's sweep, a latch,
    /// an un-latch.
    pub(crate) fn mount_open_rows(&self, i: usize) -> Vec<MountOpenRow> {
        let attr = &self.mount_attribution[i];
        let (venue, symbol) = &self.mount_vs[i];
        std::iter::once((venue, None, symbol, attr.size))
            .chain(attr.legs.iter().map(|l| (&l.venue, l.account.as_ref(), &l.symbol, l.attr.size)))
            .filter(|(.., size)| size.abs() > 1e-12)
            .map(|(venue, account, symbol, size)| {
                (venue.clone(), account.cloned(), symbol.clone(), size)
            })
            .collect()
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
            let Some(budget) = self.mount_budget[i] else { continue };
            if !budget.is_active() {
                continue;
            }
            // A STOPPED account's mount (decision 0169) is skipped, as the margin call skips its engine:
            // the flatten would be refused (its client is detaching) and the fire-once latch spent on it,
            // leaving the position unjudged for good once the account starts again.
            if self.is_retired(self.mount_engine(i)) {
                continue;
            }
            // Fire-once for the JUDGEMENT and the cancel; a latched mount whose flatten did not take is
            // offered it again ([`Self::reoffer_mount_flatten`]).
            if self.mount_latched[i] {
                if budget.loss_action == vike_model::MountLossAction::Flatten {
                    self.reoffer_mount_flatten(i, now);
                }
                continue;
            }
            let config = if pass == RiskPass::Bar {
                self.config.price_config
            } else {
                match self.price_only_basis(self.mount_engine(i), pass) {
                    PriceBasis::Mounted => self.config.price_config,
                    PriceBasis::Bounded(c) => c,
                    PriceBasis::Skip(_) => continue,
                }
            };
            // The same rule for a leg on another engine: an unpriced row reads no loss, so the mount is not
            // judged off half its prices. (A leg on the mount's own engine was just judged with it.)
            let stale_leg = pass == RiskPass::Clock
                && (0..self.mount_attribution[i].legs.len()).any(|k| {
                    let leg = &self.mount_attribution[i].legs[k];
                    let engine_index = self.mount_row_engine(i, &leg.venue, leg.account.as_ref());
                    engine_index != self.mount_engine(i)
                        && matches!(self.price_only_basis(engine_index, pass), PriceBasis::Skip(_))
                });
            if stale_leg {
                continue;
            }
            let (_size, realized_net, unrealized, notional) = self.mount_valuation_at(i, &config);
            // The loss is counted from the mount's loss base: `0.0` for a mount never un-latched (the whole
            // history), the net realized PnL the operator's un-latch found it at otherwise (decision 0239).
            let loss = -(realized_net - self.mount_loss_base[i] + unrealized);
            if budget.breached(loss, notional) {
                self.latch_mount(i, now, loss, notional);
            }
        }
    }

    /// **A LATCHED mount that is still open is offered its flatten AGAIN.** The latch is fire-once, and its single flatten can fail without
    /// anyone being told: the gate denied it, the venue rejected it, the core's panic guard caught the send, or
    /// it went out before the trade link or the first reconcile pass was ready. Before this, the position then
    /// stayed open with nothing left to close it. Every sweep (the clock's and the closed bar's) a latched
    /// mount whose budget's loss action is `Flatten` looks at its attributed size, and when it is not flat and no
    /// live reduce-only MARKET close on that symbol and side covers it ([`Self::reoffer_quantity`]: the mount's own
    /// first flatten counts, so one still in flight is never stacked on) it sends the missing quantity through
    /// [`Self::send_mount_flatten`], journaled write-ahead like the first. Skipped, silently, while the mount's
    /// engine is HALTED (the gate refuses even a close there and the operator's `resume` is the cure, so a
    /// re-offer every 30 s would only fill the ring with denials) or its trade link is down (the close would be
    /// rejected at once). Said ONCE per episode, until the mount is flat. Cold: per sweep, only for a latched mount.
    ///
    /// What it did is returned ([`Reoffer`]) for the one caller that tells an operator: the un-latch
    /// ([`Self::unlatch_mount`], decision 0239), which refuses a mount that still holds a position and says whether
    /// its flatten was offered again just now. The sweeps ignore it. A mount with legs is offered it per open row,
    /// and the answer is the row that did most: one sent, else one waiting, else a close in flight.
    #[inline(never)]
    pub(crate) fn reoffer_mount_flatten(&mut self, i: usize, now: i64) -> Reoffer {
        let rows = self.mount_open_rows(i);
        if rows.is_empty() {
            self.mount_reoffer_said.remove(&i);
            return Reoffer::Flat;
        }
        let mut out = Reoffer::Flat;
        for (venue, account, symbol, size) in rows {
            let book = (venue.as_str(), account.as_ref());
            out = match (out, self.reoffer_row(i, now, book, &symbol, size)) {
                (Reoffer::Sent(a), Reoffer::Sent(b)) => Reoffer::Sent(a + b),
                (Reoffer::Sent(q), _) | (_, Reoffer::Sent(q)) => Reoffer::Sent(q),
                (Reoffer::Waits, _) | (_, Reoffer::Waits) => Reoffer::Waits,
                (_, row) => row,
            };
        }
        out
    }

    /// [`Self::reoffer_mount_flatten`] for one open row of the ledger, on the row's own engine.
    fn reoffer_row(
        &mut self,
        i: usize,
        now: i64,
        (venue, account): (&str, Option<&vike_model::accounts::account_keys::AccountLabel>),
        symbol: &str,
        size: f64,
    ) -> Reoffer {
        let engine_index = self.mount_row_engine(i, venue, account);
        if self.engine(engine_index).trading_state == TradingState::Halted
            || self.execution_link_is_down(&self.engine(engine_index).route_key)
        {
            return Reoffer::Waits;
        }
        let side = vike_model::closing_side(size);
        let qty = self.reoffer_quantity(i, (venue, account), symbol, side, size.abs());
        if qty <= 1e-12 {
            // a close is out (the first flatten, the margin call's, an operator's): wait for it
            return Reoffer::InFlight;
        }
        if self.mount_reoffer_said.insert(i) {
            tracing::warn!(
                target: "vike_core::risk_clock",
                mount = i,
                %venue,
                %symbol,
                size,
                "a latched mount still holds a position with no close in flight: its flatten is offered again"
            );
            self.note(format!(
                "MOUNT BUDGET LATCH: mount {i} ({venue}/{symbol}) still holds {size:.8} with no close in \
                 flight: its flatten is offered again ({qty:.8}), and again at every sweep until it is flat"
            ));
        }
        self.send_mount_flatten(i, now, (venue, account), symbol, side, qty);
        self.dirty = true;
        Reoffer::Sent(qty)
    }

    /// Latch ONE mount liquidate-only after a budget breach (steal/core-per-mount-budget): set its
    /// fire-once `mount_latched` flag, cancel EXACTLY its own resting orders (its attributed order client ids —
    /// so a sibling mount on the same symbol is untouched), and — when its loss action is `Flatten` — submit
    /// a reduce-only MARKET to close its attributed net position, journaled write-ahead exactly like
    /// a margin-call liquidation (same [`vike_journal::JournalRecord::MarginCallLiquidate`] shape +
    /// replay contract: replay re-applies it as a `Submit`, re-minting the same order client id — so a
    /// budget-flatten session replays deterministically like a margin-call one). This NEVER touches
    /// `engine.trading_state` — that is the ACCOUNT-wide drawdown/HALT latch; this is scoped to one
    /// mount so every other mount keeps trading. Cold path (per breach, off the fold).
    fn latch_mount(&mut self, i: usize, now: i64, loss: f64, notional: f64) {
        self.mount_latched[i] = true;
        let (venue, symbol) = self.mount_vs[i].clone();
        let budget = self.mount_budget[i];
        let rows = self.mount_open_rows(i);
        self.note(format!(
            "MOUNT BUDGET LATCH: mount {i} ({venue}/{symbol}) liquidate-only \
             (loss={loss:.2} notional={notional:.2})"
        ));
        // (1) cancel THIS mount's resting orders, scoped by its attributed order client ids (NOT a
        // (venue,symbol) MassCancel, which would also cancel a sibling mount's orders on the same
        // symbol). Terminal/unknown order client ids are no-ops in the engine's cancel path, so passing every
        // attributed order client id is safe. Replay-neutral like the GTD sweep's cancel: the venue's
        // authoritative `OrderCanceled` is journaled as its own `Ingest::Event` and replays
        // independently, so no cancel record is needed here.
        let order_client_ids: Vec<String> = self
            .order_client_id_mount
            .iter()
            .filter(|&(_, &m)| m == i)
            .map(|(c, _)| c.clone())
            .collect();
        if !order_client_ids.is_empty() {
            // RISK-OFF, declared: the latch is fire-once, so a cancel a venue held back is never
            // re-offered and the mount stays liquidate-only with live orders out.
            self.apply_intent_with_cancel_intent(
                OrderIntent::CancelBatch(order_client_ids),
                now,
                OrderCancelIntent::RiskOff,
            );
        }
        // (2) optional flatten of the mount's attributed net position, one reduce-only MARKET per open row
        // of its ledger, journaled write-ahead like the margin-call sweep (same shape + replay contract).
        // Skipped when flat.
        //
        // ⚠ CLAMPED to what the book still holds after the reduce-only orders already in flight
        // ([`Self::flatten_quantity`]): one clock sweep runs the margin call first, which has just released a
        // reduce-only close of its excess, and the gate nets in-flight reduce-only orders only against a
        // FLAT book (`Engine::cover_position_size`), so on a non-flat book it admits this one
        // too and the two add up to more than the position. A venue that sends `reduce_only` on the wire
        // trims the second; oanda, ig, fxcm, ibkr, ctrader and alpaca do not, and the position flips.
        if budget.is_some_and(|b| b.loss_action == vike_model::MountLossAction::Flatten) {
            for (venue, account, symbol, size) in rows {
                let side = vike_model::closing_side(size);
                let want = size.abs();
                let book = (venue.as_str(), account.as_ref());
                let qty = self.flatten_quantity(i, book, &symbol, side, want);
                if qty < want {
                    let what = if qty > 1e-12 { "clamped" } else { "skipped" };
                    self.note(format!(
                        "MOUNT BUDGET LATCH: mount {i} ({venue}/{symbol}) flatten {what}: it wanted {want:.8}, \
                         the book can still take {qty:.8} beside the closes already in flight"
                    ));
                }
                self.send_mount_flatten(i, now, book, &symbol, side, qty);
            }
        }
        self.dirty = true;
    }

    /// Release the reduce-only MARKET flatten of one row of latched mount `i` for `qty` (nothing when `qty`
    /// is not positive), journaled write-ahead and attributed to the mount: the ONE sender of the latch's
    /// first flatten ([`Self::latch_mount`]) and of a re-offered one ([`Self::reoffer_mount_flatten`]), so
    /// the two cannot differ in what they journal or how they route.
    fn send_mount_flatten(
        &mut self,
        i: usize,
        now: i64,
        (venue, account): (&str, Option<&vike_model::accounts::account_keys::AccountLabel>),
        symbol: &str,
        side: i32,
        qty: f64,
    ) {
        if qty > 1e-12 {
            // A row of ANOTHER account of the venue (decision 0293) is closed on that account's engine, named on the
            // order; every other row on the mount's route, as before.
            let engine_index = self.mount_row_engine(i, venue, account);
            let route = if account.is_some() {
                EngineRoute::Engine(engine_index)
            } else {
                EngineRoute::Mount(i)
            };
            let req = OrderRequest {
                order_client_id: String::new(),
                venue: venue.to_string(),
                account: account.cloned(),
                symbol: symbol.to_string(),
                side,
                qty,
                order_type: vike_model::OrderType::Market,
                reduce_only: true,
                timestamp: now,
                ..Default::default()
            };
            // …and the ACCOUNT (journal v16, §9 item 12): the latched mount's own engine, which is
            // the book this flatten closes. `mount_id` says WHOSE loss it is; this says WHICH of an
            // exchange's accounts holds it — two different questions, and a multi-account venue is
            // where they stop having one answer. Resolved before the journal borrow.
            let rk = self.journal_route_key(engine_index, &req.venue);
            if let Some(j) = self.journal.as_mut() {
                // Stamp the OWNING mount id (journal v14). Without it this record is
                // indistinguishable on disk from the account-wide margin-call sweep's, and
                // `replay::fold_order_client_id_mounts` — which rebuilds attribution FROM the journal — had no
                // owner to credit: after a restart this flatten's fill booked into the RESIDUAL row
                // instead of the mount whose budget breach caused it, so that mount's ledger came
                // back under-reporting the very loss the latch exists to bound. The in-memory
                // `own_order_client_id` write below was always right; only the durable half was missing.
                let mid = self.mount_ids[i].clone();
                j.append_margin_call_liquidate(now, &req, Some(&mid), rk.as_deref())
                    .expect("journal append");
                self.journaled_since_snap += 1;
            }
            // Attribute the flatten back to the mount so its own fill folds the ledger flat (and
            // realizes the loss). `apply_intent` mints + submits; its follow-on `MintedSubmit` ties
            // the order client id for a reader of the journal, exactly as the margin-call path does.
            // ⚠ `EngineRoute::Mount(i)` — the LATCHED MOUNT's own engine. Everything else in this
            // function is already mount-scoped (the attributed order client ids it cancels, the attributed
            // size it closes, the mount id it stamps on the journal record), but the flatten itself
            // lowered through `apply_intent` and was routed by the payload's venue: a labelled
            // mount's budget breach submitted its reduce-only close to the venue's DEFAULT account,
            // leaving the position the latch exists to bound wide open and opening an unwanted one
            // beside it. Routing it by the mount is also what keeps the `order_client_id_mount` attribution
            // below honest — the fill has to come back through the engine that holds the position.
            let flat_order_client_ids = self.apply_intent_routed(
                OrderIntent::Submit(Box::new(req)),
                now,
                OrderCancelIntent::Unspecified,
                route,
            );
            for c in flat_order_client_ids {
                self.own_order_client_id(c, i, None);
            }
        }
        self.dirty = true;
    }
}
