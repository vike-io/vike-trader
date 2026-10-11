//! ExecutionEngine — the live composition root: account/gate/registry/client for ONE
//! (venue, symbol).
//!
//! `submit_order` runs the gate and publishes `OrderDenied` on veto, else calls `client.submit`.
//! `on_event` folds the venue stream: a bare `FillEvent` → `Account::apply_fill`, which is
//! IDEMPOTENT PER `trade_id` itself (the reconnect dedup lives on `Account`, see `account.rs`'s
//! module doc; this fold reads the [`crate::account::FillFold`] verdict back), admitted by mounted
//! symbol (`accepts_symbol`) or by coid ownership (`owns_fill_symbol`: fills of orders THIS engine
//! manages fold even under never-mounted symbols, a combo's aggregate net-price print never does);
//! `Order*` lifecycle → the `ManagedOrder` registry with the SEPARATE `seen_fsm_trade_ids` wrap
//! dedup; liquidation frames dedup via `seen_liq_ids`. The cancel-vs-fill race guard (LEAN
//! `CancelPendingOrders`): a venue-seeded PENDING_CANCEL order is restored to its pre-cancel live
//! status before a fill wrap / cancel-reject applies, so the FSM table never drops venue truth.
//! This engine folds state only: durable persistence is the runtime's command journal and its
//! materialized `exec_fill`/`exec_order` series.

use indexmap::IndexMap;
use std::collections::HashSet;
use vike_model::OrderRequest;
use vike_model::events::{Event, OrderDenied};

use crate::Account;
use crate::bus::Outbox;
use crate::order::ManagedOrder;
use crate::risk::types::RiskContext;
use crate::risk::{RiskGate, TradingState};

// Re-exported so `execution_engine::X` and the crate-root `vike_exec::X` are the paths imported.
mod client;
mod reconcile;
// The in-process doubles exist only under `cfg(test)` or the `test-support` feature (a DEV edge
// only): a `TestExecutionClient` fills every order instantly, so it must never ship in the
// order-signing daemon. `crates/vike-ops/tests/architecture/test_surface_gate.rs` refuses an
// ungated double and a shipped edge enabling the feature alike.
#[cfg(any(test, feature = "test-support"))]
mod test_clients;

pub use client::{CancelIntent, ExecutionClient};
pub use reconcile::{AppliedFill, OrderEventOut, ReconcileSnapshot};
#[cfg(any(test, feature = "test-support"))]
pub use test_clients::{RecordingClient, TestExecutionClient};

mod fold;
mod queries;
mod reconcile_apply;
mod snapshot;
mod valuation;

/// One open position priced only off a STALE last-known value: a row of
/// [`ExecutionEngine::stale_marks`] (`age_ms` = how far past the engine clock the value's
/// timestamp sits). NOT journaled — a live health signal derived from non-persisted market data,
/// like the `PriceBoard` it reads.
#[derive(Debug, Clone, PartialEq)]
pub struct StaleMark {
    pub venue: String,
    pub symbol: String,
    pub position_side: String,
    pub source: crate::price_board::PriceSource,
    pub age_ms: i64,
}

/// Reconcile drift tolerance: a position-size / balance divergence within
/// `DRIFT_ABS_TOL + DRIFT_REL_TOL * |venue|` is float noise (venue rounding, last-ulp wire
/// re-encode), NOT drift. The absolute floor covers values near zero.
const DRIFT_REL_TOL: f64 = 1e-6;
const DRIFT_ABS_TOL: f64 = 1e-8;

/// `LocalView::qty_tol` for [`ExecutionEngine::local_view`]: no engine-level source exists, so it
/// mirrors the `recon::diff` tests' `1e-9`.
const LOCAL_VIEW_QTY_TOL: f64 = 1e-9;

/// True when locally-folded `local` and venue-truth `venue` differ by more than the reconcile
/// drift tolerance — the single decision site for [`ExecutionEngine::diff_snapshot`].
#[inline]
fn drift_diverges(local: f64, venue: f64) -> bool {
    (local - venue).abs() > DRIFT_ABS_TOL + DRIFT_REL_TOL * venue.abs()
}

