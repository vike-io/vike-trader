//! Cross-bridge EXEC conformance: ONE scenario table run against every covered venue's REAL
//! `event_mapper` and the shared exec plumbing, machine-checking the CLAUDE.md venue-adapter
//! contract. Crypto venues decode private-WS frames; the FX/CFD/equity venues decode REST-reply,
//! transactions-stream or SSE frames — see [`FillShape`].
//!
//! ## The contract under test
//! * **Emitter split** — Rust emits `OrderSubmitted` synchronously at submit; the venue emits
//!   `OrderAccepted / Rejected / Canceled / (Partially)Filled`.
//! * **No silent vanish** — a dead venue path MUST synthesize a terminal `OrderRejected`.
//! * **Exactly one terminal** — the `ManagedOrder` FSM must reach exactly one terminal state
//!   (`Filled | Canceled | Rejected | Denied | Expired`); illegal/out-of-order events are dropped
//!   idempotently, never corrupting the single terminal.
//!
//! The invariant oracle is the REAL FSM (`vike_exec::ManagedOrder::apply`) — the code the live core
//! folds through — wrapped by [`ContractOrder`], which mirrors `ExecutionEngine::on_event`'s thin
//! per-order policy (bare-fill vs wrap-fill split, trade-id dedup on reconnect replays, and the
//! "terminal dropped on a live order" loss counter). Each venue plugs in ONLY its venue-native JSON
//! frames and its fixture-tested mapper; the scenarios, folds and assertions are shared.
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
//! 5. **PositionFold** — the MONEY side, which rows 1-4 never look at: the Lifecycle frames folded
//!    through a REAL `vike_exec::ExecutionEngine` mounted on the symbol the venue's own order names,
//!    asserting every bare `Event::Fill` they decode to moved the position. The lifecycle wraps
//!    route by client order id, so an FSM row stays green while every fill is dropped at the
//!    engine's symbol filter; this row is the one that sees a label mismatch. See [`AccountLane`]
//!    for the one venue whose lifecycle frames carry no bare fill.
//!
//! ## Coverage and the roster gate
//! `covered_bridges()` is the covered column and `DEFERRED` the rest, each row with its reason;
//! `coverage_matrix` prints both under `--nocapture`, and each impl in `bridge_conformance/venues.rs`
//! says which real mappers its `decode` dispatches to. `conformance_roster_is_exhaustive` checks the
//! two EXHAUSTIVE against `vike_model::VENUES`: a new roster venue fails until it is wired in or
//! deferred with a non-empty reason. Adding a venue is a drop-in [`ConformanceBridge`] impl; the
//! table and assertions do not change.
//!
//! ## Captured-template sourcing
//! A venue's `frame_*` methods may source their frame from that venue's COMMITTED sanitized capture
//! (`crates/bridges/<venue>/tests/fixtures/captured/<kind>.json`, written by its `*_capture_smoke`
//! arm) instead of a hand-authored `json!`: [`captured_template`] loads the first captured frame as
//! a STRUCTURE template — the real envelope, field set and value TYPES — and the impl patches ONLY
//! the scenario-driven leaves (coid, trade id, qty, px, terminal marker) through the type-preserving
//! `patch_f64`/`patch_str`. Fixture absent → the hand-authored fallback. The `coverage_matrix`
//! printout names which venues sourced captured templates.

// `check!` funnels every failure through `format!`, and a few checks carry a bare literal message,
// which `clippy::useless_format` flags under `-D warnings`; allowed crate-wide so the macro stays
// one-armed.
#![allow(clippy::useless_format)]

use std::collections::HashSet;

use serde_json::Value;

use vike_exec::{ManagedOrder, OrderStatus};
use vike_model::OrderRequest;
use vike_model::events::{Event, OrderSubmitted};

