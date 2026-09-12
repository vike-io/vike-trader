//! ExecutionEngine — the live composition root. Exact port of `exec/live_oms.py`.
//!
//! Owns account/gate/registry/client for ONE (venue, symbol). Manual-ticket order path:
//! `submit_order` calls the gate and publishes `OrderDenied` on veto or `client.submit` on ok.
//! `on_event` folds the venue stream: bare `FillEvent` → `Account::apply_fill`, which is
//! IDEMPOTENT PER `trade_id` in its own right — the always-on reconnect dedup lives on `Account`,
//! welded to the money it protects, and this fold only reads the [`crate::account::FillFold`]
//! verdict back (see `account.rs`'s "Fill dedup" section) — admitted by mounted symbol
//! (`accepts_symbol`) or by coid ownership (`owns_fill_symbol`, combo gate 4: fills of orders
//! THIS engine manages fold even under never-mounted symbols, while a combo's aggregate
//! net-price print never does); `Order*` lifecycle → the `ManagedOrder` registry with the
//! SEPARATE `_seen_fsm_trade_ids` wrap dedup; liquidation frames dedup via `_seen_liq_ids`. The fold also carries the cancel-vs-fill race guard
//! (LEAN `CancelPendingOrders` semantics): a venue-seeded PENDING_CANCEL order is restored to
//! its pre-cancel live status before a fill wrap / cancel-reject applies, so venue truth is
//! never dropped by the fixture-pinned FSM table. Durable persistence is the runtime's job now: the
//! vike-data command journal + its materialized `exec_fill`/`exec_order` series — this engine
//! folds state only (the retired SQLite `exec_db` audit sink is gone).

use indexmap::IndexMap;
use std::collections::HashSet;
use vike_model::FiniteNumbers;
use vike_model::OrderRequest;
use vike_model::events::{Event, FillEvent, OrderDenied, OrderLiquidated, PositionLiquidated};

use crate::Account;
use crate::bus::{EventHandler, Fold, Outbox};
use crate::order::{ManagedOrder, OrderStatus};
use crate::risk::{RiskContext, RiskGate, TradingState};

// The venue-adapter contract, the test doubles, and the engine output/snapshot types were split
// into sibling modules (behavior byte-identical). Re-exported here so `execution_engine::X` and the
// crate-root `vike_exec::X` paths every venue/runtime import stay exactly as before.
mod client;
mod reconcile;
mod test_clients;

pub use client::{CancelIntent, ExecutionClient};
pub use reconcile::{AppliedFill, OrderEventOut, ReconcileSnapshot};
pub use test_clients::{RecordingClient, TestExecutionClient};

/// One open position priced only off a STALE last-known value (portfolio-robustness Ext 2 —
/// mark-health tracking). The operator-facing "why" behind [`ExecutionEngine::stale_marks`]:
/// which position, off which price source, and how stale (`age_ms` = how far past the engine
/// clock the value's timestamp sits). NOT journaled — a live health signal derived from
/// non-persisted market data, like the `PriceBoard` it reads.
#[derive(Debug, Clone, PartialEq)]
pub struct StaleMark {
    pub venue: String,
    pub symbol: String,
    pub position_side: String,
    pub source: crate::price_board::PriceSource,
    pub age_ms: i64,
}

/// Reconcile drift tolerance (audit exec#2): a position-size / balance divergence within
/// `DRIFT_ABS_TOL + DRIFT_REL_TOL * |venue|` is float noise (venue rounding of a folded size,
/// last-ulp wire re-encode), NOT real drift — only a divergence BEYOND it is surfaced. The
/// absolute floor covers values near zero where the relative term collapses.
const DRIFT_REL_TOL: f64 = 1e-6;
const DRIFT_ABS_TOL: f64 = 1e-8;

/// `LocalView::qty_tol` default for [`ExecutionEngine::local_view`] — no engine-level qty_tol
/// source yet (Task 8 brief); mirrors the `recon::diff` tests' own `1e-9` convention.
const LOCAL_VIEW_QTY_TOL: f64 = 1e-9;

/// True when locally-folded `local` and venue-truth `venue` differ by more than the reconcile
/// drift tolerance — the single decision site for [`ExecutionEngine::diff_snapshot`].
#[inline]
fn drift_diverges(local: f64, venue: f64) -> bool {
    (local - venue).abs() > DRIFT_ABS_TOL + DRIFT_REL_TOL * venue.abs()
}

/// Live composition root. Drive orders with `submit_order`; read `.account` / `.registry`.
pub struct ExecutionEngine<C: ExecutionClient> {
    pub account: Account,
    /// Read-side price board (portfolio-observer PR-1): per-source price cells the resolver
    /// walks on COLD paths only. Fed alongside `account.set_mark` at the write sites; never
    /// read in the fold. Deliberately outside `EngineSnapshot` (journal hash-fence freeze).
    pub price_board: crate::price_board::PriceBoard,
    /// Resolver knobs for the ENGINE-INTERNAL decision sites (the pre-trade gate's
    /// `resolved_equity` / `resolved_margin_in_use_by` reads). The runtime sets it from
    /// `CoreConfig::price_cfg` at spawn — the SAME source every vike-core caller passes
    /// explicitly — so one config governs every resolver read. Permissive default
    /// (`PriceCfg::default()`: mark enabled, no freshness windows). Like `price_board`, NOT
    /// serialized (config, not state — re-imposed by the runtime on restore, exactly as
    /// `equity_seed` is).
    pub price_cfg: crate::price_board::PriceCfg,
    pub gate: RiskGate,
    pub client: C,
    /// The CANONICAL venue id (`"binance"`) — the key every per-venue capability table is looked
    /// up under: [`vike_model::caps_for`], [`vike_model::amend_semantics`],
    /// [`vike_model::fee_schedule_for`], `vike_bridge_core::tif::venue_tif`,
    /// [`vike_model::venue_margin_support`]. It is ALSO the label this engine writes its own
    /// `Account` position keys, its registry's `OrderRequest::venue`s and its published
    /// `VenueBlock::venue` under, so every self-comparison in this file reads THIS field and not
    /// [`Self::route_key`].
    ///
    /// ⚠ It must stay a roster id ([`vike_model::VENUES`]). Every one of those tables is
    /// fail-closed on a miss — `caps_for` answers `VenueCaps::UNSUPPORTED` (empty
    /// `supported_order_kinds`, so preflight rejects every order) and `amend_semantics` answers
    /// `AmendSemantics::Unknown`, which SILENTLY changes what a modify's quantity means. That is
    /// why an account-distinguishing suffix goes on [`Self::route_key`] instead: decorating THIS
    /// field is the `"binance#2"` trap, and `crates/vike-exec/tests/engine/route_key.rs` is the
    /// gate over it.
    pub venue: String,
    /// The ROUTING key — which ENGINE an inbound venue-tagged payload belongs to, unique per venue
    /// ACCOUNT rather than per venue. `vike_core`'s `CoreThread::engine_idx_for_route_key` matches
    /// on this and on nothing else.
    ///
    /// **Not a synonym of [`Self::venue`], and deliberately not spelled like one.** They answer
    /// different questions: `venue` answers "what does this venue support" (a fact about the
    /// exchange, shared by every account on it), `route_key` answers "which of this process's
    /// engines is this fill for" (a fact about the account). They were ONE field, and that is
    /// exactly why two accounts of one venue could not be expressed in one process: labelling both
    /// engines `"binance"` made the second unreachable for fills (routing returns the first match,
    /// so both accounts' fills folded into one book and reconcile's auto-applied `PositionDrift`
    /// then rewrote each onto the other's number in turn), while labelling the second
    /// `"binance#2"` fixed routing and broke every capability lookup at once.
    ///
    /// **[`Self::new`] sets it EQUAL to `venue`, and nothing in this workspace sets it otherwise.**
    /// While the two are equal every routing decision is bit-identical to the single-field
    /// behaviour, by construction — this split ENABLES the multi-account shape, it does not
    /// introduce it.
    ///
    /// ⚠ If something ever does set it: it is used as a FILENAME component by
    /// `vike_ops::live_lock::LiveLock::acquire` (`<state_dir>/LIVE-<route_key>.lock`), so it must
    /// be path-safe — no `/`, `\`, or `..`. Suffixing the canonical id (`"binance#2"`) satisfies
    /// that; nothing validates it today because nothing sets it.
    pub route_key: String,
    pub symbol: String,
    /// The effective per-venue fee schedule the mount resolved for this engine (fee model
    /// follow-up 1): the LIVE account-actual rate when a `ReconClient::fetch_fee_rates` producer
    /// returned one, else the static [`vike_model::fee_schedule_for`] default. Surfaced read-only
    /// for cost display (the snapshot's `VenueBlock.fee_schedule`); it does NOT drive the fold —
    /// a live engine's fills report the venue's real commission, and a paper engine's own
    /// `PaperExecutionClient` carries its schedule separately. `None` until the mount sets it
    /// (a GUI-only / test engine never does), so the default path is byte-identical.
    pub fee_schedule: Option<vike_model::FeeSchedule>,
    /// `Account::apply_account_state` quote-asset selector (Python default "USDT")
    pub quote_asset: String,
    /// perp: force reduce_only on submit_close flattens (read by the GUI ticket path)
    pub reduce_only_on_close: bool,
    pub registry: IndexMap<String, ManagedOrder>,
    pub trading_state: TradingState,
    /// wall-clock the runtime stamps before each dispatch (persistence `updated_ts`;
    /// Python used the `now_ms` ctor lambda)
    pub now_ms: i64,
    /// When true (the runtime sets it iff a strategy is mounted), every fill the account fold
    /// ACCEPTS (post-dedup) is also buffered in `applied_fills` for `Strategy::on_fill`
    /// delivery. Capturing at the one `apply_fill` site means a WS reconnect replay can never
    /// double-fire the handler. Default false so a GUI-only engine never grows the buffer.
    pub collect_applied_fills: bool,
    /// fills accepted since the runtime last drained (see `collect_applied_fills`)
    pub applied_fills: Vec<AppliedFill>,
    /// NON-FILL order-lifecycle transitions (accept/reject/deny/cancel/expire — NOT fills) captured
    /// for `Strategy::on_order_event` delivery, gated by the SAME `collect_applied_fills` flag (set
    /// iff a strategy is mounted). Each carries the order's (venue, symbol) so the runtime routes it
    /// to the owning mount exactly like `applied_fills`. Default empty; a GUI-only engine never
    /// grows it, and it is not serialized (deliveries are not replayed).
    pub order_events: Vec<OrderEventOut>,
    /// seed for the per-fill `equity_after` snapshot (the runtime's `seed_cash`)
    pub equity_seed: f64,
    /// Phase D multi-symbol opt-in (RUST-NATIVE): additional symbols this engine accepts
    /// venue events for (fills/funding/liquidations fold into the one multi-symbol
    /// Account). Empty (default) = today's single-symbol filter, byte-identical.
    pub extra_symbols: Vec<String>,
    // NOTE: there is no `seen_trade_ids` field here any more. The bare-`Event::Fill` dedup ledger
    // moved ONTO the aggregate it protects — `Account`'s own private set, consulted inside
    // `Account::apply_fill` — so the check cannot be bypassed by a path that forgets it, and there is
    // no second set that can disagree with the first. This type still OWNS the snapshot/`local_view`
    // spelling of it (`EngineSnapshot::seen_trade_ids` is unchanged on the wire); it just reads it
    // through `Self::seen_trade_ids()` instead of holding a copy. The two sets below stay: they guard
    // DIFFERENT aggregates (the `ManagedOrder` FSM's `filled_qty`/`avg_fill_px`, and the liquidation
    // lane), neither of which lives on `Account`.
    seen_fsm_trade_ids: HashSet<String>,
    seen_liq_ids: HashSet<String>,
    /// Audit C1 observability: count of terminal events that failed to apply while the order was
    /// still LIVE (a genuinely-lost terminal — the order may be stranded). Benign idempotent replays
    /// on already-terminal orders are NOT counted. Read by monitoring/tests; never affects the fold.
    pub dropped_terminal_on_live: u64,
    /// Audit C2 observability: count of lifecycle events whose coid was not in the registry
    /// (pre-restart order absent from reconcile, external-account order, or a bug). The event is
    /// still dropped — this only makes the drop visible.
    pub dropped_unknown_coid: u64,
    /// Confirm-race hardening (audit C3 follow-up): count of venue LIVENESS/EXECUTION events
    /// (`OrderAccepted`/`OrderPartiallyFilled`/`OrderFilled`) dropped because the order had ALREADY
    /// been terminalized via a KILL path (`Rejected`/`Canceled`/`Expired`/`Denied`) — the signature
    /// of a premature terminal (classically the watchdog's stage-2 phantom-reject racing a slow venue
    /// confirm) leaving a real venue position STRANDED: the venue accepted/filled an order our FSM
    /// believes is dead. The C1 counter above MISSES this (its `!status_after.is_terminal()` guard is
    /// false once the order is terminal), so without this the clobber is invisible. Benign same-state
    /// terminal replays are NOT counted (they are not liveness/fill events). Read by monitoring/tests;
    /// never affects the fold, never changes WHAT is dropped — only makes the strand observable. Not
    /// serialized (a live health signal, like `dropped_unknown_coid`; re-derived from events on
    /// restart).
    pub stranded_terminal_drops: u64,
    /// HOSTILE-VENUE observability: count of money-lane events (`Fill`/`Funding`/`AccountState`/
    /// `PositionLiquidated`) refused because a folded f64 was NOT FINITE — see
    /// [`vike_model::FiniteNumbers`] for how `"NaN"` reaches a typed field off the wire in the first
    /// place, and why one such value poisons the ledger IRRECOVERABLY.
    ///
    /// **This one is never routine.** The other drop counters above all have benign explanations (a
    /// reconnect replay, an external-account order, an out-of-order WS frame); a non-finite venue
    /// number has NONE. Any nonzero value here means a venue sent something no venue should ever
    /// send, so the fold logs it at ERROR rather than debug/warn. Read by monitoring/tests; never
    /// affects anything but the one event it rejects.
    pub dropped_nonfinite: u64,
}

impl<C: ExecutionClient> ExecutionEngine<C> {
    /// `venue` seeds BOTH [`Self::venue`] and [`Self::route_key`], so a freshly-built engine is
    /// the single-account shape and behaves exactly as it did when the two were one field. The
    /// signature is deliberately unchanged — 170-odd call sites construct engines this way, and a
    /// route key that had to be passed at every one of them would be a route key nobody could
    /// leave alone. A second account sets [`Self::route_key`] afterwards.
    pub fn new(account: Account, gate: RiskGate, client: C, venue: &str, symbol: &str) -> Self {
        ExecutionEngine {
            account,
            gate,
            client,
            venue: venue.to_string(),
            route_key: venue.to_string(),
            symbol: symbol.to_string(),
            fee_schedule: None,
            quote_asset: "USDT".to_string(),
            reduce_only_on_close: false,
            registry: IndexMap::new(),
            trading_state: TradingState::Active,
            price_board: crate::price_board::PriceBoard::default(),
            price_cfg: crate::price_board::PriceCfg::default(),
            now_ms: 0,
            collect_applied_fills: false,
            applied_fills: Vec::new(),
            order_events: Vec::new(),
            equity_seed: 0.0,
            extra_symbols: Vec::new(),
            seen_fsm_trade_ids: HashSet::new(),
            seen_liq_ids: HashSet::new(),
            dropped_terminal_on_live: 0,
            dropped_unknown_coid: 0,
            stranded_terminal_drops: 0,
            dropped_nonfinite: 0,
        }
    }

    /// Cold-start layer-2 dedup seed from a durable source (e.g. the replayed command journal).
    /// Python gated the fold on `record_fill -> fresh` at apply time; the restart twin seeds the
    /// in-memory set once at startup instead (same net semantics).
    ///
    /// Delegates to [`Account::seed_seen_fill_ids`] — the ledger lives on the account now, so this
    /// is the same set `Account::apply_fill` consults, not a parallel copy.
    pub fn seed_seen_trade_ids<I: IntoIterator<Item = String>>(&mut self, ids: I) {
        self.account.seed_seen_fill_ids(ids);
    }

    /// The bare-fill dedup ledger, read-only. Reads THROUGH to [`Account::seen_fill_ids`]: this type
    /// no longer holds a copy, so a reader here and the guard inside `Account::apply_fill` cannot
    /// diverge. `pub` because the reconcile `LocalView`, the `EngineSnapshot` wire and the journal
    /// cross-check all need to see what has been folded.
    pub fn seen_trade_ids(&self) -> impl Iterator<Item = &str> {
        self.account.seen_fill_ids()
    }

    /// Gate the order; publish OrderDenied on veto or submit to the venue.
    /// `now_ms` is the injected clock (Python's `now_ms=lambda` ctor arg, passed per-call here).
    /// Coarse per-ORDER span (NOT per-message): `skip_all` keeps the hot path free of arg
    /// formatting; only the coid is recorded. No subscriber = a cheap no-op (the latency gate).
    #[tracing::instrument(level = "info", target = "vike_exec::oms", skip_all, fields(coid = %request.client_order_id))]
    pub fn submit_order(&mut self, request: &OrderRequest, now_ms: i64, outbox: &mut Outbox) {
        if let Some(req) = self.gate_and_register(request, now_ms, outbox) {
            self.client.submit(&req);
        }
    }

    /// Risk-gate + register one intent. Returns the (possibly gate-adjusted) request to send to the
    /// venue, or `None` after publishing `OrderDenied`. Shared by `submit_order` and
    /// `submit_order_batch` so the veto/registration path is identical single or batched.
    fn gate_and_register(
        &mut self,
        request: &OrderRequest,
        now_ms: i64,
        outbox: &mut Outbox,
    ) -> Option<OrderRequest> {
        let ctx = self.risk_ctx(request, now_ms);
        let verdict = self.gate.check(request, &ctx);
        let Some(req) = (if verdict.ok { verdict.request } else { None }) else {
            // Per-ORDER (not per-tick): a RiskGate veto is fault-adjacent, off the measured hop.
            tracing::warn!(target: "vike_exec::risk", coid = %request.client_order_id, reason = %verdict.reason, "order denied by RiskGate");
            let denied = OrderDenied {
                client_order_id: request.client_order_id.clone(),
                reason: verdict.reason.into(),
                ts: request.ts,
            };
            // Capture the veto for `Strategy::on_order_event`: a denied order never enters the
            // registry, so the FSM-apply capture site below can't see it — grab (venue, symbol)
            // from the request HERE. Gated on the same mount flag as `applied_fills` (only when a
            // strategy is mounted), so a GUI-only engine and the latency gate pay nothing; a veto is
            // per-ORDER, off the measured per-tick hop anyway.
            if self.collect_applied_fills {
                self.order_events.push(OrderEventOut {
                    venue: request.venue.clone(),
                    symbol: request.symbol.clone(),
                    event: vike_model::strategy::OrderLifecycle {
                        client_order_id: denied.client_order_id.clone(),
                        // The ENGINE holds no tag registry — it is the runtime's, and the runtime
                        // stamps this at delivery. See `OrderLifecycle::tag`.
                        tag: None,
                        kind: vike_model::strategy::OrderEventKind::Denied {
                            reason: denied.reason.to_string(),
                        },
                    },
                });
            }
            outbox.publish(Event::OrderDenied(denied));
            return None;
        };
        let mut mo = ManagedOrder::new(req.clone());
        mo.created_ms = Some(now_ms); // audit C3: stamp on the injected clock the watchdog sweeps on
        self.registry.insert(req.client_order_id.clone(), mo);
        Some(req)
    }

