//! Order-latency models for the tick/book replay backtest (`run_ticks`).
//!
//! Without a latency model a simulated order is submitted, matched and canceled with ZERO
//! delay: the strategy sees a tick and its order is already resting at the matching engine on
//! that SAME tick, and a cancel issued on the tick that would have filled it always wins. That
//! gives a maker impossible reflexes and is the single largest source of optimism left in the
//! replay path after the queue-position model ([`crate::queue_model`], its natural partner).
//!
//! This module supplies the missing clock skew — an independent reimplementation of the
//! two-leg order-latency mechanism popularized by hftbacktest (no code taken from it):
//!
//! - **entry latency** — local submit time `T` → the action becomes visible to the matching
//!   logic at `T + entry()`. Everything the strategy sends (new orders, cancels, amends) is
//!   held in an in-flight queue keyed by that delivery timestamp.
//! - **response latency** — the exchange-side event at `exchange_timestamp` → the strategy learns about it
//!   at `exchange_timestamp + response()`. Fills are therefore delivered to `Strategy::on_fill` LATE.
//!
//! WHAT THE RESPONSE LEG COVERS, EXACTLY. Only the strategy's VIEW is delayed, never the
//! exchange's own state: `OrderBrokerSimulator::apply_fill` folds a fill into `symbol[si].pos`, cash and the
//! equity curve at the instant it matches, and `BacktestResult` is computed from that truth.
//! What the response leg holds back is (a) the `Strategy::on_fill` callback and (b) the
//! strategy-visible SHADOW POSITION — a per-symbol signed size advanced ONLY by DELIVERED fills,
//! which `OrderBroker::position` reads while the gate is armed (see
//! `OrderBrokerSimulator::shadow_pos`). That shadow is what makes the leg mean something for a maker that
//! polls its inventory instead of accumulating it in `on_fill`: the spread maker file's `SpreadMaker` reads
//! `position("")` on every requote to drive its inventory skew and its Avellaneda–Stoikov
//! reservation price, and before the shadow existed it saw every fill with ZERO latency no matter
//! what `response_ns` said — the leg was inert for the flagship maker.
//!
//! STILL BYPASSING THE MODEL (know these before trusting a number): `OrderBrokerSimulator::position_of`,
//! `pending_of`, `equity_now`/`OrderBroker::equity`, `drawdown_now` and the target-percent verbs all
//! read EXCHANGE TRUTH synchronously and are deliberately NOT shadowed — they are the engine's own
//! accounting surface, and shadowing them would make `on_stop` disagree with `BacktestResult`. A
//! strategy that sizes off `equity()` or `position_of()` therefore still reacts to a fill with
//! zero response latency. Likewise an in-flight ORDER is invisible to `pending_of` (see
//! `OrderBrokerSimulator::in_flight_of` for the read that does see it).
//!
//! Both are signed nanoseconds. A NEGATIVE entry latency encodes a recorded **rejection**: the
//! request never reached the matching engine (see [`LatencyRow`]), so the engine drops the
//! action outright rather than delivering it.
//!
//! UNITS: the engine's own timestamps are epoch **milliseconds** (`Bar::ts`, `TickQuote::ts`);
//! latencies here are **nanoseconds**. The wire-in keys its in-flight queue in nanoseconds
//! (`timestamp_ms * 1_000_000 + latency_ns`) so a sub-millisecond latency is not silently truncated —
//! it simply means the action lands on the next tick whose millisecond stamp has caught up.
//!
//! OPT-IN AND REPLAY-ONLY: activated by `StrategyEngineConfig::latency_model = Some(kind)`. `None`
//! (the default) means no in-flight queue is ever built, no deferral happens and every existing
//! code path is byte-identical. When set, only `StrategyEngine::run_ticks` arms it; the bar
//! engine (`StrategyEngine::run`) and the vectorized kernel never consult it.

use std::sync::Arc;

/// The minimal order descriptor a latency model may condition on. Deliberately tiny (no engine
/// types) so a user impl stays pure and the seam does not leak `OrderResting` internals.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LatencyOrder {
    /// `+1` buy / `-1` sell; `0` for actions with no side (e.g. a bulk cancel).
    pub side: i32,
    /// order quantity (`0.0` when the action carries none).
    pub qty: f64,
    /// limit price, `None` for market/cancel actions.
    pub price: Option<f64>,
}

