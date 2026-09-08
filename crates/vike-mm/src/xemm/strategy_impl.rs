//! `impl<B: HftBroker> Strategy<B> for XemmMaker` — the tick / reference / fill / control lanes,
//! plus the durable-state DTO.
//!
//! # ⚠ WHICH LANES MAY EMIT, AND WHY IT IS NOT UNIFORM
//!
//! The live runtime drains a strategy's buffered intents against the DISPATCHING series, not the
//! mount's declared one — `drain_broker(ctx, venue, symbol, …)` is called with the bar's / tick's /
//! **fill's** `(venue, symbol)`, and a SYMBOL-LESS intent resolves to exactly that. For a two-venue
//! mount the drain series therefore differs between lanes. So:
//!
//! | lane | drain series | this maker's behaviour |
//! |---|---|---|
//! | `on_quote_tick` / `on_order_book` / `on_trade_tick` (maker venue) | MAKER | update `a_touch`, **requote** |
//! | `on_reference_quote` (taker venue) | **MAKER** — the lane drains on the mount's own series | update `b_touch`, **requote** |
//! | `on_fill`, maker leg | MAKER | fold, arm breaker, **fire the hedge**, requote |
//! | `on_fill`, hedge leg | **TAKER** | fold + hedge retry ONLY — **never a tagged SUBMIT** |
//! | `on_feed_status` (maker venue only) | MAKER | halt + pull |
//! | `on_schedule` | MAKER | the all-feeds-dead safety sweep |
//! | `on_params_updated` | MAKER | hot swap, **no order verb** |
//!
//! The hedge-leg row is the sharp one, and ⚠ **half its hazard is now HISTORY** — worth recording,
//! because the surviving half is the one that would actually bite a future edit here. It used to be
//! both: a `cancel_tagged("bid")` from a hedge-fill dispatch looked up
//! `{mount}|TAKER_VENUE|HEDGE_SYM|bid`, found nothing, and SILENTLY LEFT A REAL QUOTE RESTING on the
//! maker venue. That is fixed — `CoreThread::tag_key` keys the registry on the MOUNT's own series
//! for the insert and both lookups, so a tag names one quote from every lane
//! (`crates/vike-core/tests/wiring/multi_symbol_reads.rs`). What SURVIVES is the emission side, untouched
//! by that fix: a symbol-less SUBMIT — which every tagged submit is, by `HftBroker` contract — still
//! resolves to the DRAIN's series, so `submit_limit_tagged` from the hedge arm would place the
//! maker's quote on the TAKER venue. Only `Broker::submit_market(hedge_symbol, …)` is safe there —
//! it names its symbol, so `resolve_intent_venue` routes it correctly from EITHER dispatch.
//! `on_fill`'s hedge arm therefore still returns without touching `requote`/`pull_all`: `requote`'s
//! place arm is a tagged submit, and keeping the whole arm order-free is one rule rather than a
//! per-verb audit.
//!
//! `on_reference_quote` is safe to emit from for a second, independent reason:
//! `CoreThread::drive_strategy_reference_quote` deliberately drains on the MOUNT's own series, so a
//! quote PLACED there lands on the maker venue rather than the reference one (pinned by
//! `vike-core/tests/wiring/xemm_reference_quote.rs`). Making the reference lane the PRIMARY re-quote clock
//! is the whole point: a reference move is what makes a resting quote stale, and if venue A is
//! quiet while venue B moves, an observe-only reference lane would leave the maker to be picked
//! off — the exact adverse selection this strategy exists to avoid.
//!
//! `on_order_event` is still NOT implemented — but the reason has CHANGED, and the old one no
//! longer holds. It used to be that `OrderLifecycle` carried a client-order-id and a transition but
//! no symbol, while this maker never sees a coid (the tag registry is the runtime's, not the
//! strategy's), so it had no way to attribute an event to a leg at all. `OrderLifecycle::tag` now
//! carries the maker's OWN tag, stamped by the runtime from that registry, so attribution IS
//! available: a per-leg tag names the leg. What remains is that wiring it means deciding how a
//! rejected or externally-cancelled HEDGE leg folds back into `HedgeLedger`/`InFlight` — a hedge
//! accounting question, not a plumbing one, and the ledger is the one piece of state that must never
//! be wrong. Until that is designed, no reaction is better than a guessed one. (`SpreadMaker`'s own
//! `on_order_event` is the single-quote precedent: see `vike_mm::strategy_impl`.)

