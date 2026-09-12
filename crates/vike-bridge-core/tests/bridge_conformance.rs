//! Cross-bridge conformance suite — ONE scenario table run against every covered venue's REAL
//! `event_mapper` + the shared exec plumbing, machine-checking the CLAUDE.md **venue-adapter
//! contract** that was previously only verified ad hoc, per venue (audit br5). The crypto venues
//! decode private-WS frames; the FX/CFD/equity venues (oanda/ig/alpaca, plan §3.3 PR 3c) decode
//! their REST-reply / transactions-stream / SSE frames — see [`FillShape`].
//!
//! ## The contract under test (CLAUDE.md "venue-adapter contract")
//! * **Emitter split** — Rust emits `OrderSubmitted` synchronously at submit; the venue emits
//!   `OrderAccepted / Rejected / Canceled / (Partially)Filled`.
//! * **No silent vanish** — a dead venue path MUST synthesize a terminal `OrderRejected`.
//! * **Exactly one terminal** — the `ManagedOrder` FSM must reach exactly one terminal state
//!   (`Filled | Canceled | Rejected | Denied | Expired`); illegal/out-of-order events are dropped
//!   idempotently, never corrupting the single terminal.
//!
//! The invariant oracle is the REAL FSM (`vike_exec::ManagedOrder::apply`) — the same code the live
//! core folds through — wrapped by [`ContractOrder`], which mirrors `ExecutionEngine::on_event`'s
//! thin per-order policy (bare-fill vs wrap-fill split, trade-id dedup on reconnect replays, and the
//! audit-C1 "terminal dropped on a live order" loss counter). Each venue plugs in ONLY its
//! venue-native JSON frames + its fixture-tested `mapper` module; the scenarios, folds, and
//! assertions are shared.
//!
//! ## Scenarios (the rows of the table)
//! 1. **Lifecycle** — submit → accept → partial fill → full fill reaches `Filled`, one terminal.
//! 2. **TransportDeath** — a dead venue path still synthesizes a terminal (no vanish), via the exact
//!    shared seam each venue uses (`ExecActor` for command venues; `resolve_ambiguous_submit` for
//!    REST-poll venues).
//! 3. **CancelAfterClose** — a late venue `OrderCanceled` replay AND a late `OrderCancelRejected`
//!    advisory arriving after the order already closed are both dropped without corrupting the
//!    single terminal.
//! 4. **ReconnectMidOrder** — driven through the REAL shared pump (`run_user_data_forever`) with a
//!    scripted drop+reconnect that REPLAYS the mid-order partial: no duplicate terminal, no
//!    double-counted fill, no lost terminal.
//!
//! ## Coverage (honest matrix — see the `coverage_matrix` test's printout)
//! * COVERED (crypto, [`FillShape::Cumulative`]): binance, bybit, okx, deribit, aster, hyperliquid —
//!   every crypto venue with a fixture-tested private-WS `event_mapper`. binance/bybit/okx/aster
//!   carry the whole lifecycle on one WS mapper; deribit dispatches its fills-only
//!   `map_deribit_private` + the A3 order-history cancel mapper + the JSON-RPC accept; hyperliquid
//!   dispatches `orderUpdates` + the `/exchange` partial-fill response mapper.
//! * COVERED (FX/CFD/equity, [`FillShape::Whole`], plan §3.3 PR 3c): oanda maps the order-POST reply
//!   (`map_order_response`) + the transactions-stream fill/cancel (`decode_transaction_events`); ig
//!   the sync `/confirms` accept (`map_confirm`) + the Lightstreamer trade-update
//!   (`decode_trade_confirm`); alpaca the `/v2/events/trades` SSE object (`decode_trade_event`). See
//!   [`FillShape`] for why these run a whole-fill Lifecycle/Reconnect variant.
//! * COVERED (native-SDK FX, [`FillShape::Whole`]): **fxcm** — the venue with no wire. Its fill and
//!   cancel envelopes are JSON the C++ shim builds and the exec thread polls (`map_fxcm_event`,
//!   sourced from that shim's committed grammar fixture), and its accept is the synchronous return
//!   of a placement FFI call (`map_placement`, lifted out of the exec loop for this). See [`Fxcm`]
//!   — including what a green row here does NOT say about the SDK behind it, which matters more on
//!   this venue than on any other because no CI machine can compile that half at all.
//! * DEFERRED (recorded, not faked): polymarket, dukascopy, ibkr, ctrader — each with a
//!   one-line reason (empty-by-default crate / Java-sidecar / stateful-mapper / protobuf-framed
//!   shapes that don't fit the stateless `decode(&Value)` seam). Adding one is a drop-in
//!   [`ConformanceBridge`] impl — the table + assertions do not change.
//!
//! ## The roster gate (`conformance_roster_is_exhaustive`)
//! The COVERED and DEFERRED lists are checked EXHAUSTIVE against `vike_model::VENUES` (the canonical
//! bridge-crate roster): every roster venue must be classified exactly once — either a
//! `covered_bridges()` [`ConformanceBridge`] impl OR a `DEFERRED` row with a non-empty reason. So a
//! NEW bridge crate can no longer be added to the roster and silently escape this harness — it fails
//! the gate until it is either wired in or deliberately deferred-with-reason. Same completeness shape
//! as vike-model's `fees::every_roster_venue_has_a_fee_schedule` / `tif`'s roster test.
//!
//! ## Captured-template sourcing (testing-arch plan §5, PR 5c)
//! A venue's `frame_*` methods may OPTIONALLY source their frame from that venue's COMMITTED
//! sanitized real-capture fixture (`crates/bridges/<venue>/tests/fixtures/captured/<kind>.json`,
//! written by the venue's `*_capture_smoke` arm — plan 5a/5b) instead of hand-authoring `json!`:
//! [`captured_template`] loads the first captured frame as a STRUCTURE template — the venue's real
//! envelope, field set, and value TYPES (string-typed qtys etc.) — and the venue impl patches ONLY
//! the scenario-driven leaves (coid, trade id, qty, px, terminal marker) via the type-preserving
//! [`patch_f64`]/[`patch_str`]. The scenarios then exercise each mapper against the venue's REAL
//! wire shape, extra fields and all. Fixture absent → the hand-authored `json!` fallback,
//! byte-identical to before (default unchanged — binance/bybit are the wired pilots; the other
//! venues fall back until their captures land). The `coverage_matrix` printout names which venues
//! sourced captured templates, so what ran against real wire is explicit.

// The `check!` assertion macro below funnels every failure through `format!`; a handful of checks
// carry a bare literal message (no interpolation), which `clippy::useless_format` would flag under
// the `-D warnings` gate. Allowing it file-wide keeps the macro one-armed and the call sites uniform.
#![allow(clippy::useless_format)]

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde_json::{Value, json};

use vike_bridge_core::exec_actor::{ExecActor, ExecCommand};
use vike_bridge_core::rest::resolve_ambiguous_submit;
use vike_bridge_core::transport::VenueApiError;
// The shared scripted user-data stream double (testing-arch Phase 4c, `test-support` feature) —
// replaces the per-venue `r6_*_userdata`-pattern copy that used to live inline below. Spelled
// `as Scripted`, the same shorthand the three sibling user-data suites use: `scripted` ALSO defines
// a distinct `ScriptedStream` (the market/depth double), so aliasing to that name here would give
// one type the other's spelling.
use vike_bridge_core::scripted::ScriptedUserStream as Scripted;
use vike_bridge_core::user_data::{OpenOutcome, StreamError, StreamMsg, run_user_data_forever};
use vike_exec::{EventSender, ExecutionClient, Ingest, ManagedOrder, OrderStatus, event_channel};
use vike_model::OrderRequest;
use vike_model::events::{
    Event, OrderAccepted, OrderCancelRejected, OrderRejected, OrderSubmitted,
};

// ===================================================================================================
// The invariant oracle: the REAL FSM plus ExecutionEngine's thin per-order fold policy.
// ===================================================================================================

/// A terminal lifecycle event — one that (in a legal transition) closes the order. Mirrors the
/// private `is_terminal_event` in `vike_exec::execution_engine` (kept in lockstep with
/// `OrderStatus::is_terminal`), used to tell a genuinely-lost terminal from a benign replay.
fn is_terminal_event(ev: &Event) -> bool {
    matches!(
        ev,
        Event::OrderFilled(_)
            | Event::OrderCanceled(_)
            | Event::OrderRejected(_)
            | Event::OrderExpired(_)
            | Event::OrderDenied(_)
    )
}

/// Folds a venue event stream through the REAL `ManagedOrder` FSM exactly as `ExecutionEngine::
/// on_event` does for a single order: the bare `FillEvent` goes to the Account side (dedup only,
/// never the FSM); the wrapping `OrderPartiallyFilled`/`OrderFilled` advance the FSM, deduped by
/// `trade_id` so a reconnect replay never double-counts; an `apply` error is a benign idempotent
/// drop UNLESS a terminal event fails on a still-live order (audit C1 — a real, counted loss).
struct ContractOrder {
    mo: ManagedOrder,
    /// Account-side (bare-fill) dedup keys — mirrors `seen_trade_ids`.
    seen_bare: HashSet<String>,
    /// FSM-side (wrap-fill) dedup keys — mirrors `seen_fsm_trade_ids`.
    seen_wrap: HashSet<String>,
    /// Count of successful transitions INTO a terminal state — MUST end at exactly 1.
    terminals_applied: usize,
    /// A terminal event that failed to apply while the order was still live (audit C1) — a
    /// genuinely-lost terminal. MUST stay 0 in every healthy scenario.
    dropped_terminal_on_live: usize,
}

impl ContractOrder {
    fn new(req: OrderRequest) -> Self {
        Self {
            mo: ManagedOrder::new(req),
            seen_bare: HashSet::new(),
            seen_wrap: HashSet::new(),
            terminals_applied: 0,
            dropped_terminal_on_live: 0,
        }
    }