    /// Build the [`RiskContext`] one gate call judges `request` against.
    ///
    /// Split out of [`Self::gate_and_register`] (byte-identically — every field is computed by the
    /// same expression in the same order, so every pre-existing verdict is unchanged) because the
    /// MODIFY path needs the same context: a modify that raises qty is economically a bigger order,
    /// and judging it required the same equity/margin/mark/multiplier basis a submit is judged on.
    /// Sharing the BUILDER rather than copying it is what keeps the two from drifting — a modify
    /// judged on a different price or a different multiplier than a submit of the same terms would
    /// be a silent risk-arithmetic divergence of exactly the kind the one-price law exists to stop.
    fn risk_ctx(&self, request: &OrderRequest, now_ms: i64) -> RiskContext {
        // Phase B margin fields: computed ONLY when the opt-in knob is set — the default
        // path builds the same 4-field ctx as before (resolved_equity is O(positions) per
        // order; fine per-order, never free).
        //
        // THE CONTRACT MULTIPLIER IS LANE-INDEPENDENT (4th confirmed instance of the #458
        // multiplier-in-context class: #458 gate notional, #477 UI cap, #479 ibkr_mount).
        // `ctx.multiplier` feeds the gate's notional (`qty × ref_price × ctx.multiplier`,
        // consumed by min_notional AND max_notional_per_order — see `risk.rs`), not only the
        // buying-power lane, so it is minted HERE, outside the `im_for` arm. It used to be
        // computed only inside the armed branch (else → 1.0), so an engine with notional
        // floors/caps armed but NO margin lane (`im_for` None — the live default when a venue
        // mounts `RiskLimits::from_properties` with no configured leverage) judged notional at
        // multiplier 1.0 for mult≠1 instruments (deribit inverse perps), while the backtest
        // builds its ctx with the real multiplier unconditionally — the live side was the
        // WRONG side of that divergence. `multiplier_of` returns 1.0 for any symbol outside
        // the grid, so every mult==1 mount is byte-identical (`x * 1.0` is an IEEE-754 no-op).
        let multiplier = self.account.multiplier_of(&request.symbol);
        // Hedge-aware coverage basis, computed ONCE for the reversal credit and the ctx (see
        // `gate_position_size` — one-way accounts read exactly the old `position_size("BOTH")`).
        //
        // Keyed on the ORDER's symbol, not the engine's mounted one — the same basis
        // `multiplier` (above) and `im_for` (below) already use. An engine `accepts_symbol`s its
        // primary OR anything in `extra_symbols`, so a foreign-symbol order is reachable TODAY;
        // reading the mount's position here judged that order against a MIXTURE of two
        // instruments (the order symbol's margin rate and multiplier, the mount symbol's position
        // and mark), which is worse than either basis alone. BYTE-IDENTICAL for every
        // single-symbol engine — `request.symbol == self.symbol` there, so this reads the same
        // f64 — and a correction exactly where the two differ.
        let pos = self.gate_position_size(&request.symbol);
        let (equity, margin_used, closing_credit) =
            if let Some(im_req) = self.gate.limits.im_for(&request.symbol) {
                // THE shared margin-in-use fold, resolver-priced (`resolved_margin_in_use_by`)
                // so the margin numerator shares `resolved_equity`'s price basis below — one
                // price law across the free-BP comparison. RATE POLICY unchanged: each open
                // position uses its OWN per-symbol IM, falling back to the ORDER symbol's
                // `im_req` when it has no override (a no-override position is priced as if this
                // order's leverage applied to it — never skipped). POOL POLICY (the liquidation
                // law's partition, mirroring `check_margin_call`): only CROSS positions consume
                // the shared account equity this gate admits against — an Isolated position is
                // backed by its own walled-off wallet and a Cash position is fully funded, so
                // counting either here would DOUBLE-CHARGE a mixed account (their collateral is
                // not the equity in `ctx.equity`). Every position defaults Cross today, so an
                // all-cross account takes the filter as a no-op.
                let used = self.resolved_margin_in_use_by(&self.price_cfg, |(_v, s, _side), p| {
                    p.margin_mode.is_cross().then(|| self.gate.limits.im_for(s).unwrap_or(im_req))
                })
                // ...plus the margin already COMMITTED by this engine's live un-filled orders. See
                // `live_order_margin`: counting positions alone overstated free buying power by
                // every order in flight, so a second order was judged as though the first committed
                // nothing.
                + self.live_order_margin(&self.price_cfg, im_req, &request.client_order_id);
                // direction-reversing order: credit the margin the close frees + the LEAN
                // re-open credit (single-requirement model: mm == im in the gate).
                //
                // Priced through the SAME resolver as `used` above and `resolved_equity` below —
                // the last split basis in the MARGIN lane (#524 moved the other two and left this
                // one on the raw `Account.marks` scalar). A stale-high mark used to credit margin the
                // resolver-priced `used` never charged, so a reversal was admitted against
                // buying power that did not exist. `resolved_position_price` reads the position's
                // OWN symbol/side chain, exactly like the `used` fold's per-position price.
                //
                // `ctx.mark_price` below now shares this resolver too (its priceless/market arm) —
                // the LAST `self.mark()` site in this gate. That field is load-bearing for the
                // SEPARATE notional lane — `risk.rs`'s `ref_price` (the min/max-notional fallback)
                // and its projected-exposure cap both read it — and it used to fall back to the raw
                // `Account.marks` scalar while equity/margin/credit priced off the board, so a
                // market order's notional/exposure judged on a DIFFERENT price than the same call's
                // margin lane (and than the single-price SimBroker gate). Closed below: the whole
                // gate call now speaks ONE price.
                //
                // A `Missing` resolution credits NOTHING, matching `resolved_margin_in_use_by`,
                // which drops an unpriceable position from `used` entirely: a position that
                // consumed no margin frees none. The two ends are CONSISTENT with each other, not
                // both conservative — zeroing the credit denies more (the conservative direction),
                // while dropping the position from `used` inflates free buying power by charging
                // no margin for a real position (the ANTI-conservative one). Do not read the
                // `Missing` path as uniformly safe.
                let credit = if pos != 0.0 && request.side as f64 * pos < 0.0 {
                    let px = self
                        .resolved_position_price(&self.venue, &request.symbol, pos, &self.price_cfg)
                        .unwrap_or(0.0);
                    2.0 * pos.abs() * px * multiplier * im_req
                } else {
                    0.0
                };
                // One-price law (the #518 leftovers): the SAME resolver-priced equity the
                // watchdog/strategy/snapshot read, not the mark-only `equity_all` — a
                // quote-lane crash the marks never saw now tightens admission too.
                //
                // ⚠ `sizing_equity`, not `resolved_equity`: this lane SPENDS against equity, so
                // `RiskLimits::max_sizing_equity` applies here and a lower figure can only refuse
                // sooner. Bit-identical when no ceiling is armed. The margin-CALL sweep keeps the
                // uncapped resolver — `Self::sizing_equity`'s doc carries the asymmetry and names
                // which consumer is on which side.
                (self.sizing_equity(self.equity_seed, &self.price_cfg), used, credit)
            } else {
                (0.0, 0.0, 0.0)
            };
        RiskContext {
            position_size: pos,
            // The priceless (market/stop) notional+exposure reference joins equity/margin/credit
            // on the ONE resolver basis, priced through the position's own side chain like the
            // credit above. `Missing` -> 0.0, exactly the empty raw `Account.marks` read returned;
            // a fresh venue mark prices both stores identically, so an ordinary marked symbol on a
            // market order is byte-identical to the pre-convergence raw-scalar read.
            // ⚠ **THE MARK FIRST, the order's own price only as a last resort.** This used to read
            // `request.price` first, which made `mark_price` mean "the order's price" for every
            // priced order — and the field is the VALUATION basis, not an order-price one. Two
            // consequences, both silent:
            //
            //  * `risk.rs`'s projected-exposure cap values `|pos + side*qty| * ctx.mark_price *
            //    multiplier`. On a limit it judged exposure at the LIMIT price, so a resting order
            //    far from the mark understated (or overstated) what the account would be exposed to.
            //  * `risk.rs`'s PRICE COLLAR compares `|req.price - ctx.mark_price| > band`. With the
            //    two equal that difference is ZERO, so the collar could NEVER fire for a limit
            //    order — a fat-finger protection that was structurally dead for exactly the orders
            //    it exists to catch. (A stop still worked: it carries no `price`, so `mark_price`
            //    resolved to a real mark and its trigger was collared.)
            //
            // ⚠ The NOTIONAL lanes are untouched, which is what makes this safe to change:
            // `check_inner` computes `ref_price = req.price.or(req.trigger_price)
            // .unwrap_or(ctx.mark_price)`, so min/max-notional and the buying-power lane already
            // take the order's price FIRST and never consulted this field for a priced order.
            //
            // ⚠ `request.price` stays as the final fallback rather than letting an unresolvable
            // mark drop to `0.0`. A `0.0` mark makes projected exposure 0, which VACATES the
            // exposure cap at any size — `check_combo` treats that case as a hard `"leg X: no-mark"`
            // deny for exactly this reason. Today's code provides that protection for priced orders
            // by accident; keep it deliberately.
            mark_price: self
                .resolved_position_price(&self.venue, &request.symbol, pos, &self.price_cfg)
                .or(request.price)
                .unwrap_or(0.0),
            trading_state: self.trading_state,
            now_ms,
            equity,
            margin_used,
            closing_credit,
            multiplier,
            // The ACCOUNT-aggregate ceiling's other half, folded ONLY when that ceiling is armed —
            // the same `if let Some(..)` discipline the Phase-B margin fields above follow, and for
            // the same reason: the fold is O(positions + live orders) and every deployment that
            // writes no `max_account_exposure` line must pay nothing for a lane it did not turn on.
            // The `0.0` is inert rather than approximate: the only lane that reads this field is
            // the one the `is_some()` above just answered `false` for.
            //
            // ⚠ `request.client_order_id` is the JUDGED order, excluded from the resting-order half
            // — on a submit it is not in the registry, and on an AMEND (`modify_order` builds this
            // ctx from the projected request under the resting order's own coid) it is, where
            // counting it as well as projecting it would refuse the amend at a ceiling the same
            // order was admitted under at submit.
            account_exposure_excl_order: if self.gate.limits.max_account_exposure.is_some() {
                self.resolved_account_exposure_excluding(
                    &request.symbol,
                    &request.client_order_id,
                    &self.price_cfg,
                )
            } else {
                0.0
            },
        }
    }

    /// Batch-submit (RUST-NATIVE HFT surface). Each order is risk-gated + registered independently
    /// — a denied order emits `OrderDenied` and drops from the batch, the rest still go — and the
    /// accepted orders reach the venue in ONE `client.submit_batch` call.
    #[tracing::instrument(level = "info", target = "vike_exec::oms", skip_all, fields(n = requests.len()))]
    pub fn submit_order_batch(
        &mut self,
        requests: &[OrderRequest],
        now_ms: i64,
        outbox: &mut Outbox,
    ) {
        let mut accepted: Vec<OrderRequest> = Vec::with_capacity(requests.len());
        for r in requests {
            if let Some(req) = self.gate_and_register(r, now_ms, outbox) {
                accepted.push(req);
            }
        }
        if !accepted.is_empty() {
            self.client.submit_batch(&accepted);
        }
    }

    /// A registered order still worth acting on (cancel/confirm-send gate) — the ONE set
    /// definition is [`OrderStatus::is_live`]; see its doc for why it is neither `!is_terminal()`
    /// (Liquidated) nor the FSM's `can_receive_cancel` allowed-from set.
    fn is_live(&self, client_order_id: &str) -> bool {
        self.registry.get(client_order_id).is_some_and(|mo| mo.status.is_live())
    }

    /// Cancel a live order by client-order-id. Idempotent: no-op if unknown or already
    /// terminal. Publishes NOTHING — the venue stream emits the authoritative OrderCanceled
    /// that advances the FSM (`on_event`).
    ///
    /// Says nothing about WHY, so the venue sees [`CancelIntent::Unspecified`] — the flatten-safe
    /// value, never held back. A caller that CAN classify uses
    /// [`ExecutionEngine::cancel_order_with_intent`] instead.
    pub fn cancel_order(&mut self, client_order_id: &str) {
        self.cancel_order_with_intent(client_order_id, CancelIntent::Unspecified);
    }

    /// [`ExecutionEngine::cancel_order`], saying WHY (see [`CancelIntent`]). Same idempotence, same
    /// terminal guard, same fire-and-forget contract — the intent only travels; nothing in this
    /// engine reads it. Only the venue client may act on it, and only ever by holding back a
    /// [`CancelIntent::Routine`] one.
    #[tracing::instrument(level = "info", target = "vike_exec::oms", skip_all, fields(coid = %client_order_id, intent = ?intent))]
    pub fn cancel_order_with_intent(&mut self, client_order_id: &str, intent: CancelIntent) {
        if self.is_live(client_order_id) {
            self.client.cancel_with_intent(client_order_id, intent);
        }
    }

    /// Cancel a batch of orders by client-order-id (RUST-NATIVE). Live orders only (the terminal
    /// guard mirrors `cancel_order`); the venue side is ONE `client.cancel_batch` call.
    /// Unclassified, exactly like [`ExecutionEngine::cancel_order`].
    pub fn cancel_order_batch(&mut self, client_order_ids: &[String]) {
        self.cancel_order_batch_with_intent(client_order_ids, CancelIntent::Unspecified);
    }

    /// [`ExecutionEngine::cancel_order_batch`], saying WHY (see [`CancelIntent`]).
    #[tracing::instrument(level = "info", target = "vike_exec::oms", skip_all, fields(n = client_order_ids.len(), intent = ?intent))]
    pub fn cancel_order_batch_with_intent(
        &mut self,
        client_order_ids: &[String],
        intent: CancelIntent,
    ) {
        let live: Vec<String> =
            client_order_ids.iter().filter(|c| self.is_live(c)).cloned().collect();
        if !live.is_empty() {
            self.client.cancel_batch_with_intent(&live, intent);
        }
    }

    /// Cancel ALL live orders on this engine (pull-all-quotes) in one `client.cancel_batch`.
    /// Unclassified — a strategy's own "pull my quotes" reaches here too, and that is NOT a
    /// declared risk-off action, so it takes the flatten-safe default rather than being labeled
    /// one. The emergency callers ([`CancelIntent::RiskOff`]: market-exit, dead-man, safe state)
    /// say so through [`ExecutionEngine::mass_cancel_with_intent`].
    pub fn mass_cancel(&mut self) {
        self.mass_cancel_with_intent(CancelIntent::Unspecified);
    }

    /// [`ExecutionEngine::mass_cancel`], saying WHY (see [`CancelIntent`]).
    pub fn mass_cancel_with_intent(&mut self, intent: CancelIntent) {
        let live: Vec<String> = self
            .registry
            .iter()
            .filter(|(_, mo)| mo.status.is_live())
            .map(|(coid, _)| coid.clone())
            .collect();
        if !live.is_empty() {
            self.client.cancel_batch_with_intent(&live, intent);
        }
    }