impl LatencyOrder {
    /// A side-less, size-less action (cancel-all and friends).
    pub const NONE: LatencyOrder = LatencyOrder { side: 0, qty: 0.0, price: None };

    pub fn new(side: i32, qty: f64, price: Option<f64>) -> Self {
        LatencyOrder { side, qty, price }
    }
}

/// The two-leg order-latency seam. Both verbs are pure functions of `(ts, order)` — no I/O,
/// no clock, no interior mutation — so a replay is reproducible.
pub trait LatencyModel {
    /// Nanoseconds from the strategy's local submit at `ts` (epoch **ms**) until the action is
    /// visible to the matching engine. A NEGATIVE value means the request was rejected before
    /// ever reaching the matching engine, and the engine drops the action.
    fn entry(&self, ts: i64, order: &LatencyOrder) -> i64;

    /// Nanoseconds from an exchange-side event at `ts` (epoch **ms**) until the strategy learns
    /// about it. Clamped to `>= 0` by the wire-in (a result cannot arrive before it happened).
    fn response(&self, ts: i64, order: &LatencyOrder) -> i64;
}

/// Fixed latency on both legs — the calibration-free baseline. `entry_ns: 0`/`response_ns: 0`
/// reproduces the zero-latency behavior exactly (while still exercising the in-flight queue,
/// which is why it is NOT the same thing as `latency_model: None`: an action submitted at `T`
/// still only applies at the NEXT drain, i.e. the next tick at `ts >= T`).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct LatencyModelConstant {
    pub entry_ns: i64,
    pub response_ns: i64,
}

impl LatencyModelConstant {
    pub fn new(entry_ns: i64, response_ns: i64) -> Self {
        LatencyModelConstant { entry_ns, response_ns }
    }
}

impl LatencyModel for LatencyModelConstant {
    fn entry(&self, _ts: i64, _order: &LatencyOrder) -> i64 {
        self.entry_ns
    }

    fn response(&self, _ts: i64, _order: &LatencyOrder) -> i64 {
        self.response_ns
    }
}

/// One recorded round trip, all three stamps in epoch **nanoseconds**.
///
/// - `request_timestamp` — when the local process sent the request.
/// - `exchange_timestamp` — when the matching engine acted on it. **`<= 0` marks a REJECTION**: the
///   request never reached the matching engine (a rate-limit knock-back, a rejected auth, a
///   dropped connection). Such a row's entry latency is reported as the NEGATIVE round trip
///   `-(response_timestamp - request_timestamp)` — negative meaning "technically rejected", magnitude meaning "and
///   here is how long the strategy waited to be told".
/// - `response_timestamp` — when the local process received the acknowledgement.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LatencyRow {
    pub request_timestamp: i64,
    pub exchange_timestamp: i64,
    pub response_timestamp: i64,
}

impl LatencyRow {
    pub fn new(request_timestamp: i64, exchange_timestamp: i64, response_timestamp: i64) -> Self {
        LatencyRow { request_timestamp, exchange_timestamp, response_timestamp }
    }

    /// Did this row record a request that never reached the matching engine?
    #[inline]
    pub fn is_rejection(&self) -> bool {
        self.exchange_timestamp <= 0
    }

    /// This row's entry latency: `exchange_timestamp - request_timestamp`, or the NEGATIVE round trip for a rejection.
    #[inline]
    pub fn entry_ns(&self) -> i64 {
        if self.is_rejection() {
            -(self.response_timestamp - self.request_timestamp)
        } else {
            self.exchange_timestamp - self.request_timestamp
        }
    }

    /// This row's response latency: `response_timestamp - exchange_timestamp`, or the whole round trip for a
    /// rejection (there is no exchange stamp to measure from).
    #[inline]
    pub fn response_ns(&self) -> i64 {
        if self.is_rejection() {
            self.response_timestamp - self.request_timestamp
        } else {
            self.response_timestamp - self.exchange_timestamp
        }
    }
}