    fn fold(&mut self, ev: &Event) {
        // Bare fill → Account side: dedup only, NEVER applied to the FSM (the FSM has no FillEvent
        // transition — only the wraps advance it). Exactly the ExecutionEngine split.
        if let Event::Fill(f) = ev {
            if !f.trade_id.as_str().is_empty() {
                self.seen_bare.insert(f.trade_id.as_str().to_string());
            }
            return;
        }
        // Wrap-fill FSM dedup by trade_id (a reconnect resync re-emits the wrap).
        let tid = match ev {
            Event::OrderPartiallyFilled(w) => w.fill.trade_id.as_str().to_string(),
            Event::OrderFilled(w) => w.fill.trade_id.as_str().to_string(),
            _ => String::new(),
        };
        if !tid.is_empty() && self.seen_wrap.contains(&tid) {
            return; // reconnect replay — the FSM already advanced for this fill
        }
        let was_terminal = self.mo.status.is_terminal();
        match self.mo.apply(ev) {
            Ok(()) => {
                if !tid.is_empty() {
                    self.seen_wrap.insert(tid);
                }
                if !was_terminal && self.mo.status.is_terminal() {
                    self.terminals_applied += 1;
                }
            }
            Err(_) => {
                // Audit C1: a terminal event rejected while the order is still LIVE is a real loss;
                // otherwise it is a benign idempotent/out-of-order WS replay (e.g. a late cancel on
                // an already-terminal order) and is silently dropped.
                if is_terminal_event(ev) && !self.mo.status.is_terminal() {
                    self.dropped_terminal_on_live += 1;
                }
            }
        }
    }

    fn status(&self) -> OrderStatus {
        self.mo.status
    }
    fn filled_qty(&self) -> f64 {
        self.mo.filled_qty
    }
}

/// The Rust-side half of the emitter split: a venue-agnostic `OrderSubmitted` (Initialized →
/// Submitted). Real adapters emit this synchronously at submit; the venue frames drive the rest.
fn submitted(coid: &str) -> Event {
    Event::OrderSubmitted(OrderSubmitted { client_order_id: coid.into(), ts: 1 })
}

// ===================================================================================================
// The venue extension point — implement this to add a venue to the table.
// ===================================================================================================

/// How a venue's `ExecutionClient` reaches the venue at submit — selects the shared no-vanish seam
/// exercised by the TransportDeath scenario.
///
/// ⚠ WHICH venue is which is each [`ConformanceBridge::exec_kind`] impl below, not a list in this
/// comment. The list that used to live here named four of the seven `CommandActor` venues: it was
/// written when the table held six crypto venues and never grew when oanda/ig/alpaca joined, and
/// nothing could notice — a doc comment is not a declaration anything is checked against.
/// [`FillShape`] is the counter-example worth copying: its variants name venues too, but each name
/// carries a REASON (that venue's real mapper has no `OrderPartiallyFilled` path at all), so it
/// reads as evidence rather than as a roster to keep in sync.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ExecKind {
    /// Command-thread venue (`ExecActor`). A dead thread synthesizes `OrderRejected`.
    CommandActor,
    /// REST-poll venue (`LiveRestClient`). An ambiguous/timed-out submit is resolved by the shared
    /// `resolve_ambiguous_submit`.
    RestPoll,
}

/// A venue's fill granularity — the SECOND harness axis (plan §3.3, PR 3c), orthogonal to
/// [`ExecKind`]. Crypto venues stream CUMULATIVE partials: a resting order fills in pieces, each a
/// distinct `OrderPartiallyFilled` before the terminal `OrderFilled`. The FX/CFD (oanda/ig) and
/// US-equity (alpaca) venues fill WHOLE — ONE execution reports the full qty and the venue's REAL
/// mapper has NO `OrderPartiallyFilled` path at all (oanda's `map_order_response`/
/// `decode_transaction_events`, ig's `decode_trade_confirm`, and alpaca's `decode_trade_event` each
/// only ever emit `OrderFilled` on an execution — alpaca even folds a `"partial_fill"` SSE event
/// into `OrderFilled`). The Lifecycle/Reconnect scenarios branch on this so a whole-fill venue is
/// exercised through its real mapper faithfully instead of asserting a partial state it can never
/// emit; TransportDeath and CancelAfterClose are already shape-agnostic (they never drive a partial).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum FillShape {
    /// Cumulative partials then a terminal fill (binance/bybit/okx/deribit/aster/hyperliquid).
    Cumulative,
    /// One execution = the whole order; no partial-fill state (oanda/ig/alpaca).
    Whole,
}

/// A venue's plug-in for the shared table: its order shape, its venue-native user-data JSON frames,
/// and its REAL fixture-tested private-WS mapper. NOTHING else is venue-specific.
trait ConformanceBridge {
    fn venue(&self) -> &'static str;
    fn exec_kind(&self) -> ExecKind;

    /// This venue's fill granularity (see [`FillShape`]). Defaults to `Cumulative` (the crypto
    /// shape the six original venues share); the whole-fill FX/CFD/equity venues override it.
    fn fill_shape(&self) -> FillShape {
        FillShape::Cumulative
    }

    /// A well-formed limit order for this venue.
    fn order(&self, coid: &str) -> OrderRequest;

    /// Venue-native user-data JSON that ACCEPTS a resting order → `[OrderAccepted]`.
    fn frame_accepted(&self, coid: &str, venue_order_id: &str) -> Value;

    /// Venue-native user-data JSON for ONE fill of `this_qty` (cumulative `cum_qty` of `total_qty`
    /// base at `px`), keyed by `trade_id` (the reconnect-dedup key). `terminal` selects the FULL
    /// fill (→ `[Fill, OrderFilled]`) vs a PARTIAL (→ `[Fill, OrderPartiallyFilled]`).
    #[allow(clippy::too_many_arguments)]
    fn frame_fill(
        &self,
        coid: &str,
        trade_id: &'static str,
        this_qty: f64,
        cum_qty: f64,
        total_qty: f64,
        px: f64,
        terminal: bool,
    ) -> Value;

    /// Venue-native user-data JSON for a venue-side cancel confirmation → `[OrderCanceled]`.
    fn frame_canceled(&self, coid: &str) -> Value;

    /// The venue's REAL private-WS mapper (the fixture-tested `mapper` module).
    fn decode(&self, frame: &Value) -> Vec<Event>;
}

// ===================================================================================================
// Captured-template sourcing (plan 5c) — real sanitized wire frames as scenario templates.
// ===================================================================================================

/// Load the FIRST committed captured frame for `(venue, kind)` — `None` when the venue has no
/// committed capture (the hand-authored fallback then applies). Reads the plan-5a fixture format
/// (`{_provenance, frames:[..]}`) with plain fs+serde_json so the default (feature-less) test
/// build sources real frames too; a malformed committed file panics rather than silently falling
/// back. The kind vocabulary is the capture smokes' (`ws_accepted`/`ws_fill`/`ws_canceled`).
fn captured_template(venue: &str, kind: &str) -> Option<Value> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../bridges")
        .join(venue)
        .join("tests/fixtures/captured")
        .join(format!("{kind}.json"));
    let body = std::fs::read_to_string(&path).ok()?;
    let root: Value = serde_json::from_str(&body)
        .unwrap_or_else(|e| panic!("malformed captured fixture {}: {e}", path.display()));
    root.get("frames").and_then(|f| f.as_array()).and_then(|a| a.first()).cloned()
}

/// Walk `ptr` (JSON-pointer style, object keys + array indices) to the leaf's PARENT object,
/// returning it with the leaf key — the insert-capable twin of `Value::pointer_mut` (which cannot
/// create an absent leaf).
fn leaf_parent<'a>(
    frame: &'a mut Value,
    ptr: &str,
) -> Option<(&'a mut serde_json::Map<String, Value>, String)> {
    let mut parts: Vec<&str> = ptr.trim_start_matches('/').split('/').collect();
    let leaf = parts.pop()?.to_string();
    let mut cur = frame;
    for p in parts {
        cur = match p.parse::<usize>() {
            Ok(i) => cur.get_mut(i)?,
            Err(_) => cur.get_mut(p)?,
        };
    }
    cur.as_object_mut().map(|o| (o, leaf))
}

/// Patch a numeric scenario value into a captured template, PRESERVING the captured leaf's wire
/// TYPE: a string-typed leaf (the common venue encoding for qty/px) receives the formatted string,
/// a number-typed (or absent) leaf the JSON number — so the template keeps proving the mapper's
/// string-coercion against the real wire shape.
fn patch_f64(frame: &mut Value, ptr: &str, val: f64) {
    let Some((parent, leaf)) = leaf_parent(frame, ptr) else {
        panic!("captured template missing patch path {ptr}")
    };
    let patched = match parent.get(&leaf) {
        Some(Value::String(_)) => Value::String(format!("{val}")),
        _ => json!(val),
    };
    parent.insert(leaf, patched);
}

/// Patch a string scenario value (coid / trade id / status marker) into a captured template.
/// Always writes a string: the scenario's ids are non-numeric, and every mapper reads ids through
/// the shared string-coercing accessors, so a numeric-on-the-wire id leaf (binance's `t`) taking a
/// string here is the one deliberate type departure (documented, assertion-neutral).
fn patch_str(frame: &mut Value, ptr: &str, val: &str) {
    let Some((parent, leaf)) = leaf_parent(frame, ptr) else {
        panic!("captured template missing patch path {ptr}")
    };
    parent.insert(leaf, Value::String(val.to_string()));
}

// --- Binance (spot executionReport; LiveRestClient) -------------------------------------------------

struct Binance;

impl Binance {
    /// Captured WS-API user-data frames arrive ENVELOPED (`{"subscriptionId":..,"event":{..}}`)
    /// while the hand-authored fallbacks are bare executionReports — this is the pointer PREFIX to
    /// wherever the report actually lives in this frame (computed before any `&mut` patch borrow).
    fn prefix(frame: &Value) -> &'static str {
        if frame.get("event").is_some() { "/event" } else { "" }
    }
}