use serde::{Deserialize, Serialize};
use vike_model::{
    FeedStatus, Fill, HftBroker, L2Book, QuoteTick, Strategy, StrategyParams, TradeTick,
};

use super::XemmMaker;
use super::guards::{Halt, HaltReason};
use super::hedge::{HedgeLedger, InFlight};
use super::pricing::Touch;

/// Wire version this build writes and accepts. Bump on any breaking shape change to
/// [`XemmStateV1`]; [`XemmMaker::load_state`] ignores (fail-open, warns) any other value rather
/// than guessing at a migration — the `SpreadMakerStateV1` convention.
const XEMM_STATE_VERSION: u32 = 1;

/// Durable-state wire DTO — a DEDICATED type, never a derive on the live struct, so the maker's
/// internals can be reshaped without a version bump.
///
/// Persists the two things a restart must NOT silently reset:
///
/// 1. **the hedge ledger** — an unhedged position is REAL money at risk on two venues; forgetting
///    it means the maker restarts believing it is flat and quotes on top of an open exposure;
/// 2. **the halt latch** — a maker halted because its hedge would not fill, or because the pair
///    decoupled, must not un-halt merely by being restarted. That is the whole failure mode a
///    manual-resume default exists to prevent, and a restart is the easiest accidental resume.
///
/// Deliberately EXCLUDES: the two touches (a restart's first ticks re-seed them, and a stale saved
/// touch would be worse than none), the basis estimate (re-warms from live paired observations —
/// and a saved estimate could halt a healthy maker on a regime that has since ended), the breaker's
/// raw fill tape (the DEADLINES are its behavioural effect and are cheap to keep), and `cfg` (a
/// restart with a different tuning must price with the NEW tuning).
#[derive(Debug, Serialize, Deserialize)]
struct XemmStateV1 {
    v: u32,
    maker_pos: f64,
    hedge_pos: f64,
    hedge_attempts: u32,
    /// EVENT ts of an in-flight hedge, if one was outstanding at save time.
    hedge_in_flight_ts: Option<i64>,
    /// `Some((reason, since))` when halted.
    halt: Option<(HaltReason, i64)>,
    bid_suppressed_until: i64,
    ask_suppressed_until: i64,
}

impl<B: HftBroker> Strategy<B> for XemmMaker {
    /// The MAKER venue's L1 lane: record venue A's own touch (the passive clamp's anchor) and
    /// re-quote. Note this is NOT the pricing input — venue B's touch is (`on_reference_quote`);
    /// A's book is only ever used to keep the quote non-marketable.
    fn on_quote_tick(&mut self, broker: &mut B, q: &QuoteTick) {
        self.observe_touch(false, Touch { bid: q.bid, ask: q.ask, ts: q.ts });
        self.requote(broker, q.ts);
    }

    /// The MAKER venue's L2 lane. Learns the venue's REAL tick grid from the book and adopts it on
    /// the L1 lane too (`learned_maker_tick`), so the standoff and the directional snap can never
    /// resolve against two different grids. A one-sided book has no touch to anchor on, so it
    /// updates nothing and lets the freshness bound speak.
    fn on_order_book(&mut self, broker: &mut B, book: &L2Book) {
        if book.tick_size > 0.0 {
            self.learned_maker_tick = Some(book.tick_size);
        }
        let now = broker.now();
        if let (Some((bid, _)), Some((ask, _))) = (book.best_bid(), book.best_ask()) {
            self.observe_touch(false, Touch { bid, ask, ts: now });
        }
        self.requote(broker, now);
    }

    /// The MAKER venue's trade tape. Carries no touch, so it updates nothing — but it DOES advance
    /// the clock through `requote`, which is what lets a freshness bound fire on a venue that is
    /// printing trades while its quote feed has silently frozen.
    fn on_trade_tick(&mut self, broker: &mut B, t: &TradeTick) {
        self.requote(broker, t.ts);
    }