/// Latency interpolated from a RECORDED series of round trips — the calibrated model.
///
/// Lookup is a binary search for the bracketing pair, then a LINEAR interpolation of that pair's
/// latencies. The two legs interpolate along DIFFERENT abscissae, so the model keeps TWO indexes
/// — a binary search is only well-defined over a series sorted by the key it searches:
///
/// - the entry leg interpolates in `request_timestamp` (that is the stamp the caller has at submit time),
///   over every row, sorted by `request_timestamp` (the constructor sorts an arbitrarily ordered capture);
/// - the response leg interpolates in `exchange_timestamp` (the stamp of the exchange-side event), over a
///   SECOND index sorted by `exchange_timestamp` with REJECTION rows excluded. Both exclusions matter:
///   `exchange_timestamp` is not monotone in `request_timestamp` for ordinary pipelined round trips (a request sent
///   first can be acked last), and a rejection has no `exchange_timestamp` at all (`<= 0`), so leaving
///   either in would break the search's monotone predicate and could hand back an unrelated
///   row's — or a rejection's whole round-trip — latency. A series of nothing but rejections
///   leaves the response index empty, and an empty index is inert (`0`).
///
/// **Edge clamping**: a `ts` before the first row's stamp yields the FIRST row's latency and a
/// `ts` after the last row's stamp the LAST row's — never an extrapolation off the ends.
///
/// **Rejections are not interpolated across.** If either bracketing row is a rejection
/// ([`LatencyRow::is_rejection`]) the nearer of the two rows' latency is returned verbatim, so
/// a negative entry latency stays negative (blending it with a healthy neighbour would silently
/// turn a recorded rejection into a merely-slow accept). An empty series is inert: both legs
/// return `0`.
#[derive(Debug, Clone)]
pub struct LatencyModelInterpolated {
    /// every row, sorted by `request_timestamp` — the ENTRY leg's index
    rows: Arc<[LatencyRow]>,
    /// accepted rows only, sorted by `exchange_timestamp` — the RESPONSE leg's index (see the type doc)
    by_exchange: Arc<[LatencyRow]>,
}

impl LatencyModelInterpolated {
    /// Build from a recorded series; the rows are sorted by `request_timestamp` (stable) if needed, and the
    /// response-leg index is derived from them (accepted rows only, sorted by `exchange_timestamp`).
    pub fn new(rows: impl Into<Vec<LatencyRow>>) -> Self {
        let mut rows: Vec<LatencyRow> = rows.into();
        if !rows.windows(2).all(|w| w[0].request_timestamp <= w[1].request_timestamp) {
            rows.sort_by_key(|r| r.request_timestamp);
        }
        let mut by_exchange: Vec<LatencyRow> =
            rows.iter().copied().filter(|r| !r.is_rejection()).collect();
        // stable sort: equal `exchange_timestamp` keeps `request_timestamp` order, so the model stays deterministic
        by_exchange.sort_by_key(|r| r.exchange_timestamp);
        LatencyModelInterpolated { rows: rows.into(), by_exchange: by_exchange.into() }
    }

    pub fn rows(&self) -> &[LatencyRow] {
        &self.rows
    }

    /// The response leg's index: accepted rows only, sorted by `exchange_timestamp`.
    pub fn response_rows(&self) -> &[LatencyRow] {
        &self.by_exchange
    }

    /// The shared bracket-and-blend over a series ALREADY SORTED BY `key`. `key` reads a row's
    /// interpolation stamp, `val` its latency. Both callers uphold that precondition by
    /// construction (see the type doc); a binary search over an unsorted key is meaningless.
    fn interp(
        rows: &[LatencyRow],
        timestamp_ns: i64,
        key: impl Fn(&LatencyRow) -> i64,
        val: impl Fn(&LatencyRow) -> i64,
    ) -> i64 {
        if rows.is_empty() {
            return 0;
        }
        // partition_point: index of the first row whose key is STRICTLY greater than timestamp_ns.
        let hi = rows.partition_point(|r| key(r) <= timestamp_ns);
        if hi == 0 {
            return val(&rows[0]); // before the series — clamp to the first row
        }
        if hi >= rows.len() {
            return val(&rows[rows.len() - 1]); // after the series — clamp to the last
        }
        let (lo_row, hi_row) = (&rows[hi - 1], &rows[hi]);
        let (lo_k, hi_k) = (key(lo_row), key(hi_row));
        let (lo_v, hi_v) = (val(lo_row), val(hi_row));
        if lo_row.is_rejection() || hi_row.is_rejection() {
            // Never blend a rejection with an accept: snap to the NEARER row (ties → the lower,
            // which is the row whose request actually preceded `timestamp_ns`). Well-formed because the
            // series is sorted by `key`, so `lo_k <= timestamp_ns < hi_k` and both distances are >= 0.
            // Only the ENTRY leg can reach this — the response index excludes rejections.
            return if (timestamp_ns - lo_k) <= (hi_k - timestamp_ns) { lo_v } else { hi_v };
        }
        let span = hi_k - lo_k;
        if span <= 0 {
            // Defensive only: `partition_point` consumes an entire run of equal keys, so a sorted
            // series always brackets with `lo_k <= timestamp_ns < hi_k` and this is unreachable. Kept as
            // the divide-by-zero guard should a future caller ever break that precondition.
            return lo_v; // no gradient to interpolate along
        }
        // i128 so a wide ns span times a wide ns latency cannot overflow i64 mid-product.
        let numerator = (hi_v - lo_v) as i128 * (timestamp_ns - lo_k) as i128;
        lo_v + (numerator / span as i128) as i64
    }
}

