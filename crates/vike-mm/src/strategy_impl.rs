//! `impl<B: HftBroker> Strategy<B> for SpreadMaker` — the tick/fill/params lanes, split out of the
//! crate root; behavior is byte-identical (the impl block moved verbatim).
//!
//! Also owns the durable-state DTO (portfolio-observer PR-4 T4): [`SpreadMaker::save_state`]/
//! [`SpreadMaker::load_state`] persist the fill-rate breaker's two suppression deadlines and (when
//! Avellaneda–Stoikov is enabled) the online estimator's accumulators, so a crash-restart no
//! longer un-trips a tripped breaker or re-learns σ̂²/κ from cold. See [`SpreadMakerStateV1`] for
//! exactly what is (and isn't) persisted.

use serde::{Deserialize, Serialize};
use vike_model::{
    Fill, FlowToxicity, HftBroker, L2Book, MarkTick, OrderLifecycle, QuoteTick, Strategy,
    StrategyParams, TradeTick,
};

use crate::SpreadMaker;
use crate::book::BookView;
use crate::skew::{FillRec, net_signed_fills};

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
/// Persists ONLY the per-side fill-rate breaker's suppression deadlines and (if A-S is enabled)
/// the online estimator's accumulators — the two pieces of state a crash-restart would otherwise
/// silently reset. Deliberately EXCLUDES:
/// - `fills` (the raw sliding-window fill tape): the suppression DEADLINES are its behavioral
///   effect — replaying the raw window isn't needed to reproduce a tripped/untripped side.
/// - the per-side resting-order cache (`SideState::own`/`placed`): rediscovered by the
///   runtime's reconcile pass after a restart, same as any other venue-side order state.
/// - `AsState::params`/`alpha`: come fresh from the live config/`AsParams` at mount time (a
///   restart with a DIFFERENT tuning should price with the new tuning, not a stale saved one).
#[derive(Debug, Serialize, Deserialize)]
struct SpreadMakerStateV1 {
    /// Wire version; anything other than [`SPREAD_MAKER_STATE_VERSION`] is ignored (fail-open).
    v: u32,
    bid_suppressed_until: i64,
    ask_suppressed_until: i64,
    as_state: Option<AsStateV1>,
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
            self.side_mut(fill.side > 0).refresh_stale = true;
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
    /// [`OrderEventKind::Accepted`] is non-terminal and deliberately changes nothing.
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
        if !event.kind.is_terminal() {
            return;
        }
        // Only the SINGLE-quote tags this maker owns. A ladder rung (`"bid0"`), a foreign
        // strategy's tag, or an untagged order (`None`) falls through untouched — an exact match,
        // never a prefix, so `"bid0"` can never be mistaken for `"bid"`.
        let is_bid = match event.tag.as_deref() {
            Some("bid") => true,
            Some("ask") => false,
            _ => return,
        };
        if !self.side(is_bid).placed {
            return; // already free — this maker pulled it itself, or a duplicate terminal
        }
        let side = self.side_mut(is_bid);
        side.placed = false;
        side.own = None;
        side.refresh_stale = false;
        side.quoted_ts = 0;
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

#[cfg(test)]
mod tests {
    use super::*;
    use vike_core::LiveBroker;
    use vike_model::{AsParams, HorizonMode, KappaMode, OrderEventKind, QuoteStyle};

    /// A bare in-crate `LiveBroker` at a given inventory + EVENT ts, empty order buffers —
    /// mirrors `lib.rs`'s own test helper of the same shape (this module can't reach that
    /// private helper, so it gets its own minimal copy).
    fn broker(position: f64, now: i64) -> LiveBroker {
        LiveBroker {
            positions: Vec::new(),
            prices: Vec::new(),
            bar_views: Vec::new(),
            position,
            price: 0.0,
            equity: 0.0,
            bars: std::sync::Arc::new(Vec::new()),
            index: 0,
            now,
            multiplier: 1.0,
            lot_size: 0.0,
            submissions: Vec::new(),
            modifications: Vec::new(),
            cancels: Vec::new(),
            brackets: Vec::new(),
            conditionals: Vec::new(),
            mass_cancel: false,
        }
    }