// `#[cfg(test)]` is always true in a test crate; it is here so `new_venue_gate`'s row-site
// derivation folds both children into THIS file, which carries the `DEFERRED` scaffold marker and so
// must stay a row site while `venues.rs` holds the covered venues' literals.
#[path = "bridge_conformance/scenarios.rs"]
#[cfg(test)]
mod scenarios;
#[path = "bridge_conformance/venues.rs"]
#[cfg(test)]
mod venues;

use scenarios::{Scenario, run_cell};
use venues::{
    Alpaca, Aster, Binance, Bybit, Deribit, Fxcm, Hyperliquid, Ig, Oanda, Okx, captured_template,
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
/// drop UNLESS a terminal event fails on a still-live order (a real, counted loss).
struct ContractOrder {
    mo: ManagedOrder,
    /// Account-side (bare-fill) dedup keys — mirrors `seen_trade_ids`.
    seen_bare: HashSet<String>,
    /// FSM-side (wrap-fill) dedup keys — mirrors `seen_fsm_trade_ids`.
    seen_wrap: HashSet<String>,
    /// Count of successful transitions INTO a terminal state — MUST end at exactly 1.
    terminals_applied: usize,
    /// A terminal event that failed to apply while the order was still live — a genuinely-lost
    /// terminal. MUST stay 0 in every healthy scenario.
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
                // A terminal event rejected while the order is still LIVE is a real loss;
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
/// ⚠ WHICH venue is which is each [`ConformanceBridge::exec_kind`] impl, never a list in a comment:
/// a doc comment is checked against nothing, and such a list here had already gone stale.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ExecKind {
    /// Command-thread venue (`ExecActor`). A dead thread synthesizes `OrderRejected`.
    CommandActor,
    /// REST-poll venue (`LiveRestClient`). An ambiguous/timed-out submit is resolved by the shared
    /// `resolve_ambiguous_submit`.
    RestPoll,
}

/// A venue's fill granularity — the SECOND harness axis, orthogonal to [`ExecKind`]. Crypto venues
/// stream CUMULATIVE partials: a resting order fills in pieces, each a distinct
/// `OrderPartiallyFilled` before the terminal `OrderFilled`. A `Whole` venue's REAL mapper has NO
/// `OrderPartiallyFilled` path at all — one execution reports the full qty as `OrderFilled` (alpaca
/// even folds a `"partial_fill"` SSE event into `OrderFilled`); each impl's `fill_shape` names its
/// evidence. The Lifecycle/Reconnect scenarios branch on this so a whole-fill venue is exercised
/// through its real mapper instead of asserting a partial state it can never emit; TransportDeath
/// and CancelAfterClose never drive a partial.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum FillShape {
    /// Cumulative partials then a terminal fill.
    Cumulative,
    /// One execution = the whole order; no partial-fill state.
    Whole,
}

/// Where a venue's POSITION-moving executions arrive — the THIRD harness axis, read only by the
/// PositionFold scenario.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum AccountLane {
    /// The fill frames the table decodes carry the bare `Event::Fill` the `Account` folds (every
    /// covered venue but one).
    FillFrames,
    /// The lifecycle frames carry the FSM wraps only and the position moves on ANOTHER lane, named
    /// by the reason. The scenario then asserts the declaration is TRUE — no bare fill decoded —
    /// rather than skipping the venue, so a mapper that starts emitting bare fills here reddens
    /// until it is reclassified.
    Separate(&'static str),
}

/// A venue's plug-in for the shared table: its order shape, its venue-native user-data JSON frames,
/// and its REAL fixture-tested private-WS mapper. NOTHING else is venue-specific.
trait ConformanceBridge {
    fn venue(&self) -> &'static str;
    fn exec_kind(&self) -> ExecKind;

    /// This venue's fill granularity (see [`FillShape`]). Defaults to `Cumulative` (the crypto
    /// shape); the whole-fill venues override it.
    fn fill_shape(&self) -> FillShape {
        FillShape::Cumulative
    }

    /// Where this venue's position-moving executions arrive (see [`AccountLane`]). Defaults to
    /// `FillFrames`; the one venue whose lifecycle frames carry no bare fill overrides it.
    fn account_lane(&self) -> AccountLane {
        AccountLane::FillFrames
    }

