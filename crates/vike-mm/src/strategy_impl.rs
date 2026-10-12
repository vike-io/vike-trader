//! `impl<B: HftBroker> Strategy<B> for SpreadMaker` — the tick/fill/params lanes, split out of the
//! crate root; behavior is byte-identical (the impl block moved verbatim).
//!
//! Also owns the durable-state DTO (portfolio-observer PR-4 T4): [`SpreadMaker::save_state`]/
//! [`SpreadMaker::load_state`] persist the fill-rate breaker's two suppression deadlines, (when
//! Avellaneda–Stoikov is enabled) the online estimator's accumulators, and each side's RESTING
//! single quote, so a crash-restart no longer un-trips a tripped breaker, re-learns σ̂²/κ from cold
//! or quotes a second bid/ask beside the one the previous session left resting. See
//! [`SpreadMakerStateV1`] for exactly what is (and isn't) persisted.

use serde::{Deserialize, Serialize};
use vike_model::{
    Fill, FlowToxicity, HftBroker, L2Book, MarkTick, OrderLifecycle, QuoteTick, Strategy,
    StrategyParams, TradeTick,
};

use crate::book::BookView;
use crate::skew::{FillRec, net_signed_fills};
use crate::{SideState, SpreadMaker};

/// Wire version this build writes and accepts. Bump on any breaking shape change to
/// [`SpreadMakerStateV1`]/[`AsStateV1`]; [`SpreadMaker::load_state`] ignores (fail-open, warns)
/// any other value rather than guessing at a migration — mirrors vike-data's
/// `datafusion_hist::manifest::MANIFEST_FORMAT` versioning shape.
const SPREAD_MAKER_STATE_VERSION: u32 = 1;

/// Durable-state wire DTO for [`SpreadMaker`] (portfolio-observer PR-4 T4). A DEDICATED type
/// (never `#[derive(Serialize, Deserialize)]` directly on the live `SpreadMaker`/`AsState`
/// structs) so the wire format is decoupled from the internal representation — the maker's live
/// fields can be renamed/reshaped freely without a version bump, since only this struct's shape is
/// a compatibility contract.
///
/// Persists the per-side fill-rate breaker's suppression deadlines, (if A-S is enabled) the online
/// estimator's accumulators, and each side's RESTING SINGLE QUOTE ([`SideQuoteV1`]) — the state a
/// crash-restart would otherwise silently reset. Deliberately EXCLUDES:
/// - `fills` (the raw sliding-window fill tape): the suppression DEADLINES are its behavioral
///   effect — replaying the raw window isn't needed to reproduce a tripped/untripped side.
/// - `SideState::rungs` (the ladder's per-rung snapshot) and the optional own-order ladder
///   (`own_book`): neither is reachable from any binary's default config, and a rung's tag is its
///   INDEX, so restoring one without the others would renumber tags under live orders. A restarted
///   LADDER maker re-places its rungs.
/// - `AsState::params`/`alpha`: come fresh from the live config/`AsParams` at mount time (a
///   restart with a DIFFERENT tuning should price with the new tuning, not a stale saved one).
///
/// ⚠ **Nothing rediscovers a resting order for the maker.** An earlier version of this doc said the
/// per-side order cache was "rediscovered by the runtime's reconcile pass"; it is not — a LIVE
/// mount's reconcile pass reports divergence and folds none of it into a strategy, so a maker that
/// booted with `placed = false` quoted a NEW bid/ask at its first tick while the old order kept
/// resting (and, with the core's restored tag registry, the re-submit overwrote the tag entry and
/// orphaned the old order). The maker therefore persists what it needs to CONTINUE the quote: the
/// core puts the order and its tag back, the maker modifies/cancels it by that tag and hears its
/// terminal event, exactly as in the session that placed it.
///
/// ADDITIVE on the wire: `bid`/`ask` are `#[serde(default)]`, so a file written before they existed
/// still loads (both sides unplaced), and `v` is not bumped.
#[derive(Debug, Serialize, Deserialize)]
struct SpreadMakerStateV1 {
    /// Wire version; anything other than [`SPREAD_MAKER_STATE_VERSION`] is ignored (fail-open).
    v: u32,
    bid_suppressed_until: i64,
    ask_suppressed_until: i64,
    as_state: Option<AsStateV1>,
    /// The bid side's resting quote; absent in a file from before it was persisted.
    #[serde(default)]
    bid: SideQuoteV1,
    /// The ask side's resting quote — the mirror of `bid`.
    #[serde(default)]
    ask: SideQuoteV1,
}