    fn quote(ts: i64, bid: f64, ask: f64) -> QuoteTick {
        QuoteTick { ts, local_ts: 0, bid, ask, bid_size: 1.0, ask_size: 1.0, symbol: String::new() }
    }

    fn trade(ts: i64, price: f64, size: f64) -> TradeTick {
        TradeTick { ts, local_ts: 0, price, size, is_buyer_maker: false, symbol: String::new() }
    }

    fn a_fill(side: i32, size: f64, ts: i64) -> Fill {
        Fill { side, size, price: 100.0, fee: 0.0, ts, is_maker: true, symbol: String::new() }
    }

    /// A maker with BOTH the fill-rate breaker and A-S enabled — the fixed config the roundtrip
    /// test builds two instances of (the "driven" maker and the "fresh, restart-simulating" one
    /// that loads its saved state — a real mount always reconstructs from the same config first).
    fn maker_with_breaker_and_as() -> SpreadMaker {
        SpreadMaker::new(1.0, 0.5)
            .with_fill_breaker(1_000, 2.5, 5_000)
            .with_avellaneda_stoikov(AsParams::default())
    }

    // The full round trip (portfolio-observer PR-4 T4): drive a maker until BOTH the breaker has
    // tripped a side AND the A-S estimator has warmed accumulators, save it, load it into a FRESH
    // same-config maker (as a restart would construct before load_state), and confirm both the
    // breaker deadline and the A-S accumulators — but NOT the params/alpha, which come from the
    // fresh config either way — land bit-for-bit.
    #[test]
    fn spreadmaker_state_roundtrips_breaker_and_as() {
        let mut m = maker_with_breaker_and_as();

        // two priced quote ticks seed sigma2/last_mid/last_quote_mid (the first only seeds
        // last_mid; the second computes the first sigma2 sample from the mid delta over dt).
        m.on_quote_tick(&mut broker(0.0, 100), &quote(100, 0.40, 0.42));
        m.on_quote_tick(&mut broker(0.0, 200), &quote(200, 0.41, 0.43));
        // a trade AFTER the first priced tick folds into the kappa-MLE running sums.
        m.on_trade_tick(&mut broker(0.0, 200), &trade(210, 0.415, 5.0));

        // three same-side (bid) fills inside the window trip the breaker's bid suppression.
        for t in [110, 120, 130] {
            m.on_fill(&mut broker(0.0, t), &a_fill(1, 1.0, t));
        }

        assert_ne!(m.bid_suppressed_until(), 0, "precondition: the breaker tripped");
        assert!(m.as_sigma2().is_some(), "precondition: sigma2 seeded by the second priced tick");
        let (sum_w, _sum_w_delta, n_trades) = m.as_accumulators().expect("A-S is enabled");
        assert!(sum_w > 0.0, "precondition: the trade tick folded into sum_w");
        assert_eq!(n_trades, 1, "precondition: one trade recorded");

        let saved = Strategy::<LiveBroker>::save_state(&m).expect("SpreadMaker always saves Some");

        // a FRESH maker, same config — as a restart would construct at mount, before load_state
        let mut m2 = maker_with_breaker_and_as();
        assert_eq!(m2.bid_suppressed_until(), 0, "precondition: fresh maker starts untripped");
        assert_eq!(m2.as_sigma2(), None, "precondition: fresh maker's estimator starts cold");

        Strategy::<LiveBroker>::load_state(&mut m2, &saved);

        assert_eq!(
            m2.bid_suppressed_until(),
            m.bid_suppressed_until(),
            "breaker deadline restored"
        );
        assert_eq!(m2.ask_suppressed_until(), m.ask_suppressed_until(), "untripped side stays 0");
        assert_eq!(m2.as_sigma2(), m.as_sigma2(), "sigma2 accumulator restored");
        assert_eq!(
            m2.as_accumulators(),
            m.as_accumulators(),
            "kappa-MLE sums + trade tape restored"
        );
    }