/// **What stands behind an engine's orders.** Published per account as
/// [`crate::VenueBlock::mode`].
///
/// Set by the mount (`vike_mount`'s `assemble_engine`), the one place that knows whether it built a
/// venue client and on which tier. [`ExecutionEngine::new`] and [`ExecutionEngine::from_snapshot`]
/// (a snapshot does not carry it) seed [`EngineMode::Paper`] whatever client they are handed;
/// nothing infers the mode from the client.
///
/// ⚠ **`Paper` is the SEED, not proof**: an engine that says `Paper` has not been shown to be a
/// simulated book. A caller that builds or restores one around a real client must set
/// [`ExecutionEngine::mode`] itself.
///
/// ⚠ **A second paper/demo/live enum beside `vike_config::VenueMode`, on purpose.** Both crates sit
/// at layer 20, so neither can name the other, and they answer different questions: `VenueMode` is
/// the TIER an account row states, this is what an engine's orders go to. Do not join them
/// with an alias.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineMode {
    /// A simulated book: no order leaves the process.
    Paper,
    /// A venue client on the venue's demo, testnet or sandbox account.
    Demo,
    /// A venue client on a real-money account.
    Live,
}

/// Live composition root. Drive orders with `submit_order`; read `.account` / `.registry`.
pub struct ExecutionEngine<C: ExecutionClient> {
    pub account: Account,
    /// Read-side price board: per-source price cells the resolver walks on COLD paths only. Fed
    /// alongside `account.set_mark` at the write sites; never read in the fold. Deliberately
    /// outside `EngineSnapshot` (journal hash-fence freeze).
    pub price_board: crate::price_board::PriceBoard,
    /// Resolver knobs for the ENGINE-INTERNAL decision sites (the pre-trade gate's
    /// `resolved_equity` / `resolved_margin_in_use_by` reads). The runtime sets it from
    /// `CoreConfig::price_cfg` at spawn, the source every vike-core caller passes, so one config
    /// governs every resolver read. Default permissive (mark enabled, no freshness windows). NOT
    /// serialized: config, re-imposed by the runtime on restore like `equity_seed`.
    pub price_cfg: crate::price_board::PriceCfg,
    pub gate: RiskGate,
    pub client: C,
    /// The CANONICAL venue id (`"binance"`): the key every per-venue capability table is looked
    /// up under ([`vike_model::caps_for`], [`vike_model::amend_semantics`],
    /// [`vike_model::fee_schedule_for`], `vike_model::venues::venue_tif::venue_tif`,
    /// [`fn@vike_model::venue_margin_support`]), and the label of this engine's `Account` position
    /// keys, its registry's `OrderRequest::venue`s and its published `VenueBlock::venue`, so every
    /// self-comparison in this file reads THIS field, not [`Self::route_key`].
    ///
    /// ⚠ It must stay a roster id ([`vike_model::VENUES`]). Every one of those tables is
    /// fail-closed on a miss — `caps_for` answers `VenueCaps::UNSUPPORTED` (preflight rejects every
    /// order) and `amend_semantics` answers `AmendSemantics::Unknown`, which SILENTLY changes what
    /// a modify's quantity means. So an account-distinguishing suffix goes on [`Self::route_key`]:
    /// decorating THIS field is the `"binance#2"` trap, gated by
    /// `crates/vike-exec/tests/engine/route_key.rs`.
    pub venue: String,
    /// The ROUTING key: which ENGINE an inbound venue-tagged payload belongs to, unique per venue
    /// ACCOUNT rather than per venue. `vike_core`'s `CoreThread::engine_idx_for_route_key` matches
    /// on this and nothing else.
    ///
    /// **Not a synonym of [`Self::venue`].** `venue` answers "what does this venue support" (shared
    /// by every account on it), `route_key` "which of this process's engines is this fill for". As
    /// one field, two accounts of one venue could not share a process: labelling both `"binance"`
    /// folded both accounts' fills into the first match's book (and reconcile's `PositionDrift`
    /// rewrote each onto the other's number), while `"binance#2"` broke every capability lookup.
    ///
    /// **[`Self::new`] sets it EQUAL to `venue`, and nothing in this workspace sets it otherwise**,
    /// so every routing decision is the single-field behaviour by construction.
    ///
    /// ⚠ If something ever does set it: `vike_ops::live_lock::LiveLock::acquire` uses it as a
    /// FILENAME component (`<state_dir>/LIVE-<route_key>.lock`), so it must be path-safe — no `/`,
    /// `\`, or `..`. Nothing validates it today because nothing sets it.
    pub route_key: String,
    pub symbol: String,
    /// What stands behind this engine's orders ([`EngineMode`]). `Paper` from [`Self::new`] and from
    /// [`Self::from_snapshot`]; the mount sets it where it builds around a venue client.
    pub mode: EngineMode,
    /// The effective fee schedule the mount resolved for this engine: the LIVE account-actual rate
    /// when a `ReconClient::fetch_fee_rates` producer returned one, else the static
    /// [`vike_model::fee_schedule_for`] default. Read-only, for cost display (the snapshot's
    /// `VenueBlock.fee_schedule`); it does NOT drive the fold — live fills report the venue's real
    /// commission and a paper engine's `PaperExecutionClient` carries its own schedule. `None`
    /// until the mount sets it (a GUI-only / test engine never does).
    pub fee_schedule: Option<vike_model::FeeSchedule>,
    /// `Account::apply_account_state` quote-asset selector (default "USDT")
    pub quote_asset: String,
    /// perp: force reduce_only on submit_close flattens (read by the GUI ticket path)
    pub reduce_only_on_close: bool,
    pub registry: IndexMap<String, ManagedOrder>,
    pub trading_state: TradingState,
    /// wall-clock the runtime stamps before each dispatch (persistence `updated_ts`)
    pub now_ms: i64,
    /// When true (the runtime sets it iff a strategy is mounted), every fill the account fold
    /// ACCEPTS (post-dedup) is also buffered in `applied_fills` for `Strategy::on_fill`
    /// delivery. Capturing at the one `apply_fill` site means a WS reconnect replay can never
    /// double-fire the handler. Default false so a GUI-only engine never grows the buffer.
    pub collect_applied_fills: bool,
    /// fills accepted since the runtime last drained (see `collect_applied_fills`)
    pub applied_fills: Vec<AppliedFill>,
    /// NON-FILL order-lifecycle transitions (accept/reject/deny/cancel/expire) captured for
    /// `Strategy::on_order_event` delivery, gated by the SAME `collect_applied_fills` flag. Each
    /// carries the order's (venue, symbol) so the runtime routes it to the owning mount like
    /// `applied_fills`. Not serialized (deliveries are not replayed).
    pub order_events: Vec<OrderEventOut>,
    /// seed for the per-fill `equity_after` snapshot (the runtime's `seed_cash`)
    pub equity_seed: f64,
    /// Multi-symbol opt-in (RUST-NATIVE): additional symbols this engine accepts venue events for
    /// (fills/funding/liquidations fold into the one multi-symbol Account). Empty (default) = the
    /// single-symbol filter.
    pub extra_symbols: Vec<String>,
    // There is no `seen_trade_ids` field: the bare-`Event::Fill` dedup ledger is `Account`'s own
    // set, consulted inside `Account::apply_fill`, so no path can bypass it and no second copy can
    // disagree; `Self::seen_trade_ids()` reads it through for the snapshot/`local_view`
    // (`EngineSnapshot::seen_trade_ids` on the wire). The two sets below guard DIFFERENT
    // aggregates: the `ManagedOrder` FSM's `filled_qty`/`avg_fill_px`, and the liquidation lane.
    seen_fsm_trade_ids: HashSet<String>,
    seen_liq_ids: HashSet<String>,
    /// Count of terminal events that failed to apply while the order was still LIVE (a genuinely
    /// lost terminal — the order may be stranded). Benign idempotent replays on already-terminal
    /// orders are NOT counted. Read by monitoring/tests; never affects the fold.
    pub dropped_terminal_on_live: u64,
    /// Count of lifecycle events whose coid was not in the registry (pre-restart order absent from
    /// reconcile, external-account order, or a bug). The event is still dropped — this only makes
    /// the drop visible.
    pub dropped_unknown_coid: u64,
    /// Count of venue LIVENESS/EXECUTION events (`OrderAccepted`/`OrderPartiallyFilled`/
    /// `OrderFilled`) dropped because the order was ALREADY terminalized via a KILL path
    /// (`Rejected`/`Canceled`/`Expired`/`Denied`): the signature of a premature terminal
    /// (classically the watchdog's stage-2 phantom-reject racing a slow venue confirm) leaving a
    /// real venue position STRANDED. `dropped_terminal_on_live` misses it (its
    /// `!status_after.is_terminal()` guard is false once the order is terminal). Benign same-state
    /// terminal replays are NOT counted. Observability only: never changes WHAT is dropped. Not
    /// serialized (a live health signal, like `dropped_unknown_coid`).
    pub stranded_terminal_drops: u64,
    /// HOSTILE-VENUE observability: count of money-lane events (`Fill`/`Funding`/`AccountState`/
    /// `PositionLiquidated`) refused because a folded f64 was NOT FINITE — see
    /// [`vike_model::FiniteNumbers`] for how `"NaN"` reaches a typed field off the wire, and why
    /// one such value poisons the ledger IRRECOVERABLY.
    ///
    /// **Never routine**: the other drop counters have benign explanations, a non-finite venue
    /// number has NONE, so the fold logs it at ERROR. Never affects anything but the one event it
    /// rejects.
    pub dropped_nonfinite: u64,
}