/// Wire DTO for one side's resting single quote: exactly what a post-restart tick needs and no
/// more. `own` is the maker's INTENDED `(price, size)` — the pair `refresh_skips` and the reward
/// moas gate compare a re-price against, and what single-snapshot own-order filtration subtracts —
/// not a venue handle (the maker never sees a client-order-id; the core resolves the tag).
/// `quoted_ts` is the placement clock (moas age, OwnFillFit exposure). `refresh_stale` is NOT
/// persisted: a restored side is always stale (see [`SpreadMaker::restore_side`]).
#[derive(Debug, Default, Serialize, Deserialize)]
struct SideQuoteV1 {
    #[serde(default)]
    placed: bool,
    #[serde(default)]
    own: Option<(f64, f64)>,
    #[serde(default)]
    quoted_ts: i64,
}

impl SideQuoteV1 {
    /// The wire form of `side`. A snapshot with a non-finite number is dropped (JSON has no NaN, and
    /// one such value would make the whole payload unparsable), which leaves the side unplaced.
    fn of(side: &SideState) -> Self {
        match side.own {
            Some((px, qty)) if side.placed && px.is_finite() && qty.is_finite() => {
                Self { placed: true, own: Some((px, qty)), quoted_ts: side.quoted_ts }
            }
            _ => Self::default(),
        }
    }
}

/// Wire DTO for `AsState`'s persisted accumulators — everything EXCEPT `params`/`alpha` (see
/// [`SpreadMakerStateV1`]'s doc for why). Field-for-field the `AsState` accumulators
/// (`avellaneda.rs`), with `trades` a `Vec` on the wire (JSON has no `VecDeque`; converted at the
/// `save_state`/`load_state` boundary).
#[derive(Debug, Serialize, Deserialize)]
struct AsStateV1 {
    sigma2: Option<f64>,
    last_mid: Option<(f64, i64)>,
    last_quote_mid: Option<f64>,
    trades: Vec<(f64, f64, i64)>,
    sum_w: f64,
    sum_w_delta: f64,
}

impl<B: HftBroker> Strategy<B> for SpreadMaker {
    /// L1 quote lane: build a one-level-per-side [`BookView`] from the touch and run the shared
    /// `requote`. The grid is the venue tick LEARNED from the L2 book lane (`learned_book_tick`) when
    /// one has been seen, else the configured `tick_size` param (pure-L1 fallback) —
    /// so the `*_half_spread_ticks` floor/cap and grid snap resolve against the SAME tick the book
    /// lane uses, never a divergent one. Byte-identical when the param already matches the book grid
    /// (the correct config). With the default (`Mid`, no filtration) this is the original
    /// `mid ± half_spread` maker.
    fn on_quote_tick(&mut self, broker: &mut B, q: &QuoteTick) {
        let tick = self.learned_book_tick.unwrap_or(self.cfg.tick_size);
        let book = BookView::from_quote(q, tick);
        self.requote(broker, &book, q.ts);
    }

    /// L2 book lane (R8 HFT track): build a [`BookView`] from the top of the [`L2Book`] and run the
    /// SAME `requote`. This is where `Depth` gets real levels and own-order filtration can shift the
    /// derived best. We take `depth_levels + 2` levels per side — enough to index the `Depth` level
    /// AND still expose a genuine best after filtration removes our own top-of-book level. The tick
    /// ts is the mount's `now` (the runtime stamps it on the tick context).
    fn on_order_book(&mut self, broker: &mut B, book: &L2Book) {
        // Learn the venue's REAL grid from the book and adopt it on the L1 quote lane too (see
        // `learned_book_tick`) — so one `*_half_spread_ticks` can't resolve to two different spreads.
        if book.tick_size > 0.0 {
            self.learned_book_tick = Some(book.tick_size);
        }
        let view = BookView::from_l2(book, self.cfg.depth_levels + 2);
        self.requote(broker, &view, broker.now());
    }

