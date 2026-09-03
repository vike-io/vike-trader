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
//! - **response latency** — the exchange-side event at `exch_ts` → the strategy learns about it
//!   at `exch_ts + response()`. Fills are therefore delivered to `Strategy::on_fill` LATE.
//!
//! WHAT THE RESPONSE LEG COVERS, EXACTLY. Only the strategy's VIEW is delayed, never the
//! exchange's own state: `SimBroker::apply_fill` folds a fill into `sym[si].pos`, cash and the
//! equity curve at the instant it matches, and `BacktestResult` is computed from that truth.
//! What the response leg holds back is (a) the `Strategy::on_fill` callback and (b) the
//! strategy-visible SHADOW POSITION — a per-symbol signed size advanced ONLY by DELIVERED fills,
//! which `Broker::position` and `HftBroker::position` read while the gate is armed (see
//! `SimBroker::shadow_pos`). That shadow is what makes the leg mean something for a maker that
//! polls its inventory instead of accumulating it in `on_fill`: `vike-mm`'s `SpreadMaker` reads
//! `HftBroker::position` on every requote to drive its inventory skew and its Avellaneda–Stoikov
//! reservation price, and before the shadow existed it saw every fill with ZERO latency no matter
//! what `response_ns` said — the leg was inert for the crate's flagship maker.
//!
//! STILL BYPASSING THE MODEL (know these before trusting a number): `SimBroker::position_of`,
//! `pending_of`, `equity_now`/`Broker::equity`, `drawdown_now` and the target-percent verbs all
//! read EXCHANGE TRUTH synchronously and are deliberately NOT shadowed — they are the engine's own
//! accounting surface, and shadowing them would make `on_stop` disagree with `BacktestResult`. A
//! strategy that sizes off `equity()` or `position_of()` therefore still reacts to a fill with
//! zero response latency. Likewise an in-flight ORDER is invisible to `pending_of` (see
//! `SimBroker::in_flight_of` for the read that does see it).
//!
//! Both are signed nanoseconds. A NEGATIVE entry latency encodes a recorded **rejection**: the
//! request never reached the matching engine (see [`LatencyRow`]), so the engine drops the
//! action outright rather than delivering it.
//!
//! UNITS: the engine's own timestamps are epoch **milliseconds** (`Bar::ts`, `QuoteTick::ts`);
//! latencies here are **nanoseconds**. The wire-in keys its in-flight queue in nanoseconds
//! (`ts_ms * 1_000_000 + latency_ns`) so a sub-millisecond latency is not silently truncated —
//! it simply means the action lands on the next tick whose millisecond stamp has caught up.
//!
//! OPT-IN AND REPLAY-ONLY: activated by `EngineParams::latency_model = Some(kind)`. `None`
//! (the default) means no in-flight queue is ever built, no deferral happens and every existing
//! code path is byte-identical. When set, only `StrategyEngine::run_ticks` arms it; the bar
//! engine (`StrategyEngine::run`) and the vectorized kernel never consult it.

use std::sync::Arc;

/// The minimal order descriptor a latency model may condition on. Deliberately tiny (no engine
/// types) so a user impl stays pure and the seam does not leak `WorkingOrder` internals.
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
pub struct ConstantLatency {
    pub entry_ns: i64,
    pub response_ns: i64,
}

impl ConstantLatency {
    pub fn new(entry_ns: i64, response_ns: i64) -> Self {
        ConstantLatency { entry_ns, response_ns }
    }
}

impl LatencyModel for ConstantLatency {
    fn entry(&self, _ts: i64, _order: &LatencyOrder) -> i64 {
        self.entry_ns
    }

    fn response(&self, _ts: i64, _order: &LatencyOrder) -> i64 {
        self.response_ns
    }
}

/// One recorded round trip, all three stamps in epoch **nanoseconds**.
///
/// - `req_ts` — when the local process sent the request.
/// - `exch_ts` — when the matching engine acted on it. **`<= 0` marks a REJECTION**: the
///   request never reached the matching engine (a rate-limit knock-back, a rejected auth, a
///   dropped connection). Such a row's entry latency is reported as the NEGATIVE round trip
///   `-(resp_ts - req_ts)` — negative meaning "technically rejected", magnitude meaning "and
///   here is how long the strategy waited to be told".
/// - `resp_ts` — when the local process received the acknowledgement.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LatencyRow {
    pub req_ts: i64,
    pub exch_ts: i64,
    pub resp_ts: i64,
}

impl LatencyRow {
    pub fn new(req_ts: i64, exch_ts: i64, resp_ts: i64) -> Self {
        LatencyRow { req_ts, exch_ts, resp_ts }
    }

    /// Did this row record a request that never reached the matching engine?
    #[inline]
    pub fn is_rejection(&self) -> bool {
        self.exch_ts <= 0
    }