/// Epoch **ms** (the engine's stamp) → epoch **ns** (the recorded series' stamp), saturating.
#[inline]
fn ms_to_ns(ts_ms: i64) -> i64 {
    ts_ms.saturating_mul(1_000_000)
}

impl LatencyModel for LatencyModelInterpolated {
    fn entry(&self, ts: i64, _order: &LatencyOrder) -> i64 {
        Self::interp(&self.rows, ms_to_ns(ts), |r| r.request_timestamp, |r| r.entry_ns())
    }

    fn response(&self, ts: i64, _order: &LatencyOrder) -> i64 {
        Self::interp(&self.by_exchange, ms_to_ns(ts), |r| r.exchange_timestamp, |r| r.response_ns())
    }
}

/// The `StrategyEngineConfig` selector — which latency model gates the tick-replay order path.
/// `Clone` (not `Copy`) because the interpolated variant carries its recorded series; the
/// `Arc` keeps the clone cheap.
#[derive(Debug, Clone)]
pub enum LatencyModelKind {
    /// [`LatencyModelConstant`] — fixed nanoseconds on both legs.
    Constant { entry_ns: i64, response_ns: i64 },
    /// [`LatencyModelInterpolated`] over a recorded round-trip series.
    Intp(LatencyModelInterpolated),
}

// ⚠ **Two `i64` ALIASES stood here and are RETIRED** — `VENUE_HOLD_POLYMARKET_UPDOWN_MS` and
// `VENUE_HOLD_POLYMARKET_SPORTS_GAME_MS`, each `const X: i64 = vike_model::Y as i64` over a venue
// fact that crate owns. That is the alias shape the root `CLAUDE.md`'s one-name rule lists, and the
// name had already SPLIT (one caller spelled the flat crate-root name, another the `latency::`
// path), so both went when the simulator left `vike-backtest` (docs/decisions/0087). The venue holds
// have ONE home, `vike_venues::venue_hold`, and a caller doing nanosecond arithmetic widens the `u32`
// itself: `i64::from(vike_venues::POLYMARKET_ITODE_HOLD_MS)`. How to MODEL a hold with this module's
// machinery — the part of their docs that was about the engine rather than the venue — now lives on
// `LatencyModelKind::venue_hold_ms` below.

impl LatencyModelKind {
    /// Fixed latency on both legs.
    pub fn constant(entry_ns: i64, response_ns: i64) -> Self {
        LatencyModelKind::Constant { entry_ns, response_ns }
    }