    /// Trade-tape lane for the A-S κ fit (audit mm-quote): when A-S is enabled, fold each executed
    /// trade's size-weighted distance-from-mid into the online intensity estimator's running sums
    /// over an event-time window (O(1) amortised). A NO-OP when A-S is off (`as_state` is `None`) —
    /// the default maker never had an `on_trade_tick`, so this stays additive. The κ fit is only
    /// USED to price in [`KappaMode::LiveFit`](vike_model::KappaMode::LiveFit); in the default
    /// [`KappaMode::Fixed`](vike_model::KappaMode::Fixed) the tape is kept warm but the fixed
    /// `κ_default` prices.
    fn on_trade_tick(&mut self, _broker: &mut B, t: &TradeTick) {
        if let Some(as_state) = self.as_state.as_mut() {
            as_state.record_trade(t);
        }
    }

    /// Underlying-mark lane ("Option B" cross-symbol routing): the runtime routes a mark of the
    /// DIFFERENT symbol this maker's A-S underlying-anchored fair mid / ATM guard WATCH (e.g. the
    /// `btcusdt` RTDS spot for a BTC up/down PM token) here. It folds the mark into the
    /// [`UnderlyingTracker`](crate::underlying::UnderlyingTracker) and, once that has a window-open
    /// reference + a σ estimate, feeds the derived `(s_now, s_open, sigma_per_sec)` into the A-S state
    /// via `set_underlying`. A NO-OP when A-S is off (`as_state` is `None`) — every non-A-S maker is
    /// byte-identical, the trait's default `on_mark` being a no-op — and INERT until the A-S
    /// `underlying_weight` / `atm_blackout_scale` knobs are raised (a stored underlying that no knob
    /// reads changes no quote). The broker is intentionally UNTOUCHED (no order placed here): the
    /// anchor takes effect on the NEXT quote/book tick's `requote`.
    fn on_mark(&mut self, _broker: &mut B, mark: &MarkTick) {
        // Read the A-S window params (and bail if A-S is off) WITHOUT holding the borrow across the
        // `self.underlying` update below — `as_state` and `underlying` are sibling fields of `self`,
        // so the mutable-borrow overlap must be broken by copying the two params out first.
        let (window_secs, resolution_ts) = match self.as_state.as_ref() {
            Some(st) => (st.params.window_secs, st.params.resolution_ts),
            None => return,
        };
        if let Some((s_now, s_open, sigma)) =
            self.underlying.observe(mark.price, mark.ts, window_secs, resolution_ts)
            && let Some(as_state) = self.as_state.as_mut()
        {
            as_state.set_underlying(s_now, s_open, sigma);
        }
    }

    /// Flow-toxicity lane (RTDS wallet-toxicity guard, 5c): STORE the latest per-side toxic-flow
    /// reading and return — the reaction (widen + size-cut on the toxic side) is applied at the NEXT
    /// quote in `requote`, not here, so no order is placed/canceled from this hook (the broker is
    /// intentionally UNTOUCHED, mirroring `on_mark`). INERT until the maker's `toxicity` guard
    /// is configured with a non-zero knob: a stored reading no active knob reads changes no quote, so a
    /// maker without the guard is byte-identical to before (the trait's default `on_flow` is a no-op).
    fn on_flow(&mut self, _broker: &mut B, flow: FlowToxicity) {
        self.last_flow = Some(flow);
        // Mark the reading EXTERNAL so the OFI-toxicity synthesis fallback (Group-B, PR-3) never
        // clobbers it: once any real `on_flow` has arrived, the external feed takes precedence
        // permanently. Inert on a maker with no toxicity guard / `ofi_toxicity_scale == 0` (the
        // synthesis this gates never runs there anyway).
        self.flow_external = true;
    }