    /// **THE PRICING INPUT** — the REFERENCE (taker) venue's touch, and the maker's PRIMARY
    /// re-quote clock. See the module doc for why emitting from this lane is safe (the runtime
    /// drains it on the mount's OWN series) and why observe-only would be worse than useless.
    fn on_reference_quote(&mut self, broker: &mut B, _venue: &str, q: &QuoteTick) {
        self.observe_touch(true, Touch { bid: q.bid, ask: q.ask, ts: q.ts });
        self.requote(broker, q.ts);
    }

    /// Fills, routed by `Fill::symbol` — the only leg discriminator available (a `Fill` carries a
    /// symbol but no venue), which is why distinct leg symbols are a construction-time assertion.
    ///
    /// An EMPTY symbol is read as the MAKER leg, following the same `""` means "my mount"
    /// convention `LiveBroker::named` uses for the order verbs. Live fills always carry a real
    /// symbol (`dispatch_applied_fills` stamps `f.symbol`), so this only affects test/paper doubles.
    ///
    /// A fill for NEITHER leg fails CLOSED (`HaltReason::UnknownFillSymbol`): the maker can no
    /// longer account for its own inventory, and continuing to quote on top of an exposure it
    /// cannot measure is the worst available option. It latches only — no order verb — because the
    /// dispatch series of an unattributable fill is unknown, and a tagged cancel from the wrong
    /// series is a silent no-op. The next maker-venue lane pulls both sides through `requote`'s
    /// halt branch.
    fn on_fill(&mut self, broker: &mut B, fill: &Fill) {
        if fill.symbol == self.hedge_symbol {
            // ⚠ THE HEDGE LEG — this dispatch drains on the TAKER venue's series. Fold and (if the
            // hedge is still short of its target) re-fire, using the SYMBOL-CARRYING market verb
            // only. No tagged verb, no `requote`, no `pull_all`: see the module doc.
            self.hedge.on_hedge_fill(fill.side, fill.size);
            self.drive_hedge(broker, fill.ts);
            return;
        }
        if !fill.symbol.is_empty() && fill.symbol != self.maker_symbol {
            self.enter_halt(HaltReason::UnknownFillSymbol, fill.ts);
            return;
        }
        // The MAKER leg — this dispatch drains on the mount's OWN series, so every verb is safe.
        self.hedge.on_maker_fill(fill.side, fill.size);
        if fill.side != 0 {
            // `SideState::own` is the INTENDED quote, not the venue-side remainder, so a partial
            // fill's top-up would otherwise read as "no change" and be silently lost.
            self.side_mut(fill.side > 0).refresh_stale = true;
        }
        self.record_maker_fill(fill.side, fill.size, fill.ts);
        // HEDGE FIRST, quote second: the exposure this fill just created is the urgent thing, and
        // re-quoting first would leave it open for the length of the quote path.
        self.drive_hedge(broker, fill.ts);
        self.requote(broker, fill.ts);
        self.drive_flatten(broker);
    }

    /// The MAKER venue's feed health (the only one the runtime routes here — `FeedStatus` carries
    /// neither venue nor symbol, so the reference venue's status is not deliverable). Any non-`Live`
    /// status halts and pulls: without its own book the passive clamp has no anchor.
    ///
    /// The REFERENCE venue's liveness is covered by `max_ref_age_ms`, which is strictly stronger
    /// than a status subscription anyway — it also catches a feed that freezes without ever
    /// reporting a disconnect.
    fn on_feed_status(&mut self, broker: &mut B, status: FeedStatus) {
        if status != FeedStatus::Live {
            self.enter_halt(HaltReason::FeedImpaired, broker.now());
            self.pull_all(broker);
        }
    }