impl ConformanceBridge for Binance {
    fn venue(&self) -> &'static str {
        "binance"
    }
    fn exec_kind(&self) -> ExecKind {
        ExecKind::RestPoll
    }
    fn order(&self, coid: &str) -> OrderRequest {
        limit_order(coid, "binance", "BTCUSDT")
    }
    fn frame_accepted(&self, coid: &str, venue_order_id: &str) -> Value {
        // Plan 5c: the venue's REAL captured x=NEW frame as the structure template, scenario
        // coid/venue-id patched in; hand-authored fallback when no capture is committed.
        if let Some(mut f) = captured_template("binance", "ws_accepted") {
            let p = Self::prefix(&f);
            patch_str(&mut f, &format!("{p}/c"), coid);
            patch_str(&mut f, &format!("{p}/i"), venue_order_id);
            return f;
        }
        json!({"e": "executionReport", "s": "BTCUSDT", "c": coid, "T": 1,
               "i": venue_order_id, "x": "NEW"})
    }
    fn frame_fill(
        &self,
        coid: &str,
        trade_id: &'static str,
        this_qty: f64,
        _cum_qty: f64,
        _total_qty: f64,
        px: f64,
        terminal: bool,
    ) -> Value {
        // Binance drives terminality off `X` (the order status AFTER this fill), not a cumulative.
        // Captured template: a REAL x=TRADE/X=FILLED frame; the scenario patches the routed coid,
        // dedup trade id, incremental qty/px (type-preserving — the real wire's STRING qtys stay
        // strings), and the X terminal marker (a PARTIAL is the captured full-fill template with
        // X flipped — market demo orders fill whole, so no real partial frame exists to capture).
        if let Some(mut f) = captured_template("binance", "ws_fill") {
            let p = Self::prefix(&f);
            patch_str(&mut f, &format!("{p}/c"), coid);
            patch_str(&mut f, &format!("{p}/t"), trade_id);
            patch_f64(&mut f, &format!("{p}/l"), this_qty);
            patch_f64(&mut f, &format!("{p}/L"), px);
            patch_str(
                &mut f,
                &format!("{p}/X"),
                if terminal { "FILLED" } else { "PARTIALLY_FILLED" },
            );
            return f;
        }
        json!({"e": "executionReport", "s": "BTCUSDT", "c": coid, "T": 1, "x": "TRADE",
               "X": if terminal { "FILLED" } else { "PARTIALLY_FILLED" },
               "t": trade_id, "S": "BUY", "l": this_qty, "L": px, "n": 0.0, "N": "USDT", "m": false})
    }
    fn frame_canceled(&self, coid: &str) -> Value {
        if let Some(mut f) = captured_template("binance", "ws_canceled") {
            let p = Self::prefix(&f);
            patch_str(&mut f, &format!("{p}/c"), coid);
            return f;
        }
        json!({"e": "executionReport", "s": "BTCUSDT", "c": coid, "T": 1, "x": "CANCELED",
               "r": "NONE"})
    }
    fn decode(&self, frame: &Value) -> Vec<Event> {
        vike_binance::event_mapper::map_binance_private(frame, "binance", "BTCUSDT")
    }
}

// --- Bybit (V5 linear-perp execution/order topics; ExecActor) --------------------------------------

struct Bybit;

impl Bybit {
    /// A captured V5 private frame batches rows in `data[]`; the scenarios drive ONE logical
    /// event per frame, so a multi-row capture keeps row 0 only (row structure stays verbatim —
    /// stray sibling rows would otherwise fold foreign-coid events into the ContractOrder).
    fn one_row(frame: &mut Value) {
        if let Some(rows) = frame.get_mut("data").and_then(|d| d.as_array_mut()) {
            rows.truncate(1);
        }
    }
}

impl ConformanceBridge for Bybit {
    fn venue(&self) -> &'static str {
        "bybit"
    }
    fn exec_kind(&self) -> ExecKind {
        ExecKind::CommandActor
    }
    fn order(&self, coid: &str) -> OrderRequest {
        limit_order(coid, "bybit", "BTCUSDT")
    }
    fn frame_accepted(&self, coid: &str, venue_order_id: &str) -> Value {
        // Plan 5c: the venue's REAL captured order-topic New frame as the structure template.
        if let Some(mut f) = captured_template("bybit", "ws_accepted") {
            Self::one_row(&mut f);
            patch_str(&mut f, "/data/0/orderLinkId", coid);
            patch_str(&mut f, "/data/0/orderId", venue_order_id);
            return f;
        }
        json!({"topic": "order", "data": [
            {"orderLinkId": coid, "orderStatus": "New", "orderId": venue_order_id,
             "updatedTime": 1}]})
    }
    fn frame_fill(
        &self,
        coid: &str,
        trade_id: &'static str,
        this_qty: f64,
        cum_qty: f64,
        total_qty: f64,
        px: f64,
        _terminal: bool,
    ) -> Value {
        // Bybit's terminality is cumExecQty >= orderQty (fallback leavesQty == 0). Captured
        // template: a REAL execution-topic Trade row; the scenario patches coid, the execId dedup
        // key, and the qty grid (type-preserving — the real wire's STRING numerics stay strings),
        // so a PARTIAL is the same real row with cum < orderQty.
        if let Some(mut f) = captured_template("bybit", "ws_fill") {
            Self::one_row(&mut f);
            patch_str(&mut f, "/data/0/orderLinkId", coid);
            patch_str(&mut f, "/data/0/execId", trade_id);
            patch_f64(&mut f, "/data/0/execQty", this_qty);
            patch_f64(&mut f, "/data/0/execPrice", px);
            patch_f64(&mut f, "/data/0/cumExecQty", cum_qty);
            patch_f64(&mut f, "/data/0/orderQty", total_qty);
            patch_f64(&mut f, "/data/0/leavesQty", total_qty - cum_qty);
            return f;
        }
        json!({"topic": "execution", "data": [
            {"execType": "Trade", "orderLinkId": coid, "execId": trade_id, "execTime": 1,
             "side": "Buy", "execQty": this_qty, "execPrice": px, "execFee": 0.0,
             "feeCurrency": "USDT", "isMaker": false,
             "cumExecQty": cum_qty, "orderQty": total_qty, "leavesQty": total_qty - cum_qty}]})
    }
    fn frame_canceled(&self, coid: &str) -> Value {
        if let Some(mut f) = captured_template("bybit", "ws_canceled") {
            Self::one_row(&mut f);
            patch_str(&mut f, "/data/0/orderLinkId", coid);
            return f;
        }
        json!({"topic": "order", "data": [
            {"orderLinkId": coid, "orderStatus": "Cancelled", "cancelType": "CancelByUser",
             "updatedTime": 1}]})
    }
    fn decode(&self, frame: &Value) -> Vec<Event> {
        vike_bybit::event_mapper::map_bybit_perp(frame, "bybit", "BTCUSDT")
    }
}

// --- OKX (V5 SWAP-perp orders channel; ExecActor) --------------------------------------------------

struct Okx;
impl ConformanceBridge for Okx {
    fn venue(&self) -> &'static str {
        "okx"
    }
    fn exec_kind(&self) -> ExecKind {
        ExecKind::CommandActor
    }
    fn order(&self, coid: &str) -> OrderRequest {
        limit_order(coid, "okx", "BTC-USDT-SWAP")
    }
    fn frame_accepted(&self, coid: &str, venue_order_id: &str) -> Value {
        json!({"arg": {"channel": "orders"}, "data": [
            {"clOrdId": coid, "state": "live", "ordId": venue_order_id, "uTime": 1}]})
    }
    fn frame_fill(
        &self,
        coid: &str,
        trade_id: &'static str,
        this_qty: f64,
        cum_qty: f64,
        total_qty: f64,
        px: f64,
        terminal: bool,
    ) -> Value {
        // OKX's terminality is state=="filled" (fallback accFillSz >= sz). ct_val is 1.0 here so
        // base qty passes through unchanged.
        json!({"arg": {"channel": "orders"}, "data": [
            {"clOrdId": coid, "state": if terminal { "filled" } else { "partially_filled" },
             "tradeId": trade_id, "fillSz": this_qty, "fillPx": px, "fillFee": "-0.1",
             "fillFeeCcy": "USDT", "side": "buy", "execType": "T",
             "accFillSz": cum_qty, "sz": total_qty, "fillTime": 1}]})
    }
    fn frame_canceled(&self, coid: &str) -> Value {
        json!({"arg": {"channel": "orders"}, "data": [
            {"clOrdId": coid, "state": "canceled", "cancelSource": "user", "uTime": 1}]})
    }
    fn decode(&self, frame: &Value) -> Vec<Event> {
        vike_okx::event_mapper::map_okx_perp(frame, "okx", "BTC-USDT-SWAP", 1.0)
    }
}