    /// Adverse-selection guard (audit mm1): record every fill into the sliding EVENT-TIME window
    /// and, when the NET one-directional accumulation on one side trips the threshold, arm that
    /// side's cooldown. `requote` (from either the quote or book lane) acts on the arm (pulls +
    /// withholds the side). Round-trip netting lives entirely in `net_signed_fills`: an offset
    /// bid/ask pair nets to ~0 and never arms. A no-op (and no allocation/tracking) when the breaker
    /// is disabled, so a maker without `with_fill_breaker` behaves exactly as before (the trait's
    /// default `on_fill` was a no-op).
    fn on_fill(&mut self, _broker: &mut B, fill: &Fill) {
        // Order-refresh TOLERANCE invalidation — deliberately BEFORE the breaker's early return,
        // because it is independent of the breaker: the filled side's resting snapshot
        // (`own_bid`/`own_ask`) is the maker's INTENDED quote, while a PARTIAL fill leaves a
        // SMALLER remainder at the venue. Left alone, the intended-vs-target comparison would read
        // "no change" and the size top-up would be silently lost, so the maker would quote less
        // size than configured. Marking the side stale forces the next tick through the modify arm
        // (which re-issues the full configured size) exactly once. INERT when the gate is off: the
        // flag is only ever read by `refresh_skips`, which short-circuits while `refresh_tolerance`
        // is `None` — so a maker without the gate is byte-identical to before.
        if fill.side != 0 {
            let side = self.side_mut(fill.side > 0);
            side.refresh_stale = true;
            // A fill on this side can only come from the one order we hold there, so a RESTORED
            // side that fills is confirmed alive (see `SideState::unverified`).
            side.unverified = None;
        }
        // OwnFillFit κ MLE: fold this fill as a FILLED own-order outcome. A guarded no-op unless A-S
        // is on AND kappa_mode is OwnFillFit (see `SpreadMaker::feed_own_outcome`), so every other
        // maker is byte-identical. Deliberately BEFORE the breaker early-return: OwnFillFit does not
        // require the fill-rate breaker to be enabled.
        if fill.side != 0 {
            self.feed_own_outcome(fill.side > 0, true, fill.ts);
        }
        if !self.breaker_enabled() {
            return;
        }
        // record this fill at its EVENT ts, then evict everything older than the window (the fill
        // ts is "now" — the window rides the latest fill, matching the runtime's event clock)
        self.fills.push_back(FillRec { side: fill.side, size: fill.size, ts: fill.ts });
        let cutoff = fill.ts - self.cfg.fill_window_ms;
        while self.fills.front().is_some_and(|f| f.ts < cutoff) {
            self.fills.pop_front();
        }
        // net over the window ending at THIS fill's event time; a one-sided run survives the sum,
        // a round-trip cancels. Arm (or refresh) the over-hit side's cooldown from the event ts.
        let net = net_signed_fills(&self.fills, fill.ts, self.cfg.fill_window_ms);
        // ACCELERATED PULL (Group-B, PR-3): near a binary resolution DIVIDE the effective net-fill
        // threshold by the time-acceleration multiplier so the breaker trips SOONER as τ → 0 (a larger
        // multiplier ⇒ a lower effective threshold ⇒ trips on less one-directional fill). The pure
        // `net_signed_fills` above is UNTOUCHED — the multiplier is applied ONLY at this comparison
        // site. `pull_accel_mult` is `>= 1.0` always and exactly `1.0` when the knobs are off (or no
        // `resolution_ts` is known), so `net_fill_threshold / 1.0 == net_fill_threshold` bit-for-bit and
        // the default path compares against the unchanged threshold.
        let eff_threshold = self.cfg.net_fill_threshold / self.pull_accel_mult(fill.ts);
        if net >= eff_threshold {
            self.bid.suppressed_until = fill.ts + self.cfg.suppress_cooldown_ms;
        } else if net <= -eff_threshold {
            self.ask.suppressed_until = fill.ts + self.cfg.suppress_cooldown_ms;
        }
    }