    // A future/foreign version tag must be ignored (fail-open): no panic, the maker keeps its
    // freshly-constructed state rather than misapplying a payload shape it doesn't recognize.
    #[test]
    fn spreadmaker_load_ignores_version_mismatch() {
        let mut m = maker_with_breaker_and_as();
        Strategy::<LiveBroker>::load_state(&mut m, &serde_json::json!({"v": 999}));
        assert_eq!(
            m.bid_suppressed_until(),
            0,
            "fail-open: deadlines stay at constructed defaults"
        );
        assert_eq!(
            m.ask_suppressed_until(),
            0,
            "fail-open: deadlines stay at constructed defaults"
        );
        assert_eq!(m.as_sigma2(), None, "fail-open: A-S estimator stays cold");
    }

    // A maker with A-S DISABLED loading a payload that DOES carry an as_state (e.g. saved by a
    // differently-configured maker) must restore the breaker deadlines but silently skip the A-S
    // restore — never turning A-S on as a side effect of loading state.
    #[test]
    fn spreadmaker_load_without_as_skips_as_restore() {
        let mut m = SpreadMaker::new(1.0, 0.5).with_fill_breaker(1_000, 2.5, 5_000);
        assert!(!m.as_enabled(), "precondition: A-S is off on this config");

        let payload = serde_json::json!({
            "v": 1,
            "bid_suppressed_until": 777,
            "ask_suppressed_until": 0,
            "as_state": {
                "sigma2": 0.001,
                "last_mid": [0.41, 200],
                "last_quote_mid": 0.41,
                "trades": [[0.01, 5.0, 210]],
                "sum_w": 5.0,
                "sum_w_delta": 0.05
            }
        });
        Strategy::<LiveBroker>::load_state(&mut m, &payload);

        assert_eq!(m.bid_suppressed_until(), 777, "the breaker deadline still restores");
        assert!(!m.as_enabled(), "A-S stays off — the payload's as_state is silently skipped");
    }

    /// A maker with A-S in [`KappaMode::OwnFillFit`] mode. The fixed config the wiring tests build.
    fn own_fill_fit_maker() -> SpreadMaker {
        SpreadMaker::new(1.0, 0.5)
            .with_avellaneda_stoikov(AsParams {
                kappa_mode: KappaMode::OwnFillFit,
                ..AsParams::default()
            })
            .with_fill_breaker(1_000, 2.5, 5_000)
    }

    // OwnFillFit WIRING: a fill folds a FILLED own outcome, and a subsequent breaker PULL folds a
    // CENSORED one — the two live sources that feed the censored-hazard κ MLE.
    #[test]
    fn own_fill_fit_records_fills_and_breaker_pulls() {
        let mut m = own_fill_fit_maker();
        // a priced quote tick places the bid and sets its placement clock + the A-S fair mid.
        m.on_quote_tick(&mut broker(0.0, 100), &quote(100, 0.40, 0.42));
        // a bid fill folds a FILLED own outcome (and the 5.0 same-side size trips the bid breaker).
        m.on_fill(&mut broker(0.0, 110), &a_fill(1, 5.0, 110));
        let (len1, nfill1, _) = m.own_fill_stats().expect("A-S enabled");
        assert_eq!((len1, nfill1), (1, 1), "the fill recorded one FILLED own outcome");
        // the next tick: the tripped breaker PULLS the bid, folding a CENSORED own outcome.
        m.on_quote_tick(&mut broker(0.0, 120), &quote(120, 0.40, 0.42));
        let (len2, nfill2, _) = m.own_fill_stats().expect("A-S enabled");
        assert_eq!((len2, nfill2), (2, 1), "the breaker pull recorded one CENSORED own outcome");
    }