// --- Deribit (user.trades fills + JSON-RPC accept + order-history cancel; LiveRestClient) ----------
//
// Deribit is the odd crypto venue: its private-WS `event_mapper` (`map_deribit_private`) is
// FILLS-ONLY — lifecycle accept is the SYNCHRONOUS `private/buy` JSON-RPC reply, and a cancel that
// never traded surfaces via the audit-A3 order-history replay (`map_deribit_history`). So `decode`
// dispatches each venue-native frame to the REAL Deribit mapper that owns that lifecycle edge, and
// the fills stay incremental (`amount` per row, `state=="filled"` the SOLE terminal signal).
struct Deribit;
impl ConformanceBridge for Deribit {
    fn venue(&self) -> &'static str {
        "deribit"
    }
    fn exec_kind(&self) -> ExecKind {
        ExecKind::RestPoll
    }
    fn order(&self, coid: &str) -> OrderRequest {
        limit_order(coid, "deribit", "BTC-PERPETUAL")
    }
    fn frame_accepted(&self, coid: &str, venue_order_id: &str) -> Value {
        // A `private/buy` JSON-RPC reply — the venue half of the emitter split. `label` is our coid
        // (Deribit echoes it on every order/trade row), `order.order_id` the venue id.
        json!({"id": 1, "result": {"order": {
            "order_id": venue_order_id, "label": coid, "order_state": "open"}}})
    }
    fn frame_fill(
        &self,
        coid: &str,
        trade_id: &'static str,
        this_qty: f64,
        _cum_qty: f64,
        _total_qty: f64,
        px: f64,
        terminal: bool,
    ) -> Value {
        // A `user.trades.<instrument>.raw` subscription row: `amount` is the INCREMENTAL fill qty,
        // `state=="filled"` is Deribit's SOLE terminal signal (there are no cum/leaves fields).
        json!({"method": "subscription", "params": {
            "channel": "user.trades.BTC-PERPETUAL.raw", "data": [
            {"trade_id": trade_id, "label": coid, "instrument_name": "BTC-PERPETUAL",
             "direction": "buy", "amount": this_qty, "price": px, "fee": 0.0,
             "fee_currency": "USDT", "liquidity": "T", "timestamp": 1,
             "state": if terminal { "filled" } else { "open" }}]}})
    }
    fn frame_canceled(&self, coid: &str) -> Value {
        // A bare `private/get_order_history_by_instrument` array — `order_state=="cancelled"` is the
        // non-fill terminal the A3 replay recovers (the fills stream carries no lifecycle cancel).
        json!([{"label": coid, "order_id": "o-1", "order_state": "cancelled",
                "last_update_timestamp": 1}])
    }
    fn decode(&self, frame: &Value) -> Vec<Event> {
        // Fills → the fixture-tested `user.trades` mapper.
        if frame.get("method").and_then(|m| m.as_str()) == Some("subscription") {
            return vike_deribit::event_mapper::map_deribit_private(
                frame,
                "deribit",
                "BTC-PERPETUAL",
            );
        }
        // Order-history array → the A3 replay mapper (cancel/reject terminals).
        if frame.is_array() {
            return vike_deribit::history::map_deribit_history(
                frame,
                &Value::Array(Vec::new()),
                "deribit",
                "BTC-PERPETUAL",
            );
        }
        // JSON-RPC order reply → the accept. Deribit has NO pure accept mapper (the assembly lives in
        // `client::dispatch_submit`, which needs a live socket); mirror its exact three-field
        // construction here over the REAL `rpc::parse_response` envelope decode. An error envelope
        // resolves to a terminal reject, exactly as `dispatch_submit` does (no silent vanish).
        let (_id, result, error) = vike_deribit::rpc::parse_response(frame);
        if let Some(err) = error.filter(|e| !e.is_null()) {
            let reason = err.get("message").and_then(|m| m.as_str()).unwrap_or("").to_string();
            return vec![Event::OrderRejected(OrderRejected {
                client_order_id: order_field(&result, "label"),
                reason: reason.into(),
                ts: 1,
            })];
        }
        let order_id = order_field(&result, "order_id");
        vec![Event::OrderAccepted(OrderAccepted {
            client_order_id: order_field(&result, "label"),
            venue_order_id: Some(order_id.into()),
            ts: 1,
        })]
    }
}

/// Read `result.order.<key>` as an owned string (numbers stringified) — the tiny helper Deribit's
/// `decode` uses to lift the coid/venue-id off a JSON-RPC order reply, mirroring `dispatch_submit`.
fn order_field(result: &Option<Value>, key: &str) -> String {
    match result.as_ref().and_then(|r| r.get("order")).and_then(|o| o.get(key)) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

// --- Aster (Binance-wire USDⓈ-M perp ORDER_TRADE_UPDATE; ExecActor) --------------------------------
//
// Aster's perp user-data stream is Binance-verbatim, so its `event_mapper` re-exports the shared
// Binance-grammar `map_perp` — ONE mapper carries accept (`x=="NEW"`), incremental fills
// (`x=="TRADE"`, terminal on `X=="FILLED"`), and cancel (`x=="CANCELED"`), exactly like bybit/okx.
struct Aster;
impl ConformanceBridge for Aster {
    fn venue(&self) -> &'static str {
        "aster"
    }
    fn exec_kind(&self) -> ExecKind {
        ExecKind::CommandActor
    }
    fn order(&self, coid: &str) -> OrderRequest {
        limit_order(coid, "aster", "BTCUSDT")
    }
    fn frame_accepted(&self, coid: &str, venue_order_id: &str) -> Value {
        json!({"e": "ORDER_TRADE_UPDATE", "T": 1, "o": {
            "s": "BTCUSDT", "c": coid, "x": "NEW", "i": venue_order_id}})
    }
    fn frame_fill(
        &self,
        coid: &str,
        trade_id: &'static str,
        this_qty: f64,
        _cum_qty: f64,
        _total_qty: f64,
        px: f64,
        terminal: bool,
    ) -> Value {
        // Aster (Binance perp) terminality is `X=="FILLED"`; `l` is the INCREMENTAL last-fill qty.
        json!({"e": "ORDER_TRADE_UPDATE", "T": 1, "o": {
            "s": "BTCUSDT", "c": coid, "x": "TRADE",
            "X": if terminal { "FILLED" } else { "PARTIALLY_FILLED" },
            "t": trade_id, "S": "BUY", "l": this_qty, "L": px, "n": 0.0, "N": "USDT",
            "m": false, "ps": "BOTH"}})
    }
    fn frame_canceled(&self, coid: &str) -> Value {
        json!({"e": "ORDER_TRADE_UPDATE", "T": 1, "o": {
            "s": "BTCUSDT", "c": coid, "x": "CANCELED"}})
    }
    fn decode(&self, frame: &Value) -> Vec<Event> {
        vike_aster::perp_mapper::map_aster_perp(frame, "aster", "BTCUSDT")
    }
}

// --- Hyperliquid (orderUpdates lifecycle + /exchange partials; bespoke-signer ExecActor) -----------
//
// HL splits the FSM lane across two surfaces: WS `orderUpdates` carries accept (`open`), the
// terminal fill (`filled`, qty = `origSz`), and cancel (`canceled`) via the REAL `map_order_updates`;
// but `orderUpdates` has NO partial status, so a resting PARTIAL is only ever an
// `OrderPartiallyFilled` through the `/exchange` submit response (`filled.totalSz` < requested) via
// `map_order_response`. That response is POSITIONAL (the exec side pairs each `statuses[i]` with the
// order it sent), so `frame_fill(partial)` carries the same per-slot context in a non-wire `_ctx`
// sidecar that `decode` rebuilds into a `SubmittedOrder` before folding through the real mapper.
struct Hyperliquid;
impl ConformanceBridge for Hyperliquid {
    fn venue(&self) -> &'static str {
        "hyperliquid"
    }
    fn exec_kind(&self) -> ExecKind {
        ExecKind::CommandActor
    }
    fn order(&self, coid: &str) -> OrderRequest {
        limit_order(coid, "hyperliquid", "BTC")
    }
    fn frame_accepted(&self, coid: &str, venue_order_id: &str) -> Value {
        // `orderUpdates status=="open"` rests the order. The `cloid` carries our coid directly (the
        // caller's cloid→coid remap is shortcut here, as the other venues' frames carry the coid).
        json!({"channel": "orderUpdates", "data": [
            {"order": {"coin": "BTC", "side": "B", "limitPx": "50000.0", "sz": "1.0",
                       "oid": venue_order_id, "cloid": coid, "timestamp": 1},
             "status": "open", "statusTimestamp": 1}]})
    }
    fn frame_fill(
        &self,
        coid: &str,
        trade_id: &'static str,
        this_qty: f64,
        _cum_qty: f64,
        total_qty: f64,
        px: f64,
        terminal: bool,
    ) -> Value {
        if terminal {
            // A completing fill rests as `orderUpdates status=="filled"` → terminal OrderFilled;
            // `origSz` is the wrap's qty, `oid` its FSM-dedup trade id.
            json!({"channel": "orderUpdates", "data": [
                {"order": {"coin": "BTC", "side": "B", "limitPx": px, "sz": "0.0",
                           "origSz": this_qty, "oid": trade_id, "cloid": coid, "timestamp": 1},
                 "status": "filled", "statusTimestamp": 1}]})
        } else {
            // A resting partial: an `/exchange` response whose `filled.totalSz` (< requested `req_sz`)
            // → OrderPartiallyFilled. `_ctx` carries the positional SubmittedOrder context.
            json!({"status": "ok",
                   "response": {"type": "order", "data": {"statuses": [
                       {"filled": {"totalSz": this_qty, "avgPx": px, "oid": trade_id}}]}},
                   "_ctx": [{"coid": coid, "coin": "BTC", "side": 1, "req_sz": total_qty, "ts": 1}]})
        }
    }
    fn frame_canceled(&self, coid: &str) -> Value {
        json!({"channel": "orderUpdates", "data": [
            {"order": {"coin": "BTC", "side": "B", "oid": "o-1", "cloid": coid, "timestamp": 1},
             "status": "canceled", "statusTimestamp": 1}]})
    }
    fn decode(&self, frame: &Value) -> Vec<Event> {
        // An `/exchange` response (our `_ctx` sidecar present) → rebuild the positional
        // SubmittedOrder context and fold through the REAL response mapper; otherwise it is an
        // `orderUpdates` frame.
        if let Some(ctx) = frame.get("_ctx").and_then(|c| c.as_array()) {
            let orders: Vec<vike_hyperliquid::event_mapper::SubmittedOrder> = ctx
                .iter()
                .map(|o| vike_hyperliquid::event_mapper::SubmittedOrder {
                    client_order_id: o
                        .get("coid")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    coin: o.get("coin").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                    side: o.get("side").and_then(|v| v.as_i64()).unwrap_or(1) as i32,
                    req_sz: o.get("req_sz").and_then(|v| v.as_f64()).unwrap_or(0.0),
                    ts: o.get("ts").and_then(|v| v.as_i64()).unwrap_or(0),
                })
                .collect();
            return vike_hyperliquid::event_mapper::map_order_response(
                frame,
                "hyperliquid",
                &orders,
            );
        }
        vike_hyperliquid::event_mapper::map_order_updates(frame, "hyperliquid")
    }
}