    /// Order-DEATH lane: when one of this maker's own tagged quotes reaches a terminal state, free
    /// the [`SideState`](crate::SideState) slot it was resting in, so the next tick's
    /// `requote_single_side` takes the PLACE arm instead of the re-price arm.
    ///
    /// ## ⚠ The wedge this exists to close
    ///
    /// `requote_single_side` is a three-arm gate on `SideState::placed`, and `placed` was written in
    /// exactly three places — its own suppression arm, its own place arm, and the ladder-mode
    /// `retire_single_order`. NOTHING cleared it when the order died at the VENUE. So after a full
    /// fill `placed` stayed `true`, every later tick took the modify arm, the runtime resolved the
    /// tag to the now-terminal client-order-id, and `vike_exec::ExecutionEngine::modify_order`'s
    /// not-modifiable early return swallowed it — no error, no event, no ring line. That side never
    /// quoted again for the life of the process. Measured on the the CI box live mount: `orders:2` was a
    /// permanent ceiling and `working` sat at `0` across every session.
    ///
    /// The maker could not fix this alone, and that is the point: it never sees a client-order-id
    /// (the tag registry is the runtime's), so until [`OrderLifecycle::tag`] existed there was no
    /// name in this event it could match against anything it owns. Guessing from `on_fill` instead
    /// was not an option either — [`Fill`] carries no order identity and no remaining quantity, so
    /// "was that the whole order" is unanswerable from it, and answered wrong by any size heuristic
    /// on a partial-fill sequence.
    ///
    /// ## What it covers
    ///
    /// ALL THREE ways a single quote dies, through the one hook: a full FILL
    /// ([`OrderEventKind::Filled`]), a CANCEL/EXPIRY (venue-side or external), and a
    /// REJECT/`RiskGate` DENY — the last of which wedged the maker just as hard as a fill, because
    /// the place arm sets `placed` the moment it buffers the submit, before any verdict.
    /// [`OrderEventKind::Accepted`] is non-terminal and frees nothing: on a side that is placed it
    /// only confirms the order alive, and on a side that holds nothing it ADOPTS the order (below).
    ///
    /// ## Adoption: the crash window `save_state` cannot close
    ///
    /// A tagged NON-terminal single-quote event on a side with `placed == false` names a live order
    /// this maker does not know it has: after a restart the core restored the tag and told us at
    /// the first reconcile pass (a synthetic tagged `Accepted`), but the saved state predates the
    /// order (it was placed after the last `save_state`, then the daemon died). Ignoring it made the
    /// first tick submit a SECOND order under the same tag and orphan the older one. So the side is
    /// ADOPTED: `placed`, price/size unknown (`own = None`), `refresh_stale` so the next tick takes
    /// the modify-by-tag arm and re-prices price AND full size once. The ladder (`"bid0"`..) is
    /// not covered, as above.
    ///
    /// ⚠ A side adopted and not yet re-priced saves as UNPLACED (`SideQuoteV1::of` needs a quote),
    /// so a save in that one-tick window re-opens the gap; the persisted shape is unchanged.
    ///
    /// ⚠ **The LADDER path is NOT covered.** A rung's tag is `"bid{k}"`/`"ask{k}"` where `k` is its
    /// INDEX into `SideState::rungs`, so forgetting one rung would renumber every deeper rung's tag
    /// away from the order actually resting under it — and rebuilding the whole side instead would
    /// churn queue position on every fill, which is most of what a laddered maker is trying to keep.
    /// Untangling that needs `rungs` to hold a per-rung liveness slot rather than a dense vec, which
    /// is a reshape of the ladder diff and not this fix. A laddered maker therefore still wedges
    /// per-rung; the ladder is off by default (`levels >= 2`), and the single-quote path — the one
    /// the live mount runs — is closed.
    ///
    /// ## What it deliberately does NOT do
    ///
    /// It feeds NO own-order outcome into the OwnFillFit κ MLE. A `Filled` is already fed as a
    /// FILLED observation by [`Strategy::on_fill`], and a cancel the maker itself issued is already
    /// fed as CENSORED by `requote_single_side`'s suppression arm (which clears `placed` first, so
    /// this hook early-returns on it) — feeding again would double-count the same exposure. An
    /// EXTERNALLY-originated cancel/reject records no observation at all, which is the conservative
    /// choice for an estimator and is in any case strictly more than the nothing it recorded while
    /// the side was wedged.
    ///
    /// [`Fill`]: vike_model::Fill
    /// [`OrderEventKind::Accepted`]: vike_model::OrderEventKind::Accepted
    /// [`OrderEventKind::Filled`]: vike_model::OrderEventKind::Filled
    /// [`OrderLifecycle::tag`]: vike_model::OrderLifecycle::tag
    fn on_order_event(&mut self, _broker: &mut B, event: &OrderLifecycle) {
        // Only the SINGLE-quote tags this maker owns. A ladder rung (`"bid0"`), a foreign
        // strategy's tag, or an untagged order (`None`) falls through untouched — an exact match,
        // never a prefix, so `"bid0"` can never be mistaken for `"bid"`.
        let is_bid = match event.tag.as_deref() {
            Some("bid") => true,
            Some("ask") => false,
            _ => return,
        };
        if !event.kind.is_terminal() {
            // A tagged non-terminal event (the core resolved this tag against its registry) is a
            // sign of life for a RESTORED side — it need not be pulled and re-placed. Nothing else
            // changes: the side stays resting. On a side this maker holds NOTHING for, the same
            // event is an ADOPTION (see `SpreadMaker::adopt_unplaced_side`).
            if !self.side(is_bid).placed {
                self.adopt_unplaced_side(is_bid);
            }
            self.side_mut(is_bid).unverified = None;
            return;
        }
        if !self.side(is_bid).placed {
            return; // already free — this maker pulled it itself, or a duplicate terminal
        }
        let side = self.side_mut(is_bid);
        side.placed = false;
        side.own = None;
        side.refresh_stale = false;
        side.quoted_ts = 0;
        side.unverified = None;
        // Mirror into the own-order ladder exactly as the suppression PULL does, so own-filtration
        // stops subtracting a quote that is no longer on the book. A no-op while the ladder is
        // `None` (the default), which is what keeps every non-ladder maker byte-identical.
        self.own_book_pull(if is_bid { "bid" } else { "ask" });
    }