    /// Modify a live order's qty/price by client-order-id (RUST-NATIVE; no Python twin). Legal only
    /// while the order rests at the venue (ACCEPTED/TRIGGERED/PARTIALLY_FILLED) — a not-yet-accepted
    /// (SUBMITTED) or terminal order is a no-op. Publishes NOTHING — the venue stream emits the
    /// authoritative OrderModified that advances the FSM (`on_event`), mirroring cancel.
    /// ⚠ **RISK-GATED since the modify-bypass fix.** A modify that RAISES qty (or price) is
    /// economically a bigger order, so it is judged by the same [`RiskGate`] a submit of the
    /// resulting terms would face — against the PROJECTED order (the resting request with `new_qty`
    /// / `new_price` folded in), never against the delta. Before this, `gate.check` appeared exactly
    /// once in this file (the submit path) and `/modify <coid> qty=<huge>` walked straight past
    /// every ceiling `max_notional_per_order` is supposed to be: place a small in-cap order, then
    /// modify it up. A veto publishes a NON-TERMINAL [`Event::OrderModifyRejected`] — the order
    /// keeps its existing terms, exactly as when a venue refuses a modify — and nothing reaches the
    /// venue.
    ///
    /// Two deliberate departures from the submit path, both documented at their site:
    /// - **No throttle slot is consumed** ([`RiskGate::check_modify`]): the resting order already
    ///   paid one at submit, and re-charging every amend would make an amend-heavy maker
    ///   self-throttle out of quoting.
    /// - **The gate's ROUNDED request is not substituted onto the wire.** The venue modify takes
    ///   `(original order, new_qty, new_price)`, so the caller's values still go out verbatim on an
    ///   ACCEPTED modify — the verdict is used as a veto only, keeping accepted modifies
    ///   byte-identical to before this gate existed.
    ///
    /// ⚠ **THE ALREADY-EXECUTED PART OF A PARTIALLY FILLED ORDER IS NETTED OUT, AND WHETHER IT MAY
    /// BE IS A PER-VENUE FACT.** On an IN-PLACE amend venue the projected order's `qty` is the new
    /// TOTAL — executed lots included — while [`RiskContext::position_size`] already holds those very
    /// lots, so every `position + side × qty` projection counted them twice and refused amends that
    /// were economically no-ops (worst of all the re-price of a half-done exit under a halt). On a
    /// CANCEL-REPLACE venue the resting order dies and a FRESH order of `new_qty` takes its place, so
    /// the untouched sum is the CORRECT projection and netting there would ADMIT an order the gate
    /// should refuse. `AmendSemantics::already_in_position` returns a non-zero value for exactly one
    /// variant, so every venue whose convention is not established keeps the old, conservative
    /// arithmetic unchanged. `crates/vike-exec/src/risk.rs`'s `still_executable` states which lanes
    /// consume it, and states the `filled_qty`-is-inside-`position_size` invariant it rests on.
    ///
    /// ⚠ **THE CLIENT IS ASKED FIRST; THE VENUE STRING ANSWERS ONLY WHEN IT DECLINES.** Reading the
    /// venue table alone would be wrong, and the reason is not hypothetical: `vike_mount::make_engine`
    /// builds this engine with the REAL venue string even when the absent-credentials gate fell back
    /// to `vike_paper::PaperExecutionClient`, and `vike_run::build_paper_maker_core` does the same
    /// with its profile's venue — so a PAPER mount on binance/okx/bybit would be judged under
    /// `InPlaceTotal` while the paper book implements a THIRD convention (its `modify` assigns the
    /// amend's quantity to the order's REMAINING size, so the whole `new_qty` is still coming).
    /// [`ExecutionClient::amend_semantics`] returns `None` for every venue adapter — defer to the
    /// table, byte-identical — and `Some(AmendSemantics::InPlaceRemaining)` for the paper exchange,
    /// which nets nothing. `crates/vike-paper/tests/paper_amend_is_not_the_venues_amend.rs` drives
    /// this exact mount end to end.
    #[tracing::instrument(level = "info", target = "vike_exec::oms", skip_all, fields(coid = %client_order_id))]
    pub fn modify_order(
        &mut self,
        client_order_id: &str,
        new_qty: Option<f64>,
        new_price: Option<f64>,
        now_ms: i64,
        outbox: &mut Outbox,
    ) {
        // clone the resting request out of the registry (drops the borrow before &mut client) —
        // the venue modify gets the full order context (side, current qty/price) it may need.
        // `filled_qty` comes out in the same read: it is the order's own accumulated execution, and
        // on an in-place amend venue it is the part of the projected qty the position ALREADY holds
        // (see this fn's doc).
        let (req, filled_qty) = match self.registry.get(client_order_id) {
            Some(mo) if mo.status.is_modifiable() => (mo.request.clone(), mo.filled_qty),
            _ => return, // unknown, not-yet-accepted, or terminal — nothing to modify
        };
        // The order AS MODIFIED — the thing the gate must judge. An omitted field keeps the resting
        // value, which is exactly what the venue does with it.
        let mut projected = req.clone();
        if let Some(q) = new_qty {
            projected.qty = q;
        }
        if let Some(p) = new_price {
            projected.price = Some(p);
        }
        let ctx = self.risk_ctx(&projected, now_ms);
        // The amend convention decides whether the executed lots are inside `projected.qty`
        // (in-place TOTAL: net them out — they are already in `ctx.position_size`) or not
        // (cancel-replace / in-place REMAINING: net nothing — the whole `new_qty` can still execute
        // on top of the position).
        //
        // THE CLIENT IS ASKED FIRST. `self.venue` is the venue this engine is LABELLED with, which is
        // not the same thing as what `self.client` implements: every paper mount in this workspace
        // carries the real venue string (see this fn's doc), and the paper book's own convention is
        // neither of the venue ones. A client that declines (`None` — every venue adapter) falls
        // through to the venue table exactly as before.
        let already_executed = self
            .client
            .amend_semantics()
            .unwrap_or_else(|| vike_model::amend_semantics(&self.venue))
            .already_in_position(filled_qty);
        let verdict = self.gate.check_modify(&projected, &ctx, already_executed);
        if !verdict.ok {
            // Per-ORDER, fault-adjacent — off the measured hop, exactly like the submit veto.
            tracing::warn!(
                target: "vike_exec::risk",
                coid = %client_order_id,
                reason = %verdict.reason,
                "modify denied by RiskGate",
            );
            outbox.publish(Event::OrderModifyRejected(vike_model::events::OrderModifyRejected {
                client_order_id: client_order_id.to_string(),
                reason: format!("risk: {}", verdict.reason).into(),
                ts: now_ms,
            }));
            return;
        }
        self.client.modify(&req, new_qty, new_price);
    }

    /// ACTIVELY ask the venue to re-confirm one order's status by client-order-id (audit ex1 residual
    /// / A3; RUST-NATIVE, no Python twin). The core's stuck-order watchdog issues this during the
    /// confirm-grace so a truly-WEDGED adapter is PRODDED into answering rather than only waited on.
    /// Idempotent + fire-and-forget like `cancel_order`: only a still-LIVE order is worth confirming
    /// (terminal/unknown → no-op), and this publishes NOTHING — the venue's authoritative terminal
    /// (`OrderAccepted`/`OrderFilled`/`OrderRejected`) returns over the ingest lane. `client.confirm`
    /// re-queries on the adapter's OWN thread, so NO network ever touches the core fold. A no-op for
    /// clients without a re-query (the `ExecutionClient::confirm` default) — the watchdog's stage-2
    /// backstop still covers those.
    #[tracing::instrument(level = "info", target = "vike_exec::oms", skip_all, fields(coid = %client_order_id))]
    pub fn confirm_order(&mut self, client_order_id: &str) {
        if self.is_live(client_order_id) {
            self.client.confirm(client_order_id);
        }
    }

    /// Read-only drift audit (audit exec#2): DIFF the currently-folded [`Account`]/registry against
    /// the incoming venue-truth `snapshot`, returning one human-readable warning per divergence
    /// (position size, authoritative balance, or the open-order set) beyond [`drift_diverges`]'s
    /// tolerance. Empty = no drift. This ONLY detects — it never mutates; [`Self::apply_snapshot`]
    /// still overwrites local state with venue truth right after (venue wins the seed, unchanged).
    /// The vike-core runtime calls this immediately BEFORE `apply_snapshot` and pushes the returned
    /// lines into the GUI-visible recent-events ring + a `tracing::warn`, so the silent overwrite the
    /// audit flagged now raises a signal. It fires on every ReconcileSnapshot the core receives (the
    /// startup/session reconcile; and any reconnect resync wired to re-issue one) — no new wire verb
    /// / `Event` variant / periodic timer. Each message names venue, symbol, and the local-vs-venue
    /// values.
    pub fn diff_snapshot(&self, snapshot: &ReconcileSnapshot) -> Vec<String> {
        let mut warnings = Vec::new();
        let venue = &self.venue;

        // --- positions: each venue-truth (symbol, side) leg vs the locally-folded size ---
        let sides = &snapshot.position_sides;
        let mut seen: HashSet<(String, String)> = HashSet::new();
        for (i, (sym, venue_qty)) in snapshot.positions.iter().enumerate() {
            let side = sides.get(i).map(|s| s.1.as_str()).unwrap_or("BOTH");
            seen.insert((sym.clone(), side.to_string()));
            let local_qty = self
                .account
                .positions
                .get(&(
                    ustr::Ustr::from(venue.as_str()),
                    ustr::Ustr::from(sym.as_str()),
                    vike_model::events::PositionSide::from(side),
                ))
                .map(|p| p.size)
                .unwrap_or(0.0);
            if drift_diverges(local_qty, *venue_qty) {
                warnings.push(format!(
                    "DRIFT position {venue}/{sym}[{side}]: local {local_qty} vs venue {venue_qty}"
                ));
            }
        }
        // Local legs the venue snapshot never mentioned (venue implies flat) — a stranded local
        // ghost the seed would silently zero out. Skips legs already diffed above and sub-tolerance
        // dust; only THIS engine's venue.
        for ((v, sym, side), entry) in &self.account.positions {
            if v.as_str() == venue
                && entry.size.abs() > DRIFT_ABS_TOL
                && !seen.contains(&(sym.to_string(), side.to_string()))
            {
                warnings.push(format!(
                    "DRIFT position {venue}/{sym}[{side}]: local {} vs venue 0 (venue reports flat)",
                    entry.size
                ));
            }
        }

        // --- balance: only when the snapshot carries an authoritative (non-zero) balance, mirroring
        // apply_snapshot's own `snapshot.balance != 0.0` overwrite guard (a zero balance is "not
        // reported", not "flat", so it is never diffed) ---
        if snapshot.balance != 0.0 && drift_diverges(self.account.balance, snapshot.balance) {
            warnings.push(format!(
                "DRIFT balance {venue}: local {} vs venue {}",
                self.account.balance, snapshot.balance
            ));
        }

        // --- open orders: the set of live coids we track vs the venue's reported open-order set ---
        let venue_coids: HashSet<&str> =
            snapshot.open_orders.iter().map(|mo| mo.client_order_id()).collect();
        let local_live: HashSet<&str> = self
            .registry
            .iter()
            .filter(|(_, mo)| mo.status.is_live())
            .map(|(coid, _)| coid.as_str())
            .collect();
        let venue_only = venue_coids.difference(&local_live).count();
        let local_only = local_live.difference(&venue_coids).count();
        if venue_only > 0 || local_only > 0 {
            warnings.push(format!(
                "DRIFT open-orders {venue}: local {} live vs venue {} open \
                 ({venue_only} venue-only, {local_only} local-only)",
                local_live.len(),
                venue_coids.len()
            ));
        }

        warnings
    }

    /// Seed Account positions (size + avg_px per (symbol, side)) and the open-order registry
    /// from a reconcile snapshot. A net/spot snapshot (no position_sides) writes the
    /// (venue, sym, 'BOTH') key. Non-zero snapshot balance seeds cash so equity_now() =
    /// real_wallet_balance + unrealized instead of PnL-from-zero.
    pub fn apply_snapshot(&mut self, snapshot: &ReconcileSnapshot) {
        let sides = &snapshot.position_sides;
        // (sym, side) -> avg_px, parallel to positions
        let mut avg_by_key: IndexMap<(String, String), f64> = IndexMap::new();
        for (i, (sym, avg)) in snapshot.position_avg_px.iter().enumerate() {
            let side = sides.get(i).map(|s| s.1.as_str()).unwrap_or("BOTH");
            avg_by_key.insert((sym.clone(), side.to_string()), *avg);
        }
        for (i, (sym, qty)) in snapshot.positions.iter().enumerate() {
            assert!(
                self.accepts_symbol(sym),
                "snapshot symbol {sym} not accepted by engine (symbol {}, extra {:?})",
                self.symbol,
                self.extra_symbols
            );
            let side = sides.get(i).map(|s| s.1.as_str()).unwrap_or("BOTH");
            let avg = avg_by_key.get(&(sym.clone(), side.to_string())).copied().unwrap_or(0.0);
            // Interned/enum key (perf audit finding #3). Cold reconcile path, but the key TYPE is
            // shared with the fill fold, so it is built the same way — `PositionSide::from`
            // normalizes the venue's label, which every adapter already emits as BOTH/LONG/SHORT.
            let key: crate::account::PositionKey = (
                ustr::Ustr::from(self.venue.as_str()),
                ustr::Ustr::from(sym.as_str()),
                vike_model::events::PositionSide::from(side),
            );
            // Margin-mode carrier (step-2): when the snapshot carries a venue-REPORTED mode for
            // this row (`position_margin`, index-aligned like `position_sides`), venue truth WINS
            // the overwrite — including a Cross report flipping a stale local Isolated back to
            // Cross. When the venue reported nothing (empty/short vec — every pre-step-2 caller,
            // spot snapshots, deribit), the prior entry's carrier is carried forward (same law as
            // `Account::fold`, the #487 behavior) so the overwrite never silently flips an
            // isolated position back to cross. Cross/None writes are serialization no-ops on the
            // state-hash surface (`skip_serializing_if`), so all-cross accounts stay byte-identical.
            let prior = self.account.positions.get(&key).copied().unwrap_or_default();
            let (margin_mode, isolated_margin) = match snapshot.position_margin.get(i) {
                Some((_, m, iso)) => (*m, *iso),
                None => (prior.margin_mode, prior.isolated_margin),
            };
            self.account.positions.insert(
                key,
                crate::PositionEntry { size: *qty, avg_px: avg, margin_mode, isolated_margin },
            );
        }
        // A reconcile snapshot's `position_mark_px` IS the venue's own mark for the position —
        // just sampled at the reconcile cadence instead of streamed. It is therefore filed as a
        // genuine venue mark (`MarkSource::ReconcileMark`) and OWNS the account slot for the
        // RECONCILE window (`reconcile_staleness_ms`, default 150s — NOT the shorter streamed
        // `mark_staleness_ms`), on EVERY venue that reports one — including venues with no mark
        // stream, and including perps under `VIKE_MARK_STREAMS=0`. That is deliberate: it is the
        // most authoritative price those venues ever produce, and it is exactly the number the
        // venue values the position at. The window is sized to COVER the reconcile cadence
        // (default 60s), so ownership is continuous between passes — no per-minute alternation with
        // candle closes — yet still degrades: if reconcile STOPS for longer than the window,
        // closes/ticks reclaim the slot rather than pinning valuation to a frozen mark.
        for (sym, mark) in &snapshot.position_mark_px {
            if *mark > 0.0 {
                self.account.set_mark_from(
                    &self.venue,
                    sym,
                    *mark,
                    crate::MarkSource::ReconcileMark,
                    self.now_ms,
                );
                self.price_board.set_mark(&self.venue, sym, *mark, self.now_ms);
            }
        }
        for mo in &snapshot.open_orders {
            self.registry.insert(mo.client_order_id().to_string(), mo.clone());
        }
        // --- Reap stale local orders (continuous-reconcile terminalization) ---
        // Above this line `apply_snapshot` is INSERT-ONLY: it seeds venue-reported open orders but
        // never closes a local order the venue has STOPPED reporting. A lost cancel-ack would then
        // leave a phantom-live local order forever (the reconcile contract's second gap). Close it
        // here: any order WE still hold live whose client_order_id is absent from the venue's
        // open-order set is terminalized by synthesizing a local `OrderCanceled` and driving it
        // through the SAME FSM path the live fold uses (`on_event` → `ManagedOrder::apply` →
        // `on_order_event` capture + `persist_order`), so a mounted strategy learns of the cancel.
        // We never mutate registry state directly — the FSM stays the one authority. Contract:
        //   * Gated on registry-liveness + snapshot-absence ONLY. A synthesized cancel carries NO
        //     trade_id, so `seen_trade_ids`/`seen_fsm_trade_ids` stay untouched and the
        //     reconnect-resync fill-replay dedup (bridge-core user_data) is unaffected.
        //   * Idempotent: re-applying the same snapshot finds the order already terminal (not live),
        //     so it is no longer a candidate; and even if re-driven, the FSM drops the invalid
        //     transition — a no-op.
        //   * The cancel transition (Accepted/Triggered/PartiallyFilled/PendingCancel → Canceled)
        //     self-guards the pre-ack race: a still-`Submitted` local order is live but NOT
        //     cancelable, so `apply` drops it and the order survives until its real venue ack lands.
        //   * Only THIS engine's own venue is reaped; candidates are collected in `registry`
        //     (IndexMap) insertion order so `EngineSnapshot` round-trips stay byte-identical.
        //   * A RARE command path (never the per-event hot fold) — a warn per reaped order is fine.
        let venue_open: HashSet<&str> =
            snapshot.open_orders.iter().map(|mo| mo.client_order_id()).collect();
        let reap: Vec<String> = self
            .registry
            .iter()
            .filter(|(coid, mo)| {
                mo.request.venue == self.venue
                    && mo.status.is_live()
                    && !venue_open.contains(coid.as_str())
            })
            .map(|(coid, _)| coid.clone())
            .collect();
        for coid in reap {
            tracing::warn!(
                target: "vike_exec::reconcile",
                venue = %self.venue,
                coid = %coid,
                "reconcile reap: local order absent from venue open-orders — terminalizing (synthetic OrderCanceled)"
            );
            let ev = Event::OrderCanceled(vike_model::events::OrderCanceled {
                client_order_id: coid,
                reason: "reconcile-reap".into(),
                ts: self.now_ms,
            });
            // Route through the ONE FSM apply site; the throwaway outbox stays empty (a lifecycle
            // fold publishes nothing) — venue truth wins with NO new wire verb or Event variant.
            let mut outbox = Outbox::default();
            self.on_event(&ev, &mut outbox);
        }
        if snapshot.balance != 0.0 {
            self.account.balance = snapshot.balance;
            // A snapshot reseed is a balance sync just like `apply_account_state`, so move the
            // cash-reconcile realized-PnL baseline with it (expected == balance right after a
            // reseed). Without this, a stale baseline would make `diff_balance` double-count Σ
            // realized-PnL between the old baseline and this reseed and false-flag a `BalanceDrift`.
            // INERT unless Feature 2 (`VIKE_RECONCILE_BALANCE`) is on — the baseline is read ONLY by
            // `diff_balance`, and is not snapshotted, so this touches no journal/state-hash.
            self.account.realized_pnl_at_balance_sync = Some(self.account.realized_pnl);
        }
    }

    /// Operator-gated order-loss RECOVERY (recon `JournalDivergence` re-registration): INSERT-ONLY
    /// re-seed of venue-reported orders the live local registry has lost, each reconstructed from
    /// its `OrderStatusReport`. Mirrors [`Self::apply_snapshot`]'s open-order seed (an adopted
    /// `created_ms: None` order the stuck-order watchdog never sweeps) but WITHOUT the reap — it only
    /// ADDS the lost order back and never touches any other order. A coid ALREADY present in the
    /// registry is left untouched (idempotent, and never clobbers a fresher local state), and a
    /// report with no `client_order_id` is skipped. The venue report carries no limit/trigger price,
    /// so the re-registered order's `price`/`trigger_price` are `None` — it restores the order's
    /// EXISTENCE + status + filled qty + venue id for tracking/cancel, not its resting price.
    /// Returns the count actually re-registered. Only ever called from `confirm_recon` (an operator
    /// action), never the hot fold.
    pub fn reregister_orders(&mut self, reports: &[vike_model::OrderStatusReport]) -> usize {
        let mut n = 0;
        for r in reports {
            let Some(coid) = r.client_order_id.clone() else { continue };
            if self.registry.contains_key(&coid) {
                continue; // already known locally — never clobber
            }
            let request = OrderRequest {
                client_order_id: coid.clone(),
                venue: r.venue.clone(),
                symbol: r.symbol.clone(),
                side: r.side,
                qty: r.qty,
                order_type: r.order_type.clone(),
                ..Default::default()
            };
            let status =
                OrderStatus::parse(&r.status).unwrap_or(crate::order::OrderStatus::Accepted);
            self.registry.insert(
                coid,
                crate::order::ManagedOrder {
                    request,
                    status,
                    venue_order_id: (!r.venue_order_id.is_empty())
                        .then(|| r.venue_order_id.to_string()),
                    filled_qty: r.filled_qty,
                    avg_fill_px: r.avg_px,
                    created_ms: None, // adopted — never swept (matches apply_snapshot)
                },
            );
            n += 1;
        }
        n
    }