    /// A VENUE-SIDE HOLD of `ms` milliseconds on the entry leg only — the order is deliberately
    /// delayed by the exchange before it is matched or booked (the venue facts, measured, are on
    /// [`vike_venues::POLYMARKET_ITODE_HOLD_MS`] and
    /// [`vike_venues::POLYMARKET_SPORTS_GAME_HOLD_MS`]). The response leg is left at zero
    /// because a hold is not a round trip: it delays when the venue ACTS, not when the strategy
    /// hears about it.
    ///
    /// Compose with a wire round trip by adding the two entry legs — this constructor deliberately
    /// does not guess one. A deployment pays the hold ON TOP of its own network round trip, so a
    /// realistic entry leg is `wire_ns + i64::from(vike_venues::POLYMARKET_ITODE_HOLD_MS) * 1_000_000`.
    ///
    /// Paired with [`crate::engine::FillModelKind::L2Book`], a venue hold's semantics fall out of
    /// machinery that already exists — no bespoke code path:
    ///
    /// * the order is decided by the strategy on the book at `T`;
    /// * the in-flight queue delivers it to the matching logic at `T + hold`;
    /// * the L2 tier matches it against the book AS IT THEN STANDS;
    /// * an order that no longer crosses does not fill and **rests** — which is precisely the venue's
    ///   own "matched or booked" second validation, and is a different outcome from a miss.
    ///
    /// ⚠ Prefer the PER-SYMBOL hold table the gate builds from `StrategyEngineConfig::properties`
    /// (`vike_model::SymbolProperties::taker_hold_ms`, recorded off the wire) over this constructor
    /// wherever a tape carries more than one kind of market: the hold is a per-MARKET venue property
    /// (Polymarket's crypto up/down `itode` hold and its sports-game delay are two independent
    /// mechanisms, and the esports book declares a third value), so no single constant models a
    /// mixed tape.
    pub fn venue_hold_ms(ms: i64) -> Self {
        LatencyModelKind::Constant { entry_ns: ms.saturating_mul(1_000_000), response_ns: 0 }
    }

    /// Interpolated latency over a recorded series.
    pub fn intp(rows: impl Into<Vec<LatencyRow>>) -> Self {
        LatencyModelKind::Intp(LatencyModelInterpolated::new(rows))
    }