    /// Live-parameter plane: hot-swap this maker's tunables from a [`StrategyParams::SpreadMaker`]
    /// update WITHOUT unmounting (the runtime routes it here off `Command::UpdateParams`). The
    /// broker is intentionally UNTOUCHED — no order is placed/canceled/modified here; the new knobs
    /// take effect on the NEXT quote/book tick, where `requote` re-prices the resting bid/ask IN
    /// PLACE (modify, not cancel/replace), so queue position is preserved and a re-tune that doesn't
    /// move the price doesn't disturb the book at all. The maker consumes ONLY its own
    /// [`StrategyParams::SpreadMaker`] arm; a foreign variant (e.g. the position-executor
    /// [`StrategyParams::PositionController`]) is IGNORED — matching one's own variant and ignoring
    /// the rest is the live-params contract (`Strategy::on_params_updated`).
    fn on_params_updated(&mut self, _broker: &mut B, params: &StrategyParams) {
        if let StrategyParams::SpreadMaker(p) = params {
            self.apply_params(p);
        }
    }

    /// The READ half of the same plane: route the maker's own [`SpreadMaker::params`] accessor out
    /// through the trait, so the runtime can publish this mount's live tunables on its snapshot. The
    /// accessor is AUTHORITATIVE by construction — it reads the fields the next tick will price
    /// from, and re-attaches the A-S bag from the live `as_state` (which `set_params` may have
    /// clamped) rather than echoing whatever the last update carried.
    fn params(&self) -> Option<StrategyParams> {
        Some(StrategyParams::SpreadMaker(SpreadMaker::params(self)))
    }