    /// This row's entry latency: `exch_ts - req_ts`, or the NEGATIVE round trip for a rejection.
    #[inline]
    pub fn entry_ns(&self) -> i64 {
        if self.is_rejection() {
            -(self.resp_ts - self.req_ts)
        } else {
            self.exch_ts - self.req_ts
        }
    }

    /// This row's response latency: `resp_ts - exch_ts`, or the whole round trip for a
    /// rejection (there is no exchange stamp to measure from).
    #[inline]
    pub fn response_ns(&self) -> i64 {
        if self.is_rejection() {
            self.resp_ts - self.req_ts
        } else {
            self.resp_ts - self.exch_ts
        }
    }
}

/// Latency interpolated from a RECORDED series of round trips — the calibrated model.
///
/// Lookup is a binary search for the bracketing pair, then a LINEAR interpolation of that pair's
/// latencies. The two legs interpolate along DIFFERENT abscissae, so the model keeps TWO indexes
/// — a binary search is only well-defined over a series sorted by the key it searches:
///
/// - the entry leg interpolates in `req_ts` (that is the stamp the caller has at submit time),
///   over every row, sorted by `req_ts` (the constructor sorts an arbitrarily ordered capture);
/// - the response leg interpolates in `exch_ts` (the stamp of the exchange-side event), over a
///   SECOND index sorted by `exch_ts` with REJECTION rows excluded. Both exclusions matter:
///   `exch_ts` is not monotone in `req_ts` for ordinary pipelined round trips (a request sent
///   first can be acked last), and a rejection has no `exch_ts` at all (`<= 0`), so leaving
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
pub struct IntpOrderLatency {
    /// every row, sorted by `req_ts` — the ENTRY leg's index
    rows: Arc<[LatencyRow]>,
    /// accepted rows only, sorted by `exch_ts` — the RESPONSE leg's index (see the type doc)
    by_exch: Arc<[LatencyRow]>,
}

impl IntpOrderLatency {
    /// Build from a recorded series; the rows are sorted by `req_ts` (stable) if needed, and the
    /// response-leg index is derived from them (accepted rows only, sorted by `exch_ts`).
    pub fn new(rows: impl Into<Vec<LatencyRow>>) -> Self {
        let mut rows: Vec<LatencyRow> = rows.into();
        if !rows.windows(2).all(|w| w[0].req_ts <= w[1].req_ts) {
            rows.sort_by_key(|r| r.req_ts);
        }
        let mut by_exch: Vec<LatencyRow> =
            rows.iter().copied().filter(|r| !r.is_rejection()).collect();
        // stable sort: equal `exch_ts` keeps `req_ts` order, so the model stays deterministic
        by_exch.sort_by_key(|r| r.exch_ts);
        IntpOrderLatency { rows: rows.into(), by_exch: by_exch.into() }
    }

    pub fn rows(&self) -> &[LatencyRow] {
        &self.rows
    }

    /// The response leg's index: accepted rows only, sorted by `exch_ts`.
    pub fn response_rows(&self) -> &[LatencyRow] {
        &self.by_exch
    }

    /// The shared bracket-and-blend over a series ALREADY SORTED BY `key`. `key` reads a row's
    /// interpolation stamp, `val` its latency. Both callers uphold that precondition by
    /// construction (see the type doc); a binary search over an unsorted key is meaningless.
    fn interp(
        rows: &[LatencyRow],
        ts_ns: i64,
        key: impl Fn(&LatencyRow) -> i64,
        val: impl Fn(&LatencyRow) -> i64,
    ) -> i64 {
        if rows.is_empty() {
            return 0;
        }
        // partition_point: index of the first row whose key is STRICTLY greater than ts_ns.
        let hi = rows.partition_point(|r| key(r) <= ts_ns);
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
            // which is the row whose request actually preceded `ts_ns`). Well-formed because the
            // series is sorted by `key`, so `lo_k <= ts_ns < hi_k` and both distances are >= 0.
            // Only the ENTRY leg can reach this — the response index excludes rejections.
            return if (ts_ns - lo_k) <= (hi_k - ts_ns) { lo_v } else { hi_v };
        }
        let span = hi_k - lo_k;
        if span <= 0 {
            // Defensive only: `partition_point` consumes an entire run of equal keys, so a sorted
            // series always brackets with `lo_k <= ts_ns < hi_k` and this is unreachable. Kept as
            // the divide-by-zero guard should a future caller ever break that precondition.
            return lo_v; // no gradient to interpolate along
        }
        // i128 so a wide ns span times a wide ns latency cannot overflow i64 mid-product.
        let num = (hi_v - lo_v) as i128 * (ts_ns - lo_k) as i128;
        lo_v + (num / span as i128) as i64
    }
}

/// Epoch **ms** (the engine's stamp) → epoch **ns** (the recorded series' stamp), saturating.
#[inline]
fn ms_to_ns(ts_ms: i64) -> i64 {
    ts_ms.saturating_mul(1_000_000)
}