// --- OANDA (v20 FX; ExecActor + transactions stream; whole-fill) -----------------------------------
//
// OANDA is the FX-streaming shape (plan §3.3, PR 3c). It fills WHOLE ([`FillShape::Whole`]): the
// order-POST reply carries the accept (`orderCreateTransaction`) and, for a MARKET order, an inline
// fill; delayed LIMIT/STOP fills + cancels arrive one-per-line on the transactions stream. So
// `decode` dispatches: a stream transaction (has `"type"`) → the REAL `decode_transaction_events`
// (`ORDER_FILL` → `[Fill, OrderFilled]`, `ORDER_CANCEL` → `[OrderCanceled]`); anything else is a POST
// reply → `map_order_response`, which takes the coid as a PARAM (the reply body doesn't echo it into
// the fields the mapper reads), so the accept frame carries it in a non-wire `_coid` sidecar — the
// same shortcut hyperliquid uses for its positional `_ctx`.
struct Oanda;
impl ConformanceBridge for Oanda {
    fn venue(&self) -> &'static str {
        "oanda"
    }
    fn exec_kind(&self) -> ExecKind {
        ExecKind::CommandActor
    }
    fn fill_shape(&self) -> FillShape {
        FillShape::Whole
    }
    fn order(&self, coid: &str) -> OrderRequest {
        limit_order(coid, "oanda", "EUR_USD")
    }
    fn frame_accepted(&self, coid: &str, venue_order_id: &str) -> Value {
        // A resting-LIMIT order-POST reply: `orderCreateTransaction` alone (no fill) → OrderAccepted.
        json!({"_coid": coid,
               "orderCreateTransaction": {"id": venue_order_id, "type": "LIMIT_ORDER"},
               "lastTransactionID": venue_order_id})
    }
    fn frame_fill(
        &self,
        coid: &str,
        trade_id: &'static str,
        this_qty: f64,
        _cum_qty: f64,
        _total_qty: f64,
        px: f64,
        _terminal: bool,
    ) -> Value {
        // A transactions-stream ORDER_FILL — OANDA reports the FULL units at once (whole fill).
        // `units`/`price` are STRING-typed on the wire (the mapper `parse()`s them).
        json!({"type": "ORDER_FILL", "id": trade_id, "time": "1", "orderID": "v-1",
               "instrument": "EUR_USD", "units": this_qty.to_string(), "price": px.to_string(),
               "commission": "0", "clientExtensions": {"id": coid}})
    }
    fn frame_canceled(&self, coid: &str) -> Value {
        json!({"type": "ORDER_CANCEL", "id": "700", "time": "1", "orderID": "v-1",
               "reason": "CLIENT_REQUEST", "clientExtensions": {"id": coid}})
    }
    fn decode(&self, frame: &Value) -> Vec<Event> {
        if frame.get("type").is_some() {
            return vike_oanda::decode_transaction_events(frame);
        }
        let coid = frame.get("_coid").and_then(|c| c.as_str()).unwrap_or_default();
        vike_oanda::map_order_response(coid, 1, frame)
    }
}

// --- IG (FX/CFD; ExecActor + Lightstreamer; whole-fill) --------------------------------------------
//
// IG splits its lifecycle across TWO real mappers, and fills WHOLE ([`FillShape::Whole`]): the
// synchronous `/confirms/{ref}` reply carries the accept for a resting working order (`map_confirm`
// with `market=false` → OrderAccepted only — its streaming twin `decode_trade_confirm` deliberately
// NEVER re-emits an accept), while the delayed working-order fill/cancel arrives on the Lightstreamer
// trade-update stream (`decode_trade_confirm`: an OPEN execution with size>0 → `[Fill, OrderFilled]`,
// a DELETED status → `[OrderCanceled]`, a REJECTED dealStatus → `[OrderRejected]`). Both mappers take
// the coid as a PARAM, carried in the `_coid` sidecar. ⚠ This harness's `decode` routes on `status`
// PRESENCE, and that is a convention of ITS OWN synthetic frames, NOT a wire fact: the live sync
// `/confirms` reply carries `status` too — `crates/bridges/ig/tests/ig_close_position_smoke.rs`
// asserts `Some("CLOSED")` on the SYNC close confirm. The real bridge never routes on the field, it
// routes by SOURCE LANE: `crates/bridges/ig/src/exec.rs`'s confirm fetch feeds `map_confirm`, and
// the Lightstreamer frames in `crates/bridges/ig/src/stream.rs` feed `decode_trade_confirm`. Stated
// because reading the old wording as wire truth would send someone looking for a `status`-less sync
// reply that IG does not send.
struct Ig;
impl ConformanceBridge for Ig {
    fn venue(&self) -> &'static str {
        "ig"
    }
    fn exec_kind(&self) -> ExecKind {
        ExecKind::CommandActor
    }
    fn fill_shape(&self) -> FillShape {
        FillShape::Whole
    }
    fn order(&self, coid: &str) -> OrderRequest {
        limit_order(coid, "ig", "CS.D.EURUSD.MINI.IP")
    }
    fn frame_accepted(&self, coid: &str, venue_order_id: &str) -> Value {
        // A working-order `/confirms` reply: dealStatus ACCEPTED, no `status`/`level` (still resting).
        json!({"_coid": coid, "dealStatus": "ACCEPTED", "dealId": venue_order_id,
               "epic": "CS.D.EURUSD.MINI.IP", "direction": "BUY", "size": 1.0})
    }
    fn frame_fill(
        &self,
        coid: &str,
        trade_id: &'static str,
        this_qty: f64,
        _cum_qty: f64,
        _total_qty: f64,
        px: f64,
        _terminal: bool,
    ) -> Value {
        // A streamed CONFIRMS execution: an OPEN working order with a level+size → whole fill.
        json!({"_coid": coid, "dealStatus": "ACCEPTED", "status": "OPEN", "dealId": trade_id,
               "epic": "CS.D.EURUSD.MINI.IP", "direction": "BUY",
               "size": this_qty, "level": px})
    }
    fn frame_canceled(&self, coid: &str) -> Value {
        // A streamed CONFIRMS with `status=="DELETED"` — a working order removed without executing.
        json!({"_coid": coid, "dealStatus": "ACCEPTED", "status": "DELETED", "size": 1.0,
               "reason": "CANCELLED"})
    }
    fn decode(&self, frame: &Value) -> Vec<Event> {
        let coid = frame.get("_coid").and_then(|c| c.as_str()).unwrap_or_default();
        // The sync-accept FRAME this harness builds above omits `status` so this decode can route on
        // its presence. That is a SYNTHETIC convention (see the struct doc) — the live sync
        // `/confirms` reply does carry `status`, so do not read this branch as a wire fact.
        if frame.get("status").is_some() {
            return vike_ig::decode_trade_confirm(frame, coid, 1);
        }
        vike_ig::map_confirm(coid, 1, false, frame)
    }
}

// --- Alpaca (US equities/crypto; ExecActor + SSE; whole-fill) --------------------------------------
//
// Alpaca is the SSE shape (plan §3.3, PR 3c) and the cleanest of the three: ONE real mapper
// (`decode_trade_event`) decodes every `/v2/events/trades` object — `event=="new"` → OrderAccepted,
// `event=="fill"` → `[Fill, OrderFilled]`, `event=="canceled"` → `[OrderCanceled]` — and the coid
// rides in `order.client_order_id` on the wire (no sidecar). It fills WHOLE ([`FillShape::Whole`]):
// the mapper has no partial state — a `"partial_fill"` event folds into `OrderFilled` too, so the
// cumulative-partial Lifecycle would wrongly reach a terminal on the first (0.4) fill.
struct Alpaca;
impl ConformanceBridge for Alpaca {
    fn venue(&self) -> &'static str {
        "alpaca"
    }
    fn exec_kind(&self) -> ExecKind {
        ExecKind::CommandActor
    }
    fn fill_shape(&self) -> FillShape {
        FillShape::Whole
    }
    fn order(&self, coid: &str) -> OrderRequest {
        limit_order(coid, "alpaca", "AAPL")
    }
    fn frame_accepted(&self, coid: &str, venue_order_id: &str) -> Value {
        json!({"event": "new", "timestamp": "2026-07-14T05:39:31.4Z",
               "order": {"id": venue_order_id, "client_order_id": coid, "symbol": "AAPL",
                         "side": "buy"}})
    }
    fn frame_fill(
        &self,
        coid: &str,
        trade_id: &'static str,
        this_qty: f64,
        _cum_qty: f64,
        _total_qty: f64,
        px: f64,
        _terminal: bool,
    ) -> Value {
        // event="fill" → dual-publish. `qty`/`price` are STRING-typed on the wire (mapper parses).
        json!({"event": "fill", "timestamp": "2026-07-14T05:39:31.4Z", "execution_id": trade_id,
               "qty": this_qty.to_string(), "price": px.to_string(),
               "order": {"id": "v-1", "client_order_id": coid, "symbol": "AAPL", "side": "buy"}})
    }
    fn frame_canceled(&self, coid: &str) -> Value {
        json!({"event": "canceled", "timestamp": "2026-07-14T05:39:31.4Z",
               "order": {"id": "v-1", "client_order_id": coid, "symbol": "AAPL", "side": "buy"}})
    }
    fn decode(&self, frame: &Value) -> Vec<Event> {
        vike_alpaca::decode_trade_event(frame)
    }
}

// --- FXCM (ForexConnect C++ SDK; ExecActor + a polled shim event queue; whole-fill) ---------------
//
// The venue with no wire at all. FXCM speaks a native C++ SDK, so its "frames" are JSON envelopes
// `crates/bridges/fxcm/src/shim/fcshim.cpp` builds with `snprintf` and enqueues for the exec
// thread to drain — and its ACCEPT is not a frame in any sense: it is the synchronous return of a
// placement FFI call. Both halves still reduce to the harness's stateless seam:
//
// * fill / cancel → the REAL `map_fxcm_event`, sourced from `crates/bridges/fxcm/tests/fixtures/
//   shim_events.json`, which is the shim's own emitter grammar transcribed and kept in step with
//   the C++ by `crates/bridges/fxcm/tests/fxcm_shim_envelope_grammar.rs`. So these scenarios run
//   against the shape the venue really produces, and a shim rename reddens there rather than here.
// * accept / reject → the REAL `map_placement`, lifted out of the exec loop for exactly this. The
//   `_placement` frame below is a HARNESS CARRIER, not a wire shape (there is no wire): it holds
//   the two values the FFI call returns, and `decode` folds them through the venue's own function.
//   Same shortcut oanda/ig use for their `_coid` param and hyperliquid for its positional `_ctx`.
//
// [`FillShape::Whole`], on the same evidence oanda/ig/alpaca carry: `map_fxcm_event` has NO
// `OrderPartiallyFilled` path at all — every `kind:"fill"` envelope maps to a terminal `OrderFilled`
// (FXCM's Trades table reports each execution as a whole trade row, not a cumulative on an order).
// [`ExecKind::CommandActor`]: `FxcmExecutionClient` is a newtype over the shared `ExecActor`.
//
// ⚠ What this does NOT cover, stated because a green row here is easy to over-read: the C++ shim
// itself, the FFI, and every ForexConnect behaviour behind it. No CI machine has ever linked that
// SDK. This row covers the venue's PURE layer — which is where the last four recorded defects on
// this bridge lived.
struct Fxcm;