    /// Durable-state SAVE (portfolio-observer PR-4 T4): see [`SpreadMakerStateV1`] for exactly
    /// what is (and isn't) persisted. Always `Some` — unlike the trait's `None`-by-default (a
    /// strategy with genuinely nothing to save), a `SpreadMaker` always has at least the two
    /// (possibly-zero) breaker deadlines worth round-tripping.
    fn save_state(&self) -> Option<serde_json::Value> {
        let dto = SpreadMakerStateV1 {
            v: SPREAD_MAKER_STATE_VERSION,
            bid_suppressed_until: self.bid.suppressed_until,
            ask_suppressed_until: self.ask.suppressed_until,
            bid: SideQuoteV1::of(&self.bid),
            ask: SideQuoteV1::of(&self.ask),
            as_state: self.as_state.as_ref().map(|s| AsStateV1 {
                sigma2: s.sigma2,
                last_mid: s.last_mid,
                last_quote_mid: s.last_quote_mid,
                trades: s.trades.iter().copied().collect(),
                sum_w: s.sum_w,
                sum_w_delta: s.sum_w_delta,
            }),
        };
        serde_json::to_value(&dto).ok()
    }

    /// Durable-state LOAD (portfolio-observer PR-4 T4). FAIL-OPEN: a payload that doesn't parse
    /// as [`SpreadMakerStateV1`], or whose `v` isn't [`SPREAD_MAKER_STATE_VERSION`], is warned and
    /// ignored — this maker keeps its freshly-constructed state rather than panicking or
    /// partially applying a shape it doesn't recognize.
    ///
    /// On a recognized payload: the two breaker deadlines always restore. The A-S accumulators
    /// restore ONLY when BOTH this (live, freshly-mounted) maker has A-S enabled (`self.as_state`
    /// is `Some` — this config wants A-S) AND the saved payload has an `as_state` too —
    /// overwriting just the accumulator fields on the existing `AsState`, so its live
    /// `params`/`alpha` (already set from the current config) are untouched. Either side being
    /// `None` (A-S off in this config, or the saved maker never had A-S on) skips the A-S restore
    /// entirely, leaving the estimator fresh — it never turns A-S on/off as a side effect.
    fn load_state(&mut self, state: &serde_json::Value) {
        let dto: SpreadMakerStateV1 = match serde_json::from_value(state.clone()) {
            Ok(dto) => dto,
            Err(err) => {
                tracing::warn!(
                    %err,
                    "SpreadMaker::load_state: unparsable saved state, keeping fresh state"
                );
                return;
            }
        };
        if dto.v != SPREAD_MAKER_STATE_VERSION {
            tracing::warn!(
                found_version = dto.v,
                expected_version = SPREAD_MAKER_STATE_VERSION,
                "SpreadMaker::load_state: saved state version mismatch, keeping fresh state"
            );
            return;
        }
        self.bid.suppressed_until = dto.bid_suppressed_until;
        self.ask.suppressed_until = dto.ask_suppressed_until;
        self.restore_side(true, dto.bid);
        self.restore_side(false, dto.ask);
        if let (Some(live), Some(saved)) = (self.as_state.as_mut(), dto.as_state) {
            live.sigma2 = saved.sigma2;
            live.last_mid = saved.last_mid;
            live.last_quote_mid = saved.last_quote_mid;
            live.trades = saved.trades.into_iter().collect();
            live.sum_w = saved.sum_w;
            live.sum_w_delta = saved.sum_w_delta;
        }
    }
}

impl SpreadMaker {
    /// ADOPT a live order the core named by this side's single-quote tag while the side held
    /// nothing (see [`Strategy::on_order_event`]'s "Adoption"). The quoted `(price, size)` is
    /// unknown, so `own` stays `None` — every reader of it (`refresh_skips`, `reward_moas_holds`,
    /// `filter_own`, `feed_own_outcome`) already treats `None` as "nothing to compare" — and
    /// `refresh_stale` forces the next tick through the modify arm regardless of tolerance or the
    /// reward moas hold. `unverified` is `None`: the event itself is the proof of life.
    fn adopt_unplaced_side(&mut self, is_bid: bool) {
        let side = self.side_mut(is_bid);
        side.placed = true;
        side.own = None;
        side.refresh_stale = true;
        side.unverified = None;
    }