    /// Build the concrete model once (the gate holds it for the whole replay).
    pub fn build(&self) -> Box<dyn LatencyModel> {
        match self {
            LatencyModelKind::Constant { entry_ns, response_ns } => {
                Box::new(LatencyModelConstant::new(*entry_ns, *response_ns))
            }
            LatencyModelKind::Intp(m) => Box::new(m.clone()),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Engine-side in-flight queue (the wire-in state). pub(crate): only the engine drives it.
// ---------------------------------------------------------------------------------------------

/// One order-path mutation held in flight between local submit and exchange delivery.
///
/// These are exactly the strategy-facing write verbs of `OrderBrokerSimulator` (a tagged quote's verbs are
/// the `OrderBroker` ones); capturing them as data (instead of applying
/// them) is what buys the delay without duplicating any fill logic — the drain replays each
/// variant through the SAME apply path the zero-latency engine uses.
pub(crate) enum LatencyModelInFlightAction {
    /// `OrderBrokerSimulator::push_pending` — a new working order for symbol index `si`, a tagged quote's too.
    Push { si: usize, order: vike_model::OrderResting },
    /// `OrderBrokerSimulator::cancel_all` — drop every resting order for symbol index `si` but its quotes.
    CancelAll { si: usize },
    /// `OrderBroker::amend` — amend the order `qid` of symbol index `si` in place (resolved from its
    /// tag when the strategy amended: by delivery the tag may name a newer order).
    AmendQueueId {
        si: usize,
        qid: u64,
        new_qty: Option<f64>,
        new_price: Option<f64>,
        new_trigger: Option<f64>,
    },
    /// `OrderBroker::cancel` — cancel the order `qid` of symbol index `si`, placed under `tag`.
    CancelQueueId { si: usize, qid: u64, tag: String },
}

/// An in-flight action plus its delivery stamp and submission sequence.
pub(crate) struct LatencyModelInFlight {
    /// epoch NANOSECONDS at which the matching engine sees this action.
    pub delivery_ns: i64,
    /// Monotonic submission counter — the tiebreak that keeps a batch sharing ONE `delivery_ns`
    /// in the order the strategy issued it (a cancel then a re-submit must not swap).
    ///
    /// PRECONDITION, and it is the model's to uphold: the ordering guarantee holds only where the
    /// two actions get the same `delivery_ns`, i.e. for a model whose `entry` does not depend on
    /// `order`. Both shipped models qualify ([`LatencyModelConstant`] and [`LatencyModelInterpolated`] ignore
    /// the descriptor entirely). A USER model that returns a different entry latency for a bare
    /// cancel ([`LatencyOrder::NONE`], which is what `cancel_all`/`cancel` pass) than for
    /// the sized re-submit that follows it CAN legitimately reorder the pair — and that is the
    /// model speaking, not a bug: a slower cancel really does land after a faster new order live.
    /// Note the consequence for a maker, though: a requote whose cancel lands last leaves the tag
    /// pulled rather than re-priced, so condition `entry` on `order` only deliberately.
    pub sequence: u64,
    pub action: LatencyModelInFlightAction,
}

/// The armed latency gate: the built model + the in-flight queue.
///
/// The queue is bounded in practice, not by an arbitrary cap: every price tick drains
/// everything whose delivery stamp has arrived, so it only ever holds the actions a strategy
/// issued within ONE latency window. It is a plain `Vec` in submission order; the drain
/// partitions it (preserving that order for the survivors) and sorts only the DUE slice by
/// `(delivery_ns, seq)`, which is what makes a varying entry latency reorder correctly while
/// staying deterministic.
pub(crate) struct LatencyGate {
    pub model: Box<dyn LatencyModel>,
    pub queue: Vec<LatencyModelInFlight>,
    next_sequence: u64,
    /// Per-symbol VENUE HOLD in NANOSECONDS, indexed like `OrderBrokerSimulator::symbols` — the venue's own
    /// declared delay for that market (`vike_model::SymbolProperties::taker_hold_ms`), resolved
    /// ONCE when the gate is armed and never re-read (a hold is a property of the market, not of
    /// the moment; re-reading it per action would make the queue's ordering depend on a PIT lookup
    /// on the hot path).
    ///
    /// **EMPTY is the default and the acceptance bar**: with no properties source, or with every
    /// symbol declaring `0`, this stays empty, [`Self::hold_ns`] returns `0` on an `is_empty`
    /// branch, and `delivery_ns` is the exact expression the pre-hold gate computed — byte-identical.
    holds_ns: Vec<i64>,
}

impl LatencyGate {
    /// The PRE-HOLD constructor, kept as the byte-identity REFERENCE the tests measure
    /// [`Self::with_holds`] against (`an_empty_hold_table_is_byte_identical_to_no_table`). The
    /// engine always arms through `with_holds`, which passes an empty table on every path that
    /// declares no hold — so this is deliberately test-only rather than a second live entry point.
    #[cfg(test)]
    pub(crate) fn new(kind: &LatencyModelKind) -> Self {
        LatencyGate {
            model: kind.build(),
            queue: Vec::new(),
            next_sequence: 0,
            holds_ns: Vec::new(),
        }
    }

    /// Arm with a per-symbol venue-hold table (see [`Self::holds_ns`]). An ALL-ZERO or empty table
    /// is normalized to empty here, so "every venue that declares no hold" and "no properties
    /// source at all" are the same, provably-inert state rather than two.
    pub(crate) fn with_holds(kind: &LatencyModelKind, holds_ns: Vec<i64>) -> Self {
        let holds_ns = if holds_ns.iter().all(|&h| h == 0) { Vec::new() } else { holds_ns };
        LatencyGate { model: kind.build(), queue: Vec::new(), next_sequence: 0, holds_ns }
    }

    /// The venue hold (ns) an action must serve ON TOP of the model's entry latency: the hold of
    /// the symbol the action names (a tagged quote's is its order's symbol, the market it rests on).
    #[inline]
    fn hold_ns(&self, action: &LatencyModelInFlightAction) -> i64 {
        if self.holds_ns.is_empty() {
            return 0; // the frozen path: no table, no lookup, no change to `delivery_ns`
        }
        let si = match action {
            LatencyModelInFlightAction::Push { si, .. }
            | LatencyModelInFlightAction::CancelAll { si }
            | LatencyModelInFlightAction::AmendQueueId { si, .. }
            | LatencyModelInFlightAction::CancelQueueId { si, .. } => *si,
        };
        self.holds_ns.get(si).copied().unwrap_or(0)
    }

    /// Hold `action` in flight. Returns `false` when the model reported a NEGATIVE entry
    /// latency — a recorded rejection: the request never reached the matching engine, so the
    /// action is discarded and never queued.
    ///
    /// A rejection is decided BEFORE the venue hold is added: the venue never saw the request, so
    /// it cannot have held it.
    pub(crate) fn submit(
        &mut self,
        ts_ms: i64,
        desc: &LatencyOrder,
        action: LatencyModelInFlightAction,
    ) -> bool {
        let lat = self.model.entry(ts_ms, desc);
        if lat < 0 {
            return false; // rejected before reaching the matching engine
        }
        // The venue's own hold rides ON TOP of the model's entry leg (wire) — they are separate
        // delays a live order pays in series, which is exactly what `LatencyModelKind::venue_hold_ms`'s
        // docs state.
        let lat = lat.saturating_add(self.hold_ns(&action));
        let seq = self.next_sequence;
        self.next_sequence += 1;
        self.queue.push(LatencyModelInFlight {
            delivery_ns: ms_to_ns(ts_ms).saturating_add(lat),
            sequence: seq,
            action,
        });
        true
    }

    /// Response latency for an exchange-side event at `timestamp_ms`, clamped to `>= 0` (a result can
    /// never become visible before it happened).
    pub(crate) fn response_ns(&self, ts_ms: i64, desc: &LatencyOrder) -> i64 {
        self.model.response(ts_ms, desc).max(0)
    }

    /// Remove and return every action whose delivery stamp is at or before `timestamp_ms`, ordered by
    /// `(delivery_ns, seq)`. Survivors keep their submission order.
    pub(crate) fn drain_due(&mut self, ts_ms: i64) -> Vec<LatencyModelInFlight> {
        if self.queue.is_empty() {
            return Vec::new();
        }
        let now_ns = ms_to_ns(ts_ms);
        let mut due: Vec<LatencyModelInFlight> = Vec::new();
        let mut still: Vec<LatencyModelInFlight> = Vec::new();
        for f in self.queue.drain(..) {
            if f.delivery_ns <= now_ns {
                due.push(f);
            } else {
                still.push(f);
            }
        }
        self.queue = still;
        due.sort_by_key(|f| (f.delivery_ns, f.sequence));
        due
    }

    /// How many actions are still in flight. Note there is deliberately NO end-of-run flush for
    /// the order queue: an action undelivered when the tape ends never reached the venue, so it
    /// must not retro-actively apply. (The RESPONSE leg is flushed — see
    /// `StrategyEngine::flush_deferred_fills`.)
    pub(crate) fn in_flight_len(&self) -> usize {
        self.queue.len()
    }

    /// Signed size of the MARKET orders held in flight for symbol index `si` — `Σ side * size`,
    /// a naive fold in submission order.
    ///
    /// This is what makes the account's pre-trade leverage cap see the orders it just sent. A
    /// local risk check must model the LOCAL view: at zero latency a market order sits in
    /// `pending` and the cap counts it, so once the gate holds that same order in flight instead,
    /// not counting it would grant every submit inside one entry-latency window the full leverage
    /// room — the cap silently unenforced in exactly the regime the model exists to make
    /// pessimistic. Delivery moves the order from here into `pending`, so it is counted by
    /// exactly one of the two at any instant, never both.
    pub(crate) fn in_flight_market_signed(&self, si: usize) -> f64 {
        let mut accumulator = 0.0;
        for f in &self.queue {
            if let LatencyModelInFlightAction::Push { si: s, order } = &f.action
                && *s == si
                && order.kind == vike_model::OrderKind::Market
            {
                accumulator += order.side as f64 * order.size;
            }
        }
        accumulator
    }

    /// The orders of any kind held in flight for symbol index `si`, a tagged quote included: what the
    /// pre-trade gate's margin and exposure folds count, because live counts an order from the moment
    /// it is sent. The order an amend judges is left out by `skip_queue_id`.
    pub(crate) fn in_flight_orders(
        &self,
        si: usize,
        skip_queue_id: Option<u64>,
    ) -> impl Iterator<Item = &vike_model::OrderResting> {
        self.queue.iter().filter_map(move |f| match &f.action {
            LatencyModelInFlightAction::Push { si: s, order }
                if *s == si && skip_queue_id != Some(order.qid) =>
            {
                Some(order)
            }
            _ => None,
        })
    }

    /// How many NEW-ORDER actions ([`LatencyModelInFlightAction::Push`]) are in flight for symbol index `si`
    /// — the read behind `OrderBrokerSimulator::in_flight_of`, so a strategy can write
    /// `if pending_of(s).is_empty() && in_flight_of(s) == 0 { submit(..) }` and not fire a fresh
    /// duplicate on every tick of the entry-latency window.
    pub(crate) fn in_flight_orders_for(&self, si: usize) -> usize {
        self.queue
            .iter()
            .filter(
                |f| matches!(&f.action, LatencyModelInFlightAction::Push { si: s, .. } if *s == si),
            )
            .count()
    }
}

#[cfg(test)]
mod tests;