impl Fxcm {
    /// One committed shim envelope, by `kind`, as a scenario template.
    ///
    /// Panics rather than falling back to a hand-authored `json!`: the fixture IS this venue's
    /// grammar, and a silent fallback would let the harness keep passing against a shape the shim
    /// no longer emits — the exact failure mode the fixture exists to prevent.
    fn envelope(kind: &str) -> Value {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../bridges/fxcm/tests/fixtures/shim_events.json");
        let body = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("missing fxcm shim fixture {}: {e}", path.display()));
        let root: Value = serde_json::from_str(&body)
            .unwrap_or_else(|e| panic!("malformed fxcm shim fixture {}: {e}", path.display()));
        root.get("envelopes")
            .and_then(|e| e.get(kind))
            .cloned()
            .unwrap_or_else(|| panic!("fxcm shim fixture has no `{kind}` envelope"))
    }
}

impl ConformanceBridge for Fxcm {
    fn venue(&self) -> &'static str {
        "fxcm"
    }
    fn exec_kind(&self) -> ExecKind {
        ExecKind::CommandActor
    }
    fn fill_shape(&self) -> FillShape {
        FillShape::Whole
    }
    fn order(&self, coid: &str) -> OrderRequest {
        // ⚠ NO `price`, unlike every other venue's `limit_order`. FXCM's exec loop now REFUSES a
        // limit request carrying one (`preflight_request`) because the shim cannot honor it — it
        // rests at a fixed pip distance from the live quote instead. A priced limit here would be a
        // request this venue never accepts, so the scenarios would be exercising a lifecycle that
        // cannot happen.
        //
        // ⚠ `qty` is BASE UNITS, and this comment used to say it was a whole LOT COUNT. It was, and
        // that was the defect: the shim places `Amount = base_unit * lots`, so `qty: 1.0` meant one
        // LOT — while the fill this harness patches from it (`/amount`) is base units, i.e. the two
        // halves of one scenario were in different units. `vike_fxcm::event_mapper::lots_for` now
        // divides by the live base unit size and refuses an inexact size, so one lot of the
        // ordinary EUR/USD 1000 is what a request looks like.
        OrderRequest {
            client_order_id: coid.into(),
            venue: "fxcm".into(),
            symbol: "EURUSD".into(),
            side: 1,
            qty: 1000.0,
            order_type: "limit".into(),
            price: None,
            ts: 1,
            ..Default::default()
        }
    }
    fn frame_accepted(&self, coid: &str, venue_order_id: &str) -> Value {
        // The harness carrier for a SYNCHRONOUS placement return (see the module note above).
        json!({"_kind": "_placement", "_coid": coid, "order_id": venue_order_id})
    }
    fn frame_fill(
        &self,
        coid: &str,
        trade_id: &'static str,
        this_qty: f64,
        _cum_qty: f64,
        _total_qty: f64,
        px: f64,
        _terminal: bool,
    ) -> Value {
        // The shim's REAL fill envelope, scenario leaves patched in. `amount` is the executed size
        // in BASE UNITS and `trade_id` the reconnect-dedup key; `_terminal` is unread because this
        // venue has no partial state to select — see [`FillShape::Whole`].
        let mut f = Self::envelope("fill");
        patch_str(&mut f, "/_coid", coid);
        patch_str(&mut f, "/trade_id", trade_id);
        patch_f64(&mut f, "/amount", this_qty);
        patch_f64(&mut f, "/rate", px);
        f
    }
    fn frame_canceled(&self, coid: &str) -> Value {
        let mut f = Self::envelope("canceled");
        patch_str(&mut f, "/_coid", coid);
        f
    }
    fn decode(&self, frame: &Value) -> Vec<Event> {
        let coid = frame.get("_coid").and_then(|c| c.as_str()).unwrap_or_default();
        // A placement carrier → the REAL placement mapper (the venue half of the emitter split).
        if frame.get("_kind").and_then(|k| k.as_str()) == Some("_placement") {
            let oid = frame.get("order_id").and_then(|o| o.as_str()).unwrap_or_default();
            return vike_fxcm::event_mapper::map_placement(coid, 1, Ok(oid));
        }
        // …otherwise a drained shim envelope → the REAL stateless decode.
        vike_fxcm::event_mapper::map_fxcm_event(frame, coid)
    }
}

fn limit_order(coid: &str, venue: &str, symbol: &str) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.into(),
        venue: venue.into(),
        symbol: symbol.into(),
        side: 1,
        qty: 1.0,
        order_type: "limit".into(),
        price: Some(50_000.0),
        ts: 1,
        ..Default::default()
    }
}

/// The venues COVERED by the harness. Extending: implement [`ConformanceBridge`] and add it here.
/// (No longer crypto-only — oanda/ig/alpaca joined via the whole-fill axis, plan §3.3 PR 3c.)
fn covered_bridges() -> Vec<Box<dyn ConformanceBridge>> {
    vec![
        Box::new(Binance),
        Box::new(Bybit),
        Box::new(Okx),
        Box::new(Deribit),
        Box::new(Aster),
        Box::new(Hyperliquid),
        Box::new(Oanda),
        Box::new(Ig),
        Box::new(Alpaca),
        Box::new(Fxcm),
    ]
}

/// Venues explicitly DEFERRED — recorded (never faked) with a reason, per CLAUDE.md's
/// no-silent-caps convention. Each becomes a `ConformanceBridge` impl when wired (a SEPARATE
/// promotion PR). `conformance_roster_is_exhaustive` asserts this set plus `covered_bridges()`
/// partitions `vike_model::VENUES` exactly once, so no roster venue can silently escape the harness.
/// PR 3c cleared the JSON-mapper FX/equity shapes (oanda/ig/alpaca); what remains is genuinely
/// incompatible with the stateless `decode(&Value) -> Vec<Event>` seam:
///
/// ⚠ The fxcm row is GONE, and how it read is worth remembering: *"behind the `fxcm` feature
/// (ForexConnect C++ FFI); default/CI build is an Unavailable stub — no real event_mapper is
/// compiled to drive"*. Every clause of that was false. `crates/bridges/fxcm/src/lib.rs` declares
/// its module tree with no `cfg` at all, `map_fxcm_event` was always compiled in every build, and
/// the `fxcm` feature gates nothing but `build.rs`. The obstacle was one keyword — `mod` instead of
/// `pub mod` — and a deferral reason nobody re-read for as long as it sounded plausible. When
/// deferring a venue, name the SHAPE that does not fit the seam (as the four rows below do), never
/// a feature gate: a feature is a claim about a build, and a build is exactly the thing a stale
/// reason stops describing.
const DEFERRED: &[(&str, &str)] = &[
    (
        "polymarket",
        "behind the `polymarket` feature (EIP-712); default build is an empty crate — no mapper reachable",
    ),
    (
        "dukascopy",
        "Java sidecar over JSON-lines (proto.rs), not a private-WS/JSON mapper; fake_jforex_bridge covers its lifecycle",
    ),
    (
        "ibkr",
        "stateful `EventMapper` (private module) whose fills need a two-message execDetails+commissionReport join keyed by exec_id and typed (non-JSON) reports — no stateless `decode(&Value)` shape; covered by its own tests/ibkr_lifecycle.rs",
    ),
    (
        "ctrader",
        "cTrader Open API is protobuf-framed — `exec_event_to_events` takes `ProtoOaExecutionEvent` structs + a `SymbolMap`, not JSON; wiring it needs a JSON→proto reconstruction axis beyond this PR",
    ),
    // vike:new-venue:row ("{venue}", "TODO(new-venue: {venue}): DEFERRED is the honest default only until this bridge has a fixture-tested stateless event_mapper — the moment it does, delete this row and add a ConformanceBridge impl to covered_bridges() instead. Replace this text with the REAL reason it cannot be covered yet."),
];

// ===================================================================================================
// Scenarios — shared bodies returning Ok / Err(reason) so the matrix can record per-cell outcomes.
// ===================================================================================================

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Scenario {
    Lifecycle,
    TransportDeath,
    CancelAfterClose,
    ReconnectMidOrder,
}

impl Scenario {
    const ALL: [Scenario; 4] = [
        Scenario::Lifecycle,
        Scenario::TransportDeath,
        Scenario::CancelAfterClose,
        Scenario::ReconnectMidOrder,
    ];
    fn label(self) -> &'static str {
        match self {
            Scenario::Lifecycle => "lifecycle",
            Scenario::TransportDeath => "transport-death",
            Scenario::CancelAfterClose => "cancel-after-close",
            Scenario::ReconnectMidOrder => "reconnect-mid-order",
        }
    }
}

/// A per-check assertion that records the failure reason rather than unwinding, so the coverage
/// matrix can show which cell failed and why. Uses an `if/else` guard (not `if !cond`) so a float
/// comparison inside `$cond` never trips `clippy::neg_cmp_op_on_partial_ord`.
macro_rules! check {
    ($cond:expr, $($arg:tt)*) => {
        if $cond {
        } else {
            return Err(format!($($arg)*));
        }
    };
}

fn run_cell(bridge: &dyn ConformanceBridge, scenario: Scenario) -> Result<(), String> {
    match scenario {
        Scenario::Lifecycle => scenario_lifecycle(bridge),
        Scenario::TransportDeath => scenario_transport_death(bridge),
        Scenario::CancelAfterClose => scenario_cancel_after_close(bridge),
        Scenario::ReconnectMidOrder => scenario_reconnect(bridge),
    }
}