    /// Read-only OWNED snapshot of this engine's local state for the reconcile driver
    /// (`recon::diff` consumes it via [`crate::recon::OwnedLocalState::as_view`]). Clones the
    /// order registry and the fill-dedup trade-id set, and folds `account.positions` — filtered
    /// to THIS engine's venue — into the `(symbol, position_side) -> signed qty` shape
    /// `LocalView` expects. A pure read: never mutates the engine, so it does not touch the
    /// single-writer fold invariant — the runtime calls this on the fold thread and hands the
    /// owned result across to the reconcile driver's own thread.
    pub fn local_view(&self) -> crate::recon::OwnedLocalState {
        let positions = self
            .account
            .positions
            .iter()
            .filter(|((v, _sym, _side), _)| v.as_str() == self.venue)
            // `OwnedLocalState` is the RECONCILE-side DTO and stays `String`-keyed (it is compared
            // against venue-report strings); this cold, per-pass materialization is where the
            // interned fold keys are rendered back out.
            .map(|((_v, sym, side), entry)| ((sym.to_string(), side.to_string()), entry.size))
            .collect();
        crate::recon::OwnedLocalState {
            venue: self.venue.clone(),
            orders: self.registry.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
            seen_trade_ids: self.seen_trade_ids().map(str::to_string).collect(),
            positions,
            qty_tol: LOCAL_VIEW_QTY_TOL,
            // Cash slice for the first-class cash reconcile (`recon::diff_balance`). Read-only
            // snapshot of the account's cash state; inert unless `VIKE_RECONCILE_BALANCE` is on.
            cash: crate::recon::LocalCash {
                balance: self.account.balance,
                realized_pnl: self.account.realized_pnl,
                realized_at_sync: self.account.realized_pnl_at_balance_sync,
                mode: self.account.balance_mode,
            },
        }
    }

    /// Phase one of a teardown spanning SEVERAL engines: raise this engine's client stop flags and
    /// return at once, joining nothing. Forwards to [`ExecutionClient::begin_detach`], whose doc
    /// carries the whole cost model and the two rules an override must hold.
    ///
    /// Touches NO engine state — not the OMS registry, not the account, not `trading_state`. It is
    /// purely the client's stop signal hoisted out of [`Self::shutdown`] so a caller holding N
    /// engines can raise all N flags before it joins the first. Calling it is therefore optional:
    /// [`Self::shutdown`] alone is still a complete, correct teardown for a single engine, and a
    /// caller that skips this simply pays the serial cost.
    pub fn begin_shutdown(&mut self) {
        self.client.begin_detach();
    }

    /// Symmetric detach (the bus unsubscribe half is the core loop's ownership in Rust).
    ///
    /// A multi-engine caller should run [`Self::begin_shutdown`] over every engine FIRST — this
    /// method joins the client's threads, and joining one venue while the next has not yet been
    /// told to stop is what makes a teardown cost one wind-down per venue.
    pub fn shutdown(&mut self) {
        self.client.detach();
    }

    // --- internals ---

    /// True when this engine folds venue events for `symbol` — the primary symbol or any
    /// Phase D `extra_symbols` entry.
    pub fn accepts_symbol(&self, symbol: &str) -> bool {
        symbol == self.symbol || self.extra_symbols.iter().any(|s| s == symbol)
    }

    /// Combo gate 4 (the fill-drop hazard): coid-ownership routing for a bare [`Event::Fill`]
    /// whose symbol is NOT mounted. True iff the fill names an order THIS engine manages
    /// (registry hit on its `client_order_id`) AND carries position truth for that order — the
    /// order's own `request.symbol` (a single-leg order submitted on a never-mounted instrument,
    /// e.g. the deribit options-ticket class) or one of its `combo_legs` symbols (venue leg
    /// prints: the real per-leg position deltas). Without this, such a fill was silently
    /// symbol-filtered — order Filled via the coid-routed wrap, `Account` flat, position/PnL
    /// wrong.
    ///
    /// The combo-INSTRUMENT net print matches NEITHER arm (a combo request's `symbol` is EMPTY by
    /// `build_combo`'s contract, and the venue-minted combo id is not a leg) and deliberately
    /// stays OUT of the fold: Deribit reports one combo execution as N leg rows PLUS one
    /// aggregate net-price row under the combo instrument itself, and the venue books NO position
    /// under the combo id (captured live 2026-07-19 — `vike-deribit`'s
    /// `deribit_combo_fill_probe`: legs `BTC-20JUL26`/`BTC-25DEC26` at real leg prices with
    /// `combo_id` set, plus one `BTC-FS-25DEC26_20JUL26` row at the net 1199.0 with NO
    /// `combo_id`; positions after = per-leg only). Folding the net print would mint a phantom
    /// position at the net price on top of the real leg positions and double-count PnL.
    ///
    /// Hot-fold budget: mounted-symbol fills short-circuit at [`Self::accepts_symbol`] and never
    /// reach this; label-less fills (the account-wide stream's external-order case) return on the
    /// empty check without hashing; only LABELED foreign-symbol fills pay the one registry probe.
    /// No logging on any path (the p99 gate).
    fn owns_fill_symbol(&self, fill: &FillEvent) -> bool {
        if fill.client_order_id.is_empty() {
            return false;
        }
        let Some(mo) = self.registry.get(&fill.client_order_id) else {
            return false;
        };
        let sym = fill.symbol.as_str();
        sym == mo.request.symbol || mo.request.combo_legs.iter().any(|l| l.symbol == sym)
    }

    /// Refuse one money-lane event whose folded f64s were not all finite: count it, say so loudly,
    /// and answer [`Fold::Dropped`]. The ONE policy site for [`vike_model::FiniteNumbers`] —
    /// the predicate is pure and holds no policy, this decides what a violation DOES.
    ///
    /// **Why drop rather than halt.** Halting the core on one malformed frame hands any single
    /// venue a kill switch over every OTHER venue in the process — one `"NaN"` would stop trading
    /// account-wide, which is a strictly better attack than the poisoning it prevents. Dropping
    /// costs at most this one event, and the loss is RECOVERABLE by the mechanism already built for
    /// it: reconcile diffs venue truth against local state and raises the gap as `MissingFill` /
    /// `PositionDrift`. A folded NaN is recoverable by nothing — it propagates through
    /// `compute_fill` into the stored `avg_px`, and every later comparison against it is false, so
    /// reconcile cannot even SEE the divergence, let alone repair it.
    ///
    /// **Why ERROR, not warn.** Every other drop counter on this engine has a benign reading (a
    /// reconnect replay, a shared-account order, an out-of-order frame). This one does not: no
    /// venue legitimately sends a non-finite number, so a single occurrence is either a venue bug
    /// worth a page or a compromised socket, and both want an operator.
    ///
    /// `#[cold]` + `#[inline(never)]`: the caller keeps only a `jmp` on its rejection edge, so the
    /// per-message fold the `p99 < 10µs` gate measures carries none of this formatting.
    #[cold]
    #[inline(never)]
    fn refuse_nonfinite(&mut self, lane: &str, symbol: &str, coid: &str) -> Fold {
        self.dropped_nonfinite += 1;
        tracing::error!(
            target: "vike_exec::oms",
            lane,
            venue = %self.venue,
            symbol,
            coid,
            "REFUSED a venue event carrying a NON-FINITE number (NaN/inf) — no venue sends this; \
             the event was dropped so it cannot poison the ledger. Treat a nonzero \
             dropped_nonfinite as a venue fault or a compromised feed, and reconcile the venue."
        );
        Fold::Dropped
    }

    /// Signed size of one leg ('BOTH' = the one-way/spot leg; hedge callers pass LONG/SHORT).
    pub fn position_size(&self, position_side: &str) -> f64 {
        self.position_size_of(&self.symbol.clone(), position_side)
    }

    /// Per-symbol twin of [`ExecutionEngine::position_size`] (Phase D multi-mount).
    pub fn position_size_of(&self, symbol: &str, position_side: &str) -> f64 {
        self.account
            .positions
            .get(&(
                ustr::Ustr::from(self.venue.as_str()),
                ustr::Ustr::from(symbol),
                vike_model::events::PositionSide::from(position_side),
            ))
            .map(|p| p.size)
            .unwrap_or(0.0)
    }

    /// The signed position size the PRE-TRADE GATE's `RiskContext` carries — hedge-mode-aware,
    /// unlike a bare `position_size("BOTH")` (which sees 0 for an account whose venue reports
    /// hedge-mode `LONG`/`SHORT` buckets, so `is_covered_reduce`/`is_implicit_reduce` judged
    /// every hedge-mode close as an OPENING order and DENIED below-min flattens — defeating the
    /// #458 anti-stranding floor bypass).
    ///
    /// Law: the one-way `BOTH` bucket when it is non-zero (one-way accounts: byte-identical to
    /// before — hedge buckets are never consulted); otherwise the NET of the hedge buckets
    /// (`LONG + SHORT`, sizes signed as the venue recon parsers fold them: LONG ≥ 0, SHORT ≤ 0).
    /// NET is the honest one-number answer for a request that cannot name a bucket —
    /// [`OrderRequest`] carries no `position_side` field today, so which bucket a hedge-mode
    /// order reduces is the VENUE's routing decision; the net errs conservative (a both-buckets
    /// account nets toward 0, so coverage-gated bypasses stay off) while the common
    /// single-bucket hedge account gets exactly its bucket size. A flat one-way account with no
    /// hedge buckets folds `0.0 + 0.0` — same verdicts as the old bare 0.0.
    fn gate_position_size(&self, symbol: &str) -> f64 {
        let both = self.position_size_of(symbol, "BOTH");
        if both != 0.0 {
            return both;
        }
        self.position_size_of(symbol, "LONG") + self.position_size_of(symbol, "SHORT")
    }

    /// Gross notional across ALL legs of this engine's symbol at the current mark — SUMS abs()
    /// values, never nets LONG against SHORT. 0.0 if no mark recorded yet.
    pub fn total_exposure(&self) -> f64 {
        let mark = self.account.mark_of(&self.venue, &self.symbol).unwrap_or(0.0);
        // Python builtin sum() over a generator → py_sum, insertion order
        vike_model::py_sum(
            self.account
                .positions
                .iter()
                .filter(|((v, s, _ps), _)| *v == self.venue && *s == self.symbol)
                .map(|(_, pos)| pos.size.abs() * mark),
        )
    }

    /// Per-symbol latest recorded mark (Phase D multi-mount; pub for the runtime's per-mount ctx).
    pub fn mark_of(&self, symbol: &str) -> f64 {
        self.account.mark_of(&self.venue, symbol).unwrap_or(0.0)
    }

    /// Mode-aware equity with every open position priced through the PR-1 resolver
    /// (mark -> side-aware quote -> last -> bar-close -> Missing). Cold publish path only
    /// (`CoreSnapshot::build`); NOT the per-message fold. A `Missing` resolution contributes
    /// 0.0 unrealized — identical to today's `equity_all` silent-zero for an unmarked
    /// position — and is counted in `missing` for the GUI badge. Pure `&self` — warn-once
    /// tracking (`PriceBoard::note`) is wired by PR-3's cold sampler, not here.
    ///
    /// `unrealized_total`'s fold law and `equity`'s mode expression are copied VERBATIM from
    /// `Account::equity_all` (account.rs) so an all-`Missing` run (unrealized_total == 0.0 for
    /// every position) is bit-identical to `equity_all` — the parity guard for this PR.
    pub fn resolve_equity(
        &self,
        seed: f64,
        cfg: &crate::price_board::PriceCfg,
    ) -> crate::price_board::ResolvedEquity {
        use crate::price_board::{ResolvedEquity, ResolvedPosition};
        let mut per_position = Vec::with_capacity(self.account.positions.len());
        let mut missing = 0u32;
        for ((venue, symbol, position_side), entry) in self.account.positions.iter() {
            let (unrealized, mark_source) =
                self.resolve_position_unrealized(venue, symbol, *position_side, entry, cfg);
            if mark_source.is_none() {
                missing += 1;
            }
            per_position.push(ResolvedPosition { unrealized, mark_source });
        }
        // SAME fold law as Account::equity_all (account.rs): Python builtin sum() over the
        // positions generator -> Neumaier (py_sum), in insertion order.
        let unrealized_total = vike_model::py_sum(per_position.iter().map(|p| p.unrealized));
        let equity = self.mode_equity(seed, unrealized_total);
        ResolvedEquity { equity, unrealized_total, missing, per_position }
    }

    /// Scalar twin of [`Self::resolve_equity`] for the DECISION paths (the one-price law): the
    /// liquidation watchdog and the portfolio-snap journal read equity through THIS entry point, so
    /// an auto-liquidation acts on exactly the number the snapshot/sampler display. SAME resolver
    /// chain, per-position
    /// law ([`Self::resolve_position_unrealized`]), fold law (`py_sum` in `positions` insertion
    /// order — the lazily-mapped iterator feeds the identical value sequence, so the Neumaier
    /// fold is bit-identical to `resolve_equity`'s), and mode-aware seed expression
    /// ([`Self::mode_equity`]) — without the per-position Vec `resolve_equity` builds for
    /// display. Cold/per-order/per-bar cadence only (never the p99 message fold): no logging,
    /// no allocation. With an empty board (every position `Missing`) it is bit-identical to
    /// `Account::equity_all(seed)`.
    ///
    /// ⚠ **THIS IS THE UNCAPPED FIGURE, and that is deliberate — see [`Self::sizing_equity`].**
    /// Its two remaining callers are the ones that must judge against the account as it really is:
    /// `vike_core`'s `sweep_margin_call_engine`, where a smaller number LIQUIDATES, and the
    /// portfolio-snap journal, which is a record rather than a decision. Strategy `ctx.equity` and
    /// the armed margin gate MOVED to `sizing_equity`.
    pub fn resolved_equity(&self, seed: f64, cfg: &crate::price_board::PriceCfg) -> f64 {
        let unrealized_total =
            vike_model::py_sum(self.account.positions.iter().map(|((v, s, side), entry)| {
                self.resolve_position_unrealized(v, s, *side, entry, cfg).0
            }));
        self.mode_equity(seed, unrealized_total)
    }

    /// **THE one answer to "what equity may this engine size and admit against"** —
    /// [`Self::resolved_equity`] capped by [`crate::RiskLimits::max_sizing_equity`]. With no
    /// ceiling armed (`None`, the default and every deployment that wrote no `policy.toml` line)
    /// it is BIT-IDENTICAL to `resolved_equity`, so every existing verdict and every existing
    /// position size is byte-identical.
    ///
    /// # Why a second resolver rather than a cap inside the first
    ///
    /// Because the cap is not uniformly safe, and applying it to the wrong consumer is worse than
    /// not having it. Under [`crate::BalanceMode::Authoritative`] resolved equity is
    /// `venue wallet + unrealized`, and the wallet is the venue's number for the WHOLE account the
    /// credentials open — so a third party's deposit or withdrawal on a shared account moves it
    /// with nothing this daemon did (MEASURED on the CI box 2026-08-17). Capping it is:
    ///
    /// * **conservative** for anything that SPENDS against equity — a percent-of-equity sizer buys
    ///   fewer units, the pre-trade margin lane admits less — and
    /// * **destructive** for anything that judges SOLVENCY against it: the margin-call sweep reads
    ///   equity as the collateral backing open positions, so a capped figure makes a healthy
    ///   account look under-margined and it LIQUIDATES.
    ///
    /// One resolver could not serve both, and a cap threaded through call sites would be a rule
    /// nobody could check. Two named resolvers make each call site state which side it is on.
    ///
    /// # The consumers, and which side each is on
    ///
    /// **ACTING (they read this method, capped):**
    ///
    /// * the pre-trade gate's margin lane — this type's `risk_ctx`'s `equity` field, which
    ///   `crate::RiskGate`'s `check_inner` buying-power comparison judges against;
    /// * the COMBO gate's per-leg equity — `vike_core`'s `apply.rs` folds one account-level figure
    ///   for every leg, the combo twin of the line above;
    /// * every strategy context — `vike_core`'s `strategy_drive.rs` builds `LiveBroker::equity`,
    ///   which `LiveBroker::order_target_percent` multiplies through
    ///   `vike_model::units_from_percent` and which `vike-script` hands Rhai strategies as
    ///   `ctx.equity()`;
    /// * the `EventHandler::on_event` fold's per-fill `AppliedFill::equity_after` — the
    ///   strategy-facing equity a grep for `resolved_equity` does NOT find, because it is computed
    ///   from `Account::equity_all` instead. It is `Strategy::on_fill`'s `ctx.equity`, so a sizer
    ///   re-sizing inside a fill handler reads it; capping it through [`Self::cap_sizing_equity`]
    ///   is what keeps that one entry point from being the hole in every other one. ⚠ No count is
    ///   written here on purpose — `git grep -n 'equity:' -- crates/vike-core/src/runtime` plus
    ///   this site is the derivation, and a count is the shape of claim that rots.
    ///
    /// **REPORT-ONLY or SOLVENCY (they read [`Self::resolved_equity`], uncapped, deliberately):**
    ///
    /// * the margin-call sweep — the asymmetry above; it must see the real collateral;
    /// * the equity SAMPLER and the portfolio-snap journal (`vike_core`'s `timers.rs`) — an
    ///   observation of the account, and a record that read a ceiling instead of the account would
    ///   make an incident review read the operator's number back to them;
    /// * `CoreSnapshot`/`Portfolio` display and `vike-tradehub`'s book summary — same reason;
    /// * the DRAWDOWN latch reads neither: it moved to `capital_base + resolved_own_pnl` for a
    ///   related but distinct reason (`vike_core`'s `sweep_drawdown_latch` argues it), so this
    ///   ceiling is not the fix for that measure and does not touch it.
    ///
    /// Cold/per-order/per-bar cadence only, exactly like the method it wraps.
    pub fn sizing_equity(&self, seed: f64, cfg: &crate::price_board::PriceCfg) -> f64 {
        self.cap_sizing_equity(self.resolved_equity(seed, cfg))
    }

    /// Apply [`crate::RiskLimits::max_sizing_equity`] to an already-resolved equity figure — the
    /// ONE place the ceiling is applied, so [`Self::sizing_equity`] and the `equity_all`-derived
    /// `AppliedFill::equity_after` cannot answer differently about the same ceiling.
    ///
    /// `None` returns the figure verbatim (an `f64` untouched, not `min`'d against an infinity), so
    /// an unarmed deployment is bit-identical rather than merely numerically equal.
    #[inline]
    #[must_use]
    pub fn cap_sizing_equity(&self, equity: f64) -> f64 {
        // ⚠ **A comparison, deliberately NOT `f64::min`.** `min` treats NaN as the missing operand
        // and returns the OTHER one, so a poisoned equity — `Account::equity_all` goes NaN off a
        // single NaN mark slot, which `Account::set_mark_from` exists to guard — would come out of
        // here as the operator's ceiling: a finite, plausible number that the gate then ADMITS
        // against, where the NaN it replaced would have failed every comparison and denied. A cap
        // may only ever LOWER a real figure; laundering a poisoned one into a real one is the one
        // way this function could make a decision LESS safe. `NaN > cap` is false, so the
        // arm below returns the NaN untouched and every downstream guard still sees it.
        match self.gate.limits.max_sizing_equity {
            Some(cap) if equity > cap => cap,
            _ => equity,
        }
    }