impl<C: ExecutionClient> ExecutionEngine<C> {
    /// `venue` seeds BOTH [`Self::venue`] and [`Self::route_key`]: a fresh engine is the
    /// single-account shape. A second account sets [`Self::route_key`] afterwards (kept off the
    /// signature so the many construction sites need not pass one).
    pub fn new(account: Account, gate: RiskGate, client: C, venue: &str, symbol: &str) -> Self {
        ExecutionEngine {
            account,
            gate,
            client,
            venue: venue.to_string(),
            route_key: venue.to_string(),
            symbol: symbol.to_string(),
            mode: EngineMode::Paper,
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

    /// Cold-start dedup seed from a durable source (e.g. the replayed command journal), applied
    /// once at startup. Delegates to [`Account::seed_seen_fill_ids`]: the same set
    /// `Account::apply_fill` consults, not a copy.
    pub fn seed_seen_trade_ids<I: IntoIterator<Item = String>>(&mut self, ids: I) {
        self.account.seed_seen_fill_ids(ids);
    }

    /// The bare-fill dedup ledger, read-only, THROUGH to [`Account::seen_fill_ids`] (no copy, so
    /// this reader and the guard in `Account::apply_fill` cannot diverge). `pub` for the reconcile
    /// `LocalView`, the `EngineSnapshot` wire and the journal cross-check.
    pub fn seen_trade_ids(&self) -> impl Iterator<Item = &str> {
        self.account.seen_fill_ids()
    }

    /// Gate the order; publish OrderDenied on veto or submit to the venue. `now_ms` is the injected
    /// clock. Coarse per-ORDER span (NOT per-message): `skip_all` keeps the hot path free of arg
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
            // Capture the veto for `Strategy::on_order_event` HERE: a denied order never enters
            // the registry, so the FSM-apply capture site cannot see it. Gated on the same mount
            // flag as `applied_fills`, so a GUI-only engine and the latency gate pay nothing.
            if self.collect_applied_fills {
                self.order_events.push(OrderEventOut {
                    venue: request.venue.clone(),
                    symbol: request.symbol.clone(),
                    event: vike_model::OrderLifecycle {
                        client_order_id: denied.client_order_id.clone(),
                        // The ENGINE holds no tag registry — it is the runtime's, and the runtime
                        // stamps this at delivery. See `OrderLifecycle::tag`.
                        tag: None,
                        kind: vike_model::OrderEventKind::Denied {
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

    /// Build the [`RiskContext`] one gate call judges `request` against. Shared by submit and
    /// MODIFY ([`Self::modify_order`]): a modify that raises qty is economically a bigger order,
    /// and sharing the BUILDER rather than a copy keeps both judged on the same
    /// equity/margin/mark/multiplier basis — a divergence there is exactly what the one-price law
    /// exists to stop.
    fn risk_ctx(&self, request: &OrderRequest, now_ms: i64) -> RiskContext {
        // Margin fields are computed ONLY when the margin lane is armed (`im_for`):
        // resolved_equity is O(positions) per order.
        //
        // THE CONTRACT MULTIPLIER IS LANE-INDEPENDENT (the #458 multiplier-in-context class).
        // `ctx.multiplier` feeds the gate's notional (`qty × ref_price × ctx.multiplier`, read by
        // min_notional AND max_notional_per_order — see `risk.rs`), not only the buying-power
        // lane, so it is minted HERE, outside the `im_for` arm: an engine with notional caps but
        // no margin lane (the live default with no configured leverage) must still judge a
        // mult≠1 instrument (deribit inverse perps) at its real multiplier, as the backtest does.
        // `multiplier_of` returns 1.0 for any symbol outside the grid.
        let multiplier = self.account.multiplier_of(&request.symbol);
        // Hedge-aware coverage basis, computed ONCE for the reversal credit and the ctx (see
        // `gate_position_size`). Keyed on the ORDER's symbol, like `multiplier` and `im_for`: an
        // engine also accepts `extra_symbols`, and reading the mount's position would judge a
        // foreign-symbol order against a MIXTURE of two instruments.
        let pos = self.gate_position_size(&request.symbol);
        let (equity, margin_used, closing_credit) =
            if let Some(im_req) = self.gate.limits.im_for(&request.symbol) {
                // THE shared margin-in-use fold, resolver-priced so the margin numerator shares
                // `resolved_equity`'s price basis (one price law). RATE: each open position uses
                // its OWN per-symbol IM, falling back to the ORDER symbol's `im_req` (never
                // skipped). POOL (the liquidation law's partition, mirroring `check_margin_call`):
                // only CROSS positions consume the shared equity this gate admits against — an
                // Isolated position has its own wallet and a Cash one is fully funded, so counting
                // either would DOUBLE-CHARGE a mixed account.
                let used = self.resolved_margin_in_use_by(&self.price_cfg, |(_v, s, _side), p| {
                    p.margin_mode.is_cross().then(|| self.gate.limits.im_for(s).unwrap_or(im_req))
                })
                // ...plus the margin COMMITTED by this engine's live un-filled orders
                // (`live_order_margin`), or a second order is judged as if the first committed
                // nothing.
                + self.live_order_margin(&self.price_cfg, im_req, &request.client_order_id);
                // Direction-reversing order: credit the margin the close frees + the LEAN re-open
                // credit (single-requirement model: mm == im in the gate), priced through the SAME
                // resolver as `used` and `resolved_equity` (the position's own symbol/side chain):
                // a raw stale-high mark would credit margin `used` never charged.
                //
                // A `Missing` resolution credits NOTHING, matching `resolved_margin_in_use_by`,
                // which drops an unpriceable position from `used`: a position that consumed no
                // margin frees none. The two ends are CONSISTENT, not both conservative — zeroing
                // the credit denies more, while dropping the position from `used` inflates free
                // buying power (the ANTI-conservative one). Do not read `Missing` as uniformly
                // safe.
                let credit = if pos != 0.0 && request.side as f64 * pos < 0.0 {
                    let px = self
                        .resolved_position_price(&self.venue, &request.symbol, pos, &self.price_cfg)
                        .unwrap_or(0.0);
                    2.0 * pos.abs() * px * multiplier * im_req
                } else {
                    0.0
                };
                // One-price law: the SAME resolver-priced equity the watchdog/strategy/snapshot
                // read, not the mark-only `equity_all`.
                //
                // ⚠ `sizing_equity`, not `resolved_equity`: this lane SPENDS against equity, so
                // `RiskLimits::max_sizing_equity` applies and a lower figure can only refuse
                // sooner. The margin-CALL sweep keeps the uncapped resolver —
                // `Self::sizing_equity`'s doc carries the asymmetry.
                (self.sizing_equity(self.equity_seed, &self.price_cfg), used, credit)
            } else {
                (0.0, 0.0, 0.0)
            };
        RiskContext {
            position_size: pos,
            // The priceless (market/stop) notional+exposure reference, on the ONE resolver basis
            // shared with equity/margin/credit (the position's own side chain, like the credit).
            // ⚠ **THE MARK FIRST, the order's own price only as a last resort**: the field is the
            // VALUATION basis, not an order price. Reading `request.price` first would silently
            //
            //  * judge `risk.rs`'s projected-exposure cap (`|pos + side*qty| * ctx.mark_price *
            //    multiplier`) at a resting limit's price instead of the mark, and
            //  * kill `risk.rs`'s PRICE COLLAR (`|req.price - ctx.mark_price| > band`) for every
            //    limit order: with the two equal the difference is ZERO.
            //
            // The NOTIONAL lanes do not read this field for a priced order: `check_inner`'s
            // `ref_price = req.price.or(req.trigger_price).unwrap_or(ctx.mark_price)`.
            //
            // ⚠ `request.price` stays as the final fallback rather than letting an unresolvable
            // mark drop to `0.0`: a `0.0` mark makes projected exposure 0, which VACATES the
            // exposure cap at any size (`check_combo` hard-denies `"leg X: no-mark"` for this
            // reason). Keep it deliberately.
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
            // The ACCOUNT-aggregate ceiling's other half, folded ONLY when that ceiling is armed
            // (O(positions + live orders); a deployment without `max_account_exposure` pays
            // nothing). The `0.0` is inert: only that lane reads the field.
            //
            // ⚠ `request.client_order_id` is the JUDGED order, excluded from the resting-order
            // half: on an AMEND (`modify_order` builds this ctx from the projected request under
            // the resting order's own coid) it is in the registry, and counting it as well as
            // projecting it would refuse the amend at a ceiling it was admitted under at submit.
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
    /// definition is [`crate::order::OrderStatus::is_live`]; see its doc for why it is neither
    /// `!is_terminal()` (Liquidated) nor the FSM's `can_receive_cancel` allowed-from set.
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

    /// Modify a live order's qty/price by client-order-id (RUST-NATIVE). Legal only while the order
    /// rests at the venue (ACCEPTED/TRIGGERED/PARTIALLY_FILLED); a SUBMITTED or terminal order is a
    /// no-op. Publishes NOTHING on success — the venue stream emits the authoritative OrderModified
    /// that advances the FSM (`on_event`), mirroring cancel.
    ///
    /// ⚠ **RISK-GATED.** A modify that RAISES qty (or price) is economically a bigger order, so the
    /// [`RiskGate`] judges the PROJECTED order (the resting request with `new_qty` / `new_price`
    /// folded in), never the delta — else a small in-cap order modified up walks past
    /// `max_notional_per_order`. A veto publishes a NON-TERMINAL [`Event::OrderModifyRejected`]
    /// (the order keeps its terms, as when a venue refuses a modify) and nothing reaches the venue.
    ///
    /// Two deliberate departures from the submit path:
    /// - **No throttle slot is consumed** ([`RiskGate::check_modify`]): the resting order paid one
    ///   at submit, and re-charging every amend would self-throttle an amend-heavy maker.
    /// - **The gate's ROUNDED request is not substituted onto the wire.** The venue modify takes
    ///   `(original order, new_qty, new_price)`, so an ACCEPTED modify sends the caller's values
    ///   verbatim; the verdict is a veto only.
    ///
    /// ⚠ **WHETHER THE EXECUTED PART OF A PARTIALLY FILLED ORDER IS NETTED OUT IS A PER-VENUE
    /// FACT.** On an IN-PLACE amend venue the projected `qty` is the new TOTAL, executed lots
    /// included, while [`RiskContext::position_size`] already holds those lots, so not netting
    /// counts them twice and refuses economic no-ops (e.g. re-pricing a half-done exit under a
    /// halt). On a CANCEL-REPLACE venue a FRESH order of `new_qty` replaces the resting one, so the
    /// un-netted sum is CORRECT and netting would ADMIT what the gate should refuse.
    /// `AmendSemantics::already_in_position` is non-zero for exactly one variant, so a venue whose
    /// convention is not established keeps the conservative arithmetic.
    /// `crates/vike-exec/src/risk.rs`'s `still_executable` states which lanes consume it and the
    /// `filled_qty`-is-inside-`position_size` invariant it rests on.
    ///
    /// ⚠ **THE CLIENT IS ASKED FIRST; THE VENUE STRING ANSWERS ONLY WHEN IT DECLINES.**
    /// `vike_mount::make_engine` labels the engine with the REAL venue even when the mount fell
    /// back to `vike_paper::PaperExecutionClient` (a `paper`-tier or inactive account, a bridge's
    /// `ExecOutcome::Paper`, a venue this build does not compile), and
    /// `vike_mount::build_paper_maker_core` does the same, while the paper book implements a THIRD
    /// convention (its `modify` assigns the amend's quantity to the REMAINING size, so the whole
    /// `new_qty` is still coming).
    /// [`ExecutionClient::amend_semantics`] returns `None` for every venue adapter (defer to the
    /// table) and `Some(AmendSemantics::InPlaceRemaining)` for the paper exchange, which nets
    /// nothing. `crates/vike-paper/tests/paper_amend_is_not_the_venues_amend.rs` drives this mount
    /// end to end.
    #[tracing::instrument(level = "info", target = "vike_exec::oms", skip_all, fields(coid = %client_order_id))]
    pub fn modify_order(
        &mut self,
        client_order_id: &str,
        new_qty: Option<f64>,
        new_price: Option<f64>,
        now_ms: i64,
        outbox: &mut Outbox,
    ) {
        // Clone the resting request out (drops the borrow before &mut client); the venue modify
        // gets the full order context. `filled_qty` comes out in the same read (see this fn's doc).
        let (req, filled_qty) = match self.registry.get(client_order_id) {
            Some(mo) if mo.status.is_modifiable() => (mo.request.clone(), mo.filled_qty),
            _ => return, // unknown, not-yet-accepted, or terminal — nothing to modify
        };
        // The order AS MODIFIED — what the gate judges. An omitted field keeps the resting value,
        // as the venue does.
        let mut projected = req.clone();
        if let Some(q) = new_qty {
            projected.qty = q;
        }
        if let Some(p) = new_price {
            projected.price = Some(p);
        }
        let ctx = self.risk_ctx(&projected, now_ms);
        // The amend convention decides whether the executed lots are inside `projected.qty`
        // (in-place TOTAL: net them out) or not (cancel-replace / in-place REMAINING: net nothing).
        // The CLIENT is asked first — `self.venue` is only the label, and a paper mount carries the
        // real venue string (see this fn's doc); a client that declines falls to the venue table.
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

    /// ACTIVELY ask the venue to re-confirm one order's status by client-order-id (RUST-NATIVE).
    /// The core's stuck-order watchdog issues this during the confirm-grace so a WEDGED adapter is
    /// PRODDED rather than only waited on. Idempotent + fire-and-forget like `cancel_order`: only a
    /// still-LIVE order is confirmed (terminal/unknown → no-op), and this publishes NOTHING — the
    /// venue's authoritative answer returns over the ingest lane. `client.confirm` re-queries on
    /// the adapter's OWN thread, so no network touches the core fold. A no-op for clients without
    /// a re-query (the `ExecutionClient::confirm` default); the watchdog's stage-2 backstop covers
    /// those.
    #[tracing::instrument(level = "info", target = "vike_exec::oms", skip_all, fields(coid = %client_order_id))]
    pub fn confirm_order(&mut self, client_order_id: &str) {
        if self.is_live(client_order_id) {
            self.client.confirm(client_order_id);
        }
    }
}

#[cfg(test)]
mod execution_engine_tests;

#[cfg(test)]
mod mark_basis_tests;