    // OFF / byte-identical: a maker NOT selecting OwnFillFit (here the DEFAULT Fixed κ, A-S on)
    // never touches the own-fill tape — the whole wiring is inert, so its behavior is unchanged. It
    // drives the exact fill + breaker-pull sequence the OwnFillFit test does; nothing is recorded.
    #[test]
    fn non_own_fill_fit_maker_never_records_own_outcomes() {
        let mut m = SpreadMaker::new(1.0, 0.5)
            .with_avellaneda_stoikov(AsParams::default()) // KappaMode::Fixed, the default
            .with_fill_breaker(1_000, 2.5, 5_000);
        m.on_quote_tick(&mut broker(0.0, 100), &quote(100, 0.40, 0.42));
        m.on_fill(&mut broker(0.0, 110), &a_fill(1, 5.0, 110));
        m.on_quote_tick(&mut broker(0.0, 120), &quote(120, 0.40, 0.42));
        assert_eq!(
            m.own_fill_stats(),
            Some((0, 0, None)),
            "Fixed κ never records own outcomes — the OwnFillFit wiring is inert"
        );
    }

    // --- the order-DEATH lane: a dead quote frees its side, so the next tick PLACES ---------------

    /// A plain single-quote maker on the 0.01 grid — no breaker, no A-S, no ladder, no tolerance.
    /// The exact default shape the live the CI box mount runs, so the wedge below is the live one.
    fn plain_maker() -> SpreadMaker {
        SpreadMaker::new(1.0, 0.01)
    }

    /// `(tag, price)` of every SUBMIT on a driven broker.
    fn submits(b: &LiveBroker) -> Vec<(String, f64)> {
        b.submissions
            .iter()
            .map(|s| (s.tag.clone().unwrap_or_default(), s.price.unwrap_or(f64::NAN)))
            .collect()
    }

    /// The tags of every MODIFY on a driven broker.
    fn modify_tags(b: &LiveBroker) -> Vec<String> {
        b.modifications.iter().map(|m| m.tag.clone()).collect()
    }

    /// A terminal lifecycle event naming one of the maker's own tags.
    fn terminal(tag: &str, kind: OrderEventKind) -> OrderLifecycle {
        OrderLifecycle { client_order_id: "coid-1".into(), tag: Some(tag.into()), kind }
    }

    // ⚠ THE WEDGE. A FULL FILL kills the resting bid, but nothing used to clear `SideState::placed`
    // — so the next tick took the modify arm, the runtime resolved `"bid"` to the now-terminal
    // coid, and `ExecutionEngine::modify_order` returned silently. That side never quoted again.
    // Drive exactly that sequence and assert the next tick SUBMITS the bid rather than modifying it.
    #[test]
    fn a_fully_filled_quote_is_resubmitted_not_modified_on_the_next_tick() {
        let mut m = plain_maker();

        // tick 1: a two-sided quote goes on the book.
        let mut b1 = broker(0.0, 100);
        m.on_quote_tick(&mut b1, &quote(100, 0.40, 0.42));
        assert_eq!(submits(&b1).len(), 2, "precondition: both sides placed on the first tick");
        assert!(modify_tags(&b1).is_empty(), "precondition: nothing to modify yet");

        // the bid fills COMPLETELY: the fill itself, then the order's death by tag.
        m.on_fill(&mut broker(1.0, 110), &a_fill(1, 1.0, 110));
        m.on_order_event(&mut broker(1.0, 110), &terminal("bid", OrderEventKind::Filled));

        // tick 2 at a MOVED book, so a still-resting side genuinely wants a re-price.
        let mut b2 = broker(1.0, 120);
        m.on_quote_tick(&mut b2, &quote(120, 0.41, 0.43));

        let submitted: Vec<String> = submits(&b2).into_iter().map(|(t, _)| t).collect();
        assert!(
            submitted.contains(&"bid".to_string()),
            "the dead bid must be SUBMITTED again, got submits {submitted:?} / modifies {:?}",
            modify_tags(&b2)
        );
        assert!(
            !modify_tags(&b2).contains(&"bid".to_string()),
            "the dead bid must NOT be re-priced — that is the silent no-op that wedged the mount"
        );
        // ...and the surviving ask must still take the WORKING modify lane, untouched.
        assert_eq!(modify_tags(&b2), vec!["ask".to_string()], "the resting ask still re-prices");
        assert!(
            !submitted.contains(&"ask".to_string()),
            "the resting ask must NOT be re-submitted (that would orphan it)"
        );
    }