    /// Put one side's saved resting quote back, so the maker CONTINUES it instead of quoting a
    /// second one beside it. Only a `placed` side that carries a usable `(price, size)` is
    /// restored (anything else stays the fresh, unplaced side — never a half-trusted one).
    ///
    /// The restored side is marked:
    /// - `refresh_stale`, because `own` is the INTENDED quote and a partial fill may have landed
    ///   while the daemon was down: the first tick then re-issues price AND full size through the
    ///   modify arm, bypassing the refresh tolerance and the reward moas hold once (the same
    ///   reason `on_fill` sets it);
    /// - `unverified` (`Some(0)`), the safety net's clock: nothing proves the order still rests
    ///   (the core's ownership entry may have expired, or the venue dropped it while we were
    ///   down), and a modify on an order the core cannot resolve is swallowed silently — the same
    ///   shape as the wedge `on_order_event` closes. A tagged event or a fill on the side clears
    ///   it; otherwise `crates/vike-mm/src/quote.rs`'s `restored_quote_expired` pulls the side
    ///   after `RESTORED_QUOTE_VERIFY_MS` and the next tick places a fresh quote.
    ///
    /// The own-order LADDER (`own_book`, unreachable from any binary) is not fed: it needs an
    /// accept timestamp this hook does not have, and it is off by default.
    fn restore_side(&mut self, is_bid: bool, saved: SideQuoteV1) {
        let Some((px, qty)) = saved.own else { return };
        if !saved.placed || !px.is_finite() || !qty.is_finite() || qty <= 0.0 {
            return;
        }
        let side = self.side_mut(is_bid);
        side.placed = true;
        side.own = Some((px, qty));
        side.quoted_ts = saved.quoted_ts;
        side.refresh_stale = true;
        side.unverified = Some(0);
    }
}

/// Minimal test-only accessors for [`SpreadMaker`] fields that are private to the crate root
/// (`lib.rs`) or to the sibling `avellaneda` module — both visible here already (this module is a
/// descendant of the crate root, and the `AsState` accumulator fields are `pub(crate)`), but the
/// tests below want a clean read API rather than reaching into `self.as_state.as_ref()...` inline.
#[cfg(test)]
impl SpreadMaker {
    pub(crate) fn bid_suppressed_until(&self) -> i64 {
        self.bid.suppressed_until
    }
    pub(crate) fn ask_suppressed_until(&self) -> i64 {
        self.ask.suppressed_until
    }
    pub(crate) fn as_enabled(&self) -> bool {
        self.as_state.is_some()
    }
    pub(crate) fn as_sigma2(&self) -> Option<f64> {
        self.as_state.as_ref().and_then(|s| s.sigma2)
    }
    /// `(sum_w, sum_w_delta, trades.len())` — the κ-MLE accumulators, `None` if A-S is off.
    pub(crate) fn as_accumulators(&self) -> Option<(f64, f64, usize)> {
        self.as_state.as_ref().map(|s| (s.sum_w, s.sum_w_delta, s.trades.len()))
    }
    /// `(own_obs.len(), own_n_fill, cached_own_kappa)` — the OwnFillFit own-fill tape stats, or
    /// `None` if A-S is off. Reads the sibling `avellaneda` module's `pub(crate)` accumulators.
    pub(crate) fn own_fill_stats(&self) -> Option<(usize, usize, Option<f64>)> {
        self.as_state.as_ref().map(|s| (s.own_obs.len(), s.own_n_fill, s.cached_own_kappa))
    }
    /// The A-S state's stored underlying `(s_now, s_open, sigma_per_sec)`, or `None` when A-S is off
    /// or no routed mark has warmed the tracker yet — the observable of the "Option B" `on_mark`
    /// wiring. Reads the sibling `avellaneda` module's `pub(crate)` `underlying` field.
    pub(crate) fn as_underlying(&self) -> Option<(f64, f64, f64)> {
        self.as_state.as_ref().and_then(|s| s.underlying)
    }
}

#[path = "strategy_impl_tests.rs"]
#[cfg(test)]
mod strategy_impl_tests;