/// (1) submit → accept → fill reaches exactly one terminal (`Filled`) with `filled_qty == 1.0`.
/// A [`FillShape::Cumulative`] venue fills in two pieces (partial 0.4 → `PartiallyFilled`, then
/// 0.6 → `Filled`); a [`FillShape::Whole`] venue reports the full 1.0 in ONE execution (its mapper
/// has no partial state) → straight to `Filled`. The submit/accept setup and the terminal asserts
/// are shared; only the fill leg branches.
fn scenario_lifecycle(b: &dyn ConformanceBridge) -> Result<(), String> {
    let coid = "life-1";
    let mut order = ContractOrder::new(b.order(coid));

    order.fold(&submitted(coid)); // Rust-side half of the emitter split
    check!(
        order.status() == OrderStatus::Submitted,
        "after submit expected Submitted, got {:?}",
        order.status()
    );

    let accepted = b.decode(&b.frame_accepted(coid, "v-1"));
    check!(
        accepted.iter().any(|e| matches!(e, Event::OrderAccepted(_))),
        "accept frame did not decode to OrderAccepted: {accepted:?}"
    );
    for e in &accepted {
        order.fold(e);
    }
    check!(
        order.status() == OrderStatus::Accepted,
        "after accept expected Accepted, got {:?}",
        order.status()
    );

    match b.fill_shape() {
        FillShape::Cumulative => {
            // partial fill 0.4 of 1.0 → PartiallyFilled
            for e in &b.decode(&b.frame_fill(coid, "t1", 0.4, 0.4, 1.0, 50_000.0, false)) {
                order.fold(e);
            }
            check!(
                order.status() == OrderStatus::PartiallyFilled,
                "after partial expected PartiallyFilled, got {:?}",
                order.status()
            );
            // remaining 0.6 completes the order
            let full = b.decode(&b.frame_fill(coid, "t2", 0.6, 1.0, 1.0, 50_000.0, true));
            check!(
                full.iter().any(|e| matches!(e, Event::OrderFilled(_))),
                "full-fill frame did not decode to OrderFilled: {full:?}"
            );
            for e in &full {
                order.fold(e);
            }
        }
        FillShape::Whole => {
            // ONE execution reports the whole 1.0 → Filled (no intermediate partial state exists).
            let full = b.decode(&b.frame_fill(coid, "t1", 1.0, 1.0, 1.0, 50_000.0, true));
            check!(
                full.iter().any(|e| matches!(e, Event::OrderFilled(_))),
                "whole-fill frame did not decode to OrderFilled: {full:?}"
            );
            for e in &full {
                order.fold(e);
            }
        }
    }

    check!(
        order.status() == OrderStatus::Filled,
        "expected terminal Filled, got {:?}",
        order.status()
    );
    check!(
        order.terminals_applied == 1,
        "expected exactly ONE terminal, got {}",
        order.terminals_applied
    );
    check!(order.dropped_terminal_on_live == 0, "a terminal was lost on a live order");
    check!((order.filled_qty() - 1.0).abs() < 1e-9, "filled_qty {} != 1.0", order.filled_qty());
    Ok(())
}

/// (2) A dead venue path still synthesizes a terminal — no order silently vanishes. Exercised via
/// the EXACT shared seam each venue's `ExecutionClient` uses at submit.
fn scenario_transport_death(b: &dyn ConformanceBridge) -> Result<(), String> {
    let coid = "dead-1";
    match b.exec_kind() {
        ExecKind::CommandActor => {
            // The shared ExecActor — bybit's `BybitExecutionClient(ExecActor)` and okx's
            // `OkxExecutionClient(ExecActor)` forward `submit` straight to this. A dead venue thread
            // (login failure / panic) means the command channel is closed; submit MUST synthesize a
            // terminal OrderRejected rather than let the intent vanish.
            let (events, mut rx) = event_channel(16);
            let mut client = dead_exec_actor(events);
            client.submit(&b.order(coid));

            let ev = recv_ingest_event(&mut rx)?;
            match &ev {
                Event::OrderRejected(r) => {
                    check!(
                        r.client_order_id == coid,
                        "reject coid mismatch: {}",
                        r.client_order_id
                    );
                    check!(!r.reason.as_str().is_empty(), "synthesized reject must carry a reason");
                }
                other => {
                    return Err(format!(
                        "dead ExecActor must synthesize OrderRejected, got {other:?}"
                    ));
                }
            }
            // Folds to a terminal in the real FSM — no vanish.
            let mut order = ContractOrder::new(b.order(coid));
            order.fold(&ev);
            check!(
                order.status() == OrderStatus::Rejected,
                "expected Rejected, got {:?}",
                order.status()
            );
            check!(
                order.terminals_applied == 1,
                "expected exactly ONE terminal, got {}",
                order.terminals_applied
            );
        }
        ExecKind::RestPoll => {
            // The shared post-timeout resolver every REST-poll venue (binance/deribit) reaches for
            // after an E_TIMEOUT_AMBIGUOUS submit. The two no-vanish halves:
            //   venue-confirmed-absent → a TRUE terminal OrderRejected (never a silent vanish);
            //   inconclusive re-query   → an OPTIMISTIC OrderAccepted (never a FALSE terminal that
            //                              would strand a position the venue actually opened).
            let absent = resolve_ambiguous_submit(coid, 1, Ok(None));
            match &absent {
                Event::OrderRejected(r) => {
                    check!(r.client_order_id == coid, "reject coid mismatch: {}", r.client_order_id)
                }
                other => {
                    return Err(format!(
                        "venue-absent submit must resolve to OrderRejected, got {other:?}"
                    ));
                }
            }
            let mut order = ContractOrder::new(b.order(coid));
            order.fold(&absent);
            check!(
                order.status() == OrderStatus::Rejected,
                "expected Rejected, got {:?}",
                order.status()
            );
            check!(
                order.terminals_applied == 1,
                "expected exactly ONE terminal, got {}",
                order.terminals_applied
            );

            let inconclusive = resolve_ambiguous_submit(
                coid,
                1,
                Err(VenueApiError { code: 0, msg: "boom".into() }),
            );
            check!(
                matches!(inconclusive, Event::OrderAccepted(_)),
                "inconclusive re-query must NOT fabricate a false terminal, got {inconclusive:?}"
            );
        }
    }
    Ok(())
}

/// (3) A cancel arriving after the order already closed must be handled without corrupting the
/// single terminal — both a late venue `OrderCanceled` replay AND a late `OrderCancelRejected`.
fn scenario_cancel_after_close(b: &dyn ConformanceBridge) -> Result<(), String> {
    let coid = "cac-1";
    let mut order = ContractOrder::new(b.order(coid));

    // Drive to a terminal Filled first.
    order.fold(&submitted(coid));
    for e in &b.decode(&b.frame_accepted(coid, "v-1")) {
        order.fold(e);
    }
    for e in &b.decode(&b.frame_fill(coid, "t1", 1.0, 1.0, 1.0, 50_000.0, true)) {
        order.fold(e);
    }
    check!(
        order.status() == OrderStatus::Filled,
        "setup: expected Filled, got {:?}",
        order.status()
    );
    check!(
        order.terminals_applied == 1,
        "setup: expected one terminal, got {}",
        order.terminals_applied
    );

    // (i) A late venue OrderCanceled replay (a cancel/close race). The FSM rejects Canceled-from-
    // Filled; it is dropped as a benign out-of-order artifact — NOT a lost terminal (status is
    // already terminal) — and the order stays Filled with exactly one terminal.
    let canceled = b.decode(&b.frame_canceled(coid));
    check!(
        canceled.iter().any(|e| matches!(e, Event::OrderCanceled(_))),
        "cancel frame did not decode to OrderCanceled: {canceled:?}"
    );
    for e in &canceled {
        order.fold(e);
    }
    check!(
        order.status() == OrderStatus::Filled,
        "after late cancel expected still Filled, got {:?}",
        order.status()
    );
    check!(
        order.terminals_applied == 1,
        "late cancel added a second terminal: {}",
        order.terminals_applied
    );
    check!(order.dropped_terminal_on_live == 0, "late cancel wrongly counted as a lost terminal");

    // (ii) A late OrderCancelRejected advisory (the shared cancel path's failure event) after close
    // is likewise dropped — a non-terminal advisory on a terminal order — leaving state intact.
    order.fold(&Event::OrderCancelRejected(OrderCancelRejected {
        client_order_id: coid.into(),
        reason: "venue unavailable".into(),
        ts: 1,
    }));
    check!(
        order.status() == OrderStatus::Filled,
        "after late cancel-reject expected still Filled, got {:?}",
        order.status()
    );
    check!(
        order.terminals_applied == 1,
        "cancel-reject disturbed the single terminal: {}",
        order.terminals_applied
    );
    Ok(())
}