    /// **This engine's OWN profit and loss** — resolver-priced, and the one equity-shaped scalar
    /// that carries NO account balance level in it at all. The sibling of [`Self::resolved_equity`]
    /// (same resolver chain, same per-position law, same `py_sum` fold in `positions` insertion
    /// order, same cold-path-only cadence), differing in exactly one thing: what it is denominated
    /// against.
    ///
    /// ```text
    /// resolved_equity  = seed + balance + realized − ... + unrealized   (Delta)
    ///                  = balance + unrealized                           (Authoritative)
    /// resolved_own_pnl = realized − fees_paid + funding_paid + unrealized  (BOTH modes)
    /// ```
    ///
    /// ⚠ **`balance` is the term that makes equity unusable as a risk measure on a live venue.**
    /// Under [`crate::BalanceMode::Authoritative`] it is the venue's attested wallet for the WHOLE
    /// ACCOUNT the credentials open ([`crate::Account::apply_account_state`] sets it ABSOLUTELY),
    /// so a third party depositing to or withdrawing from a SHARED account moves `resolved_equity`
    /// by the full amount while this daemon did nothing. Measured on the CI box 2026-08-17: one bybit
    /// block carrying 53647.10600813 of a shared demo wallet next to nine paper blocks at 1000
    /// seed each.
    ///
    /// Every term here is instead written ONLY by this engine's own activity —
    /// `Account::apply_fill` / `apply_liquidation` (via `Account::fold`) write `realized_pnl` and
    /// `fees_paid`, `apply_funding` writes `funding_paid`, and the positions the unrealized fold
    /// walks are this engine's. `apply_account_state` — the authoritative wallet adoption — writes
    /// `balance` / `balances_by_asset` / `balance_mode` / `realized_pnl_at_balance_sync` and
    /// touches NONE of them. So this scalar is mode-BLIND: it does not step when a venue frame
    /// flips the account from `Delta` to `Authoritative` mid-session, which an equity-based measure
    /// does.
    ///
    /// SIGNS: `fees_paid` is a signed cost (`> 0` paid, `< 0` maker rebate) and is SUBTRACTED;
    /// `funding_paid` is a signed cashflow (`> 0` received, `< 0` paid — see
    /// [`crate::Account::apply_funding`]) and is ADDED. On a `Delta` account this makes
    /// `seed + resolved_own_pnl` equal `resolved_equity` exactly, because `balance` there is
    /// precisely `−fees_paid + funding_paid` accumulated from the same events.
    ///
    /// ⚠ ONE DECLARED RESIDUAL: a forced-close fee (`Account::apply_liquidation`'s
    /// `balance -= ev.fee`) is netted into `balance` but is NOT accrued into `fees_paid`, so it is
    /// invisible here while it does move a `Delta` account's `resolved_equity`. That is the whole
    /// of the `Delta`-mode difference between the two, it is bounded by the liquidation fee, and
    /// it arrives beside a realized loss that IS counted. Closing it means widening `fees_paid`'s
    /// meaning, which is read by the GUI and by every report; not done here.
    pub fn resolved_own_pnl(&self, cfg: &crate::price_board::PriceCfg) -> f64 {
        let unrealized_total =
            vike_model::py_sum(self.account.positions.iter().map(|((v, s, side), entry)| {
                self.resolve_position_unrealized(v, s, *side, entry, cfg).0
            }));
        self.own_pnl(unrealized_total)
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
        cfg: &crate::price_board::PriceCfg,
    ) -> (f64, Option<crate::price_board::PriceSource>) {
        // Long/short for the resolver's side-aware quote choice comes from the SIGN of the
        // folded position size (the same convention `margin_call.rs` uses for the closing
        // side) — `position_side` is the position-bucket label (`Both`/`Long`/`Short`) that
        // `Account::unrealized_at` itself does not consult.
        let is_long = entry.size >= 0.0;
        match self.price_board.resolve(venue, symbol, is_long, self.now_ms, cfg) {
            // Stale (a last-known price, returned ONLY under `PriceCfg::stale_fallback`) is valued
            // exactly like Priced — a stale mark beats a silent zero — and its `source` still rides
            // through, so the position is NOT counted as missing.
            crate::price_board::Resolution::Priced { px, source, .. }
            | crate::price_board::Resolution::Stale { px, source, .. } => (
                self.account.unrealized_at(symbol, position_side, entry.size, entry.avg_px, px),
                Some(source),
            ),
            crate::price_board::Resolution::Missing => (0.0, None),
        }
    }

    /// Resolver-priced valuation price for ONE open position — the scalar the margin lane and
    /// the margin-call sweep share with [`Self::resolve_position_unrealized`]: the SAME chain
    /// (mark -> side-aware quote -> last -> bar-close) with the SAME long/short convention (the
    /// SIGN of the folded size picks Bid/Ask — conservative liquidation-side valuation).
    /// `None` = `Missing`, which every caller treats as the unpriceable skip `Account.marks`
    /// absence used to be. Cold / per-order path only.
    pub fn resolved_position_price(
        &self,
        venue: &str,
        symbol: &str,
        size: f64,
        cfg: &crate::price_board::PriceCfg,
    ) -> Option<f64> {
        match self.price_board.resolve(venue, symbol, size >= 0.0, self.now_ms, cfg) {
            crate::price_board::Resolution::Priced { px, .. }
            | crate::price_board::Resolution::Stale { px, .. } => Some(px),
            crate::price_board::Resolution::Missing => None,
        }
    }

    /// Resolver-priced margin-in-use — [`Account::margin_in_use_priced`] with the PR-1 resolver
    /// as the price input, so the margin side of every ratio/free-BP/liquidation comparison
    /// shares [`Self::resolved_equity`]'s price basis (the one-price law, extended to the
    /// margin numerator). The rate policy is untouched — callers pass the same closure they
    /// passed `margin_in_use_by`. A `Missing` resolution excludes the position exactly as an
    /// unmarked one was excluded. Cold / per-order path only.
    pub fn resolved_margin_in_use_by(
        &self,
        cfg: &crate::price_board::PriceCfg,
        rate_of: impl Fn(&crate::account::PositionKey, &crate::account::PositionEntry) -> Option<f64>,
    ) -> f64 {
        self.account.margin_in_use_priced(
            |(v, s, _side), p| self.resolved_position_price(v, s, p.size, cfg),
            rate_of,
        )
    }

    /// **What this ACCOUNT already has on that the gate is not about to re-project** — the producer
    /// of [`crate::RiskContext::account_exposure_excl_order`], and therefore the whole of what
    /// [`crate::RiskLimits::max_account_exposure`] knows about the book it is capping.
    ///
    /// TWO halves, because a ceiling that counted only one of them would not be a ceiling:
    ///
    /// 1. **OPEN POSITIONS**, gross, every symbol of this account except `exclude_symbol` — the
    ///    order's own, which the gate re-adds PROJECTED (see the ctx field's doc).
    /// 2. **LIVE, UN-FILLED ORDERS**, gross over each one's REMAINING quantity, every order of this
    ///    engine except `judging` — the one being judged, which the gate is projecting itself.
    ///
    /// ⚠ **The second half is not an embellishment; without it the ceiling is bypassable by an
    /// arbitrary multiple, and this file already records that exact incident one lane over.**
    /// [`Self::live_order_margin`] exists because counting positions alone overstated free buying
    /// power by every order in flight, so a second order was judged as though the first committed
    /// nothing. An exposure ceiling has the identical hole: a maker resting quotes on ten symbols
    /// would see the same pre-order account exposure ten times over and pass every check until the
    /// fills landed, with only the margin lane — a bound at EQUITY, not at the number the operator
    /// wrote — underneath it. Same registry walk, same skips, same reason.
    ///
    /// `Account::gross_notional_priced` is the position fold (same iteration order, same flat-skip,
    /// same unpriceable-skip, same multiplier term as `resolved_margin_in_use_by`'s margin twin),
    /// fed [`Self::resolved_position_price`] — so the account ceiling shares the ONE price basis the
    /// equity, margin and per-symbol exposure lanes already speak (the one-price law). It is a
    /// GROSS fold: a hedge-mode LONG/SHORT pair sums to both legs rather than to zero, because
    /// exposure is what the account is holding, not what it nets to.
    ///
    /// **Four things are skipped, and each skip is a classification rather than an omission:**
    ///
    /// * `exclude_symbol`'s POSITION — the ORDER's own symbol, which the gate re-adds PROJECTED.
    ///   Expressed as a `None` from the price closure, which is exactly the "caller declined to
    ///   price this position" arm `gross_notional_priced` already documents, rather than a second
    ///   fold with a filter in it.
    /// * **a FOREIGN-VENUE position row.** This engine's `Account` is one account of one venue, and
    ///   a row carrying another venue's id did not come from this account's own fills — it reached
    ///   the map through a reconcile report, and
    ///   `crates/vike-exec/tests/risk/risk_lane_completion.rs`'s
    ///   `max_total_exposure_is_scoped_to_one_venue_and_one_symbol` shows the same shape one lane
    ///   over. Summing it would be summing positions that do NOT share a wallet, which is precisely
    ///   the mis-reading this axis exists to avoid: an "account" ceiling that quietly aggregated a
    ///   foreign venue would refuse orders on collateral the venue never sees.
    /// * **the order being JUDGED** (`judging`, matched on client-order-id). On a submit it is not
    ///   in the registry at all; on an amend ([`Self::modify_order`], which judges the PROJECTED
    ///   request under the resting order's own coid) it is, and counting it here as well as in the
    ///   gate's projection would refuse an amend at a ceiling the identical order was admitted
    ///   under at submit — the same double count `still_executable` was written for.
    /// * **a COVERED REDUCE**, via `vike_model::is_covered_reduce`, the identical predicate the
    ///   floor and margin bypasses read: a resting order that shrinks an existing position adds no
    ///   exposure, and counting it would charge the account for its own exits.
    ///
    /// ⚠ **The judged SYMBOL's own resting orders ARE counted, while its POSITION is not.** That is
    /// not an inconsistency: the position is re-added projected and the resting orders are not
    /// projected by anything, so counting both is the only way to avoid double-counting the one and
    /// dropping the other. A resting order's fill lands on top of the position the gate projected.
    ///
    /// ⚠ **An UNPRICEABLE position or order contributes 0.0, which is the ANTI-conservative
    /// direction** — it understates the account and admits an order a fully-priced fold would
    /// refuse. Inherited from the margin fold rather than chosen, and kept consistent with it
    /// deliberately: a ceiling that priced a position the margin lane skipped would make the two
    /// answer differently about the same book. It is a DECLARED residual, not a safe default; the
    /// cure is a priced book, and [`Self::missing_marks`] is what names the positions that lack one.
    ///
    /// Cold path: per-order, off the `p99 < 10µs` per-message fold, and called ONLY when the
    /// ceiling is armed — no logging, no allocation.
    pub fn resolved_account_exposure_excluding(
        &self,
        exclude_symbol: &str,
        judging: &str,
        cfg: &crate::price_board::PriceCfg,
    ) -> f64 {
        let positions = self.account.gross_notional_priced(|(v, s, _side), p| {
            if s.as_str() == exclude_symbol || v.as_str() != self.venue.as_str() {
                return None;
            }
            self.resolved_position_price(v, s, p.size, cfg)
        });
        positions + self.live_order_notional(judging, cfg)
    }

    /// Σ gross notional of this engine's OWN **live, un-filled** orders — the in-flight half of
    /// [`Self::resolved_account_exposure_excluding`], and the exposure twin of
    /// [`Self::live_order_margin`].
    ///
    /// Every skip and every term is that function's, minus the initial-margin rate: the judged
    /// coid, a non-live status, a non-finite or non-positive remainder, a covered reduce and an
    /// unpriceable order are all passed over, and what survives contributes
    /// `remaining × price × multiplier` (`vike_model::gross_notional`, the same helper the gate's
    /// own combo accumulation calls). Keeping the two walks shaped alike is deliberate: they answer
    /// two questions about the same set of orders, and a divergence in WHICH orders they see would
    /// surface as the margin and exposure lanes disagreeing about a book neither is wrong about.
    fn live_order_notional(&self, judging: &str, cfg: &crate::price_board::PriceCfg) -> f64 {
        let mut open = 0.0;
        for (coid, mo) in self.registry.iter() {
            if coid.as_str() == judging || !mo.status.is_live() {
                continue;
            }
            let remaining = mo.request.qty - mo.filled_qty;
            // Non-finite or non-positive remainder commits nothing. Spelled explicitly rather than
            // as `!(remaining > 0.0)` so the NaN case is visible instead of implied — the margin
            // twin's own wording.
            if !remaining.is_finite() || remaining <= 0.0 {
                continue;
            }
            let sym = mo.request.symbol.as_str();
            if vike_model::is_covered_reduce(
                mo.request.reduce_only,
                mo.request.side,
                self.gate_position_size(sym),
                remaining,
            ) {
                continue;
            }
            let signed = mo.request.side as f64 * remaining;
            let Some(px) = self.resolved_position_price(&mo.request.venue, sym, signed, cfg) else {
                continue;
            };
            open += vike_model::gross_notional(remaining, px, self.account.multiplier_of(sym));
        }
        open
    }

    /// Σ initial margin of this engine's OWN **live, un-filled** order exposure — the commitment
    /// term `RiskContext::margin_used` was missing.
    ///
    /// ⚠ **The gate used to count open POSITIONS only, so free buying power was overstated by every
    /// order already in flight.** Submit two orders before the first fills and the second is judged
    /// as though the first committed nothing — the gate admits size the account cannot actually
    /// back. The BACKTEST has always counted its pending orders
    /// (`SimBroker::margin_in_use_pending_aware`), and that side's doc claimed it mirrored live
    /// "field for field"; it did not, and live was the permissive one.
    ///
    /// Mirrors `RiskGate::check_combo`'s `committed_margin` — the one place that already got this
    /// right — rather than adding a second commitment model.
    ///
    /// Every term matches the POSITION fold in `Account::margin_in_use_priced`
    /// (`|qty| * px * multiplier * im`), priced through the SAME `resolved_position_price` resolver
    /// and the SAME per-symbol `im_for` fallback, so the two halves of `margin_used` cannot drift
    /// apart on price basis or rate policy.
    ///
    /// ⚠ Skips reduce-shaped orders via `vike_model::is_covered_reduce`, the identical predicate the
    /// floor bypass uses: an order that shrinks an existing position commits no NEW margin, and
    /// charging it would deny flattens during a drawdown — the exact moment they matter most.
    /// Unpriceable orders contribute 0, matching the position fold's LEAN skip.
    ///
    /// Cold path: per-order, off the `p99 < 10µs` per-message fold.
    /// ⚠ `judging` is the client-order-id of the order being GATED, and excluding it is load-bearing
    /// rather than an optimisation. On a SUBMIT it is not in the registry yet, so the skip is inert.
    /// On an AMEND it IS — the gate is asking "can I afford to raise THIS order to N?", and charging
    /// its existing commitment while also judging its new size double-counts the same order.
    /// `partial_fill_amend_accounting.rs`'s `the_buying_power_lane_charges_only_what_can_still_execute`
    /// is the pin: it asserts an in-place amend that fits in free buying power is admitted, and the
    /// first version of this fold made it fail.
    fn live_order_margin(
        &self,
        cfg: &crate::price_board::PriceCfg,
        im_req: f64,
        judging: &str,
    ) -> f64 {
        let mut used = 0.0;
        for (coid, mo) in self.registry.iter() {
            if coid.as_str() == judging {
                continue;
            }
            if !mo.status.is_live() {
                continue;
            }
            let remaining = mo.request.qty - mo.filled_qty;
            // Non-finite or non-positive remainder commits nothing. Spelled explicitly rather than
            // as `!(remaining > 0.0)` so the NaN case is visible instead of implied.
            if !remaining.is_finite() || remaining <= 0.0 {
                continue;
            }
            let sym = mo.request.symbol.as_str();
            if vike_model::is_covered_reduce(
                mo.request.reduce_only,
                mo.request.side,
                self.gate_position_size(sym),
                remaining,
            ) {
                continue;
            }
            let signed = mo.request.side as f64 * remaining;
            let Some(px) = self.resolved_position_price(&mo.request.venue, sym, signed, cfg) else {
                continue;
            };
            let im = self.gate.limits.im_for(sym).unwrap_or(im_req);
            used += remaining * px * self.account.multiplier_of(sym) * im;
        }
        used
    }

    // --- Ext 2: mark-health tracking (which open positions can't be valued, and why) ----------

    /// Open positions this engine has NO usable price for — resolver-`Missing` even with the
    /// last-known floor (the cell is empty or every slot is dead). Each `(venue, symbol,
    /// position_side)` is one leg; FLAT legs (size 0) are excluded (a closed position leaves a
    /// zero-size entry behind, and an unpriceable position you no longer hold is not worth
    /// surfacing — the same rule the snapshot's `missing_price_instruments` uses). The
    /// operator/GUI answer to "which open positions can we not value at all right now". Read-only,
    /// no fold change; cold path only.
    ///
    /// Independent of [`crate::price_board::PriceCfg::stale_fallback`]: a position priced off a
    /// stale-but-present value is NOT missing — it is reported by [`Self::stale_marks`] — so the
    /// two sets never overlap.
    pub fn missing_marks(
        &self,
        cfg: &crate::price_board::PriceCfg,
    ) -> Vec<(String, String, String)> {
        self.account
            .positions
            .iter()
            .filter(|(_k, p)| p.size != 0.0)
            .filter(|((v, s, _side), p)| {
                matches!(
                    self.price_board.classify(v, s, p.size >= 0.0, self.now_ms, cfg),
                    crate::price_board::MarkStatus::Missing
                )
            })
            .map(|((v, s, side), _p)| (v.to_string(), s.to_string(), side.to_string()))
            .collect()
    }

    /// Open positions this engine is valuing off a STALE last-known price — priced, but every slot
    /// is past its freshness window (only possible once a caller sets a freshness window on `cfg`;
    /// empty under the permissive default). Each [`StaleMark`] carries the source and age so an
    /// operator sees "we're marking this off a 4-minute-old trade". Excludes FLAT legs, like
    /// [`Self::missing_marks`]. Independent of `stale_fallback` — staleness is reported whether or
    /// not valuation is configured to fall back to it. Read-only; cold path only.
    pub fn stale_marks(&self, cfg: &crate::price_board::PriceCfg) -> Vec<StaleMark> {
        self.account
            .positions
            .iter()
            .filter(|(_k, p)| p.size != 0.0)
            .filter_map(|((v, s, side), p)| {
                match self.price_board.classify(v, s, p.size >= 0.0, self.now_ms, cfg) {
                    crate::price_board::MarkStatus::Stale { source, age_ms, .. } => {
                        Some(StaleMark {
                            venue: v.to_string(),
                            symbol: s.to_string(),
                            position_side: side.to_string(),
                            source,
                            age_ms,
                        })
                    }
                    _ => None,
                }
            })
            .collect()
    }