    // The other two death modes reach the same slot through the same hook: an externally-originated
    // CANCEL, and a REJECT of a submit the place arm had already marked `placed`. Each frees only
    // its own side.
    #[test]
    fn a_canceled_or_rejected_quote_also_frees_its_side() {
        for (tag, kind) in [
            ("bid", OrderEventKind::Canceled { reason: "venue pull".into() }),
            ("ask", OrderEventKind::Rejected { reason: "post-only would cross".into() }),
            ("bid", OrderEventKind::Denied { reason: "risk: min_qty".into() }),
            ("ask", OrderEventKind::Expired),
        ] {
            let mut m = plain_maker();
            m.on_quote_tick(&mut broker(0.0, 100), &quote(100, 0.40, 0.42));
            m.on_order_event(&mut broker(0.0, 110), &terminal(tag, kind.clone()));

            let mut b = broker(0.0, 120);
            m.on_quote_tick(&mut b, &quote(120, 0.41, 0.43));
            let submitted: Vec<String> = submits(&b).into_iter().map(|(t, _)| t).collect();
            assert_eq!(submitted, vec![tag.to_string()], "{kind:?} on {tag} must re-place it");
            let other = if tag == "bid" { "ask" } else { "bid" };
            assert_eq!(
                modify_tags(&b),
                vec![other.to_string()],
                "{kind:?} on {tag} must leave the OTHER side on the modify lane"
            );
        }
    }

    // INERT for everything that is not this maker's own dead single quote: a non-terminal Accept, a
    // LADDER rung tag (`"bid0"` — a prefix of `"bid"`'s tag, and explicitly out of scope), and an
    // UNTAGGED event. Each must leave both sides resting, i.e. the next tick still modifies both.
    #[test]
    fn non_terminal_foreign_and_untagged_events_change_nothing() {
        for ev in [
            terminal("bid", OrderEventKind::Accepted),
            terminal("bid0", OrderEventKind::Filled),
            terminal("hedge", OrderEventKind::Canceled { reason: String::new() }),
            OrderLifecycle {
                client_order_id: "coid-9".into(),
                tag: None,
                kind: OrderEventKind::Filled,
            },
        ] {
            let mut m = plain_maker();
            m.on_quote_tick(&mut broker(0.0, 100), &quote(100, 0.40, 0.42));
            m.on_order_event(&mut broker(0.0, 110), &ev);

            let mut b = broker(0.0, 120);
            m.on_quote_tick(&mut b, &quote(120, 0.41, 0.43));
            assert!(b.submissions.is_empty(), "{ev:?} must place nothing");
            assert_eq!(
                modify_tags(&b),
                vec!["bid".to_string(), "ask".to_string()],
                "{ev:?} must leave BOTH sides on the working modify lane"
            );
        }
    }

    // --- "Option B" cross-symbol underlying routing (on_mark → set_underlying) -------------------

    /// A-S params anchored on a 300 s time-to-resolution window closing at T = 300_000, with the
    /// underlying blend at `weight` (a `0.0` weight = the blend OFF control). Blackout off + fixed κ
    /// so the ONLY thing that moves between the two makers below is the underlying anchor.
    fn underlying_as_params(weight: f64) -> AsParams {
        AsParams {
            underlying_weight: weight,
            underlying_beta: 1.0,
            window_secs: 300.0,
            horizon_mode: HorizonMode::TimeToResolution,
            resolution_ts: Some(300_000),
            resolution_blackout_ms: 0,
            gamma: 0.5,
            kappa_mode: KappaMode::Fixed,
            q_scale: 1.0,
            min_standoff_ticks: 1.0,
            ..AsParams::default()
        }
    }

    /// An A-S maker on the 0.01 grid with the underlying blend at `weight` (its L1 quote lane snaps
    /// on `tick_size = 0.01`).
    fn underlying_maker(weight: f64) -> SpreadMaker {
        SpreadMaker::new(1.0, 0.01)
            .with_quote_style(QuoteStyle::Mid, 1, 0.01)
            .with_avellaneda_stoikov(underlying_as_params(weight))
    }