impl LatencyModel for IntpOrderLatency {
    fn entry(&self, ts: i64, _order: &LatencyOrder) -> i64 {
        Self::interp(&self.rows, ms_to_ns(ts), |r| r.req_ts, |r| r.entry_ns())
    }

    fn response(&self, ts: i64, _order: &LatencyOrder) -> i64 {
        Self::interp(&self.by_exch, ms_to_ns(ts), |r| r.exch_ts, |r| r.response_ns())
    }
}

/// The `EngineParams` selector — which latency model gates the tick-replay order path.
/// `Clone` (not `Copy`) because the interpolated variant carries its recorded series; the
/// `Arc` keeps the clone cheap.
#[derive(Debug, Clone)]
pub enum LatencyModelKind {
    /// [`ConstantLatency`] — fixed nanoseconds on both legs.
    Constant { entry_ns: i64, response_ns: i64 },
    /// [`IntpOrderLatency`] over a recorded round-trip series.
    Intp(IntpOrderLatency),
}

/// Polymarket's **venue-enforced taker delay** on the crypto up/down markets, in milliseconds.
///
/// Verified 2026-07-23 against `docs.polymarket.com/concepts/order-lifecycle`. This is not a
/// network estimate and not a tunable: on exactly these markets the venue **holds the order for
/// 250 ms, runs validation AGAIN, and only then matches or books it**. While pending it cannot be
/// cancelled, the balance stays reserved, and duplicates are rejected. (It was a 500 ms speed bump
/// previously — hence a named, dated constant rather than a literal sprinkled through callers.)
///
/// Model it with [`LatencyModelKind::venue_hold_ms`] on the ENTRY leg plus
/// [`crate::engine::FillModelKind::L2Book`], and the venue's semantics fall out of machinery that
/// already exists — no bespoke code path:
///
/// * the order is decided by the strategy on the book at `T`;
/// * the in-flight queue delivers it to the matching logic at `T + 250 ms`;
/// * the L2 tier matches it against the book AS IT THEN STANDS;
/// * an order that no longer crosses does not fill and **rests** — which is precisely the venue's
///   own "matched or booked" second validation, and is a different outcome from a miss.
///
/// A deployment pays this ON TOP of its own network round trip, so a realistic entry leg is
/// `wire_ns + VENUE_HOLD_POLYMARKET_UPDOWN_MS * 1_000_000`.
///
/// ⚠ **The value is NOT declared here.** It is a VENUE fact, and the bridge crate that reads it off
/// the wire (`vike_polymarket::taker_hold`) cannot depend on this crate — so the literal lives once
/// in [`vike_model::venue_hold::POLYMARKET_ITODE_HOLD_MS`] and this is the `i64` alias this crate's
/// nanosecond arithmetic and doc links keep using. Change it there, not here.
pub const VENUE_HOLD_POLYMARKET_UPDOWN_MS: i64 = vike_model::POLYMARKET_ITODE_HOLD_MS as i64;

/// Polymarket's **venue-enforced delay on SPORTS GAME markets**, in milliseconds — a SECOND,
/// entirely INDEPENDENT mechanism from [`VENUE_HOLD_POLYMARKET_UPDOWN_MS`]. Do not conflate the
/// two: they are declared by different fields, on different endpoints, over disjoint sets of
/// markets, and they never co-occur on a market observed to date.
///
/// Measured live 2026-07-23 over every OPEN Polymarket sports market. The venue declares it as
/// `seconds_delay` on the CLOB `/markets/{condition_id}` payload (and as `secondsDelay` on Gamma):
/// **400 of 400 markets carrying a `game_start_time` report `seconds_delay: 3`**, while 191 of 192
/// futures/props markets report `0`. The discriminator is exact — a market with a game start is
/// delayed, one without is not. NBA accounts for 370 of the 400, plus EPL/NFL/CBB/IPL. Politics:
/// 0 of 30. Crypto up/down markets report `seconds_delay: 0` — their delay is the `itode` 250 ms
/// hold above, on an endpoint (`/clob-markets/{condition_id}`) that carries neither field the
/// other mechanism uses.
///
/// Three seconds is TWELVE TIMES the crypto hold and long enough that the book a strategy decided
/// on is routinely gone by delivery, so a sports taker backtest run without it is not merely
/// slightly optimistic — see the same modelling note on [`VENUE_HOLD_POLYMARKET_UPDOWN_MS`]:
/// [`LatencyModelKind::venue_hold_ms`] on the entry leg plus [`crate::engine::FillModelKind::L2Book`],
/// or — preferably, since the hold is a PER-MARKET venue property and a tape can carry both kinds —
/// the per-symbol hold table the gate builds from `EngineParams::properties`
/// (`vike_model::SymbolProperties::taker_hold_ms`).
///
/// ⚠ Same ownership rule as [`VENUE_HOLD_POLYMARKET_UPDOWN_MS`]: the literal lives once in
/// [`vike_model::venue_hold::POLYMARKET_SPORTS_GAME_HOLD_MS`] and this is the `i64` alias.
///
/// ⚠ And it is NOT universal — a later live sweep found the whole open esports book declaring
/// `secondsDelay: 1` (1000 ms). Model a delayed market from the PER-SYMBOL hold table
/// (`SymbolProperties::taker_hold_ms`, recorded off the wire), never from this constant.
pub const VENUE_HOLD_POLYMARKET_SPORTS_GAME_MS: i64 =
    vike_model::POLYMARKET_SPORTS_GAME_HOLD_MS as i64;