    /// THE ALL-FEEDS-DEAD SAFETY SWEEP. Every other lane fires off a market message, so if BOTH
    /// venues go silent none of them run and a stale quote would rest forever. The runtime's
    /// wall-clock schedule is the one lane that fires regardless, and it drains on the mount's own
    /// series — so it can safely re-run the whole funnel: the freshness bounds trip, the maker
    /// halts, both quotes come off, and any owed hedge gets its retry.
    fn on_schedule(&mut self, broker: &mut B, _tag: &str) {
        let now = broker.now();
        self.drive_hedge(broker, now);
        self.requote(broker, now);
        self.drive_flatten(broker);
    }

    /// The live-parameter plane. Consumes ONLY its own variant; a foreign one (e.g. a sibling
    /// `SpreadMaker` mount's) is ignored, which is the live-params contract. The broker is
    /// UNTOUCHED — the new knobs take effect on the next tick's in-place re-price, so a re-tune
    /// that does not move a price does not disturb the book at all.
    fn on_params_updated(&mut self, _broker: &mut B, params: &StrategyParams) {
        if let StrategyParams::Xemm(p) = params {
            self.apply_params(p);
        }
    }

    /// The READ half: this maker's whole current tuning, via [`XemmMaker::params`] — the accessor
    /// that is already documented as the inverse of `apply_params`. It answers honestly because
    /// `cfg` IS the live bag (nothing is stripped out of it into a separate estimator, unlike
    /// `SpreadMaker`'s A-S sub-bag), so what comes back is exactly what the next requote prices
    /// from. The two SYMBOLS stay out of it deliberately — they are mount IDENTITY, not tunables.
    fn params(&self) -> Option<StrategyParams> {
        Some(StrategyParams::Xemm(XemmMaker::params(self)))
    }

    /// Persist the hedge ledger + the halt latch — see [`XemmStateV1`] for exactly what and why.
    /// Always `Some`: even a flat, running maker has a ledger worth round-tripping, and a `None`
    /// here would be indistinguishable from "this build forgot to save".
    fn save_state(&self) -> Option<serde_json::Value> {
        let dto = XemmStateV1 {
            v: XEMM_STATE_VERSION,
            maker_pos: self.hedge.maker_pos,
            hedge_pos: self.hedge.hedge_pos,
            hedge_attempts: self.hedge.attempts,
            hedge_in_flight_ts: self.hedge.in_flight.map(|f| f.ts),
            halt: match self.halt {
                Halt::Running => None,
                Halt::Halted { reason, since } => Some((reason, since)),
            },
            bid_suppressed_until: self.bid.suppressed_until,
            ask_suppressed_until: self.ask.suppressed_until,
        };
        serde_json::to_value(&dto).ok()
    }

    /// FAIL-OPEN restore: an unparsable payload or a version mismatch is warned and IGNORED, so a
    /// shape this build does not recognize can never be half-applied.
    ///
    /// ⚠ A restore can only make the maker MORE cautious, never less: a saved halt re-latches, a
    /// saved exposure is re-adopted. It never clears a halt the freshly-constructed maker holds
    /// (a fresh maker is `Running`, so `None` here simply leaves it running).
    fn load_state(&mut self, state: &serde_json::Value) {
        let dto: XemmStateV1 = match serde_json::from_value(state.clone()) {
            Ok(dto) => dto,
            Err(err) => {
                tracing::warn!(%err, "XemmMaker::load_state: unparsable saved state, keeping fresh");
                return;
            }
        };
        if dto.v != XEMM_STATE_VERSION {
            tracing::warn!(
                found_version = dto.v,
                expected_version = XEMM_STATE_VERSION,
                "XemmMaker::load_state: saved state version mismatch, keeping fresh state"
            );
            return;
        }
        self.hedge = HedgeLedger {
            maker_pos: dto.maker_pos,
            hedge_pos: dto.hedge_pos,
            attempts: dto.hedge_attempts,
            in_flight: dto.hedge_in_flight_ts.map(|ts| InFlight { ts }),
        };
        self.halt = match dto.halt {
            None => Halt::Running,
            Some((reason, since)) => Halt::Halted { reason, since },
        };
        self.bid.suppressed_until = dto.bid_suppressed_until;
        self.ask.suppressed_until = dto.ask_suppressed_until;
    }
}