    // --- Ext 3: net-exposure query helpers (resolver-priced, one-price law) --------------------

    /// SIGNED net notional exposure across this engine's open positions (long +, short −),
    /// resolver-priced through the SAME chain [`Self::resolved_equity`] uses (the one-price law):
    /// [`crate::Account::net_notional_priced`] with [`Self::resolved_position_price`] as the price
    /// input. An unpriceable position is excluded exactly as it is from equity/margin. Read-only;
    /// cold / per-order path only.
    pub fn net_exposure(&self, cfg: &crate::price_board::PriceCfg) -> f64 {
        self.account
            .net_notional_priced(|(v, s, _side), p| self.resolved_position_price(v, s, p.size, cfg))
    }

    /// GROSS notional exposure across this engine's open positions (Σ |notional|, never netting a
    /// long against a short), resolver-priced like [`Self::net_exposure`]. Read-only; cold path.
    pub fn gross_exposure(&self, cfg: &crate::price_board::PriceCfg) -> f64 {
        self.account.gross_notional_priced(|(v, s, _side), p| {
            self.resolved_position_price(v, s, p.size, cfg)
        })
    }

    /// Per-symbol `(symbol, net_qty, net_notional)` exposure across this engine's open positions,
    /// aggregating hedge legs of the same symbol. `net_qty` is the signed size sum (no price);
    /// `net_notional` is the signed resolver-priced notional — an unpriceable leg adds 0 to the
    /// notional while its qty still counts. Symbols in `positions` insertion order. Read-only; cold
    /// path only — the GUI's per-symbol exposure row.
    pub fn exposure_by_symbol(
        &self,
        cfg: &crate::price_board::PriceCfg,
    ) -> Vec<(String, f64, f64)> {
        let mut out: IndexMap<String, (f64, f64)> = IndexMap::new();
        for ((v, s, _side), p) in self.account.positions.iter() {
            if p.size == 0.0 {
                continue;
            }
            let e = out.entry(s.to_string()).or_insert((0.0, 0.0));
            e.0 += p.size;
            if let Some(px) = self.resolved_position_price(v, s, p.size, cfg) {
                e.1 += vike_model::signed_notional(p.size, px, self.account.multiplier_of(s));
            }
        }
        out.into_iter().map(|(s, (q, n))| (s, q, n)).collect()
    }

    /// The mode-aware seed expression copied VERBATIM from `Account::equity_all` (account.rs):
    /// delta = seed + balance + realized + unrealized; authoritative = balance + unrealized.
    #[inline]
    fn mode_equity(&self, seed: f64, unrealized_total: f64) -> f64 {
        if self.account.balance_mode == crate::BalanceMode::Authoritative {
            self.account.balance + unrealized_total
        } else {
            seed + self.account.balance + self.account.realized_pnl + unrealized_total
        }
    }

    /// The MODE-BLIND own-PnL expression [`Self::resolved_own_pnl`] documents — deliberately not
    /// branching on `balance_mode`, because no term in it is written by an authoritative balance
    /// sync. Kept beside [`Self::mode_equity`] so the two expressions are read together: the term
    /// this one omits (`seed + balance`) is exactly the account LEVEL, and the term order is fixed
    /// so `vike_core`'s snapshot-side twin (`Portfolio::pnl_total`, which folds the same four
    /// published `VenueBlock` fields) is bit-identical rather than merely close.
    #[inline]
    fn own_pnl(&self, unrealized_total: f64) -> f64 {
        self.account.realized_pnl - self.account.fees_paid
            + self.account.funding_paid
            + unrealized_total
    }

    /// Most-recent live order on this symbol/side — the leg a liquidation force-closed.
    /// One-way (BOTH) keys by symbol only; hedge (LONG/SHORT) also requires the order's leg
    /// (from request.side: +1 → LONG, -1 → SHORT) to match. Reversed insertion-order scan,
    /// first non-terminal wins. None if nothing matches (Account still flattens by key).
    fn coid_for_position(&self, ev: &PositionLiquidated) -> Option<String> {
        let want_side = match ev.position_side {
            vike_model::events::PositionSide::Long => Some("LONG"),
            vike_model::events::PositionSide::Short => Some("SHORT"),
            _ => None,
        };
        for (coid, mo) in self.registry.iter().rev() {
            // ⚠ POSITIVE selection on the FSM's OWN allowed-from set, not a negation of three
            // statuses. The skip set used to be {Liquidated, Filled, Canceled}, which ADMITS
            // SUBMITTED, PENDING_CANCEL, REJECTED, DENIED and EXPIRED — every one of which
            // `order.rs`'s `transition` refuses for `OrderLiquidated`. Since this scan is
            // newest-first, a still-SUBMITTED order shadowed the ACCEPTED one that was actually
            // force-closed: the pick landed on an order `apply` then rejected, and the real leg
            // kept its old status. Selecting on `can_receive_liquidation` makes the picker and the
            // applier the same predicate by construction, so they cannot drift.
            if mo.request.symbol != ev.symbol || !mo.status.can_receive_liquidation() {
                continue;
            }
            if let Some(want) = want_side {
                let order_leg = if mo.request.side > 0 { "LONG" } else { "SHORT" };
                if order_leg != want {
                    continue;
                }
            }
            return Some(coid.clone());
        }
        None
    }

    fn lifecycle_coid(event: &Event) -> Option<&str> {
        // Python `_LIFECYCLE` tuple — note OrderLiquidated is NOT lifecycle-dispatched
        // (it is applied via the PositionLiquidated branch).
        match event {
            Event::OrderSubmitted(e) => Some(&e.client_order_id),
            Event::OrderAccepted(e) => Some(&e.client_order_id),
            Event::OrderTriggered(e) => Some(&e.client_order_id),
            Event::OrderPartiallyFilled(e) => Some(&e.client_order_id),
            Event::OrderFilled(e) => Some(&e.client_order_id),
            Event::OrderCanceled(e) => Some(&e.client_order_id),
            Event::OrderRejected(e) => Some(&e.client_order_id),
            Event::OrderExpired(e) => Some(&e.client_order_id),
            Event::OrderDenied(e) => Some(&e.client_order_id),
            // RUST-NATIVE modify — folds via `mo.apply` (self-transition) + persist_order, exactly
            // like the other lifecycle events.
            Event::OrderModified(e) => Some(&e.client_order_id),
            // RUST-NATIVE cancel/modify-reject advisories — dispatched to `mo.apply` (a
            // non-terminal self-transition) + persist_order, so the failed intent is journaled.
            Event::OrderCancelRejected(e) => Some(&e.client_order_id),
            Event::OrderModifyRejected(e) => Some(&e.client_order_id),
            _ => None,
        }
    }

    /// Capture the full OMS state (journal `Snap` record). Dedup sets are SORTED (canonical).
    pub fn snapshot_state(&self) -> crate::engine_snapshot::EngineSnapshot {
        // The fill ledger lives on `Account` and hands out `&str`s (its stored fingerprints are an
        // implementation detail, not journal state), so it needs its own canonicalizer. Same
        // sort-into-a-Vec as `sorted` below — the wire shape (`Vec<String>`, ascending) and therefore
        // the `state_hash` fence are unchanged by the move.
        let sorted_ids = |ids: &mut dyn Iterator<Item = &str>| {
            let mut v: Vec<String> = ids.map(str::to_string).collect();
            v.sort_unstable();
            v
        };
        let sorted = |s: &std::collections::HashSet<String>| {
            let mut v: Vec<String> = s.iter().cloned().collect();
            v.sort_unstable();
            v
        };
        crate::engine_snapshot::EngineSnapshot {
            venue: self.venue.clone(),
            symbol: self.symbol.clone(),
            quote_asset: self.quote_asset.clone(),
            reduce_only_on_close: self.reduce_only_on_close,
            extra_symbols: self.extra_symbols.clone(),
            trading_state: self.trading_state,
            limits: self.gate.limits.clone(),
            gate_order_times: self.gate.throttle_times(),
            registry: self.registry.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
            account: self.account.snapshot(),
            seen_trade_ids: sorted_ids(&mut self.seen_trade_ids()),
            seen_fsm_trade_ids: sorted(&self.seen_fsm_trade_ids),
            seen_liq_ids: sorted(&self.seen_liq_ids),
            dropped_terminal_on_live: self.dropped_terminal_on_live,
            equity_seed: self.equity_seed,
            collect_applied_fills: self.collect_applied_fills,
            now_ms: self.now_ms,
        }
    }

    /// Rebuild an engine from a snapshot (restart/replay). `applied_fills` starts empty
    /// (deliveries are not replayed).
    pub fn from_snapshot(snap: &crate::engine_snapshot::EngineSnapshot, client: C) -> Self {
        let mut gate = RiskGate::new(snap.limits.clone());
        gate.set_throttle_times(snap.gate_order_times.clone());
        let mut e = ExecutionEngine::new(
            crate::Account::restore(&snap.account),
            gate,
            client,
            &snap.venue,
            &snap.symbol,
        );
        e.quote_asset = snap.quote_asset.clone();
        e.reduce_only_on_close = snap.reduce_only_on_close;
        e.extra_symbols = snap.extra_symbols.clone();
        e.trading_state = snap.trading_state;
        e.registry = snap.registry.iter().cloned().collect();
        e.seed_seen_trade_ids(snap.seen_trade_ids.iter().cloned());
        e.seen_fsm_trade_ids = snap.seen_fsm_trade_ids.iter().cloned().collect();
        e.seen_liq_ids = snap.seen_liq_ids.iter().cloned().collect();
        e.dropped_terminal_on_live = snap.dropped_terminal_on_live;
        e.equity_seed = snap.equity_seed;
        e.collect_applied_fills = snap.collect_applied_fills;
        e.now_ms = snap.now_ms;
        // Re-seed the read-side `price_board` from the restored `Account.marks` (which
        // `Account::restore` just rebuilt above). `PriceBoard` sits OUTSIDE `EngineSnapshot` (the
        // journal hash-fence), so a bare restore leaves it EMPTY while `Account.marks` is fresh —
        // the two mark stores would then DISAGREE until the next tick, and
        // `resolved_position_price` would return `Missing`/0.0 for a symbol whose restored mark is
        // actually present (a gap the resolver-routed gate makes load-bearing). Mirror exactly how
        // `apply_snapshot` keeps the two stores consistent — it pairs each `set_mark_from` with a
        // `price_board.set_mark`; here the account side is already restored, so we only add the
        // board side. Stamped with the restored core clock `snap.now_ms`, like `apply_snapshot`'s
        // own `self.now_ms` write; the board's own dead-price guard drops any non-positive mark.
        // The board is not serialized and marks are excluded from `state_hash`, so this is inert to
        // the determinism fence.
        for ((venue, symbol), mark) in &snap.account.marks {
            e.price_board.set_mark(venue, symbol, *mark, e.now_ms);
        }
        e
    }
}

/// A terminal lifecycle event: one that (in a legal transition) closes the order. Used by the
/// C1 observability check to tell a genuinely-lost terminal from a benign non-terminal replay.
fn is_terminal_event(event: &Event) -> bool {
    matches!(
        event,
        Event::OrderFilled(_)
            | Event::OrderCanceled(_)
            | Event::OrderRejected(_)
            | Event::OrderExpired(_)
            | Event::OrderDenied(_)
    )
}

/// A venue LIVENESS or EXECUTION event: the venue asserting the order is working (`OrderAccepted`)
/// or has executed (`OrderPartiallyFilled`/`OrderFilled`). Distinct from [`is_terminal_event`]:
/// `OrderAccepted`/`OrderPartiallyFilled` are non-terminal, but all three prove the venue considered
/// the order LIVE — so one arriving (and failing to apply) on an already-terminalized order is the
/// stranded-position signal the confirm-race hardening counts.
fn is_liveness_or_fill_event(event: &Event) -> bool {
    matches!(
        event,
        Event::OrderAccepted(_) | Event::OrderPartiallyFilled(_) | Event::OrderFilled(_)
    )
}

/// A KILL terminal: an order closed WITHOUT completing (venue/gate rejected, canceled, expired, or
/// denied it). Excludes `Filled` (legitimate completion — a duplicate fill is benign, not a strand)
/// and `Liquidated` (a live perp force-close, not `is_terminal`). A venue liveness/fill event landing
/// on one of these is a contradiction: the order the venue is executing is one we believe we killed.
fn is_kill_terminal(status: OrderStatus) -> bool {
    matches!(
        status,
        OrderStatus::Rejected | OrderStatus::Canceled | OrderStatus::Expired | OrderStatus::Denied
    )
}