impl LatencyModelKind {
    /// Fixed latency on both legs.
    pub fn constant(entry_ns: i64, response_ns: i64) -> Self {
        LatencyModelKind::Constant { entry_ns, response_ns }
    }

    /// A VENUE-SIDE HOLD of `ms` milliseconds on the entry leg only — the order is deliberately
    /// delayed by the exchange before it is matched or booked (see
    /// [`VENUE_HOLD_POLYMARKET_UPDOWN_MS`]). The response leg is left at zero because a hold is
    /// not a round trip: it delays when the venue ACTS, not when the strategy hears about it.
    ///
    /// Compose with a wire round trip by adding the two entry legs — this constructor deliberately
    /// does not guess one.
    pub fn venue_hold_ms(ms: i64) -> Self {
        LatencyModelKind::Constant { entry_ns: ms.saturating_mul(1_000_000), response_ns: 0 }
    }

    /// Interpolated latency over a recorded series.
    pub fn intp(rows: impl Into<Vec<LatencyRow>>) -> Self {
        LatencyModelKind::Intp(IntpOrderLatency::new(rows))
    }

    /// Build the concrete model once (the gate holds it for the whole replay).
    pub fn build(&self) -> Box<dyn LatencyModel> {
        match self {
            LatencyModelKind::Constant { entry_ns, response_ns } => {
                Box::new(ConstantLatency::new(*entry_ns, *response_ns))
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
/// These are exactly the strategy-facing write verbs of `SimBroker`/`HftBroker`; capturing them
/// as data (instead of applying them) is what buys the delay without duplicating any fill logic
/// — the drain replays each variant through the SAME apply path the zero-latency engine uses.
pub(crate) enum InFlightAction {
    /// `SimBroker::push_pending` — a new untagged working order for symbol index `si`.
    Push { si: usize, order: vike_model::WorkingOrder },
    /// `SimBroker::cancel_all` — drop every untagged resting order for symbol index `si`.
    CancelAll { si: usize },
    /// `HftBroker::submit_limit_tagged` — insert/replace the tagged maker quote.
    SubmitTagged { tag: String, order: vike_model::WorkingOrder },
    /// `HftBroker::modify_tagged` — re-price / re-size a resting tagged quote in place.
    ModifyTagged { tag: String, new_qty: Option<f64>, new_price: Option<f64> },
    /// `HftBroker::cancel_tagged` — pull one resting tagged quote.
    CancelTagged { tag: String },
}

/// An in-flight action plus its delivery stamp and submission sequence.
pub(crate) struct InFlight {
    /// epoch NANOSECONDS at which the matching engine sees this action.
    pub delivery_ns: i64,
    /// Monotonic submission counter — the tiebreak that keeps a batch sharing ONE `delivery_ns`
    /// in the order the strategy issued it (a cancel then a re-submit must not swap).
    ///
    /// PRECONDITION, and it is the model's to uphold: the ordering guarantee holds only where the
    /// two actions get the same `delivery_ns`, i.e. for a model whose `entry` does not depend on
    /// `order`. Both shipped models qualify ([`ConstantLatency`] and [`IntpOrderLatency`] ignore
    /// the descriptor entirely). A USER model that returns a different entry latency for a bare
    /// cancel ([`LatencyOrder::NONE`], which is what `cancel_all`/`cancel_tagged` pass) than for
    /// the sized re-submit that follows it CAN legitimately reorder the pair — and that is the
    /// model speaking, not a bug: a slower cancel really does land after a faster new order live.
    /// Note the consequence for a maker, though: a requote whose cancel lands last leaves the tag
    /// pulled rather than re-priced, so condition `entry` on `order` only deliberately.
    pub seq: u64,
    pub action: InFlightAction,
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
    pub queue: Vec<InFlight>,
    next_seq: u64,
    /// Per-symbol VENUE HOLD in NANOSECONDS, indexed like `SimBroker::symbols` — the venue's own
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
    pub fn new(kind: &LatencyModelKind) -> Self {
        LatencyGate { model: kind.build(), queue: Vec::new(), next_seq: 0, holds_ns: Vec::new() }
    }

    /// Arm with a per-symbol venue-hold table (see [`Self::holds_ns`]). An ALL-ZERO or empty table
    /// is normalized to empty here, so "every venue that declares no hold" and "no properties
    /// source at all" are the same, provably-inert state rather than two.
    pub fn with_holds(kind: &LatencyModelKind, holds_ns: Vec<i64>) -> Self {
        let holds_ns = if holds_ns.iter().all(|&h| h == 0) { Vec::new() } else { holds_ns };
        LatencyGate { model: kind.build(), queue: Vec::new(), next_seq: 0, holds_ns }
    }

    /// The venue hold (ns) an action must serve ON TOP of the model's entry latency.
    ///
    /// **How the SYMBOL-LESS variants resolve — decided, not incidental.** `SubmitTagged`,
    /// `ModifyTagged` and `CancelTagged` carry a `tag`, not a symbol index, because the
    /// `HftBroker` maker surface is SINGLE-SYMBOL SCOPED BY CONTRACT: its verbs take no symbol and
    /// every tagged order belongs to `SimBroker::HFT_SI` (index 0), which is the mounted market. So
    /// they resolve index 0's hold — the same market they will actually rest on — NOT zero, and not
    /// a table-wide max. If the `HftBroker` surface ever becomes multi-symbol, the variants must
    /// carry an `si` and this arm must follow; a silent index-0 read would then be wrong.
    #[inline]
    fn hold_ns(&self, action: &InFlightAction) -> i64 {
        if self.holds_ns.is_empty() {
            return 0; // the frozen path: no table, no lookup, no change to `delivery_ns`
        }
        let si = match action {
            InFlightAction::Push { si, .. } | InFlightAction::CancelAll { si } => *si,
            // the single-symbol HFT surface — see the doc above
            InFlightAction::SubmitTagged { .. }
            | InFlightAction::ModifyTagged { .. }
            | InFlightAction::CancelTagged { .. } => 0,
        };
        self.holds_ns.get(si).copied().unwrap_or(0)
    }

    /// Hold `action` in flight. Returns `false` when the model reported a NEGATIVE entry
    /// latency — a recorded rejection: the request never reached the matching engine, so the
    /// action is discarded and never queued.
    ///
    /// A rejection is decided BEFORE the venue hold is added: the venue never saw the request, so
    /// it cannot have held it.
    pub fn submit(&mut self, ts_ms: i64, desc: &LatencyOrder, action: InFlightAction) -> bool {
        let lat = self.model.entry(ts_ms, desc);
        if lat < 0 {
            return false; // rejected before reaching the matching engine
        }
        // The venue's own hold rides ON TOP of the model's entry leg (wire) — they are separate
        // delays a live order pays in series, which is exactly what `VENUE_HOLD_POLYMARKET_*`'s
        // docs state.
        let lat = lat.saturating_add(self.hold_ns(&action));
        let seq = self.next_seq;
        self.next_seq += 1;
        self.queue.push(InFlight { delivery_ns: ms_to_ns(ts_ms).saturating_add(lat), seq, action });
        true
    }

    /// Response latency for an exchange-side event at `ts_ms`, clamped to `>= 0` (a result can
    /// never become visible before it happened).
    pub fn response_ns(&self, ts_ms: i64, desc: &LatencyOrder) -> i64 {
        self.model.response(ts_ms, desc).max(0)
    }

    /// Remove and return every action whose delivery stamp is at or before `ts_ms`, ordered by
    /// `(delivery_ns, seq)`. Survivors keep their submission order.
    pub fn drain_due(&mut self, ts_ms: i64) -> Vec<InFlight> {
        if self.queue.is_empty() {
            return Vec::new();
        }
        let now_ns = ms_to_ns(ts_ms);
        let mut due: Vec<InFlight> = Vec::new();
        let mut still: Vec<InFlight> = Vec::new();
        for f in self.queue.drain(..) {
            if f.delivery_ns <= now_ns {
                due.push(f);
            } else {
                still.push(f);
            }
        }
        self.queue = still;
        due.sort_by_key(|f| (f.delivery_ns, f.seq));
        due
    }

    /// How many actions are still in flight. Note there is deliberately NO end-of-run flush for
    /// the order queue: an action undelivered when the tape ends never reached the venue, so it
    /// must not retro-actively apply. (The RESPONSE leg is flushed — see
    /// `StrategyEngine::flush_deferred_fills`.)
    pub fn in_flight_len(&self) -> usize {
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
    pub fn in_flight_market_signed(&self, si: usize) -> f64 {
        let mut acc = 0.0;
        for f in &self.queue {
            if let InFlightAction::Push { si: s, order } = &f.action {
                if *s == si && order.kind == vike_model::OrderKind::Market {
                    acc += order.side as f64 * order.size;
                }
            }
        }
        acc
    }

    /// How many NEW-ORDER actions ([`InFlightAction::Push`]) are in flight for symbol index `si`
    /// — the read behind `SimBroker::in_flight_of`, so a strategy can write
    /// `if pending_of(s).is_empty() && in_flight_of(s) == 0 { submit(..) }` and not fire a fresh
    /// duplicate on every tick of the entry-latency window.
    pub fn in_flight_orders_for(&self, si: usize) -> usize {
        self.queue
            .iter()
            .filter(|f| matches!(&f.action, InFlightAction::Push { si: s, .. } if *s == si))
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_model::OrderKind;

    const D: LatencyOrder = LatencyOrder::NONE;

    fn ms(x: i64) -> i64 {
        x * 1_000_000
    }

    #[test]
    fn constant_latency_is_flat() {
        let m = ConstantLatency::new(1_500_000, 2_500_000);
        assert_eq!(m.entry(0, &D), 1_500_000);
        assert_eq!(m.entry(i64::MAX / 2, &D), 1_500_000);
        assert_eq!(m.response(42, &D), 2_500_000);
    }

    #[test]
    fn empty_series_is_inert() {
        let m = IntpOrderLatency::new(Vec::<LatencyRow>::new());
        assert_eq!(m.entry(10, &D), 0);
        assert_eq!(m.response(10, &D), 0);
    }

    #[test]
    fn intp_entry_interpolates_linearly() {
        // req at 1ms → entry 1_000_000ns; req at 3ms → entry 3_000_000ns.
        let rows = vec![
            LatencyRow::new(ms(1), ms(1) + 1_000_000, ms(1) + 2_000_000),
            LatencyRow::new(ms(3), ms(3) + 3_000_000, ms(3) + 4_000_000),
        ];
        let m = IntpOrderLatency::new(rows);
        assert_eq!(m.entry(1, &D), 1_000_000);
        assert_eq!(m.entry(3, &D), 3_000_000);
        // halfway between the two request stamps → halfway between the two latencies
        assert_eq!(m.entry(2, &D), 2_000_000);
    }

    #[test]
    fn intp_entry_clamps_at_both_edges() {
        let rows = vec![
            LatencyRow::new(ms(10), ms(10) + 500, ms(10) + 900),
            LatencyRow::new(ms(20), ms(20) + 1_500, ms(20) + 2_900),
        ];
        let m = IntpOrderLatency::new(rows);
        // before the first row — first row's latency, NOT an extrapolation
        assert_eq!(m.entry(0, &D), 500);
        assert_eq!(m.entry(9, &D), 500);
        // after the last row — last row's latency
        assert_eq!(m.entry(20, &D), 1_500);
        assert_eq!(m.entry(1_000, &D), 1_500);
    }

    #[test]
    fn intp_response_interpolates_on_exchange_stamp() {
        // exch at 1ms → response 400ns; exch at 5ms → response 2_400ns.
        let rows = vec![
            LatencyRow::new(ms(1) - 300, ms(1), ms(1) + 400),
            LatencyRow::new(ms(5) - 300, ms(5), ms(5) + 2_400),
        ];
        let m = IntpOrderLatency::new(rows);
        assert_eq!(m.response(1, &D), 400);
        assert_eq!(m.response(5, &D), 2_400);
        assert_eq!(m.response(3, &D), 1_400); // midpoint of the exch-stamp span
        assert_eq!(m.response(0, &D), 400); // clamped low
        assert_eq!(m.response(9, &D), 2_400); // clamped high
    }

    #[test]
    fn rejection_row_reports_negative_entry_latency() {
        // exch_ts <= 0 marks "never reached the matching engine".
        let r = LatencyRow::new(ms(4), 0, ms(4) + 7_000);
        assert!(r.is_rejection());
        assert_eq!(r.entry_ns(), -7_000);
        assert_eq!(r.response_ns(), 7_000); // whole round trip; no exchange stamp to subtract
        let m = IntpOrderLatency::new(vec![r]);
        assert_eq!(m.entry(4, &D), -7_000);
        assert_eq!(m.entry(0, &D), -7_000); // single row clamps both ways
        assert_eq!(m.entry(99, &D), -7_000);
    }

    #[test]
    fn rejection_is_never_blended_with_an_accept() {
        let ok = LatencyRow::new(ms(0), ms(0) + 1_000, ms(0) + 2_000);
        let rej = LatencyRow::new(ms(10), -1, ms(10) + 6_000);
        let m = IntpOrderLatency::new(vec![ok, rej]);
        // A naive blend at the midpoint would produce (1_000 + -6_000)/2 = -2_500 — a
        // *fabricated* rejection. Snap to the nearer row instead.
        assert_eq!(m.entry(0, &D), 1_000);
        assert_eq!(m.entry(4, &D), 1_000); // nearer the accept
        assert_eq!(m.entry(6, &D), -6_000); // nearer the rejection
        assert_eq!(m.entry(10, &D), -6_000);
    }

    #[test]
    fn response_index_excludes_a_mid_series_rejection() {
        // req-sorted rows whose MIDDLE row is a rejection (exch_ts <= 0). Searching the response
        // leg over the req-sorted series would run `partition_point` over the non-monotone key
        // sequence [1ms, -1, 3ms] and could bracket onto the rejection, handing back its whole
        // 8ms round trip as the response latency of an unrelated ACCEPTED event.
        let rows = vec![
            LatencyRow::new(ms(0), ms(1), ms(1) + 1_400),
            LatencyRow::new(ms(2), -1, ms(2) + 8_000), // rejection: no exchange stamp at all
            LatencyRow::new(ms(3), ms(3), ms(3) + 3_400),
        ];
        let m = IntpOrderLatency::new(rows);
        // the response index drops the rejection and is sorted by exch_ts
        assert_eq!(m.response_rows().len(), 2);
        assert_eq!(m.response_rows()[0].exch_ts, ms(1));
        assert_eq!(m.response_rows()[1].exch_ts, ms(3));
        // the rejection's 8_000 never appears: exch 1ms → 1_400, exch 3ms → 3_400, midpoint 2ms
        assert_eq!(m.response(1, &D), 1_400);
        assert_eq!(m.response(3, &D), 3_400);
        assert_eq!(m.response(2, &D), 2_400);
        // ...while the ENTRY leg still sees the rejection (and refuses to blend it)
        assert_eq!(m.entry(2, &D), -8_000);
    }

    #[test]
    fn response_index_is_sorted_by_exchange_stamp_not_request_stamp() {
        // A pipelined pair with NO rejection: the request sent FIRST is acked LAST, so `exch_ts`
        // is not monotone in `req_ts` and the req-sorted series is the wrong search index.
        let rows = vec![
            LatencyRow::new(ms(0), ms(100), ms(100) + 5_000), // sent first, acked last
            LatencyRow::new(ms(1), ms(2), ms(2) + 1_000),     // sent second, acked first
        ];
        let m = IntpOrderLatency::new(rows);
        assert_eq!(m.rows()[0].req_ts, ms(0)); // entry index keeps req order
        assert_eq!(m.response_rows()[0].exch_ts, ms(2)); // response index is exch-sorted
        assert_eq!(m.response(2, &D), 1_000);
        assert_eq!(m.response(100, &D), 5_000);
        // exch 51ms is the midpoint of the [2ms, 100ms] span → midpoint of [1_000, 5_000]
        assert_eq!(m.response(51, &D), 3_000);
        // before the series → the exch-FIRST row (1_000), not the req-first row's 5_000
        assert_eq!(m.response(0, &D), 1_000);
    }

    #[test]
    fn an_all_rejection_series_has_an_inert_response_leg() {
        let m = IntpOrderLatency::new(vec![
            LatencyRow::new(ms(1), 0, ms(1) + 9_000),
            LatencyRow::new(ms(2), -1, ms(2) + 9_000),
        ]);
        assert!(m.response_rows().is_empty());
        assert_eq!(m.response(1, &D), 0); // empty index is inert
        assert_eq!(m.entry(1, &D), -9_000); // the entry leg still reports the rejections
    }

    #[test]
    fn unsorted_rows_are_sorted_on_construction() {
        let m = IntpOrderLatency::new(vec![
            LatencyRow::new(ms(3), ms(3) + 3_000, ms(3) + 4_000),
            LatencyRow::new(ms(1), ms(1) + 1_000, ms(1) + 2_000),
        ]);
        assert_eq!(m.rows()[0].req_ts, ms(1));
        assert_eq!(m.entry(2, &D), 2_000);
    }

    #[test]
    fn duplicate_request_stamps_have_no_gradient() {
        let m = IntpOrderLatency::new(vec![
            LatencyRow::new(ms(2), ms(2) + 100, ms(2) + 200),
            LatencyRow::new(ms(2), ms(2) + 900, ms(2) + 999),
        ]);
        // Both stamps equal: `partition_point` puts ts=2ms past both → high clamp.
        assert_eq!(m.entry(2, &D), 900);
        assert_eq!(m.entry(1, &D), 100); // before the series → first row
    }

    #[test]
    fn gate_defers_then_delivers_in_stamp_then_seq_order() {
        let mut g = LatencyGate::new(&LatencyModelKind::constant(2_000_000, 0)); // 2ms entry
        assert!(g.submit(10, &D, InFlightAction::CancelAll { si: 0 }));
        assert!(g.submit(10, &D, InFlightAction::CancelTagged { tag: "a".into() }));
        assert_eq!(g.in_flight_len(), 2);
        assert!(g.drain_due(11).is_empty()); // 10ms + 2ms = 12ms, not yet
        let due = g.drain_due(12);
        assert_eq!(due.len(), 2);
        assert_eq!(due[0].seq, 0); // submission order preserved within one delivery stamp
        assert_eq!(due[1].seq, 1);
        assert_eq!(g.in_flight_len(), 0);
    }

    #[test]
    fn gate_drops_a_rejected_action() {
        let mut g = LatencyGate::new(&LatencyModelKind::constant(-1, 0));
        assert!(!g.submit(10, &D, InFlightAction::CancelAll { si: 0 }));
        assert_eq!(g.in_flight_len(), 0);
        assert!(g.drain_due(i64::MAX / 2).is_empty());
    }

    #[test]
    fn gate_clamps_negative_response_to_zero() {
        let g = LatencyGate::new(&LatencyModelKind::constant(0, -5));
        assert_eq!(g.response_ns(1, &D), 0);
    }

    // ---- per-symbol venue hold ----------------------------------------------------------------

    fn push(si: usize) -> InFlightAction {
        InFlightAction::Push { si, order: vike_model::WorkingOrder::new(OrderKind::Market, 1, 1.0) }
    }

    /// THE acceptance bar: an empty hold table (no properties source, or no venue declaring a
    /// hold) must produce the EXACT delivery stamps the pre-hold gate produced. Proven by running
    /// both constructors over the same submissions and comparing stamps, not by assertion.
    #[test]
    fn an_empty_hold_table_is_byte_identical_to_no_table() {
        for holds in [Vec::new(), vec![0i64; 3]] {
            let kind = LatencyModelKind::constant(2_000_000, 0);
            let (mut base, mut held) =
                (LatencyGate::new(&kind), LatencyGate::with_holds(&kind, holds.clone()));
            for g in [&mut base, &mut held] {
                assert!(g.submit(10, &D, push(0)));
                assert!(g.submit(11, &D, push(2)));
                assert!(g.submit(12, &D, InFlightAction::CancelTagged { tag: "t".into() }));
            }
            let stamps = |g: &LatencyGate| -> Vec<(i64, u64)> {
                g.queue.iter().map(|f| (f.delivery_ns, f.seq)).collect()
            };
            assert_eq!(stamps(&base), stamps(&held), "holds={holds:?} must not move a stamp");
        }
    }

    /// The hold is PER SYMBOL and rides ON TOP of the model's entry leg — a two-market tape
    /// (crypto up/down at 250 ms, a sports game at 3 s) delays each order by its own market's rule.
    #[test]
    fn each_symbol_serves_its_own_venue_hold() {
        let holds = vec![
            VENUE_HOLD_POLYMARKET_UPDOWN_MS * 1_000_000,
            0,
            VENUE_HOLD_POLYMARKET_SPORTS_GAME_MS * 1_000_000,
        ];
        let mut g = LatencyGate::with_holds(&LatencyModelKind::constant(1_000_000, 0), holds);
        assert!(g.submit(1_000, &D, push(0))); // 1ms wire + 250ms hold
        assert!(g.submit(1_000, &D, push(1))); // 1ms wire, no hold
        assert!(g.submit(1_000, &D, push(2))); // 1ms wire + 3000ms hold
        let at = |i: usize| g.queue[i].delivery_ns;
        assert_eq!(at(0), ms(1_000) + 1_000_000 + 250_000_000);
        assert_eq!(at(1), ms(1_000) + 1_000_000);
        assert_eq!(at(2), ms(1_000) + 1_000_000 + 3_000_000_000);
    }

    /// The symbol-LESS (tagged) variants resolve the single-symbol `HftBroker` market, index 0 —
    /// the market a tagged quote actually rests on. See `LatencyGate::hold_ns`'s doc.
    #[test]
    fn tagged_actions_take_the_hft_symbols_hold() {
        let mut g =
            LatencyGate::with_holds(&LatencyModelKind::constant(0, 0), vec![250_000_000, 0]);
        let o = vike_model::WorkingOrder::new(OrderKind::Limit, 1, 1.0);
        assert!(g.submit(0, &D, InFlightAction::SubmitTagged { tag: "a".into(), order: o }));
        assert!(g.submit(
            0,
            &D,
            InFlightAction::ModifyTagged { tag: "a".into(), new_qty: None, new_price: None }
        ));
        assert!(g.submit(0, &D, InFlightAction::CancelTagged { tag: "a".into() }));
        for f in &g.queue {
            assert_eq!(f.delivery_ns, 250_000_000, "index 0's hold, not zero");
        }
    }

    /// A recorded REJECTION is decided before the hold: the venue never saw the request, so it
    /// cannot have held it — the action is dropped, not queued 250 ms later.
    #[test]
    fn a_rejection_is_not_held() {
        let mut g =
            LatencyGate::with_holds(&LatencyModelKind::constant(-1, 0), vec![3_000_000_000]);
        assert!(!g.submit(10, &D, push(0)));
        assert_eq!(g.in_flight_len(), 0);
    }

    /// An out-of-range symbol index (defensive — the table is built over the same `symbols` the
    /// engine indexes) resolves to no hold rather than panicking on the order path.
    #[test]
    fn an_unknown_symbol_index_serves_no_hold() {
        let mut g = LatencyGate::with_holds(&LatencyModelKind::constant(0, 0), vec![250_000_000]);
        assert!(g.submit(7, &D, push(9)));
        assert_eq!(g.queue[0].delivery_ns, ms(7));
    }
}