/// (4) A mid-order reconnect (through the REAL shared pump) that replays the mid-order state must
/// not duplicate/lose the terminal or double-count a fill.
///
/// [`FillShape::Cumulative`]: session 1 delivers accept + partial(t1) then drops; session 2 REPLAYS
/// accept + partial(t1) (as a resync would) then delivers the full fill(t2) — proving the replayed
/// partial is deduped by `trade_id` (no double-count) and exactly one terminal survives.
/// [`FillShape::Whole`]: a whole-fill venue's terminal fill inherently ENDS the pump (it sets
/// `stop`), so it can never be replayed across a reconnect — the mid-order state is the resting
/// ACCEPTED order. Session 1 delivers accept then drops; session 2 replays accept (the FSM folds the
/// second accept as a benign idempotent drop, not a lost terminal) then delivers the whole fill. It
/// proves the reconnect neither duplicates nor loses the eventual terminal; the fill-dedup guarantee
/// is the Cumulative variant's to make (documented so the weaker whole-fill check is explicit).
fn scenario_reconnect(b: &dyn ConformanceBridge) -> Result<(), String> {
    let coid = "recon-1";
    let mut order = ContractOrder::new(b.order(coid));
    order.fold(&submitted(coid)); // Rust-side submit before the venue stream opens

    let accept = || Ok(StreamMsg::Text(b.frame_accepted(coid, "v-1").to_string()));
    let (s1, s2) = match b.fill_shape() {
        FillShape::Cumulative => {
            let partial = || {
                Ok(StreamMsg::Text(
                    b.frame_fill(coid, "t1", 0.4, 0.4, 1.0, 50_000.0, false).to_string(),
                ))
            };
            let s1 = Scripted::new(vec![
                accept(),
                partial(),
                Err(StreamError::Closed("mid-order drop".into())),
            ]);
            let s2 = Scripted::new(vec![
                accept(),  // replayed accept
                partial(), // replayed partial — must be deduped, not double-counted
                Ok(StreamMsg::Text(
                    b.frame_fill(coid, "t2", 0.6, 1.0, 1.0, 50_000.0, true).to_string(),
                )),
            ]);
            (s1, s2)
        }
        FillShape::Whole => {
            // No partial exists to replay: the drop lands after the resting accept, and the whole
            // fill arrives only after the reconnect (it would otherwise stop the pump before it).
            let s1 =
                Scripted::new(vec![accept(), Err(StreamError::Closed("mid-order drop".into()))]);
            let s2 = Scripted::new(vec![
                accept(), // replayed accept — benign idempotent drop in the FSM
                Ok(StreamMsg::Text(
                    b.frame_fill(coid, "t1", 1.0, 1.0, 1.0, 50_000.0, true).to_string(),
                )),
            ]);
            (s1, s2)
        }
    };
    let mut sessions = vec![s2, s1]; // popped from the end → session 1 opens first
    let mut reconnects = 0usize;

    let stop = AtomicBool::new(false);
    let result = run_user_data_forever(
        || match sessions.pop() {
            Some(s) => OpenOutcome::Ready(s),
            None => OpenOutcome::Stopped,
        },
        |frame| b.decode(frame),
        |ev| {
            let terminal = matches!(ev, Event::OrderFilled(_));
            order.fold(&ev);
            if terminal {
                stop.store(true, Ordering::Relaxed); // full fill folded — end the pump cleanly
            }
            true
        },
        &stop,
        Duration::from_millis(1),
        Duration::from_millis(2), // tiny backoff so the reconnect sleep is fast
        None,
        || reconnects += 1,
    );

    check!(result.is_ok(), "pump ended with an auth error: {result:?}");
    check!(reconnects == 1, "expected exactly ONE reconnect, got {reconnects}");
    check!(
        order.status() == OrderStatus::Filled,
        "expected terminal Filled, got {:?}",
        order.status()
    );
    check!(
        order.terminals_applied == 1,
        "reconnect produced {} terminals (want exactly 1)",
        order.terminals_applied
    );
    check!(order.dropped_terminal_on_live == 0, "reconnect lost a terminal");
    check!(
        (order.filled_qty() - 1.0).abs() < 1e-9,
        "reconnect miscounted the fill (a replay was double-counted): filled_qty {} != 1.0",
        order.filled_qty()
    );
    Ok(())
}

// ===================================================================================================
// Shared test scaffolding.
// ===================================================================================================

/// Spawn an `ExecActor` whose venue thread dies immediately (drops the command receiver), and block
/// until that receiver is provably gone — so the subsequent `submit` deterministically lands on a
/// closed channel. Mirrors `exec_actor_dead_thread.rs`.
fn dead_exec_actor(events: EventSender) -> ExecActor {
    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
    let actor = ExecActor::spawn(
        "dead-venue",
        events,
        move |cmd_rx: std::sync::mpsc::Receiver<ExecCommand>| {
            drop(cmd_rx); // venue login failed → the command receiver is gone
            let _ = done_tx.send(());
        },
    );
    done_rx.recv().expect("dead-venue thread signalled");
    actor
}

/// Block (bounded) for the next `Event` on the ingest lane. Uses a private current-thread runtime so
/// the harness needs no `#[tokio::test]`; mirrors the model tests' `recv_event`.
fn recv_ingest_event(rx: &mut tokio::sync::mpsc::Receiver<Ingest>) -> Result<Event, String> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .map_err(|e| format!("runtime build: {e}"))?;
    let ingest = rt
        .block_on(async { tokio::time::timeout(Duration::from_secs(10), rx.recv()).await })
        .map_err(|_| "timed out waiting for an ingest event".to_string())?
        .ok_or_else(|| "ingest channel closed with no event".to_string())?;
    match ingest {
        Ingest::Event(ev) => Ok(ev),
        other => Err(format!("expected Ingest::Event, got {other:?}")),
    }
}

// ===================================================================================================
// The tests: one per scenario (granular signal) + the coverage matrix.
// ===================================================================================================

/// Run one scenario across every covered venue, failing with the offending venue named.
fn assert_scenario_all(scenario: Scenario) {
    for bridge in covered_bridges() {
        if let Err(why) = run_cell(&*bridge, scenario) {
            panic!("[{} × {}] {why}", bridge.venue(), scenario.label());
        }
    }
}

#[test]
fn lifecycle_submit_accept_fill_all_covered_venues() {
    assert_scenario_all(Scenario::Lifecycle);
}

#[test]
fn transport_death_synthesizes_terminal_all_covered_venues() {
    assert_scenario_all(Scenario::TransportDeath);
}

#[test]
fn cancel_after_close_preserves_single_terminal_all_covered_venues() {
    assert_scenario_all(Scenario::CancelAfterClose);
}

#[test]
fn reconnect_mid_order_no_duplicate_or_lost_terminal_all_covered_venues() {
    assert_scenario_all(Scenario::ReconnectMidOrder);
}

/// The honest coverage matrix: runs the FULL venue × scenario grid, prints it (visible under
/// `--nocapture`), and asserts every COVERED cell passed. The DEFERRED venues are printed with
/// their reasons so what is NOT run is explicit (CLAUDE.md no-silent-caps convention).
#[test]
fn coverage_matrix() {
    let bridges = covered_bridges();
    let mut out = String::new();
    out.push_str("\n=== cross-bridge conformance coverage (audit br5) ===\n\n");

    // header
    out.push_str(&format!("{:<10}", "venue"));
    for s in Scenario::ALL {
        out.push_str(&format!(" | {:<19}", s.label()));
    }
    out.push('\n');
    out.push_str(&"-".repeat(10 + Scenario::ALL.len() * 22));
    out.push('\n');

    let mut failures: Vec<String> = Vec::new();
    for bridge in &bridges {
        out.push_str(&format!("{:<10}", bridge.venue()));
        for s in Scenario::ALL {
            let cell = match run_cell(&**bridge, s) {
                Ok(()) => "PASS".to_string(),
                Err(why) => {
                    failures.push(format!("[{} × {}] {why}", bridge.venue(), s.label()));
                    "FAIL".to_string()
                }
            };
            out.push_str(&format!(" | {cell:<19}"));
        }
        out.push('\n');
    }

    out.push_str(
        "\nDEFERRED venues (recorded, not covered in v1 — drop-in ConformanceBridge to add):\n",
    );
    for (venue, reason) in DEFERRED {
        out.push_str(&format!("  - {venue:<11} {reason}\n"));
    }

    // Which venues ran against REAL captured wire templates (plan 5c) vs hand-authored frames —
    // explicit, so a reviewer sees exactly what the grid above exercised.
    out.push_str("\nFrame sourcing (plan 5c — committed sanitized captures as templates):\n");
    for bridge in &bridges {
        let kinds: Vec<&str> = ["ws_accepted", "ws_fill", "ws_canceled"]
            .into_iter()
            .filter(|k| captured_template(bridge.venue(), k).is_some())
            .collect();
        if kinds.is_empty() {
            out.push_str(&format!(
                "  - {:<11} hand-authored frames (no committed capture)\n",
                bridge.venue()
            ));
        } else {
            out.push_str(&format!("  - {:<11} captured: {}\n", bridge.venue(), kinds.join(", ")));
        }
    }
    out.push_str(&format!(
        "\nCovered: {} venue(s) × {} scenario(s) = {} cells. Deferred: {} venue(s).\n",
        bridges.len(),
        Scenario::ALL.len(),
        bridges.len() * Scenario::ALL.len(),
        DEFERRED.len(),
    ));

    println!("{out}");
    assert!(failures.is_empty(), "conformance cells failed:\n{}", failures.join("\n"));
}

/// The roster gate: every venue in the canonical `vike_model::VENUES` roster MUST be classified
/// exactly once — either a COVERED `covered_bridges()` [`ConformanceBridge`] impl OR a DEFERRED row
/// with a NON-EMPTY reason. A new bridge crate lands a `VENUES` entry; this test then fails until it
/// is either wired into the table or deliberately deferred-with-reason, so no venue silently escapes
/// the harness. Mirrors vike-model's `fees::every_roster_venue_has_a_fee_schedule` completeness shape
/// (`crate::venues::VENUES`-iterating, per-venue XOR, named — never a silent fallback).
#[test]
fn conformance_roster_is_exhaustive() {
    let covered: HashSet<&str> = covered_bridges().iter().map(|b| b.venue()).collect();
    let deferred: HashSet<&str> = DEFERRED.iter().map(|(v, _)| *v).collect();

    // Every DEFERRED reason is a real, non-empty justification (no blank placeholder deferrals).
    for (venue, reason) in DEFERRED {
        assert!(!reason.trim().is_empty(), "deferred venue {venue} must carry a non-empty reason");
    }
    // No duplicate deferrals, and no venue both covered AND deferred (each is one or the other).
    assert_eq!(deferred.len(), DEFERRED.len(), "duplicate venue id in DEFERRED");
    assert!(
        covered.is_disjoint(&deferred),
        "a venue is both COVERED and DEFERRED: {:?}",
        covered.intersection(&deferred).collect::<Vec<_>>()
    );

    // The two sets partition the roster exactly — same length assertion as fees.rs, so a stray
    // entry in EITHER list (not on the roster) trips here even before the per-venue loop.
    assert_eq!(
        covered.len() + deferred.len(),
        vike_model::VENUES.len(),
        "COVERED + DEFERRED must partition vike_model::VENUES exactly once \
         (covered={covered:?}, deferred={deferred:?})"
    );

    // The load-bearing check: every roster venue is classified exactly once. A NEW roster venue in
    // NEITHER set fails here (`scheduled ^ deferred` is false) — it must be wired or deferred.
    for &v in vike_model::VENUES {
        let is_covered = covered.contains(v);
        let is_deferred = deferred.contains(v);
        assert!(
            is_covered ^ is_deferred,
            "roster venue {v:?} must be classified exactly once: a covered_bridges() \
             ConformanceBridge impl OR a DEFERRED row with a reason (covered={is_covered}, \
             deferred={is_deferred})"
        );
    }
}