impl<C: ExecutionClient> EventHandler for ExecutionEngine<C> {
    fn on_event(&mut self, event: &Event, _outbox: &mut Outbox) -> Fold {
        if let Event::Fill(fill) = event {
            if !self.accepts_symbol(&fill.symbol) && !self.owns_fill_symbol(fill) {
                // account-wide WS stream: not this engine's order — OR our own combo's aggregate
                // net-price print, which must never fold (see `owns_fill_symbol`).
                return Fold::Dropped;
            }
            // HOSTILE-VENUE GUARD — the money lane's finiteness check, placed BEFORE the dedup
            // insert so a rejected fill does not burn its `trade_id` (the genuine retransmission of
            // a well-formed fill with the same id must still fold). See `vike_model::FiniteNumbers`
            // for how a `"NaN"` string reaches a typed f64 through the ordinary mapper path, and why
            // folding one is IRRECOVERABLE rather than merely wrong.
            if !fill.numbers_finite() {
                return self.refuse_nonfinite("Fill", &fill.symbol, &fill.client_order_id);
            }
            // In-memory dedup: always-on guard against WS reconnect replays (Fix 1).
            //
            // ⚠ THE CHECK IS NOT HERE ANY MORE — it is inside `Account::apply_fill`, on the aggregate
            // that owns the money, and this call site merely OBSERVES its verdict. That is the whole
            // point: this engine is one of a growing number of paths that can deliver an
            // already-folded fill (`exec_actor::run_loop`'s history resync, `run_resync_supervisor`'s
            // replay, hyperliquid's per-reconnect `userFills` snapshot), and a guard sitting in front
            // of the mutation only protects the callers that remember it. Deleting these three lines
            // no longer double-counts anything; it only makes this engine stop distinguishing a
            // replay from a fold, which the tests below pin.
            //
            // ⚠ And the guard is TOTAL, with no emptiness escape hatch, because `fill.trade_id` is a
            // `TradeId` that cannot be empty (#1341). It used to be wrapped in
            // `if !fill.trade_id.is_empty()`, so a fill whose id was `""` skipped the guard and was
            // applied UNCONDITIONALLY — every reconnect replay of it re-booked commission and realized
            // PnL, and five venue mappers reached the field through `unwrap_or_default()` and so could
            // produce exactly that. Unconditional because the type makes the empty case
            // unrepresentable, not because the case was judged impossible. That property is what makes
            // moving the check DOWN safe: the aggregate can key on the id without asking whether it
            // has one.
            if self.account.apply_fill(fill) == crate::account::FillFold::Duplicate {
                return Fold::Dropped; // reconnect replay — the account refused it, nothing moved
            }
            if let Some(mp) = fill.mark_price {
                // `mark_price` is deliberately NOT part of `numbers_finite` (a decorative field must
                // not discard a money event) — it is guarded at `Account::set_mark_from`, THE single
                // writer of the mark slot. ⚠ `mp > 0.0` alone does NOT screen it: it excludes NaN
                // and -inf, but `+inf > 0.0` is TRUE.
                if mp > 0.0 {
                    // `fill.mark_price` is the venue's mark at fill time — a genuine venue mark.
                    // Aged on the CORE clock (`now_ms`), not `fill.ts`: see `set_mark_from`.
                    self.account.set_mark_from(
                        &fill.venue,
                        &fill.symbol,
                        mp,
                        crate::MarkSource::VenueMark,
                        self.now_ms,
                    );
                    self.price_board.set_mark(&fill.venue, &fill.symbol, mp, fill.ts);
                }
            }
            if self.collect_applied_fills {
                // Snapshot the state HERE, per fill — a multi-fill batch must deliver each
                // `on_fill` with the position/equity after THAT fill, exactly matching the
                // backtest engine's synchronous firing point (fold → fire, one at a time).
                self.applied_fills.push(AppliedFill {
                    // Keyed on the FILL's symbol, not the engine's primary. `position_size`
                    // delegates to `self.symbol` (see its body), so on an engine carrying
                    // `extra_symbols` an ETHUSDT fill reported the BTCUSDT position into the
                    // strategy's `on_fill` — a wrong number, silently, for the symbol the
                    // handler is being told about. Byte-identical on a single-symbol engine,
                    // where `fill.symbol == self.symbol` for every fill it folds.
                    //
                    // The `"BOTH"` bucket is left as-is on purpose: reading `fill.position_side`
                    // here would additionally change hedge-mode behaviour, which is a separate
                    // concern from the symbol and does not belong in this diff.
                    position_after: self.position_size_of(&fill.symbol, "BOTH"),
                    // ⚠ CAPPED, and this is the site three review rounds on the abandoned
                    // predecessor each missed: it is a strategy-facing equity — the `ctx.equity` a
                    // `Strategy::on_fill` handler sizes its next order from — computed from
                    // `Account::equity_all` rather than from `resolved_equity`, so a grep for the
                    // obvious symbol does not find it. Left uncapped it would be the one hole in a
                    // ceiling every other site honours. `Self::cap_sizing_equity` is the shared
                    // comparison, so this and `Self::sizing_equity` cannot disagree; with no ceiling
                    // armed it is the untouched `f64` this line always produced.
                    equity_after: self.cap_sizing_equity(self.account.equity_all(self.equity_seed)),
                    fill: fill.clone(),
                });
            }
            return Fold::Applied;
        }
        if let Some(coid) = Self::lifecycle_coid(event) {
            let coid = coid.to_string();
            if !self.registry.contains_key(&coid) {
                // Audit C2: a lifecycle event for an order the engine doesn't know (pre-restart
                // order absent from reconcile, an external-account order, or a bug). Still dropped —
                // but counted + logged at debug so it is not INVISIBLE. Debug (not warn) because a
                // shared exchange account legitimately streams other sources' order updates.
                self.dropped_unknown_coid += 1;
                tracing::debug!(coid = %coid, "lifecycle event for unknown order dropped");
                return Fold::Dropped;
            }
            // FSM-side fill dedup: a reconnect-replayed fill re-emits the wrap. The bare
            // FillEvent was deduped via seen_trade_ids (Account stays correct), but the wrap
            // would re-run accumulate_fill and double-count filled_qty/avg_fill_px. Dedup by
            // the fill's trade_id with a SEPARATE set — seen_trade_ids was already consumed
            // by the preceding bare FillEvent, so it cannot be reused here.
            // `Option<&TradeId>` rather than a `String` whose emptiness meant two different things.
            // The old `_ => String::new()` + `!tid.is_empty()` pair conflated "this lifecycle event
            // carries no fill at all" (Accepted/Canceled/…) with "this fill's id was empty" — so the
            // guard that legitimately skips the former ALSO waved the latter straight through, and a
            // replayed empty-id wrap re-ran `accumulate_fill`, double-counting `filled_qty` and
            // `avg_fill_px`. `None` now means only the first, and `Some` is always a real id.
            let tid: Option<&vike_model::events::TradeId> = match event {
                Event::OrderPartiallyFilled(w) => Some(&w.fill.trade_id),
                Event::OrderFilled(w) => Some(&w.fill.trade_id),
                _ => None,
            };
            if let Some(tid) = tid
                && self.seen_fsm_trade_ids.contains(tid.as_str())
            {
                return Fold::Dropped; // reconnect replay — the FSM already advanced for this fill
            }
            // HOSTILE-VENUE GUARD, the FSM side: a fill WRAP carries its own embedded `FillEvent`,
            // whose qty/px `ManagedOrder::accumulate_fill` folds into `filled_qty`/`avg_fill_px`.
            // That is a second, independent path to a poisoned number — the bare `Event::Fill` above
            // is the Account's copy, this is the order's — and a NaN `avg_fill_px` makes every later
            // `remaining_qty` comparison false, so the order never completes.
            let wrap_fill = match event {
                Event::OrderPartiallyFilled(w) => Some(&w.fill),
                Event::OrderFilled(w) => Some(&w.fill),
                _ => None,
            };
            if let Some(f) = wrap_fill
                && !f.numbers_finite()
            {
                return self.refuse_nonfinite("fill wrap", &f.symbol, &coid);
            }
            let Some(mo) = self.registry.get_mut(&coid) else {
                return Fold::Dropped; // unreachable (checked above) — but never panic in the fold
            };
            // --- Cancel-vs-fill race guard (LEAN `CancelPendingOrders` semantics, engine-level) ---
            // A locally-issued cancel never mutates status (`cancel_order` is fire-and-forget), so
            // for local cancels the pre-cancel status IS the snapshot: fills-before-ack apply and a
            // cancel-reject leaves the order in its live state — nothing to restore. But an order
            // can sit at PENDING_CANCEL via venue-status seeding (`reregister_orders` / the
            // journal-view path parsing the venue's own PENDING_CANCEL string), and the r5
            // fixture-pinned FSM table rejects fills from that state and leaves a cancel-reject
            // inert — venue truth (the order is executing / still live) would be dropped, stranding
            // the order at PENDING_CANCEL until a reconcile reap mislabels a FILLED order CANCELED.
            // Resolve the race HERE, before the one `apply` site: restore the pre-cancel live
            // status (recomputed from the folded fill stream — see
            // `ManagedOrder::resolve_pending_cancel`) so the authoritative event applies through
            // the UNCHANGED table. Only the three events that prove the cancel lost or died resolve
            // it: a fill wrap or the venue's cancel-reject. `OrderCanceled` deliberately does NOT
            // (PENDING_CANCEL → CANCELED is already the legal ack edge), and everything else (e.g.
            // a stray OrderAccepted replay) keeps today's drop. Fold-deterministic — no new state,
            // journal replay and the EngineSnapshot state-hash are byte-identical.
            if matches!(
                event,
                Event::OrderPartiallyFilled(_)
                    | Event::OrderFilled(_)
                    | Event::OrderCancelRejected(_)
            ) && mo.resolve_pending_cancel()
            {
                // Per-ORDER fault-adjacent boundary (the race is rare) — never per-message.
                tracing::warn!(
                    target: "vike_exec::oms",
                    coid = %coid,
                    restored = mo.status.as_str(),
                    "cancel-vs-fill race: PENDING_CANCEL order restored to its pre-cancel status so the venue event applies"
                );
            }
            let apply_result = mo.apply(event);
            let status_after = mo.status; // Copy — releases the &mut mo borrow for the counter below
            if apply_result.is_err() {
                // Audit C1: apply rejected the event. Normally a benign idempotent/out-of-order WS
                // replay — but a TERMINAL event failing on a still-LIVE order is a genuinely-lost
                // terminal (e.g. OrderCanceled arriving before OrderAccepted): the order can stay
                // live forever. Count + warn on THAT case only; keep replays silent. (Recovery
                // needs reconcile — this only makes the loss observable.)
                if is_terminal_event(event) && !status_after.is_terminal() {
                    self.dropped_terminal_on_live += 1;
                    tracing::warn!(
                        coid = %coid,
                        status = status_after.as_str(),
                        "terminal event dropped on a live order (out-of-order); order may be stranded"
                    );
                } else if is_liveness_or_fill_event(event) && is_kill_terminal(status_after) {
                    // Confirm-race hardening: a venue liveness/execution event (Accepted/PartiallyFilled/
                    // Filled) dropped onto an order we ALREADY terminalized via a kill path — the venue
                    // says this order is alive/filling while our FSM killed it (the watchdog stage-2
                    // phantom-reject signature). `status_after.is_terminal()` is TRUE here, so the C1
                    // branch above cannot see it; without this the clobber is INVISIBLE and a live
                    // position can be stranded silently. Count + warn (naming the coid). The FSM
                    // legality is unchanged — the event is still dropped, just no longer in silence.
                    self.stranded_terminal_drops += 1;
                    tracing::warn!(
                        coid = %coid,
                        status = status_after.as_str(),
                        "venue liveness/fill event dropped onto an already-terminalized order; a live position may be STRANDED (premature terminal, e.g. watchdog phantom-reject)"
                    );
                }
                return Fold::Dropped; // idempotent/out-of-order WS replay — skip (Fix 2)
            }
            if let Some(tid) = tid {
                // mark seen only after a successful apply. `Some` ⇒ a real, non-empty id (see the
                // `Option<&TradeId>` note at the guard above); a non-fill lifecycle event is `None`
                // and inserts nothing, which is the only case the old `!tid.is_empty()` meant to skip.
                self.seen_fsm_trade_ids.insert(tid.to_string());
            }
            // Capture the NON-FILL lifecycle transition (accept/reject/cancel/expire) for
            // `Strategy::on_order_event`. `from_event` returns None for FILLS — those flow the
            // `applied_fills`/`on_fill` lane above — so a fill is never double-delivered here.
            // (venue, symbol) come from the order we just advanced (still in the registry). Gated on
            // the same mount flag as `applied_fills`; a DENIED order is captured earlier at the
            // RiskGate veto site (it never enters the registry, so it can't be seen here).
            if self.collect_applied_fills
                && let Some(lc) = vike_model::strategy::OrderLifecycle::from_event(event)
                && let Some((venue, symbol)) = self
                    .registry
                    .get(&coid)
                    .map(|mo| (mo.request.venue.clone(), mo.request.symbol.clone()))
            {
                self.order_events.push(OrderEventOut { venue, symbol, event: lc });
            }
            return Fold::Applied;
        }
        if let Event::Funding(ev) = event {
            if !self.accepts_symbol(&ev.symbol) {
                return Fold::Dropped;
            }
            // …and the ROUTING backstop, the twin of the `AccountState` arm below (whose comment
            // carries the full argument). A stamped key names ONE account of this exchange; if it
            // is not this engine's, this engine must not fold it. Load-bearing rather than
            // defensive: `accepts_symbol` above is TRUE on both engines exactly when two accounts
            // trade one instrument, which is the configuration where a funding debit belonging to
            // one account would otherwise land on the other's `balance`. An unstamped payload
            // (`None` — every payload on every single-account box) skips this and folds as it
            // always did.
            if let Some(key) = ev.route_key
                && key.as_str() != self.route_key
            {
                return Fold::Dropped;
            }
            // HOSTILE-VENUE GUARD: `amount` lands straight on `balance`/`funding_paid`.
            if !ev.numbers_finite() {
                return self.refuse_nonfinite("Funding", &ev.symbol, "");
            }
            self.account.apply_funding(ev);
            return Fold::Applied;
        }
        if let Event::AccountState(ev) = event {
            // LABEL vs LABEL, so it reads `venue` and NOT [`Self::route_key`] — the one place in
            // this file where that choice is not obvious, because the shape looks like routing.
            // It is not: `vike_core`'s `route_event` has ALREADY chosen this engine, and what is
            // left here is a filter over an account-wide stream asking "is this payload from my
            // exchange". `AccountState::venue` is minted by a venue adapter and is always a
            // canonical roster id — it cannot carry an account-distinguishing route key — so
            // comparing it against one would drop every AccountState for any engine whose
            // route_key had been decorated, silently and on the hot fold. Identical today (the two
            // fields are equal); deliberately chosen for the case where they are not.
            if ev.venue != self.venue {
                return Fold::Dropped;
            }
            // …and the SECOND filter, which IS about routing and only fires when the payload says
            // so. A stamped `route_key` names ONE account of this exchange
            // (`vike_mount::account_event_sender`); if it is not this engine's, this engine must
            // not fold it. `vike_core::CoreThread::route_event` already sends it elsewhere, so this
            // is the backstop for the one case that lookup cannot serve: a key naming NO mounted
            // engine, which falls through to the venue lookup and would otherwise overwrite the
            // DEFAULT account's balance with a labelled account's money. An unstamped payload
            // (`None` — every payload on every single-account box, and every correction
            // `vike_exec::recon::resolve` synthesizes) skips this entirely and folds exactly as it
            // always did.
            if let Some(key) = ev.route_key
                && key.as_str() != self.route_key
            {
                return Fold::Dropped;
            }
            // HOSTILE-VENUE GUARD: an AUTHORITATIVE balance assignment — the most damaging of the
            // four, since it OVERWRITES `balance` outright (and flips `balance_mode`) rather than
            // accumulating into it.
            if !ev.numbers_finite() {
                return self.refuse_nonfinite("AccountState", "", "");
            }
            let qa = self.quote_asset.clone();
            self.account.apply_account_state(ev, &qa);
            return Fold::Applied;
        }
        if let Event::PositionLiquidated(ev) = event {
            if !self.accepts_symbol(&ev.symbol) {
                return Fold::Dropped;
            }
            // The routing backstop, BEFORE the dedup below — `seen_liq_ids` is per-engine, so two
            // engines of one exchange would each accept the same frame once and the dedup can
            // never stand in for a routing decision. Same rule as the funding arm above; this is
            // the more damaging of the two, because `apply_liquidation` CLOSES the position and
            // books the realized PnL.
            if let Some(key) = ev.route_key
                && key.as_str() != self.route_key
            {
                return Fold::Dropped;
            }
            // HOSTILE-VENUE GUARD: `liq_price` reaches `compute_fill` and `fee` reaches `balance`.
            if !ev.numbers_finite() {
                return self.refuse_nonfinite("PositionLiquidated", &ev.symbol, "");
            }
            // Liquidation dedup: a WS reconnect can replay a partial liq frame. Mirror the
            // FillEvent guard — an empty trade_id skips dedup and always applies (the legacy
            // whole-flatten path); a distinct id closes its own clamped qty exactly once.
            if !ev.trade_id.is_empty() {
                if self.seen_liq_ids.contains(ev.trade_id.as_str()) {
                    return Fold::Dropped; // reconnect replay — drop
                }
                self.seen_liq_ids.insert(ev.trade_id.to_string());
            }
            self.account.apply_liquidation(ev);
            if let Some(coid) = self.coid_for_position(ev)
                && let Some(mo) = self.registry.get_mut(&coid)
            {
                let liq = Event::OrderLiquidated(OrderLiquidated {
                    client_order_id: mo.client_order_id().to_string(),
                    liq_price: ev.liq_price,
                    ts: ev.ts,
                });
                // ⚠ Now that `coid_for_position` selects on the FSM's own allowed-from set,
                // this transition cannot be refused — the picker and the applier consult one
                // predicate. The old `let _ =` asserted a benign cause ("already terminal —
                // idempotent replay") for a refusal it never actually observed, which is
                // exactly how a real one would have gone unnoticed. Log instead of discarding:
                // if this ever fires, the two predicates have drifted apart again.
                if let Err(e) = mo.apply(&liq) {
                    tracing::warn!(
                        coid = %coid,
                        status = ?mo.status,
                        error = %e,
                        "liquidation leg refused the OrderLiquidated transition — the picker \
                         and the FSM disagree about which orders can receive it"
                    );
                }
            }
            return Fold::Applied;
        }
        // PositionOpened/Changed/Closed: derived read-model events — no engine state (Python
        // falls through to an implicit return), so nothing was folded.
        Fold::Dropped
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MarkSource;
    use crate::{Account, BalanceMode, RiskLimits};
    use vike_model::OrderRequest;
    use vike_model::events::{FillEvent, OrderFilled};

    fn engine() -> ExecutionEngine<RecordingClient> {
        ExecutionEngine::new(
            Account::new(1.0, "sim", None, BalanceMode::Delta),
            RiskGate::new(RiskLimits::new()),
            RecordingClient::default(),
            "sim",
            "BTCUSDT",
        )
    }

    /// Task 8: `local_view()` must report the registered coid, the folded trade_id, and the net
    /// position after one submit+fill — the owned read `recon::diff` (Task 3) consumes across the
    /// reconcile driver's thread boundary.
    #[test]
    fn local_view_reports_order_fill_and_position() {
        let mut eng = engine();
        let mut outbox = Outbox::default();

        let req = OrderRequest {
            client_order_id: "c1".into(),
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            side: 1,
            qty: 1.0,
            order_type: "limit".into(),
            price: Some(100.0),
            ts: 1,
            ..Default::default()
        };
        eng.submit_order(&req, 1, &mut outbox);
        assert!(eng.registry.contains_key("c1"), "submit registers the coid");

        let fill = FillEvent {
            trade_id: "t1".into(),
            client_order_id: "c1".into(),
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            side: 1,
            last_qty: 1.0,
            last_px: 100.0,
            commission: 0.0,
            commission_asset: String::new().into(),
            liquidity_side: "taker".into(),
            ts: 1,
            mark_price: Some(100.0),
            position_side: "BOTH".into(),
        };
        // Real venue adapters emit both the bare fold lane and the FSM wrap for one fill (see
        // price_board_wiring.rs's comment on the same idiom).
        eng.on_event(&Event::Fill(fill.clone()), &mut outbox);
        eng.on_event(
            &Event::OrderFilled(OrderFilled { client_order_id: "c1".to_string(), fill, ts: 1 }),
            &mut outbox,
        );

        let owned = eng.local_view();
        assert_eq!(owned.venue, "sim");
        assert!(owned.orders.contains_key("c1"), "coid present in orders");
        assert!(owned.seen_trade_ids.contains("t1"), "trade_id present in seen set");
        assert_eq!(
            owned.positions.get(&("BTCUSDT".to_string(), "BOTH".to_string())),
            Some(&1.0),
            "net position folded"
        );

        // as_view borrows the same content back out as a LocalView.
        let view = owned.as_view();
        assert_eq!(view.venue, "sim");
        assert!(view.orders.contains_key("c1"));
        assert!(view.seen_trade_ids.contains("t1"));
        assert_eq!(view.positions.get(&("BTCUSDT".to_string(), "BOTH".to_string())), Some(&1.0));
    }

    /// MAJOR-3 (the liquidation law's partition at the ADMITTING gate): an Isolated position
    /// is backed by its own walled-off wallet — not the shared equity the gate admits
    /// against — so it must no longer inflate the gate's `margin_used` (double-charging a
    /// mixed account). The same book with the position CROSS must still be charged: the
    /// filter keys on the MODE, and an all-cross book is byte-identical to the old fold.
    #[test]
    fn gate_margin_used_excludes_isolated_positions() {
        use vike_model::MarginMode;
        let mk = |mode: MarginMode| {
            let mut eng = ExecutionEngine::new(
                Account::new(1.0, "sim", None, BalanceMode::Delta),
                RiskGate::new(RiskLimits { im_requirement: Some(0.1), ..RiskLimits::new() }),
                RecordingClient::default(),
                "sim",
                "BTCUSDT",
            );
            eng.equity_seed = 50.0; // equity 50 (no balance/realized/unreal: avg == mark)
            // an open ETH position that prices 10·100·1·0.1 = 100 margin IF counted
            eng.account.positions.insert(
                ("sim".into(), "ETHUSDT".into(), "BOTH".into()),
                crate::account::PositionEntry {
                    size: 10.0,
                    avg_px: 100.0,
                    margin_mode: mode,
                    isolated_margin: mode.is_isolated().then_some(100.0),
                },
            );
            eng.account.set_mark_from("sim", "ETHUSDT", 100.0, MarkSource::VenueMark, 0);
            // the gate's margin fold is resolver-priced now: feed the board's mark slot the
            // same price the write-sites would (account.marks alone no longer prices margin)
            eng.price_board.set_mark("sim", "ETHUSDT", 100.0, 0);
            eng
        };
        let order = OrderRequest {
            client_order_id: "c1".into(),
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            side: 1,
            qty: 1.0,
            order_type: "limit".into(),
            price: Some(100.0),
            ts: 1,
            ..Default::default()
        };
        // order margin = 100·1·1·0.1 = 10 vs equity 50.
        // Isolated ETH excluded → margin_used 0 → free 50 ≥ 10 → ADMITTED.
        let mut iso = mk(MarginMode::Isolated);
        let mut outbox = Outbox::default();
        iso.submit_order(&order, 1, &mut outbox);
        assert_eq!(
            iso.client.submissions.len(),
            1,
            "an isolated position must not consume the shared gate margin"
        );
        // Cross ETH counted → margin_used 100 > equity 50 → free 0 → DENIED.
        let mut cross = mk(MarginMode::Cross);
        let mut outbox = Outbox::default();
        cross.submit_order(&order, 1, &mut outbox);
        assert!(
            cross.client.submissions.is_empty(),
            "the same book cross-margined must still be charged (filter keys on mode)"
        );
        assert!(
            outbox.0.iter().any(|e| matches!(e, Event::OrderDenied(_))),
            "cross case denies through the normal veto path"
        );
    }

    fn order_report(coid: &str, status: &str, filled: f64) -> vike_model::OrderStatusReport {
        vike_model::OrderStatusReport {
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            venue_order_id: "v7".into(),
            client_order_id: Some(coid.into()),
            side: 1,
            order_type: "limit".into(),
            qty: 2.0,
            filled_qty: filled,
            avg_px: if filled > 0.0 { 100.0 } else { 0.0 },
            status: status.into(),
            ts: 5,
        }
    }

    /// Order-loss recovery (recon JournalDivergence confirm): `reregister_orders` INSERT-ONLY re-seeds
    /// a lost order from its venue report (status/filled/venue_id restored, `created_ms: None` adopted),
    /// never clobbers a coid already present, and skips reports with no client_order_id.
    #[test]
    fn reregister_orders_reseeds_lost_order_insert_only() {
        let mut eng = engine();

        // an order local already knows must NOT be clobbered by a re-register.
        let mut outbox = Outbox::default();
        let req = OrderRequest {
            client_order_id: "keep".into(),
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            side: 1,
            qty: 1.0,
            order_type: "limit".into(),
            price: Some(100.0),
            ..Default::default()
        };
        eng.submit_order(&req, 1, &mut outbox);
        let keep_before = eng.registry.get("keep").cloned().unwrap();

        // one lost order to recover + one no-coid report (skipped) + the already-known "keep".
        let no_coid = vike_model::OrderStatusReport {
            client_order_id: None,
            ..order_report("x", "ACCEPTED", 0.0)
        };
        let n = eng.reregister_orders(&[
            order_report("lost", "PARTIALLY_FILLED", 0.5),
            no_coid,
            order_report("keep", "CANCELED", 0.0), // already known → must be left untouched
        ]);
        assert_eq!(n, 1, "only the genuinely-lost order is re-registered");

        let lost = eng.registry.get("lost").expect("lost order re-registered");
        assert_eq!(
            lost.status,
            OrderStatus::PartiallyFilled,
            "status reconstructed from the report"
        );
        assert_eq!(lost.filled_qty, 0.5);
        assert_eq!(lost.avg_fill_px, 100.0);
        assert_eq!(lost.venue_order_id.as_deref(), Some("v7"));
        assert_eq!(lost.created_ms, None, "adopted order is never swept by the stuck watchdog");

        assert_eq!(eng.registry.get("keep"), Some(&keep_before), "existing order untouched");
        assert!(!eng.registry.contains_key("x"), "a no-coid report is skipped");

        // idempotent: a second pass with the same lost report (now present) re-registers nothing.
        assert_eq!(eng.reregister_orders(&[order_report("lost", "FILLED", 2.0)]), 0);
        assert_eq!(
            eng.registry.get("lost").unwrap().status,
            OrderStatus::PartiallyFilled,
            "not clobbered"
        );
    }

    // ---------------------------------------------------------------------------------------
    // 4th #458-class multiplier-in-context regression (live-vs-backtest divergence ledger:
    // #458 gate notional, #477 UI cap, #479 ibkr_mount): `gate_and_register` minted
    // `ctx.multiplier` ONLY inside the armed buying-power branch (`im_for` Some) — the
    // unarmed else returned 1.0 — while the gate's notional (`qty × ref_price ×
    // ctx.multiplier`) feeds min_notional AND max_notional_per_order regardless of the
    // margin lane. So a mult≠1 instrument with notional floors/caps armed but no margin
    // lane (the live default mount) was judged at multiplier 1.0; the backtest ctx carries
    // the real multiplier unconditionally.
    // ---------------------------------------------------------------------------------------

    /// Engine with floors/caps armed: floor 5, cap 100; optional per-symbol multiplier grid
    /// and optional buying-power lane (`im_requirement`).
    fn mult_gate_engine(
        grid_mult: Option<f64>,
        im: Option<f64>,
    ) -> ExecutionEngine<RecordingClient> {
        let grid: Option<IndexMap<String, f64>> =
            grid_mult.map(|m| [("BTCUSDT".to_string(), m)].into_iter().collect());
        let mut eng = ExecutionEngine::new(
            Account::new(1.0, "sim", grid, BalanceMode::Delta),
            RiskGate::new(RiskLimits {
                min_notional: Some(5.0),
                max_notional_per_order: Some(100.0),
                im_requirement: im,
                ..RiskLimits::new()
            }),
            RecordingClient::default(),
            "sim",
            "BTCUSDT",
        );
        eng.equity_seed = 1_000.0; // ample equity for the armed lane; ignored (0.0 ctx) unarmed
        eng
    }

    fn buy_limit(coid: &str, qty: f64, px: f64) -> OrderRequest {
        OrderRequest {
            client_order_id: coid.into(),
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            side: 1,
            qty,
            order_type: "limit".into(),
            price: Some(px),
            ts: 1,
            ..Default::default()
        }
    }

    fn denied_reason(outbox: &Outbox) -> Option<String> {
        outbox.0.iter().find_map(|e| match e {
            Event::OrderDenied(d) => Some(d.reason.to_string()),
            _ => None,
        })
    }

    /// THE REGRESSION (cap side): margin lane UNARMED + multiplier 100. qty·px = 5.0 sits on
    /// the floor and under the 100 cap at multiplier 1.0 — the pre-fix ctx ADMITTED it — but
    /// the real notional is 5·100 = 500 > cap, so it must be DENIED.
    #[test]
    fn unarmed_lane_notional_cap_judged_with_contract_multiplier() {
        let mut eng = mult_gate_engine(Some(100.0), None);
        let mut outbox = Outbox::default();
        eng.submit_order(&buy_limit("cap", 0.5, 10.0), 1, &mut outbox);
        assert_eq!(denied_reason(&outbox).as_deref(), Some("over-max-notional"));
        assert!(eng.client.submissions.is_empty(), "over-cap notional must not reach the venue");
        assert!(!eng.registry.contains_key("cap"), "a denied order never enters the registry");
    }

    /// THE REGRESSION (floor mirror): qty·px = 0.1 < floor 5 — the pre-fix ctx wrongly DENIED
    /// it below-min-notional — but the real notional 0.1·100 = 10 clears the floor and sits
    /// under the cap, so it must be ADMITTED.
    #[test]
    fn unarmed_lane_notional_floor_judged_with_contract_multiplier() {
        let mut eng = mult_gate_engine(Some(100.0), None);
        let mut outbox = Outbox::default();
        eng.submit_order(&buy_limit("floor", 0.01, 10.0), 1, &mut outbox);
        assert_eq!(denied_reason(&outbox), None);
        assert_eq!(eng.client.submissions.len(), 1, "real notional clears the floor → submitted");
        assert!(eng.registry.contains_key("floor"));
    }

    /// THE COMPAT PIN: multiplier 1.0 (symbol absent from the grid — every production mount
    /// today except deribit post-#484). Verdicts are the raw qty·px judgment in BOTH lanes,
    /// byte-identical to the pre-fix ctx (`x * 1.0` is an IEEE-754 no-op; the unarmed branch
    /// carried the literal 1.0 before, `multiplier_of`'s default 1.0 now).
    #[test]
    fn multiplier_one_verdicts_identical_across_lanes() {
        for im in [None, Some(0.001)] {
            // below the floor → denied, both lanes
            let mut eng = mult_gate_engine(None, im);
            let mut outbox = Outbox::default();
            eng.submit_order(&buy_limit("lo", 0.1, 10.0), 1, &mut outbox);
            assert_eq!(denied_reason(&outbox).as_deref(), Some("below-min-notional"), "im={im:?}");
            // over the cap → denied, both lanes
            let mut eng = mult_gate_engine(None, im);
            let mut outbox = Outbox::default();
            eng.submit_order(&buy_limit("hi", 20.0, 10.0), 1, &mut outbox);
            assert_eq!(denied_reason(&outbox).as_deref(), Some("over-max-notional"), "im={im:?}");
            // in-band → admitted, both lanes
            let mut eng = mult_gate_engine(None, im);
            let mut outbox = Outbox::default();
            eng.submit_order(&buy_limit("ok", 1.0, 10.0), 1, &mut outbox);
            assert_eq!(denied_reason(&outbox), None, "im={im:?}");
            assert_eq!(eng.client.submissions.len(), 1, "im={im:?}");
        }
    }

    /// THE ARMED-LANE PIN: the armed branch already minted the real multiplier pre-fix — the
    /// hoist must not change it. Same mult-100 over-cap order, margin lane ARMED, still
    /// denies over-max-notional; an in-band mult-100 order still clears notional AND buying
    /// power (notional 0.05·10·100 = 50 ∈ [5, 100]; IM = 50·0.01 = 0.5 ≤ equity 1000).
    #[test]
    fn armed_lane_multiplier_behavior_unchanged() {
        let mut eng = mult_gate_engine(Some(100.0), Some(0.01));
        let mut outbox = Outbox::default();
        eng.submit_order(&buy_limit("cap", 0.5, 10.0), 1, &mut outbox);
        assert_eq!(denied_reason(&outbox).as_deref(), Some("over-max-notional"));

        let mut eng = mult_gate_engine(Some(100.0), Some(0.01));
        let mut outbox = Outbox::default();
        eng.submit_order(&buy_limit("ok", 0.05, 10.0), 1, &mut outbox);
        assert_eq!(denied_reason(&outbox), None);
        assert_eq!(eng.client.submissions.len(), 1);
    }

    /// ⚠ **AN ORDER ALREADY IN FLIGHT MUST CONSUME BUYING POWER** — it did not, so the gate
    /// admitted a second order as though the first committed nothing.
    ///
    /// `RiskContext::margin_used` folded open POSITIONS only. An order that is live at the venue
    /// but not yet filled holds no position, so it contributed 0 — and a strategy that submits
    /// twice before the first fill got both admitted against the same equity. The BACKTEST has
    /// always counted its pending orders (`SimBroker::margin_in_use_pending_aware`), and that
    /// side's doc claimed it mirrored live "field for field"; live was the permissive one.
    ///
    /// The numbers: equity 50, im 0.1, mark 100. One order of 3 commits 3·100·1·0.1 = 30, leaving
    /// free 20. A second order of 3 needs 30 > 20 and must be DENIED. Before the fix `margin_used`
    /// was 0, free was 50, and it was admitted.
    ///
    /// NON-VACUOUS in both directions: the FIRST order is asserted to still be admitted (so the
    /// term did not simply break the gate), and a size that fits inside the remaining 20 is
    /// asserted to still pass (so the denial is the margin arithmetic, not a blanket refusal of any
    /// second order).
    #[test]
    fn a_live_unfilled_order_consumes_buying_power() {
        let mk = || {
            let mut eng = ExecutionEngine::new(
                Account::new(1.0, "sim", None, BalanceMode::Delta),
                RiskGate::new(RiskLimits { im_requirement: Some(0.1), ..RiskLimits::new() }),
                RecordingClient::default(),
                "sim",
                "BTCUSDT",
            );
            eng.equity_seed = 50.0;
            eng.account.set_mark_from("sim", "BTCUSDT", 100.0, MarkSource::VenueMark, 0);
            eng.price_board.set_mark("sim", "BTCUSDT", 100.0, 0);
            eng
        };
        let order = |coid: &str, qty: f64| OrderRequest {
            client_order_id: coid.into(),
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            side: 1,
            qty,
            ts: 1,
            ..Default::default()
        };

        let mut eng = mk();
        let mut outbox = Outbox::default();
        eng.submit_order(&order("c1", 3.0), 1, &mut outbox);
        assert_eq!(eng.client.submissions.len(), 1, "the first order is admitted: 30 <= 50");

        // The venue accepts it — live, un-filled, holding no position yet.
        eng.on_event(
            &Event::OrderAccepted(vike_model::events::OrderAccepted {
                client_order_id: "c1".into(),
                venue_order_id: None,
                ts: 1,
            }),
            &mut outbox,
        );

        // A second order of the same size needs 30 against a remaining 20.
        let mut ob2 = Outbox::default();
        eng.submit_order(&order("c2", 3.0), 2, &mut ob2);
        assert_eq!(
            eng.client.submissions.len(),
            1,
            "the second order must be DENIED — the first committed 30 of the 50 equity, and \
             counting positions alone made that commitment invisible"
        );
        assert!(
            ob2.0.iter().any(|e| matches!(e, Event::OrderDenied(_))),
            "and it denies through the normal veto path, not by vanishing"
        );

        // ...but one that FITS in the remaining 20 still passes, so this is the margin arithmetic
        // rather than a blanket refusal of any second order.
        let mut ob3 = Outbox::default();
        eng.submit_order(&order("c3", 1.0), 3, &mut ob3);
        assert_eq!(eng.client.submissions.len(), 2, "10 <= the remaining 20 is still admitted");
    }
}

#[cfg(test)]
mod mark_basis_tests {
    use super::*;
    use crate::MarkSource;
    use crate::execution_engine::test_clients::RecordingClient;
    use crate::{Account, BalanceMode, RiskGate, RiskLimits};
    use vike_model::OrderRequest;