    /// A mark of the underlying series (a DIFFERENT symbol than the token the maker trades).
    fn a_mark(price: f64, ts: i64) -> MarkTick {
        MarkTick { symbol: "btcusdt".into(), price, ts }
    }

    /// The `"bid"`-tagged submit's price on a driven broker.
    fn bid_px(b: &LiveBroker) -> f64 {
        b.submissions
            .iter()
            .find(|s| s.tag.as_deref() == Some("bid"))
            .and_then(|s| s.price)
            .expect("a bid submit with a price")
    }

    // on_mark ROUTES the underlying into the A-S state: the first mark captures s_open only (no σ
    // yet ⇒ nothing fed), the second (a real move) seeds σ and feeds the full triple.
    #[test]
    fn on_mark_feeds_the_underlying_into_the_as_state() {
        let mut m = underlying_maker(0.5);
        assert_eq!(m.as_underlying(), None, "cold: no underlying until a mark warms the tracker");
        m.on_mark(&mut broker(0.0, 150_000), &a_mark(100.0, 150_000));
        assert_eq!(m.as_underlying(), None, "one mark ⇒ s_open only, still no σ ⇒ not fed");
        m.on_mark(&mut broker(0.0, 151_000), &a_mark(100.05, 151_000));
        let (s_now, s_open, sigma) = m.as_underlying().expect("fed after two marks");
        assert_eq!(s_now.to_bits(), 100.05_f64.to_bits(), "s_now is the latest mark");
        assert_eq!(s_open.to_bits(), 100.0_f64.to_bits(), "s_open is the window-open reference");
        assert!(sigma > 0.0, "a real move ⇒ a positive per-second σ, got {sigma}");
    }

    // OFF / byte-identical: a maker WITHOUT the A-S layer has no `as_state`, so on_mark is a no-op
    // (never panics, feeds nothing) — the trait default is a no-op and this override honours it.
    #[test]
    fn on_mark_is_inert_without_the_as_layer() {
        let mut m = SpreadMaker::new(1.0, 0.01);
        assert!(!m.as_enabled(), "precondition: no A-S layer");
        m.on_mark(&mut broker(0.0, 151_000), &a_mark(100.05, 151_000));
        assert!(!m.as_enabled(), "on_mark never turns A-S on as a side effect");
        assert_eq!(m.as_underlying(), None, "nothing fed");
    }

    // The routed underlying reaches the PRICING: warm two makers identically with an UP-drift
    // underlying and differ ONLY in the blend weight — the weighted maker anchors toward p_up > 0.5,
    // so its next quote sits STRICTLY above the book-mid-anchored control's.
    #[test]
    fn on_mark_blend_shifts_the_next_quote() {
        let mut fed = underlying_maker(0.5);
        let mut ctrl = underlying_maker(0.0);
        // identical warmup (same marks) so only the weight differs.
        fed.on_mark(&mut broker(0.0, 150_000), &a_mark(100.0, 150_000));
        fed.on_mark(&mut broker(0.0, 151_000), &a_mark(100.05, 151_000));
        ctrl.on_mark(&mut broker(0.0, 150_000), &a_mark(100.0, 150_000));
        ctrl.on_mark(&mut broker(0.0, 151_000), &a_mark(100.05, 151_000));
        assert!(fed.as_underlying().is_some() && ctrl.as_underlying().is_some(), "both warmed");
        // one quote tick at a 0.50 book mid; both place a two-sided quote.
        let q = quote(152_000, 0.49, 0.51);
        let mut bf = broker(0.0, 152_000);
        let mut bc = broker(0.0, 152_000);
        fed.on_quote_tick(&mut bf, &q);
        ctrl.on_quote_tick(&mut bc, &q);
        assert!(
            bid_px(&bf) > bid_px(&bc),
            "the underlying blend pulls the bid up: fed {} vs ctrl {}",
            bid_px(&bf),
            bid_px(&bc)
        );
    }
}