    /// A well-formed limit order for this venue. Its `symbol` is ALSO the symbol the PositionFold
    /// scenario mounts the engine on, so it must be the spelling the venue's production mount names
    /// — the catalog spelling the Trade window sends, never a convenient bare twin.
    fn order(&self, coid: &str) -> OrderRequest;

    /// Venue-native user-data JSON that ACCEPTS a resting order → `[OrderAccepted]`.
    fn frame_accepted(&self, coid: &str, venue_order_id: &str) -> Value;

    /// Venue-native user-data JSON for ONE fill of `this_qty` (cumulative `cum_qty` of `total_qty`
    /// base at `px`), keyed by `trade_id` (the reconnect-dedup key). `terminal` selects the FULL
    /// fill (→ `[Fill, OrderFilled]`) vs a PARTIAL (→ `[Fill, OrderPartiallyFilled]`).
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

    /// The venue's REAL fixture-tested mapper(s), dispatched per frame where it has several.
    fn decode(&self, frame: &Value) -> Vec<Event>;
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

/// Venues explicitly DEFERRED — recorded, never faked, each with its reason; each becomes a
/// `ConformanceBridge` impl when wired. `conformance_roster_is_exhaustive` asserts this set plus
/// `covered_bridges()` partitions `vike_model::VENUES` exactly once. What remains here does not fit
/// the stateless `decode(&Value) -> Vec<Event>` seam.
///
/// ⚠ A reason names the SHAPE that does not fit the seam, never a feature gate: a feature is a claim
/// about a build, and a stale one stops describing it. fxcm sat here behind a reason saying its
/// mapper was feature-gated while `map_fxcm_event` compiled in every build.
const DEFERRED: &[(&str, &str)] = &[
    (
        "polymarket",
        "registry-keyed decoder, not a stateless `decode(&Value)` — `crates/bridges/polymarket/src/exec_plane/user_ws.rs`'s `decode_user` takes a shared in-memory `PolymarketRegistry` (the CLOB assigns the order id and echoes no client id, so every fill/cancel is re-keyed through a map the exec thread fills at accept, with unresolved ids parked and replayed)",
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

#[test]
fn position_fold_moves_the_mounted_position_all_covered_venues() {
    assert_scenario_all(Scenario::PositionFold);
}

/// The honest coverage matrix: runs the FULL venue × scenario grid, prints it (visible under
/// `--nocapture`), and asserts every COVERED cell passed. The DEFERRED venues are printed with
/// their reasons so what is NOT run is explicit.
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

    // Which venues ran against REAL captured wire templates vs hand-authored frames.
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
    // Which lane each venue's PositionFold cell actually exercised — a `Separate` cell asserts the
    // declaration, not a fold, and the matrix must not let it read as the same kind of PASS.
    out.push_str("\nAccount lane (position-fold):\n");
    for bridge in &bridges {
        match bridge.account_lane() {
            AccountLane::FillFrames => out.push_str(&format!(
                "  - {:<11} fill frames, engine mounted on {:?}\n",
                bridge.venue(),
                bridge.order("x").symbol
            )),
            AccountLane::Separate(lane) => out.push_str(&format!(
                "  - {:<11} SEPARATE lane, not driven here: {lane}\n",
                bridge.venue()
            )),
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
/// the harness.
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

    // The two sets partition the roster exactly, so a stray entry in EITHER list (not on the
    // roster) trips here even before the per-venue loop.
    assert_eq!(
        covered.len() + deferred.len(),
        vike_model::VENUES.len(),
        "COVERED + DEFERRED must partition vike_model::VENUES exactly once \
         (covered={covered:?}, deferred={deferred:?})"
    );

    // The load-bearing check: every roster venue is classified exactly once. A NEW roster venue in
    // NEITHER set fails here (`is_covered ^ is_deferred` is false) — it must be wired or deferred.
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