    fn engine_with(limits: RiskLimits, mark: f64) -> ExecutionEngine<RecordingClient> {
        let mut e = ExecutionEngine::new(
            Account::new(1.0, "sim", None, BalanceMode::Delta),
            RiskGate::new(limits),
            RecordingClient::default(),
            "sim",
            "BTCUSDT",
        );
        e.equity_seed = 1_000_000.0;
        e.account.set_mark_from("sim", "BTCUSDT", mark, MarkSource::VenueMark, 0);
        e.price_board.set_mark("sim", "BTCUSDT", mark, 0);
        e
    }

    /// **The cap is a comparison, not a `min`, and NaN is why.**
    ///
    /// `f64::min` treats NaN as the missing operand and returns the other one, so a poisoned
    /// equity would come out of [`ExecutionEngine::cap_sizing_equity`] as the operator's ceiling —
    /// a finite, plausible number the gate then admits against, where the NaN it replaced fails
    /// every comparison and denies. A ceiling may only ever LOWER a real figure.
    ///
    /// The other three rows are the ordinary contract: unarmed is BIT-identical (not merely equal),
    /// an armed ceiling binds only above itself, and it never RAISES a figure below it — including
    /// a negative one, which is what an account underwater on unrealized PnL looks like.
    #[test]
    fn the_sizing_cap_only_ever_lowers_and_never_launders_a_nan() {
        let unarmed = engine_with(RiskLimits::new(), 100.0);
        assert_eq!(
            unarmed.cap_sizing_equity(50_000.0).to_bits(),
            50_000.0_f64.to_bits(),
            "no ceiling ⇒ the figure is returned verbatim, not min'd against an infinity"
        );
        assert!(unarmed.cap_sizing_equity(f64::NAN).is_nan(), "and a NaN stays a NaN");

        let armed = engine_with(
            RiskLimits { max_sizing_equity: Some(20_000.0), ..RiskLimits::new() },
            100.0,
        );
        assert_eq!(armed.cap_sizing_equity(50_000.0), 20_000.0, "above the ceiling ⇒ the ceiling");
        assert_eq!(armed.cap_sizing_equity(5_000.0), 5_000.0, "below it ⇒ untouched, never raised");
        assert_eq!(armed.cap_sizing_equity(-500.0), -500.0, "and a NEGATIVE figure is not raised");
        assert!(
            armed.cap_sizing_equity(f64::NAN).is_nan(),
            "a poisoned equity must NOT come back as the ceiling — `f64::min` would have done \
             exactly that, turning an unusable number into an admissible one"
        );
    }

    fn limit(coid: &str, px: f64, qty: f64) -> OrderRequest {
        OrderRequest {
            client_order_id: coid.into(),
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            order_type: "limit".into(),
            side: 1,
            qty,
            price: Some(px),
            ts: 1,
            ..Default::default()
        }
    }

    /// ⚠ **THE PRICE COLLAR COULD NOT FIRE FOR A LIMIT ORDER.**
    ///
    /// `risk_ctx` set `mark_price` from `request.price` when the order had one, and `check_inner`'s
    /// collar compares `|req.price - ctx.mark_price| > band`. With the two equal that difference is
    /// always ZERO, so a fat-finger limit at any distance from the true mark was admitted by the
    /// very axis meant to catch it. Its own comment claims "a price 10x ABOVE the mark and one 10x
    /// BELOW are the same fat finger" — while comparing the price to itself.
    ///
    /// Mark 100, band 10%: a limit at 1000 is 10x out and must be DENIED.
    ///
    /// NON-VACUOUS: a limit INSIDE the band is asserted admitted in the same test, so this is the
    /// collar working rather than a blanket refusal of limit orders.
    #[test]
    fn the_price_collar_fires_on_a_fat_finger_limit() {
        let limits = RiskLimits {
            price_collar: Some(crate::PriceCollar { pct: 0.10, abs_floor: 0.0 }),
            ..RiskLimits::new()
        };

        let mut e = engine_with(limits.clone(), 100.0);
        let mut outbox = Outbox::default();
        e.submit_order(&limit("c1", 1000.0, 1.0), 1, &mut outbox);
        assert!(
            e.client.submissions.is_empty(),
            "a limit 10x above the mark must be denied by the collar — it compared the price to \
             ITSELF, so the collar was dead for every priced order"
        );
        assert!(
            outbox.0.iter().any(|ev| matches!(ev, Event::OrderDenied(_))),
            "and it denies through the normal veto path"
        );

        // ...and one INSIDE the band still goes.
        let mut ok = engine_with(limits, 100.0);
        let mut ob2 = Outbox::default();
        ok.submit_order(&limit("c2", 103.0, 1.0), 1, &mut ob2);
        assert_eq!(ok.client.submissions.len(), 1, "3% from the mark is inside a 10% band");
    }

    /// ⚠ **The projected-exposure cap must value at the MARK, not the order's price.**
    ///
    /// A far-from-mark limit understated what the account would actually be exposed to once filled.
    /// Mark 100, cap 500: 10 units is 1000 of exposure at the mark and must be denied — but at a
    /// limit price of 10 it computed 100 and passed.
    ///
    /// NON-VACUOUS: the same order under a cap that ACCOMMODATES the mark-valued exposure is
    /// asserted admitted, so the denial is the basis and not the cap alone.
    #[test]
    fn the_exposure_cap_values_at_the_mark_not_the_order_price() {
        let deny = RiskLimits { max_total_exposure: Some(500.0), ..RiskLimits::new() };
        let mut e = engine_with(deny, 100.0);
        let mut outbox = Outbox::default();
        e.submit_order(&limit("c1", 10.0, 10.0), 1, &mut outbox);
        assert!(
            e.client.submissions.is_empty(),
            "10 units at a MARK of 100 is 1000 of exposure against a 500 cap — pricing it at the \
             limit (10) computed 100 and admitted it"
        );

        let allow = RiskLimits { max_total_exposure: Some(2_000.0), ..RiskLimits::new() };
        let mut ok = engine_with(allow, 100.0);
        let mut ob2 = Outbox::default();
        ok.submit_order(&limit("c2", 10.0, 10.0), 1, &mut ob2);
        assert_eq!(ok.client.submissions.len(), 1, "1000 of exposure fits a 2000 cap");
    }
}
