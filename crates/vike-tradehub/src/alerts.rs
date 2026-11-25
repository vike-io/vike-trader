//! The HEADLESS alerting mount — `vike_alerting` wired into the daemon so rules keep
//! evaluating on the production box with no GUI attached.
//!
//! The engine itself (rule model, pure evaluator, delivery sinks) lives, and is CI-tested, in the
//! `vike-alerting` crate (⚠ this said "CI-tested in `vike-ops`" until 2026-09-28: the engine left
//! that crate on 2026-07-29, and the re-export that kept it reachable there on 2026-09-25); until
//! this module nothing in the workspace ever CONSTRUCTED an `AlertEngine`, so a rule could only
//! ever fire inside a running GUI — and die when the window closed. This is the half that matters
//! for an unattended node.
//!
//! ## Strictly OFF-FOLD (the `p99 < 10µs` gate is untouched)
//! Same contract as the execution stream's publisher: the mount consumes the daemon's ALREADY-PUBLISHED
//! state and never enters the `vike-core` fold. Concretely it is driven from the daemon's existing
//! periodic snapshot thread — the one that already reads the lossy `arc-swap` snapshot cell and
//! prints the one-line JSON summary — so alerting adds ONE `AlertEngine::on_snapshot` call per
//! summary tick on a thread the core never waits on. Nothing here holds a lock the fold takes,
//! back-pressures an ingest lane, or runs on a venue callback.
//!
//! ## Default OFF, byte-identical
//! The RULES FILE is the gate (the "absent config is the gate" idiom, not a second env flag):
//! [`maybe_mount`] returns `None` when there is no rules file, when the file has zero rules, and
//! when every rule in it is `enabled: false`. `None` ⇒ no engine, no sink constructed, no webhook
//! target resolved, and the summary thread runs exactly as it did before — see
//! `tests/alerts_mount.rs`.
//!
//! ## What is fed, and what is NOT
//! [`AlertEngine`] accepts four inputs; the daemon has a real source for the snapshot, the refused and filled orders
//! it publishes and the drift readings, and this module deliberately fabricates none of the others:
//!
//! - FED — `on_snapshot` (from the summary tick). Usable triggers: `Price` (last-price crossings, never a mid: the
//!   last trade price and then the bar close of a trade series the core holds, else the lane's last trade print,
//!   else the quote's side; a pair with no price source at all, or a symbol its venue's kept instrument list does not
//!   carry, is refused at creation, `AlertMount::refuse_unpriced`, and switches back on by itself once it has one,
//!   `AlertMount::readmit_priced`),
//!   `Drawdown` (the equity-peak latch) and `ReconciliationAlert` (held reconcile divergences, i.e. the
//!   reconcile path, on by default for a live mount).
//! - FED — `on_event` for `OrderRejected` (from the same tick): every order the snapshot publishes as
//!   `Rejected`, and every order the core denied (`CoreSnapshot::denied_orders`, in-process only), is one event,
//!   once, with its order client id, venue and words, a denial with its refusal id too
//!   ([`AlertMount::on_snapshot_stopped`]).
//! - FED — `on_event` for `Fill` (from the same tick): every rise of a listed order's filled quantity is one event,
//!   at the price that moved its average (`AlertMount::on_fills`). A venue fill of no listed order is not told.
//! - FED — `on_event` for `JournalGrowth` (from the same tick): the journal watch's crossing of its size or growth row
//!   (`crate::journal_watch`), once per crossing ([`AlertMount::on_journal_growth`]).
//! - FED — `on_event` for `JournalFenceHalt` (from the same tick): the core HALTED because its journal could not write
//!   (`CoreSnapshot::journal_fence_halt`, in-process only), once per process (`AlertMount::on_journal_fence_halt`).
//! - UNFED — `on_indicator` (`Indicator`): a headless daemon computes no streaming indicators;
//!   that value is a GUI/chart-side computation the consumer must supply.
//! - UNFED — `on_signal` (`Feed` / `FillRateBreaker` / `PolymarketResolution` / `SeriesStale`): the
//!   daemon exposes no feed-health, breaker, resolution or series-liveness transition off-fold (its
//!   reconcile mount's feed-status map carries CEX rows only, and no feed-health alert is sourced
//!   from it). `SeriesStale` is
//!   the one whose producer exists and is simply somewhere ELSE —
//!   `crates/vike-recorder/src/alerts.rs` raises it from `vike_recorder::liveness`, and this daemon
//!   depends on no recorder crate at all, so its rules load here and can never fire.
//!
//! - FED — `on_signal` for `ModelDrift` alone (book W9): the model drift watch
//!   (`crate::tradehub_cli`'s `drift_watch`) measures each such rule on its own thread, every
//!   `DRIFT_CHECK_EVERY`, and hands the reading over a channel; [`AlertMount::on_drift_readings`] evaluates it on the
//!   summary tick, against this engine's state and sinks.
//!
//! Rules of an unfed kind still LOAD and are still counted; they simply never fire here. Wiring the
//! remaining ones is follow-up work that needs a new off-fold producer, not a change to this file.
//!
//! ## Delivery
//! [`LogSink`] always (the durable record on the vike-log trace file — STDOUT stays protocol-only),
//! plus every webhook target the workspace credential store configures, built by the engine's own
//! PURE `webhook_configs_from_env` over the map this daemon's `main.rs` already resolved (no second
//! config path invented here, and no store read inside the alerting library). No configured target
//! ⇒ log-only, which is still a real headless notification surface. A rule naming a target that is
//! not configured is refused at mount (`refuse_unconfigured_targets`). Each target is a
//! `vike_alerting::QueuedSink` (its own thread, so the summary tick never waits on the wire) in front of a target
//! that asks again after a pause and counts an alert that failed every ask (`vike_alerting::queued_ureq_webhook_sinks`,
//! the recorder's builder too)
//! ([`AlertMount::deliveries_failed`]).

use std::collections::{HashMap, HashSet};
use std::path::Path;

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use vike_alerting::{
    AlertEngine, AlertEvent, AlertRule, AlertRuleSet, AlertSignal, AlertSink, Compare,
    DEFAULT_QUEUE_CAPACITY, FiredAlert, GroupCurveFact, LogSink, QueuedSinkOutcome, QueuedSinkStop,
    ReconcileAlertFact, RuleTrigger, SnapshotFacts, StaleRateFact, WebhookConfig,
    queued_ureq_webhook_sinks,
};
use vike_execution::{ArmingNotRunningView, CoreSnapshot, SeriesKey};
use vike_model::OrderStatus;

use crate::server::symbol_admission::SymbolAdmitter;

/// The bar interval a `Price` alert asks for its pair when the core holds none: the shortest candle every
/// bar-serving venue states, so the close the alert reads is never older than a minute.
const ALERT_BAR_INTERVAL: &str = "1m";

/// How many characters of a refusal's words an alert carries: the venue's message whole, never a whole response
/// body a bridge put in the reason.
const ALERT_REASON_CHARS: usize = 200;

/// How long the summary thread waits for the webhook targets' delivery threads at the stop
/// ([`AlertMount::stop_delivery`]), for the whole set: two idle targets each join within one
/// [`vike_alerting::STOP_POLL`].
pub const DELIVERY_STOP_BUDGET: Duration = vike_alerting::STOP_POLL.saturating_mul(2);

/// The always-registered sink: every fired alert becomes one structured `tracing` record. It rides
/// the vike-log file/stderr layer, NEVER stdout (which stays the newline-JSON daemon protocol).
/// `warn` because an alert is by definition something the operator asked to be told about.
const LOG_SINK: LogSink =
    LogSink::new(tracing::Level::WARN, "ALERT", true, |r| vike_alerting::log_sink_record!(r));

/// The mounted engine. Owned by (and moved into) the daemon's periodic snapshot thread, so the
/// per-rule evaluation state lives entirely on that off-fold thread — no lock is shared with the
/// core.
pub struct AlertMount {
    engine: AlertEngine,
    /// Where a `Price` rule's pair is priced when the core holds no bar series of it, and its bar lane opened: the
    /// control door's source ([`Self::with_prices`]; production: `TradeLeaseRegistry`). `None` on a node with no
    /// gatewayhub lease (paper): its pairs are its mounts', whose bars the core holds.
    prices: Option<Arc<dyn SymbolAdmitter>>,
    /// Whether the creation check ran ([`Self::refuse_unpriced`]): on the first snapshot the core published.
    checked: bool,
    /// The `Price` rules refused at creation and still off, `(rule id, why)` ([`Self::refused`]).
    refused: Vec<(String, String)>,
    /// The model drift watch's readings ([`Self::with_drift_readings`]); `None` without a `ModelDrift` rule.
    drift_readings: Option<Receiver<AlertSignal>>,
    /// The refused orders already told, by order client id (`Self::on_refused_orders`): kept while the snapshot
    /// still lists the order, so the memory is never longer than that list.
    told_refusals: HashSet<String>,
    /// What each listed order had filled at the last tick, `(filled quantity, average fill price)` by order client id
    /// (`Self::on_fills`).
    fills_seen: HashMap<String, (f64, f64)>,
    /// The sequence of the last denied order told (`Self::on_denied_orders`): the core numbers its denials, so the
    /// next tick tells only the newer ones.
    denials_told: u64,
    /// Whether the journal fence's halt was told (`Self::on_journal_fence_halt`): once per process, as the core halts.
    fence_halt_told: bool,
    /// How many alerts failed every ask to a webhook target, shared by the targets ([`Self::deliveries_failed`]).
    deliveries_failed: Arc<AtomicU64>,
    /// The webhook targets' delivery threads ([`Self::stop_delivery`]).
    delivery_stops: Vec<QueuedSinkStop>,
}

/// What a published `CoreSnapshot` looks like to the rule evaluator.
///
/// ⚠ **This conversion lives HERE, and that is the whole of why `vike-alerting` can be a leaf.**
/// That crate used to take a `CoreSnapshot` directly, which put a `vike-core` edge on the crate
/// whose reason for being split out of `vike-ops` was *"so a watchdog could page without it"*. It
/// now asks for facts and the caller supplies them — and the caller is this file, which already
/// holds the snapshot.
///
/// ⚠ **`drawdown_curve` MUST come from `Portfolio::drawdown_curve`, never from
/// `CoreSnapshot::equity()` or `portfolio.equity_total`, and nothing downstream can check that any
/// more.** The rule follows the core's own latch onto the daemon's OWN equity curve (configured
/// capital + realized + unrealized P&L) rather than the cross-venue equity TOTAL, which on an
/// `Authoritative` block includes the venue's wallet for the whole account the credentials open.
/// Reading the total meant a third party's withdrawal from a SHARED account fired a drawdown alert
/// with no trading behind it, while a real 25% loss on the daemon's own ~9000 of book was 3.6% of a
/// 62647 total and never fired at all (the CI box, 2026-08-17). `Portfolio::pnl_total`'s fold is pinned
/// bit-identical to the engine-side scalar `CoreThread::sweep_drawdown_latch` acts on, so the latch
/// and the alert measure one quantity by construction.
///
/// ⚠ **Once the latch has swept, the curve, its base and its PnL are the latch's OWN** (its row of
/// `Portfolio::group_curves`, [`SnapshotView::primary_curve`]): a group holding a demo or live engine leaves its paper
/// engines out of the curve (`vike_core`'s `CoreThread::counts_in_curve`, decision 0285's amendment of 2026-11-16),
/// while `Portfolio::capital_base` still adds every engine's seed, so four paper accounts at 100,000 would dilute the
/// alert as they diluted the latch. With no row (no `max_drawdown`, or before the first sweep) it is
/// `Portfolio::drawdown_curve`, as before.
///
/// The two tests below are that guard, and they MOVED here from
/// `crates/vike-alerting/src/evaluation.rs` with this conversion: once the evaluator takes a plain
/// `f64`, a test there could only prove that the number passed in is the number read back.
///
/// The second field is the node's NOT RUNNING list (`PublisherHandle::not_running`; the core publishes
/// `CoreSnapshot::not_running` empty): a stopped account keeps its last block, whose frozen rate table would
/// read as a stale rate for ever, so the stale-rate rule skips it.
///
/// The third is where a pair the core holds no bar series of is priced ([`AlertMount::with_prices`]).
struct SnapshotView<'a>(
    &'a CoreSnapshot,
    &'a [ArmingNotRunningView],
    Option<&'a dyn SymbolAdmitter>,
);

/// Whether `venue`'s candles are TRADE bars: it serves candles AND trade prints
/// (`vike_venues::VenueCapabilities::live_data`). A forex venue's candles are quotes (mid or bid), never a trade.
fn trade_bars(venue: &str) -> bool {
    let live = vike_venues::capabilities_for(venue).live_data;
    live.supports(vike_venues::DataClientVerb::Bars)
        && live.supports(vike_venues::DataClientVerb::Trades)
}

/// **The last trade price, then the bar close, the core's bars state for `(venue, symbol)`** (REVIEW-FIX finding 8b):
/// on a venue whose candles are trade bars ([`trade_bars`]), the close of the newest FORMING bar whose window traded,
/// else of the newest CLOSED one that traded, any interval. A bar that did not trade (`volume` 0) is never read: a
/// bar the node folds from quote MIDS (the cTrader and Polymarket synthesizer, the paper quote fold) carries no
/// volume, so its mid, up to a whole bar old, never stands in for a trade.
fn trade_close(snap: &CoreSnapshot, venue: &str, symbol: &str) -> Option<f64> {
    if !trade_bars(venue) {
        return None;
    }
    let series = || {
        snap.bars
            .iter()
            .filter(|((v, s, _), _)| v == venue && s == symbol)
            .map(|(_, series)| series)
    };
    let traded = |bar: &&vike_marketdata::Bar| bar.volume > 0.0;
    series()
        .filter_map(|s| s.forming.as_ref().filter(traded))
        .max_by_key(|bar| bar.timestamp)
        .or_else(|| {
            series().filter_map(|s| s.closed.last().filter(traded)).max_by_key(|bar| bar.timestamp)
        })
        .map(|bar| bar.close)
}

impl SnapshotView<'_> {
    /// The latch's row of the PRIMARY group (`Portfolio::group_curves`): its base, curve and peak, the numbers it acted
    /// on. `None` with no `max_drawdown` or before the first sweep.
    fn primary_curve(&self) -> Option<&vike_execution::GroupCurve> {
        crate::summary::latch_primary_row(&self.0.portfolio)
    }
}

impl SnapshotFacts for SnapshotView<'_> {
    /// **The last traded price, the line the chart draws, NEVER a mid** (owner, 2026-11-15: an alert compares against
    /// the last trade), from the first source that has one, none of which costs the core anything:
    ///
    /// 1. the last trade price: the close of the newest FORMING bar of a trade series the core holds for the pair,
    ///    and 2. else the close of its newest CLOSED one ([`trade_close`]; every publish carries the bar cache, and
    ///    [`AlertMount::keep_alert_bars`] opens a pair's `1m` bars where its venue serves trade candles);
    /// 3. else the last trade print the venue's lane delivered ([`SymbolAdmitter::last_trade`], the price tap);
    /// 4. else the quote's SIDE for the crossing watched: the ask for [`Compare::Above`], the bid for
    ///    [`Compare::Below`] ([`SymbolAdmitter::top_of_book`]), as a buy stop and a sell stop trigger.
    ///
    /// A bar the node folds from quote mids is never read (it states no trade). `None` only while none has a price: a
    /// pair with no source at all was refused at creation ([`AlertMount::refuse_unpriced`]).
    fn last_price(&self, venue: &str, symbol: &str, op: Compare) -> Option<f64> {
        trade_close(self.0, venue, symbol).or_else(|| self.2?.last_trade(venue, symbol)).or_else(
            || {
                let (bid, ask) = self.2?.top_of_book(venue, venue, symbol)?;
                Some(match op {
                    Compare::Above => ask,
                    Compare::Below => bid,
                })
            },
        )
    }

    /// The PRIMARY group's code (`"USD"` for the dollar group): the currency [`Self::drawdown_curve`] is in, so
    /// the rule's session peak starts over when the first live engine restarts into another currency.
    fn drawdown_currency(&self) -> String {
        vike_model::ledger_label(self.0.portfolio.primary_currency().as_str()).to_string()
    }

    /// Every rate that is stale while a position is open in its currency, of every RUNNING account's newest
    /// block (decision 0183): `QuoteRateView::held` is set by the engine only on a stale row, so a stale rate
    /// nobody holds anything in is not listed.
    fn stale_rates(&self) -> Vec<StaleRateFact> {
        crate::publish::current_venue_blocks(&self.0.portfolio.venues)
            .filter(|v| !v.quote_rates.is_empty())
            .flat_map(|v| {
                let account =
                    if v.route_key.is_empty() { v.venue.clone() } else { v.route_key.clone() };
                let running = !self.1.iter().any(|n| n.route_key() == account);
                v.quote_rates.iter().filter(move |r| running && r.stale && r.held).map(move |r| {
                    StaleRateFact {
                        account: account.clone(),
                        currency: r.currency.to_string(),
                        account_ccy: v.quote_rates_in.to_string(),
                    }
                })
            })
            .collect()
    }

    fn drawdown_curve(&self) -> f64 {
        self.primary_curve().map_or_else(|| self.0.portfolio.drawdown_curve(), |g| g.curve)
    }

    /// The latch's own high-water mark of the PRIMARY group (its row of `Portfolio::group_curves`), so the rule
    /// and the latch measure one drop from one peak. `None` with no `max_drawdown` or before the first sweep.
    fn drawdown_peak(&self) -> Option<f64> {
        self.primary_curve().map(|g| g.peak)
    }

    fn capital_base(&self) -> f64 {
        self.primary_curve().map_or(self.0.portfolio.capital_base, |g| g.base)
    }

    fn pnl_total(&self) -> f64 {
        self.primary_curve().map_or_else(|| self.0.portfolio.pnl_total(), |g| g.curve - g.base)
    }

    /// The other currency groups' curves as the core's latch last measured them (decision 0183, item 13):
    /// the latch trips the whole node when any group breaches, so the rule watches each of them too. The
    /// PRIMARY group's row (`Portfolio::group_curves` lists every group since the summary line prints them
    /// all) is left out: the rule reads that group through `drawdown_curve` and [`Self::drawdown_peak`], and a
    /// second watch on it would fire a second alert for one drop.
    fn group_curves(&self) -> Vec<GroupCurveFact> {
        let primary = self.0.portfolio.primary_currency();
        self.0
            .portfolio
            .group_curves
            .iter()
            .filter(|g| g.currency != primary)
            .map(|g| GroupCurveFact {
                currency: vike_model::ledger_label(g.currency.as_str()).to_string(),
                curve: g.curve,
                peak: g.peak,
            })
            .collect()
    }

    fn reconcile_alerts(&self) -> Vec<ReconcileAlertFact<'_>> {
        self.0
            .reconcile
            .alerts
            .iter()
            .map(|a| ReconcileAlertFact {
                kind: a.kind.word(),
                detail: a.detail.as_str(),
                proposed_event_count: a.proposed_event_count,
            })
            .collect()
    }
}

impl AlertMount {
    /// Evaluate the snapshot-driven rules against one published `CoreSnapshot` and dispatch what
    /// fired. Called once per summary tick; returns what fired (the daemon ignores it — delivery
    /// already happened inside the engine — but tests assert on it).
    pub fn on_snapshot(&mut self, snap: &CoreSnapshot, now_ms: i64) -> Vec<FiredAlert> {
        self.on_snapshot_stopped(snap, &[], now_ms)
    }

    /// [`Self::on_snapshot`] for a node that lists accounts it does NOT run (`stopped`, the publisher's
    /// `not_running`): a stopped account keeps its last block, and the stale-rate rule must not read that block's
    /// frozen rates as live ones. The summary tick calls this one.
    pub fn on_snapshot_stopped(
        &mut self,
        snap: &CoreSnapshot,
        stopped: &[ArmingNotRunningView],
        now_ms: i64,
    ) -> Vec<FiredAlert> {
        // Creation is judged on the first snapshot the core PUBLISHED (the cell starts empty at spawn): until then
        // nothing is evaluated, so no rule ever reads a node that has not said what it trades.
        let first = !self.checked;
        if first {
            if snap.portfolio.venues.is_empty() && snap.bars.is_empty() && snap.mounts.is_empty() {
                return Vec::new();
            }
            self.checked = true;
            self.refuse_unpriced(snap);
        } else {
            self.readmit_priced(snap);
        }
        self.keep_alert_bars(snap);
        let mut fired =
            self.engine.on_snapshot(&SnapshotView(snap, stopped, self.prices.as_deref()), now_ms);
        fired.extend(self.on_refused_orders(snap, now_ms));
        fired.extend(self.on_denied_orders(snap, now_ms));
        fired.extend(self.on_fills(snap, first, now_ms));
        fired.extend(self.on_journal_fence_halt(snap, now_ms));
        fired
    }

    /// **The node HALTED because its journal could not write**: one `JournalFenceHalt` event, which a
    /// `journal_fence_halt` rule tells, at the first tick that sees it (`CoreSnapshot::journal_fence_halt`, the typed
    /// fact; the body is the core's `fault`, its own words: what happens and what to do). Once per process: the halt
    /// lasts until the restart, and a clean restart boots without it (a crash's boots halted).
    fn on_journal_fence_halt(&mut self, snap: &CoreSnapshot, now_ms: i64) -> Vec<FiredAlert> {
        if !snap.journal_fence_halt || self.fence_halt_told {
            return Vec::new();
        }
        self.fence_halt_told = true;
        let fault = snap.fault.as_deref().unwrap_or("the command journal could not write");
        self.engine.on_event(&AlertEvent::JournalFenceHalt { fault }, now_ms)
    }

    /// **Every rise of an order's filled quantity is one `Fill` event** (`AlertEngine::on_event`), read off the orders
    /// the snapshot publishes (an ended order stays listed for an hour): the quantity it rose by, at the price that moved
    /// its average there, so several fills between two ticks are one event at their average. The FIRST snapshot only
    /// notes what each order had filled (a resting order a restart restored keeps the fills of the run before); an
    /// order listed later is told whole (an account started later with a restored, partly filled order would be told
    /// that part again). A venue fill of no order this node lists is no row and is not told. Nothing without an
    /// enabled `Fill` rule.
    fn on_fills(&mut self, snap: &CoreSnapshot, first: bool, now_ms: i64) -> Vec<FiredAlert> {
        let wanted = |r: &AlertRule| r.enabled && matches!(r.trigger, RuleTrigger::Fill { .. });
        if !self.engine.rules().iter().any(wanted) {
            return Vec::new();
        }
        // Rebuilt every tick from the listed orders, so the memory is never longer than that list.
        let mut seen = HashMap::with_capacity(self.fills_seen.len());
        let mut fired = Vec::new();
        for order in snap.orders.iter().filter(|o| o.filled_qty > 0.0) {
            let (filled, average) = (order.filled_qty, order.average_fill_price);
            let (id, (before, before_average)) = self
                .fills_seen
                .remove_entry(order.order_client_id.as_str())
                .unwrap_or_else(|| (order.order_client_id.clone(), (0.0, 0.0)));
            seen.insert(id, (filled, average));
            if first || filled <= before {
                continue;
            }
            let last_qty = filled - before;
            let event = AlertEvent::Fill {
                venue: &order.venue,
                symbol: &order.symbol,
                side: order.side,
                last_qty,
                last_price: (filled * average - before * before_average) / last_qty,
            };
            fired.extend(self.engine.on_event(&event, now_ms));
        }
        self.fills_seen = seen;
        fired
    }

    /// **Every order the venue rejected is told ONCE, as the event of an `OrderRejected` rule**
    /// (`AlertEngine::on_event`), with its order client id, its venue, the refusal's catalog id when its event carried
    /// one (`vike_execution::OrderView::refusal_id`) and the refusal's words (at most [`ALERT_REASON_CHARS`] of them). Read off the orders the snapshot publishes, where an ended order stays for an
    /// hour (`crates/vike-core/src/snapshot/order_window.rs`'s `ENDED_ORDER_PUBLISH_MS`), so a tick every few seconds
    /// sees each one; a restart restores only the orders still resting (decision 0120), so every refused order listed
    /// was refused in this run. A DENIED order never enters a registry, so it is told from its own list
    /// ([`Self::on_denied_orders`]).
    fn on_refused_orders(&mut self, snap: &CoreSnapshot, now_ms: i64) -> Vec<FiredAlert> {
        let refused: Vec<&vike_execution::OrderView> = snap
            .orders
            .iter()
            .map(Arc::as_ref)
            .filter(|o| o.status == OrderStatus::Rejected)
            .collect();
        self.told_refusals.retain(|id| refused.iter().any(|o| o.order_client_id == *id));
        let mut fired = Vec::new();
        for order in refused {
            if !self.told_refusals.insert(order.order_client_id.clone()) {
                continue;
            }
            let reason: String = order
                .reason
                .as_deref()
                .unwrap_or("no reason given")
                .chars()
                .take(ALERT_REASON_CHARS)
                .collect();
            let event = AlertEvent::OrderRejected {
                order_client_id: &order.order_client_id,
                venue: &order.venue,
                refusal_id: order.refusal_id.as_deref(),
                reason: &reason,
            };
            fired.extend(self.engine.on_event(&event, now_ms));
        }
        fired
    }

    /// **Every order the core denied is told ONCE, as the event of an `OrderRejected` rule**: the gate's veto of a
    /// strategy's order or of an operator's (after the door's Ack), a mount's refused intent. Read off
    /// `CoreSnapshot::denied_orders`, the core's newest denials numbered in order, so a tick tells those past the last
    /// one it told; a denial that rolled off the list between two ticks (more than the core keeps, a few seconds
    /// apart) is never told. The event carries the refusal's catalog id beside its words.
    fn on_denied_orders(&mut self, snap: &CoreSnapshot, now_ms: i64) -> Vec<FiredAlert> {
        let mut fired = Vec::new();
        let told = self.denials_told;
        let listed = snap.denied_orders.as_deref().unwrap_or_default();
        for denied in listed.iter().filter(|d| d.sequence > told) {
            let event = &denied.event;
            let reason: String = event.reason.chars().take(ALERT_REASON_CHARS).collect();
            let alert = AlertEvent::OrderDenied {
                order_client_id: &event.order_client_id,
                venue: &denied.venue,
                refusal_id: event.refusal_id.as_deref(),
                reason: if reason.is_empty() { "no reason given" } else { reason.as_str() },
            };
            fired.extend(self.engine.on_event(&alert, now_ms));
            self.denials_told = denied.sequence;
        }
        fired
    }

    /// **The journal went over its size or growth row**: one `JournalGrowth` event, which a `journal_growth` rule
    /// tells. The watch (`crate::journal_watch::JournalWatch::tick`) hands each crossing over once, so the rule needs
    /// no latch.
    pub fn on_journal_growth(
        &mut self,
        reading: &crate::journal_watch::JournalReading,
        now_ms: i64,
    ) -> Vec<FiredAlert> {
        let event = AlertEvent::JournalGrowth {
            total_bytes: reading.total_bytes,
            segments: reading.segments,
            bytes_per_hour: reading.bytes_per_hour,
        };
        self.engine.on_event(&event, now_ms)
    }

    /// How many alerts failed every ask to a webhook target since the mount, each said at `warn` when it gave up: the
    /// shared count, which the publisher puts on every frame (`PublisherHandle::count_alert_deliveries`).
    pub fn deliveries_failed(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.deliveries_failed)
    }

    /// **Stop the webhook targets' delivery threads against ONE `budget` for the set, and say what that cost**: the
    /// summary thread calls it when its loop ends, inside the teardown's bounded join. An idle target joins within one
    /// [`vike_alerting::STOP_POLL`]; one caught sending (or between two asks) is left running and said. A session in
    /// which an alert failed every ask says the total once more.
    pub fn stop_delivery(self, budget: Duration) {
        let deadline = Instant::now() + budget;
        for stop in self.delivery_stops {
            match stop.shutdown(deadline.saturating_duration_since(Instant::now())) {
                QueuedSinkOutcome::Joined { dropped: 0, .. } => {}
                QueuedSinkOutcome::Joined { name, dropped, .. } => tracing::warn!(
                    target_name = %name, dropped,
                    "alert delivery stopped: {dropped} alert(s) were never sent to this target this session (a full \
                     queue, or still queued at the stop)"
                ),
                QueuedSinkOutcome::Abandoned { name, dropped, mid_delivery, .. } => tracing::warn!(
                    target_name = %name, dropped, mid_delivery,
                    "alert delivery did not stop inside its budget and its thread was left running; with \
                     mid_delivery the alert it was sending may not arrive"
                ),
            }
        }
        let failed = self.deliveries_failed.load(Ordering::Relaxed);
        if failed > 0 {
            tracing::warn!(
                failed,
                "alerting stopped: {failed} alert(s) failed every ask to their webhook target this session"
            );
        }
    }

    /// Price the rules' pairs through `prices` too (production: the control door's source, `TradeLeaseRegistry`), so
    /// a pair with no bar series in the core reads the last trade print and then the quote's side, and its `1m` bars
    /// are opened where its venue serves them. Without it a rule's pair is priced by the core's bars alone.
    #[must_use]
    pub fn with_prices(mut self, prices: Option<Arc<dyn SymbolAdmitter>>) -> Self {
        self.prices = prices;
        self
    }

    /// The `Price` rules refused at creation and still off, `(rule id, why)`: each was said at `warn`, and leaves this
    /// list (switched back on, said once) when its pair gets a price source ([`Self::readmit_priced`]).
    pub fn refused(&self) -> &[(String, String)] {
        &self.refused
    }

    /// **A `Price` rule on a pair this node has NO price for is refused at creation, with the reason** (it would sit
    /// there and never fire): off, and said once at `warn`, until its pair gets one ([`Self::readmit_priced`]). A pair HAS a price when the core holds a bar series
    /// of it or a mount trades it, or when the node runs a price feed for its venue ([`SymbolAdmitter::price_source`])
    /// and that venue serves trade candles ([`Self::keep_alert_bars`] opens them) or its lane already delivered a trade or a
    /// quote of it.
    fn refuse_unpriced(&mut self, snap: &CoreSnapshot) {
        let rules = self.engine.rules().to_vec();
        for rule in rules.iter().filter(|r| r.enabled) {
            let RuleTrigger::Price { venue, symbol, .. } = &rule.trigger else { continue };
            let Some(why) = unpriced(snap, self.prices.as_deref(), venue, symbol) else { continue };
            tracing::warn!(
                rule = %rule.id, %venue, %symbol,
                "alert rule REFUSED: {why}. It would never fire, so it is off until its pair gets a price source \
                 (an admission, its bars, a mount, a feed of its venue): then it switches back on by itself"
            );
            self.engine.set_enabled(&rule.id, false);
            self.refused.push((rule.id.clone(), why));
        }
    }

    /// **A refused rule switches back ON by itself once its pair has a price source** (the symbol is admitted, its
    /// bars arrive, a mount trades it, its lane delivers a trade or a quote), read off the snapshot the tradehub
    /// already publishes and the price source, never the core; said once at `warn`. Its own evaluation state starts
    /// over (it seeds on its next observation); every other rule's is untouched. Per summary tick, and only while
    /// something is refused.
    fn readmit_priced(&mut self, snap: &CoreSnapshot) {
        if self.refused.is_empty() {
            return;
        }
        let (engine, prices) = (&mut self.engine, self.prices.as_deref());
        let rules = engine.rules().to_vec();
        self.refused.retain(|(id, _)| {
            let Some(RuleTrigger::Price { venue, symbol, .. }) =
                rules.iter().find(|r| &r.id == id).map(|r| &r.trigger)
            else {
                return true;
            };
            if unpriced(snap, prices, venue, symbol).is_some() {
                return true;
            }
            tracing::warn!(
                rule = %id, %venue, %symbol,
                "alert rule back ON: {venue} {symbol} now has a price source"
            );
            engine.set_enabled(id, true);
            false
        });
    }

    /// **Every enabled `Price` rule's pair gets its bar close**: a pair the core holds no bar series of, on a venue
    /// that serves TRADE candles ([`trade_bars`]), has its `1m` bars opened through the price source (the way a mount the boot did not wire
    /// gets its candles, decision 0245). Idempotent: the source opens each series once and asks a refused one again
    /// about once a minute. Nothing without a source.
    fn keep_alert_bars(&self, snap: &CoreSnapshot) {
        let Some(prices) = self.prices.as_deref() else { return };
        let wanted: Vec<SeriesKey> = self
            .engine
            .rules()
            .iter()
            .filter(|r| r.enabled)
            .filter_map(|r| match &r.trigger {
                RuleTrigger::Price { venue, symbol, .. } => Some((venue, symbol)),
                _ => None,
            })
            .filter(|(venue, symbol)| {
                trade_bars(venue) && trade_close(snap, venue, symbol).is_none()
            })
            .map(|(venue, symbol)| (venue.clone(), symbol.clone(), ALERT_BAR_INTERVAL.to_string()))
            .collect();
        if !wanted.is_empty() {
            prices.keep_bars(&wanted);
        }
    }

    /// How many rules were loaded (ENABLED and disabled alike) — for the mount banner.
    pub fn rule_count(&self) -> usize {
        self.engine.rules().len()
    }

    /// How many of those are enabled, i.e. can actually fire.
    pub fn enabled_count(&self) -> usize {
        self.engine.rules().iter().filter(|r| r.enabled).count()
    }

    /// The loaded rules, enabled or not (what the model drift watch picks its rules from).
    pub fn rules(&self) -> &[AlertRule] {
        self.engine.rules()
    }

    /// Take the model drift readings the drift watch measures OFF this thread (`ModelDrift` rules, book W9):
    /// each one is evaluated by [`Self::on_drift_readings`] on the summary tick, against this one engine's
    /// rule state and sinks. Without it (no enabled `ModelDrift` rule) nothing is drained.
    #[must_use]
    pub fn with_drift_readings(mut self, readings: Option<Receiver<AlertSignal>>) -> Self {
        self.drift_readings = readings;
        self
    }

    /// Evaluate every model drift reading that arrived since the last tick and dispatch what fired. A watch that
    /// ended (its sender dropped) leaves nothing to read, which is no alert.
    pub fn on_drift_readings(&mut self, now_ms: i64) -> Vec<FiredAlert> {
        let Some(readings) = self.drift_readings.as_ref() else { return Vec::new() };
        let mut fired = Vec::new();
        while let Ok(reading) = readings.try_recv() {
            fired.extend(self.engine.on_signal(&reading, now_ms));
        }
        fired
    }
}

/// **Why a `Price` rule on `(venue, symbol)` would have NO price on this node**, or `None` when it has a source
/// ([`AlertMount::refuse_unpriced`]'s rule).
fn unpriced(
    snap: &CoreSnapshot,
    prices: Option<&dyn SymbolAdmitter>,
    venue: &str,
    symbol: &str,
) -> Option<String> {
    if trade_close(snap, venue, symbol).is_some()
        || snap.mounts.iter().any(|m| m.venue == venue && m.symbol == symbol)
    {
        return None;
    }
    if let Some(why) = unlisted(venue, symbol) {
        return Some(why);
    }
    let Some(prices) = prices else {
        return Some(format!(
            "this node takes no venue's prices but its mounts' (no gatewayhub price feed), and no mount trades \
             {venue} {symbol}"
        ));
    };
    if let Err(why) = prices.price_source(venue) {
        return Some(why);
    }
    if trade_bars(venue)
        || prices.last_trade(venue, symbol).is_some()
        || prices.top_of_book(venue, venue, symbol).is_some()
    {
        return None;
    }
    Some(format!(
        "{venue} serves no trade candles, and this node holds no trade print or quote of {symbol} (no mount trades it \
         and no account holds its price lane)"
    ))
}

/// **Why `symbol` is not a symbol `venue` lists**: the venue's kept instrument list (`vike_catalog::kept_list`) carries
/// no row whose USER name it is (decision 0290), so its lanes never deliver a price (`binance BTCUSD`, where binance
/// lists `BTCUSDT`). The words name up to three listed user names close to it. `None` when the list carries it, or
/// when no list is held (nothing to judge by). A refused rule is judged again on every tick ([`AlertMount::readmit_priced`]), so a list refreshed with the
/// symbol switches it back on.
fn unlisted(venue: &str, symbol: &str) -> Option<String> {
    let rows = vike_catalog::kept_list(venue)?;
    if vike_catalog::kept_row_of_user_name(&rows, venue, symbol).is_ok() {
        return None;
    }
    if rows.iter().any(|row| row.venue == venue && row.naming.is_none()) {
        // A list written before names were stored names nothing: say the refresh that names it.
        return Some(vike_catalog::predates_symbol_names(venue));
    }
    let bare = |s: &str| -> String {
        s.chars().filter(char::is_ascii_alphanumeric).map(|c| c.to_ascii_uppercase()).collect()
    };
    let wanted = bare(symbol);
    let mut close: Vec<String> = rows
        .iter()
        .filter(|row| row.venue == venue)
        .filter_map(|row| vike_catalog::user_name_of_row(row).ok())
        .filter(|user| {
            let user = bare(user);
            !user.is_empty()
                && !wanted.is_empty()
                && (user.starts_with(&wanted) || wanted.starts_with(&user))
        })
        .collect();
    close.sort_by_key(|user| (user.len().abs_diff(symbol.len()), user.clone()));
    close.dedup();
    close.truncate(3);
    let spelled = if close.is_empty() {
        String::new()
    } else {
        let names: Vec<String> = close.iter().map(|raw| format!("`{raw}`")).collect();
        format!(" (it lists {})", names.join(", "))
    };
    Some(format!(
        "{venue}'s kept instrument list has no `{symbol}`{spelled}, so no price of it ever reaches this node: write the \
         rule with its user name (decision 0290)"
    ))
}

/// Load a rule set from a caller-resolved path — a thin, testable wrapper over
/// [`vike_alerting::persist::load_path`] so the daemon binary owns the path resolution (and
/// therefore the env read). Absent ⇒ `None`, the OFF state. A file that is not read (an older
/// build's words, or corrupt) is `None` too, after one `error` line naming the file and what to
/// do: a bad file never bricks the daemon, and nothing writes over it. A rule the file holds but
/// that could never fire is switched off here ([`refuse_unknown_divergence_kinds`]).
pub fn load_rules(path: &Path) -> Option<AlertRuleSet> {
    let mut set = vike_alerting::persist::load_path(path).unwrap_or_else(|hint| {
        tracing::error!("{hint}");
        None
    })?;
    refuse_unknown_divergence_kinds(&mut set);
    Some(set)
}

/// **A `ReconciliationAlert` rule naming a divergence kind that does not exist is refused at load**: it
/// would never fire (the evaluator matches the held alert's word exactly), so it is OFF, said once at
/// `error` with the words it may name. `vike-alerting` is vike-free and keeps the kind a `String`, so
/// this loader holds it to `vike_model::ReconcileDivergenceKind`. In memory only: the file is not
/// rewritten, and a rule naming no kind (every held alert) is untouched.
fn refuse_unknown_divergence_kinds(set: &mut AlertRuleSet) {
    for rule in set.rules.iter_mut().filter(|r| r.enabled) {
        let RuleTrigger::ReconciliationAlert { divergence_kind: Some(kind) } = &rule.trigger else {
            continue;
        };
        if vike_model::ReconcileDivergenceKind::from_word(kind).is_some() {
            continue;
        }
        tracing::error!(
            rule = %rule.id, %kind,
            "alert rule REFUSED: `{kind}` is no reconcile divergence kind, so it would never fire; it is off \
             until the rule names one of {}",
            vike_model::ReconcileDivergenceKind::WORDS.join(", ")
        );
        rule.enabled = false;
    }
}

/// Mount the engine IFF `rules` carries at least one ENABLED rule — the DEFAULT-OFF gate.
///
/// `None` is returned for all three "unconfigured" shapes (no file ⇒ `rules: None`; a file with an
/// empty `rules` array; a file whose every rule is `enabled: false`), and in that case NOTHING is
/// constructed: no engine, no [`LogSink`], and — critically — no `WebhookSink`.
///
/// `webhooks` is a THUNK, not a value, precisely so the gate runs FIRST: an unconfigured daemon
/// never calls it, so it never opens the credential store and no Telegram token is ever loaded into
/// memory on the OFF path. The caller (the binary) owns that I/O when it does happen — the thunk is
/// `|| webhook_configs_from_env(&workspace_credentials())` in `main.rs`, so the store read is at
/// `Layer::Binary` where the settings-registry rule puts it.
///
/// Past the gate, a rule naming a target the thunk did not configure is refused
/// (`refuse_unconfigured_targets`), and each configured target is built by `vike_alerting::queued_ureq_webhook_sinks`:
/// a queue in front of a target that asks again, so the summary tick only enqueues and the asks again sleep on the
/// target's own thread; an alert that failed every ask is counted in [`AlertMount::deliveries_failed`].
pub fn maybe_mount<F>(rules: Option<AlertRuleSet>, webhooks: F) -> Option<AlertMount>
where
    F: FnOnce() -> Vec<WebhookConfig>,
{
    let mut set = rules?;
    if !set.rules.iter().any(|r| r.enabled) {
        return None;
    }
    let configs = webhooks();
    refuse_unconfigured_targets(&mut set, &configs);
    let failed = Arc::new(AtomicU64::new(0));
    let mut sinks: Vec<Box<dyn AlertSink>> = vec![Box::new(LOG_SINK)];
    let (targets, stops) = queued_ureq_webhook_sinks(configs, DEFAULT_QUEUE_CAPACITY, &failed);
    sinks.extend(targets);
    let mut mount = mount_with_sinks(set, sinks);
    mount.deliveries_failed = failed;
    mount.delivery_stops = stops;
    Some(mount)
}

/// **A rule that delivers to a webhook target this node does not configure is refused at mount**: no sink is built
/// for that name, so the rule would never be told there. It is OFF, said once at `error` with the targets that are
/// configured and the credential rows that configure one (`vike_alerting::WEBHOOK_KEYS`, names only). In memory only,
/// as [`refuse_unknown_divergence_kinds`]: the file is not rewritten.
fn refuse_unconfigured_targets(set: &mut AlertRuleSet, configs: &[WebhookConfig]) {
    let configured: Vec<&str> = configs.iter().map(|c| c.name.as_str()).collect();
    for rule in set.rules.iter_mut().filter(|r| r.enabled) {
        let missing: Vec<&str> = rule
            .targets
            .webhooks
            .iter()
            .map(String::as_str)
            .filter(|name| !configured.contains(name))
            .collect();
        if missing.is_empty() {
            continue;
        }
        let missing = missing.join(", ");
        let present = if configured.is_empty() { "none".into() } else { configured.join(", ") };
        tracing::error!(
            rule = %rule.id, targets = %missing,
            "alert rule REFUSED: it delivers to the webhook target(s) {missing}, which this node does not \
             configure (configured: {present}), so it would never be told there; it is off until each \
             target it names is configured (the credential rows {}) or it names only configured ones",
            vike_alerting::WEBHOOK_KEYS.join(", ")
        );
        rule.enabled = false;
    }
}

/// [`maybe_mount`] with the sinks supplied directly — the TEST seam (an `InProcessSink` or a
/// counting double stands in for the real webhook transport, so no test ever touches the network).
/// Applies the SAME default-OFF gate, so a test proving the off path proves the production one.
pub fn maybe_mount_with_sinks(
    rules: Option<AlertRuleSet>,
    sinks: Vec<Box<dyn AlertSink>>,
) -> Option<AlertMount> {
    let set = rules?;
    if !set.rules.iter().any(|r| r.enabled) {
        return None;
    }
    Some(mount_with_sinks(set, sinks))
}

/// Shared tail of the two constructors, past the gate: build the engine and register the sinks.
fn mount_with_sinks(set: AlertRuleSet, sinks: Vec<Box<dyn AlertSink>>) -> AlertMount {
    let mut engine = AlertEngine::from_rule_set(set);
    for sink in sinks {
        engine.add_sink(sink);
    }
    AlertMount {
        engine,
        prices: None,
        checked: false,
        refused: Vec::new(),
        drift_readings: None,
        told_refusals: HashSet::new(),
        fills_seen: HashMap::new(),
        denials_told: 0,
        fence_halt_told: false,
        deliveries_failed: Arc::new(AtomicU64::new(0)),
        delivery_stops: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_alerting::{AlertRule, Compare, RuleTrigger};

    /// The tradehub's alert line, exactly as it was before the sink became `vike_alerting::LogSink`
    /// (B34): `warn`, `"ALERT: {body}"`, the fields `rule`, `title` and `ts_ms`, and this module's
    /// target (the record is written at this daemon's callsite).
    #[test]
    fn the_alert_line_is_a_warn_with_the_title() {
        let alert = FiredAlert {
            rule_id: "r1".into(),
            title: "BTC breakout".into(),
            body: "binance BTCUSDT crossed above 100000".into(),
            ts_ms: 42,
            targets: vike_alerting::AlertTargets::default(),
        };
        let ((), events) = vike_log::capture::captured(|| LOG_SINK.deliver(&alert));
        let [line] = events.as_slice() else { panic!("one line per alert: {events:?}") };
        assert_eq!(line.level, tracing::Level::WARN);
        assert_eq!(line.message, "ALERT: binance BTCUSDT crossed above 100000");
        let fields: Vec<(&str, &str)> =
            line.fields.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        assert_eq!(fields, [("rule", "r1"), ("title", "BTC breakout"), ("ts_ms", "42")]);
        assert_eq!(line.target, "vike_tradehub::alerts");
    }

    /// **A rule aimed at a webhook target this node does not configure is OFF at mount, said at `error`**; a rule
    /// naming a configured target and one naming none mount as written.
    #[test]
    fn a_rule_for_a_webhook_target_that_is_not_configured_is_refused_at_mount() {
        let rule = |id: &str, targets: &[&str]| {
            let mut rule = AlertRule::new(id, RuleTrigger::OrderRejected);
            rule.targets.webhooks = targets.iter().map(|t| (*t).to_string()).collect();
            rule
        };
        let set = AlertRuleSet {
            rules: vec![
                rule("told", &["telegram"]),
                rule("nowhere", &["webhook"]),
                rule("log", &[]),
            ],
            ..Default::default()
        };
        let telegram = || {
            vec![WebhookConfig {
                name: "telegram".into(),
                kind: vike_alerting::WebhookKind::Telegram {
                    token: "not-a-token".into(),
                    chat_id: "1".into(),
                },
            }]
        };
        let (mount, events) = vike_log::capture::captured(|| maybe_mount(Some(set), telegram));
        let mount = mount.expect("enabled rules mount");
        let enabled: Vec<(&str, bool)> =
            mount.rules().iter().map(|r| (r.id.as_str(), r.enabled)).collect();
        assert_eq!(enabled, [("told", true), ("nowhere", false), ("log", true)]);
        let [line] = events.as_slice() else { panic!("one line for the one refusal: {events:?}") };
        assert_eq!(line.level, tracing::Level::ERROR);
        assert_eq!(line.field("rule"), Some("nowhere"));
        assert_eq!(line.field("targets"), Some("webhook"));
    }

    #[test]
    fn the_gate_is_closed_for_every_unconfigured_shape() {
        // no file at all
        assert!(maybe_mount(None, Vec::new).is_none());
        // a file with zero rules
        assert!(maybe_mount(Some(AlertRuleSet::default()), Vec::new).is_none());
        // a file whose only rule is disabled
        let mut disabled = AlertRule::new("p", RuleTrigger::Drawdown { fraction: 0.1 });
        disabled.enabled = false;
        let set = AlertRuleSet { rules: vec![disabled], ..Default::default() };
        assert!(maybe_mount(Some(set), Vec::new).is_none());
    }

    /// **A held-alert rule naming no real divergence kind is OFF at load, said at `error`** (B1.03: the kind
    /// is the model's word, the variant name); a real kind and a rule naming none load as written, and the
    /// file is not rewritten. Through `load_rules`, the daemon's one door.
    #[test]
    fn a_reconcile_rule_naming_an_unknown_divergence_kind_is_refused_at_load() {
        let held = |id: &str, kind: Option<&str>| {
            AlertRule::new(
                id,
                RuleTrigger::ReconciliationAlert { divergence_kind: kind.map(str::to_string) },
            )
        };
        let set = AlertRuleSet {
            rules: vec![
                held("real", Some("PositionDrift")),
                held("typo", Some("PostionDrift")),
                held("any", None),
            ],
            ..Default::default()
        };
        let dir = tempfile::tempdir().expect("a temp dir");
        let path = dir.path().join("rules.json");
        vike_alerting::persist::save_path(&set, &path).expect("the rules file is written");
        let written = std::fs::read(&path).expect("the file reads");
        let (loaded, events) = vike_log::capture::captured(|| load_rules(&path));
        let loaded = loaded.expect("the file loads");
        let enabled: Vec<(&str, bool)> =
            loaded.rules.iter().map(|r| (r.id.as_str(), r.enabled)).collect();
        assert_eq!(enabled, [("real", true), ("typo", false), ("any", true)]);
        let [line] = events.as_slice() else { panic!("one line for the one refusal: {events:?}") };
        assert_eq!(line.level, tracing::Level::ERROR);
        assert_eq!(line.field("rule"), Some("typo"));
        assert!(
            line.message.contains("`PostionDrift`") && line.message.contains("PositionDrift,"),
            "{}",
            line.message
        );
        assert_eq!(
            std::fs::read(&path).expect("the file reads"),
            written,
            "the file is not rewritten"
        );
    }

    #[test]
    fn one_enabled_rule_mounts_and_counts_both_ways() {
        let mut disabled = AlertRule::new("d", RuleTrigger::OrderRejected);
        disabled.enabled = false;
        let set = AlertRuleSet {
            rules: vec![
                AlertRule::new(
                    "p",
                    RuleTrigger::Price {
                        venue: "binance".into(),
                        symbol: "BTCUSDT".into(),
                        comparison: Compare::Above,
                        level: 100.0,
                    },
                ),
                disabled,
            ],
            ..Default::default()
        };
        let mount = maybe_mount_with_sinks(Some(set), Vec::new()).expect("one enabled rule mounts");
        assert_eq!(mount.rule_count(), 2, "both rules loaded");
        assert_eq!(mount.enabled_count(), 1, "only one can fire");
    }

    /// ⚠ **A THIRD PARTY moving money in a shared venue account must not fire a drawdown rule.**
    /// The the CI box shape (2026-08-17): a bybit block that `VIKE_RECONCILE=1` flipped to
    /// `Authoritative` by adopting the SHARED UNIFIED account's 53647.10600813 USDT
    /// `walletBalance`, next to paper mounts. `equity_total` carries that wallet, so a withdrawal
    /// collapses it with no trading behind it at all.
    ///
    /// ⚠ **MOVED here from `crates/vike-alerting/src/evaluation.rs` on 2026-09-23**, with the conversion
    /// it guards. That crate no longer sees a `CoreSnapshot`, so the distinction this test turns on
    /// — `Portfolio::drawdown_curve` against `equity_total`, two numbers its own aggregation law
    /// computes — cannot be expressed there at all: an evaluator handed a plain `f64` can only be
    /// shown to read back what it was given. Here it is a property of `SnapshotView`.
    ///
    /// This test FAILS if the conversion reads the wrong field: 31000 is 43% off a 54647 peak.
    #[test]
    fn a_venue_wallet_movement_with_no_trading_behind_it_never_fires() {
        let set = AlertRuleSet {
            rules: vec![AlertRule::new("d", RuleTrigger::Drawdown { fraction: 0.10 })],
            ..Default::default()
        };
        let mut mount = maybe_mount_with_sinks(Some(set), Vec::new()).expect("one enabled rule");

        // The daemon's own book is FLAT and stays flat: no position, no fill, no fee, no funding.
        let flat = |wallet: f64| {
            let mut s = CoreSnapshot::empty("binance", "BTCUSDT");
            s.portfolio.capital_base = 1_000.0;
            s.portfolio.venues = vec![
                vike_execution::VenueBlock { venue: "binance".into(), ..Default::default() },
                vike_execution::VenueBlock {
                    venue: "bybit".into(),
                    // `balance_mode` deliberately unset (`Delta`, the `Default`): `pnl_total` is
                    // MODE-BLIND — that is exactly the property under test, and it means the guard
                    // does not depend on correctly classifying the block.
                    balance: wallet,
                    equity: wallet,
                    ..Default::default()
                },
            ];
            s.portfolio.equity_total = wallet + 1_000.0;
            s
        };

        assert!(mount.on_snapshot(&flat(53_647.10600813), 1).is_empty(), "peak established, flat");
        assert!(
            mount.on_snapshot(&flat(30_000.0), 2).is_empty(),
            "a 23647 wallet withdrawal is 43% off the equity TOTAL and must fire nothing: the \
             daemon traded nothing, so its own curve never moved"
        );
    }

    /// The other half, and the one that must NOT be lost in the process: a real loss on the
    /// daemon's OWN book still fires, at the configured fraction of its own capital — even while a
    /// large adopted wallet sits beside it making the loss look like noise in `equity_total`.
    ///
    /// Also MOVED from `vike-alerting` with the conversion. It FAILS if the conversion reads the
    /// total: a 250 loss is 25% of the daemon's 1000 of book but 0.46% of a 54647 total.
    #[test]
    fn a_real_loss_on_the_daemons_own_book_still_fires_beside_a_large_wallet() {
        let set = AlertRuleSet {
            rules: vec![AlertRule::new("d", RuleTrigger::Drawdown { fraction: 0.20 })],
            ..Default::default()
        };
        let mut mount = maybe_mount_with_sinks(Some(set), Vec::new()).expect("one enabled rule");

        let book = |own_pnl: f64| {
            let mut s = CoreSnapshot::empty("binance", "BTCUSDT");
            s.portfolio.capital_base = 1_000.0;
            s.portfolio.venues = vec![vike_execution::VenueBlock {
                venue: "bybit".into(),
                balance: 53_647.10600813,
                unrealized: own_pnl,
                equity: 53_647.10600813 + own_pnl,
                ..Default::default()
            }];
            s.portfolio.equity_total = 53_647.10600813 + own_pnl;
            s
        };

        assert!(mount.on_snapshot(&book(0.0), 1).is_empty(), "flat: peak = 1000");
        let fired = mount.on_snapshot(&book(-250.0), 2);
        assert_eq!(fired.len(), 1, "a 25% loss of the daemon's own capital must fire a 20% rule");
        assert!(fired[0].body.contains("own-PnL drawdown 25.00%"), "body: {}", fired[0].body);
        assert!(
            fired[0].body.contains("capital_base 1000"),
            "body names the base: {}",
            fired[0].body
        );
    }

    /// ⚠ **The alert watches a currency group the primary curve cannot see** (decision 0183, item 13): the
    /// core's latch trips the whole node when ANY group breaches, and the snapshot carries the other groups'
    /// curve and peak (`Portfolio::group_curves`). The primary (dollar) group is flat; the EUR group is 30%
    /// under its own peak. FAILS if the conversion does not hand the rule the group curves.
    #[test]
    fn a_breach_of_another_currency_group_fires_the_alert() {
        let set = AlertRuleSet {
            rules: vec![AlertRule::new("d", RuleTrigger::Drawdown { fraction: 0.20 })],
            ..Default::default()
        };
        let mut mount = maybe_mount_with_sinks(Some(set), Vec::new()).expect("one enabled rule");
        let snap = |eur_curve: f64| {
            let mut s = CoreSnapshot::empty("binance", "BTCUSDT");
            s.portfolio.capital_base = 1_000.0;
            s.portfolio.venues =
                vec![vike_execution::VenueBlock { venue: "binance".into(), ..Default::default() }];
            s.portfolio.group_curves = vec![vike_execution::GroupCurve {
                currency: "EUR".into(),
                base: 1_000.0,
                curve: eur_curve,
                peak: 1_000.0,
            }];
            s
        };
        assert!(mount.on_snapshot(&snap(1_000.0), 1).is_empty(), "both groups flat");
        let fired = mount.on_snapshot(&snap(700.0), 2);
        assert_eq!(fired.len(), 1, "the EUR group is 30% under its own peak");
        assert!(fired[0].body.contains("EUR group 30.00%"), "body: {}", fired[0].body);
    }

    /// **A restart under water: the alert measures from the latch's peak.** Capital base 1000, curve 800 at the
    /// first reading. The latch seeds its peak at `max(base, curve)` = 1000 and reads 20%; the rule took the first
    /// curve it saw as its peak and read 0%, so the node sat liquidate-only under a silent 10% rule. Its row of
    /// `Portfolio::group_curves` is the peak; with no row (no `max_drawdown`, or before the first sweep) the rule
    /// keeps its session peak, and a curve above the row (a new high since the last sweep) is no drawdown.
    #[test]
    fn a_restart_under_water_measures_the_drawdown_from_the_latchs_peak() {
        let rule = || AlertRuleSet {
            rules: vec![AlertRule::new("d", RuleTrigger::Drawdown { fraction: 0.10 })],
            ..Default::default()
        };
        let snap = |realized: f64, latch_peak: Option<f64>| {
            let mut s = CoreSnapshot::empty("binance", "BTCUSDT");
            s.portfolio.capital_base = 1_000.0;
            s.portfolio.venues = vec![vike_execution::VenueBlock {
                venue: "binance".into(),
                realized_pnl: realized,
                ..Default::default()
            }];
            s.portfolio.group_curves = latch_peak
                .map(|peak| vike_execution::GroupCurve {
                    currency: vike_execution::Interned::default(),
                    base: 1_000.0,
                    curve: 1_000.0 + realized,
                    peak,
                })
                .into_iter()
                .collect();
            s
        };

        let mut mount = maybe_mount_with_sinks(Some(rule()), Vec::new()).expect("one rule");
        let fired = mount.on_snapshot(&snap(-200.0, Some(1_000.0)), 1);
        assert_eq!(fired.len(), 1, "the latch reads 20% here, so a 10% rule fires");
        assert!(fired[0].body.contains("own-PnL drawdown 20.00%"), "body: {}", fired[0].body);
        assert!(mount.on_snapshot(&snap(-200.0, Some(1_000.0)), 2).is_empty(), "one per episode");

        // No row: the latch is unarmed, and the first curve the rule sees is its peak, as before.
        let mut unarmed = maybe_mount_with_sinks(Some(rule()), Vec::new()).expect("one rule");
        assert!(unarmed.on_snapshot(&snap(-200.0, None), 1).is_empty());
        assert_eq!(unarmed.on_snapshot(&snap(-400.0, None), 2).len(), 1, "25% under the 800");

        // A new high since the latch's last sweep is not a drawdown from the older peak.
        let mut high = maybe_mount_with_sinks(Some(rule()), Vec::new()).expect("one rule");
        assert!(high.on_snapshot(&snap(500.0, Some(1_000.0)), 1).is_empty());
    }

    /// ⚠ **Four paper accounts at 100,000 do not dilute the alert either** (REVIEW-1D finding 1): the published
    /// `capital_base` adds their seeds (401,000), the latch's row holds the demo's own 1,000; a 260 loss is 26% of the
    /// latch's curve and fires a 25% rule, naming the latch's base. FAILS if the curve is read from the totals.
    #[test]
    fn a_demo_loss_beside_four_paper_accounts_fires_on_the_latchs_curve() {
        let set = AlertRuleSet {
            rules: vec![AlertRule::new("d", RuleTrigger::Drawdown { fraction: 0.25 })],
            ..Default::default()
        };
        let mut mount = maybe_mount_with_sinks(Some(set), Vec::new()).expect("one enabled rule");
        let snap = |loss: f64| {
            let mut s = CoreSnapshot::empty("bybit", "ETHUSDT");
            s.portfolio.capital_base = 401_000.0;
            s.portfolio.venues = vec![vike_execution::VenueBlock {
                venue: "bybit".into(),
                realized_pnl: -loss,
                ..Default::default()
            }];
            s.portfolio.group_curves = vec![vike_execution::GroupCurve {
                currency: vike_execution::Interned::default(),
                base: 1_000.0,
                curve: 1_000.0 - loss,
                peak: 1_000.0,
            }];
            s
        };
        assert!(mount.on_snapshot(&snap(0.0), 1).is_empty(), "flat");
        let fired = mount.on_snapshot(&snap(260.0), 2);
        assert_eq!(fired.len(), 1, "26% of the demo's own 1,000");
        assert!(fired[0].body.contains("capital_base 1000"), "body: {}", fired[0].body);
    }

    /// A conversion rate that is stale while a position is open in its currency (`QuoteRateView::held`, which the
    /// engine sets only on a stale row) fires ONE alert; a stale rate nobody holds anything in fires nothing, and a
    /// STOPPED account's retained block (its rate table frozen at the last publish) is not read as a live rate.
    #[test]
    fn a_stale_rate_under_a_held_position_fires_once_and_a_stopped_accounts_block_is_skipped() {
        let set = AlertRuleSet {
            rules: vec![AlertRule::new("s", RuleTrigger::StaleRate)],
            ..Default::default()
        };
        let mut mount = maybe_mount_with_sinks(Some(set), Vec::new()).expect("one enabled rule");
        let snap = |stale: bool, held: bool| {
            let mut s = CoreSnapshot::empty("binance", "BTCUSDT");
            s.portfolio.venues = vec![
                vike_execution::VenueBlock { venue: "binance".into(), ..Default::default() },
                vike_execution::VenueBlock {
                    venue: "fxcm".into(),
                    route_key: "fxcm".into(),
                    quote_rates_in: "EUR".into(),
                    quote_rates: vec![vike_execution::QuoteRateView {
                        currency: "USD".into(),
                        rate: 0.862_069,
                        as_of_ms: 1,
                        stale,
                        held,
                    }],
                    ..Default::default()
                },
            ];
            s
        };
        assert!(mount.on_snapshot(&snap(false, false), 1).is_empty(), "fresh");
        assert!(mount.on_snapshot(&snap(true, false), 2).is_empty(), "stale, nothing held in it");
        let fired = mount.on_snapshot(&snap(true, true), 3);
        assert_eq!(fired.len(), 1);
        assert!(fired[0].body.contains("fxcm: 1 USD in EUR"), "body: {}", fired[0].body);
        assert!(mount.on_snapshot(&snap(true, true), 4).is_empty(), "one alert per episode");

        // a stopped account keeps its last block: its frozen stale row is history, not a live rate
        let stopped = [ArmingNotRunningView {
            account: vike_model::accounts::account_keys::AccountNamed::of(
                "fxcm",
                vike_model::accounts::account_keys::AccountLabel::Default,
            ),
            tier: None,
            reason: "no credentials".into(),
            block: Some(vike_model::ArmingBlock::NoCredentials),
            refusal_id: None,
        }];
        let mut quiet = maybe_mount_with_sinks(
            Some(AlertRuleSet {
                rules: vec![AlertRule::new("s", RuleTrigger::StaleRate)],
                ..Default::default()
            }),
            Vec::new(),
        )
        .expect("one enabled rule");
        assert!(quiet.on_snapshot_stopped(&snap(true, true), &stopped, 5).is_empty());
        assert_eq!(quiet.on_snapshot(&snap(true, true), 6).len(), 1, "running again: it counts");
    }

    /// **The journal fence's halt is ONE alert, at the first tick that sees it, routed to its rule's target**, its body
    /// the core's own words; a fault that is not the fence's (a panic) raises none, decided on the typed fact.
    #[test]
    fn the_journal_fence_halt_fires_one_alert_with_the_cores_words() {
        let mut rule = AlertRule::new("halt", RuleTrigger::JournalFenceHalt);
        rule.targets.webhooks = vec!["webhook".into()];
        let set = AlertRuleSet { rules: vec![rule], ..Default::default() };
        let mut mount = maybe_mount_with_sinks(Some(set), Vec::new()).expect("one enabled rule");
        let snap = |fence: bool, fault: Option<&str>| {
            let mut s = CoreSnapshot::empty("binance", "BTCUSDT");
            s.portfolio.venues =
                vec![vike_execution::VenueBlock { venue: "binance".into(), ..Default::default() }];
            s.journal_fence_halt = fence;
            s.fault = fault.map(str::to_string);
            s
        };
        assert!(mount.on_snapshot(&snap(false, None), 1).is_empty(), "healthy");
        assert!(mount.on_snapshot(&snap(false, Some("a panic")), 2).is_empty(), "not the fence's");
        let fired = mount.on_snapshot(&snap(true, Some("the journal wrote nothing")), 3);
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].body, "trading HALTED: the journal wrote nothing");
        assert_eq!(fired[0].targets.webhooks, ["webhook"]);
        assert!(mount.on_snapshot(&snap(true, Some("the journal wrote nothing")), 4).is_empty());
    }

    /// The drawdown rule keeps its session peak in the PRIMARY group's currency: the conversion hands the rule
    /// that currency, so a primary that moves from the dollar group to a EUR one starts the peak over instead of
    /// reading the dollar peak as a EUR drawdown (decision 0183, item 13).
    #[test]
    fn the_drawdown_peak_starts_over_when_the_primary_group_changes_currency() {
        let set = AlertRuleSet {
            rules: vec![AlertRule::new("d", RuleTrigger::Drawdown { fraction: 0.20 })],
            ..Default::default()
        };
        let mut mount = maybe_mount_with_sinks(Some(set), Vec::new()).expect("one enabled rule");
        let snap = |currency: &str, base: f64| {
            let mut s = CoreSnapshot::empty("binance", "BTCUSDT");
            s.portfolio.capital_base = base;
            s.portfolio.primary_currency_stated = Some(currency.into());
            s.portfolio.venues = vec![vike_execution::VenueBlock {
                venue: "binance".into(),
                ledger_currency: currency.into(),
                ..Default::default()
            }];
            s
        };
        assert!(mount.on_snapshot(&snap("", 50_000.0), 1).is_empty(), "a dollar peak of 50,000");
        assert!(
            mount.on_snapshot(&snap("EUR", 1_000.0), 2).is_empty(),
            "the EUR curve of 1,000 is not a 98% drawdown from a dollar peak"
        );
    }

    /// `Portfolio::group_curves` lists the PRIMARY group too (the summary line prints every group's base, curve
    /// and peak), and the alert must not watch it a second time: the rule already reads that group through
    /// `drawdown_curve`, which IS that row's curve once the latch has swept (the latch's own measure, paper engines
    /// out of a real group's curve). A primary row 30% under its peak fires ONE alert, through the primary curve, and
    /// none again in the episode. FAILS (two alerts) if the conversion hands the rule every row of `group_curves`.
    #[test]
    fn the_primary_groups_row_is_not_watched_a_second_time() {
        let set = AlertRuleSet {
            rules: vec![AlertRule::new("d", RuleTrigger::Drawdown { fraction: 0.20 })],
            ..Default::default()
        };
        let mut mount = maybe_mount_with_sinks(Some(set), Vec::new()).expect("one enabled rule");
        let mut s = CoreSnapshot::empty("binance", "BTCUSDT");
        s.portfolio.capital_base = 1_000.0;
        s.portfolio.venues =
            vec![vike_execution::VenueBlock { venue: "binance".into(), ..Default::default() }];
        s.portfolio.group_curves = vec![vike_execution::GroupCurve {
            currency: vike_execution::Interned::default(),
            base: 1_000.0,
            curve: 700.0,
            peak: 1_000.0,
        }];
        let fired = mount.on_snapshot(&s, 1);
        assert_eq!(fired.len(), 1, "the primary is read once, through its own curve");
        assert!(fired[0].body.contains("own-PnL drawdown 30.00%"), "body: {}", fired[0].body);
        assert!(mount.on_snapshot(&s, 2).is_empty());
    }

    // ── a `Price` rule's price where the core holds no bar series of its pair (owner, 2026-11-15) ──────────────

    /// A price source the tests script: the venues it runs a feed for, the last print and top it holds, and the
    /// bar series it was asked to open.
    #[derive(Default)]
    struct Prices {
        feeds: Vec<&'static str>,
        trade: std::sync::Mutex<Option<f64>>,
        top: std::sync::Mutex<Option<(f64, f64)>>,
        asked: std::sync::Mutex<Vec<SeriesKey>>,
    }
    impl crate::server::symbol_admission::AccountReadiness for Prices {
        fn generation(&self, _: &str) -> u64 {
            0
        }
    }
    impl SymbolAdmitter for Prices {
        fn price_source(&self, venue: &str) -> Result<(), String> {
            if self.feeds.contains(&venue) {
                Ok(())
            } else {
                Err(format!("this node runs no price feed for {venue}"))
            }
        }
        fn admission(
            &self,
            _: &str,
            _: &str,
            _: &str,
        ) -> Result<
            vike_execution::SymbolAdmission,
            vike_gatewayhub_client::trade::SymbolPropertiesRequestError,
        > {
            unreachable!("an alert asks no facts")
        }
        fn price(&self, _: &str, _: &str, _: &str, _: bool) -> Result<Option<f64>, String> {
            Ok(None)
        }
        fn last_trade(&self, _: &str, _: &str) -> Option<f64> {
            *self.trade.lock().unwrap()
        }
        fn top_of_book(&self, _: &str, _: &str, _: &str) -> Option<(f64, f64)> {
            *self.top.lock().unwrap()
        }
        fn keep_bars(&self, wanted: &[SeriesKey]) {
            self.asked.lock().unwrap().extend_from_slice(wanted);
        }
    }

    /// A snapshot the core published (one venue block), with no bar series at all.
    fn published() -> CoreSnapshot {
        let mut s = CoreSnapshot::empty("binance", "BTCUSDT");
        s.portfolio.venues =
            vec![vike_execution::VenueBlock { venue: "binance".into(), ..Default::default() }];
        s
    }

    fn price_rule(id: &str, venue: &str, symbol: &str, op: Compare) -> AlertRule {
        AlertRule::new(
            id,
            RuleTrigger::Price {
                venue: venue.into(),
                symbol: symbol.into(),
                comparison: op,
                level: 100.0,
            },
        )
    }

    fn mounted(rules: Vec<AlertRule>, prices: Option<Arc<Prices>>) -> AlertMount {
        let set = AlertRuleSet { rules, ..Default::default() };
        maybe_mount_with_sinks(Some(set), Vec::new())
            .expect("enabled rules")
            .with_prices(prices.map(|p| p as Arc<dyn SymbolAdmitter>))
    }

    /// **A pair the core holds no bar series of is never silent**: its `1m` bars are asked for at once (its venue
    /// serves candles), and until they come the rule reads the last trade PRINT the lane delivered, then the
    /// quote's SIDE for the crossing: the ask for `Above`, the bid for `Below`. With a quote of 99 / 101 both rules
    /// at 100 fire, where its mid (100) crosses neither.
    #[test]
    fn a_pair_with_no_bar_series_reads_the_last_print_then_the_quotes_side_and_asks_for_its_bars() {
        let prices = Arc::new(Prices { feeds: vec!["binance"], ..Prices::default() });
        let mut mount = mounted(
            vec![price_rule("up", "binance", "ETHUSDT", Compare::Above)],
            Some(Arc::clone(&prices)),
        );
        *prices.trade.lock().unwrap() = Some(99.0);
        assert!(mount.on_snapshot(&published(), 1).is_empty(), "seeds at the print 99");
        assert_eq!(
            *prices.asked.lock().unwrap(),
            [("binance".to_string(), "ETHUSDT".to_string(), "1m".to_string())],
            "the pair's bars were asked for"
        );
        *prices.trade.lock().unwrap() = Some(101.0);
        let fired = mount.on_snapshot(&published(), 2);
        assert_eq!(fired.len(), 1, "the print crossed 100");
        assert!(fired[0].body.contains("(last 101)"), "{}", fired[0].body);

        let prices = Arc::new(Prices { feeds: vec!["binance"], ..Prices::default() });
        let mut mount = mounted(
            vec![
                price_rule("up", "binance", "SOLUSDT", Compare::Above),
                price_rule("down", "binance", "SOLUSDT", Compare::Below),
            ],
            Some(Arc::clone(&prices)),
        );
        *prices.top.lock().unwrap() = Some((100.0, 100.0));
        assert!(mount.on_snapshot(&published(), 1).is_empty(), "both seed at 100");
        *prices.top.lock().unwrap() = Some((99.0, 101.0));
        let fired: Vec<String> =
            mount.on_snapshot(&published(), 2).into_iter().map(|f| f.rule_id).collect();
        assert_eq!(fired, ["up", "down"], "the ask 101 crossed up, the bid 99 crossed down");
    }

    /// **A `Price` rule on a pair this node has NO price for is refused at creation, with the reason, and never
    /// sits there dead**: a venue the node runs no feed for, a venue with no candles whose pair no lane delivers,
    /// and (a node with no price source but its mounts) a pair no mount trades. A pair whose bars the core holds
    /// stands. A refused rule stays off even once a price appears.
    #[test]
    fn a_price_rule_on_a_pair_with_no_price_is_refused_at_creation() {
        let prices = Arc::new(Prices { feeds: vec!["binance", "polymarket"], ..Prices::default() });
        let mut mount = mounted(
            vec![
                price_rule("kraken", "kraken", "XBTUSD", Compare::Above),
                price_rule("poly", "polymarket", "123456789012345678901", Compare::Above),
                price_rule("btc", "binance", "BTCUSDT", Compare::Above),
            ],
            Some(Arc::clone(&prices)),
        );
        let mut snap = published();
        snap.bars.insert(
            ("binance".into(), "BTCUSDT".into(), "1m".into()),
            vike_execution::BarSeries {
                closed: Arc::new(vec![vike_marketdata::test_support::flat_bar_unit_volume(
                    0, 99.0,
                )]),
                forming: None,
            },
        );
        assert!(mount.on_snapshot(&snap, 1).is_empty());
        let refused: Vec<&str> = mount.refused().iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(refused, ["kraken", "poly"]);
        assert!(mount.refused()[0].1.contains("no price feed for kraken"), "{:?}", mount.refused());
        assert!(mount.refused()[1].1.contains("serves no trade candles"), "{:?}", mount.refused());
        assert_eq!(mount.enabled_count(), 1, "the refused rules are off");
        assert!(mount.on_snapshot(&snap, 2).is_empty());
        assert_eq!(mount.refused().len(), 2, "still no source: still off");

        let mut paper =
            mounted(vec![price_rule("eth", "binance", "ETHUSDT", Compare::Above)], None);
        assert!(paper.on_snapshot(&snap, 1).is_empty());
        assert_eq!(
            paper.refused().len(),
            1,
            "no mount trades it and the node takes no other price"
        );
    }

    /// **A `Price` rule on a symbol its venue does not list is refused at creation, naming the listed user names**
    /// (REVIEW-FIX finding 8a; decision 0290): `okx BTCUSD`, where okx's kept list carries the user names `BTCUSDT` and
    /// `BTCUSDT.P`, would sit on a lane that never delivers. A listed symbol stands, and a venue whose list this node
    /// does not hold is not judged by it.
    #[test]
    fn a_price_rule_on_a_symbol_its_venue_does_not_list_is_refused_naming_the_spelling() {
        use vike_model::{Contract, ContractPerp, Instrument, Margining};
        let spot =
            |raw: &str, base: &str| Instrument::new("okx", raw, base, "USDT", Contract::CryptoSpot);
        let swap = Contract::Perp(ContractPerp {
            margining: Margining::Linear,
            settle: "USDT".into(),
            value_currency: None,
            funding_interval_ms: None,
            settling_ms: None,
        });
        vike_catalog::hold_kept_list(
            "okx",
            vec![
                spot("BTC-USDT", "BTC"),
                Instrument::new("okx", "BTC-USDT-SWAP", "BTC", "USDT", swap),
                spot("ETH-USDT", "ETH"),
            ],
        );
        let prices = Arc::new(Prices { feeds: vec!["okx", "binance"], ..Prices::default() });
        let mut mount = mounted(
            vec![
                price_rule("misspelled", "okx", "BTCUSD", Compare::Above),
                price_rule("listed", "okx", "ETHUSDT", Compare::Above),
                price_rule("no-list", "binance", "ETHUSDT", Compare::Above),
            ],
            Some(Arc::clone(&prices)),
        );
        assert!(mount.on_snapshot(&published(), 1).is_empty());
        let refused: Vec<&str> = mount.refused().iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(refused, ["misspelled"]);
        let why = &mount.refused()[0].1;
        assert!(
            why.starts_with(
                "okx's kept instrument list has no `BTCUSD` (it lists `BTCUSDT`, `BTCUSDT.P`)"
            ),
            "{why}"
        );
        assert!(mount.on_snapshot(&published(), 2).is_empty());
        assert_eq!(mount.refused().len(), 1, "still unlisted: still off");
    }

    /// **A refused rule switches back ON by itself once its pair has a price source, and the switch leaves every
    /// other rule's state as it was.** The Polymarket token's lane starts delivering (its symbol was admitted): at
    /// the next tick the rule is on again (said once), seeds on the print and fires on its next crossing; the BTC
    /// rule, which seeded at 99 before the switch, still fires on its crossing to 101 in that same tick (a switch
    /// that re-armed every rule would have re-seeded it at 101).
    #[test]
    fn a_refused_rule_switches_back_on_once_its_pair_has_a_price_source() {
        let prices = Arc::new(Prices { feeds: vec!["binance", "polymarket"], ..Prices::default() });
        let mut mount = mounted(
            vec![
                price_rule("poly", "polymarket", "123456789012345678901", Compare::Above),
                price_rule("btc", "binance", "BTCUSDT", Compare::Above),
            ],
            Some(Arc::clone(&prices)),
        );
        let at = |close: f64| {
            let mut snap = published();
            snap.bars.insert(
                ("binance".into(), "BTCUSDT".into(), "1m".into()),
                vike_execution::BarSeries {
                    closed: Arc::new(vec![vike_marketdata::test_support::flat_bar_unit_volume(
                        0, close,
                    )]),
                    forming: None,
                },
            );
            snap
        };
        let fired = |mount: &mut AlertMount, snap: &CoreSnapshot, now: i64| -> Vec<String> {
            mount.on_snapshot(snap, now).into_iter().map(|f| f.rule_id).collect()
        };
        assert!(fired(&mut mount, &at(99.0), 1).is_empty(), "btc seeds at 99");
        assert_eq!(mount.refused().len(), 1, "the token has no price source: refused");

        *prices.trade.lock().unwrap() = Some(99.0);
        assert_eq!(fired(&mut mount, &at(101.0), 2), ["btc"], "btc's state survived the switch");
        assert!(mount.refused().is_empty(), "the token's rule is back on");
        assert_eq!(mount.enabled_count(), 2);

        *prices.trade.lock().unwrap() = Some(101.0);
        assert_eq!(fired(&mut mount, &at(101.0), 3), ["poly"], "it seeded at 99, then crossed");
    }

    /// **An alert's price follows the owner's order and is NEVER a mid** (REVIEW-FIX finding 8b): the last trade price
    /// (a forming bar that traded), then the bar close (a closed bar that traded), then the trade print, then the
    /// quote's side; a bar folded from quote mids (no volume) and a forex venue's candles are never read. The
    /// review's example: a cTrader EURUSD mid bar closing at 1.08500 beside a quote of 1.08495 / 1.08505 — the rule
    /// `Above 1.08500` reads the ask 1.08505, not the bar's mid.
    #[test]
    fn an_alert_reads_the_last_trade_then_the_bar_close_then_the_print_then_the_quote_side_never_a_mid()
     {
        use vike_marketdata::test_support::{flat_bar_unit_volume, flat_bar_zero_volume};
        let prices = Prices { feeds: vec!["binance", "ctrader"], ..Prices::default() };
        *prices.trade.lock().unwrap() = Some(102.0);
        *prices.top.lock().unwrap() = Some((103.0, 104.0));
        let series = |closed: vike_marketdata::Bar, forming: Option<vike_marketdata::Bar>| {
            vike_execution::BarSeries { closed: Arc::new(vec![closed]), forming }
        };
        let price = |snap: &CoreSnapshot, venue: &str, symbol: &str, op: Compare| {
            SnapshotView(snap, &[], Some(&prices as &dyn SymbolAdmitter))
                .last_price(venue, symbol, op)
        };

        let mut snap = published();
        let key = ("binance".to_string(), "ETHUSDT".to_string(), "1m".to_string());
        snap.bars.insert(
            key.clone(),
            series(flat_bar_unit_volume(0, 100.0), Some(flat_bar_unit_volume(60_000, 101.0))),
        );
        assert_eq!(
            price(&snap, "binance", "ETHUSDT", Compare::Above),
            Some(101.0),
            "the forming trade"
        );
        snap.bars.insert(
            key.clone(),
            series(flat_bar_unit_volume(0, 100.0), Some(flat_bar_zero_volume(60_000, 99.0))),
        );
        assert_eq!(
            price(&snap, "binance", "ETHUSDT", Compare::Above),
            Some(100.0),
            "a forming bar that did not trade is passed over: the closed bar's close, before the print"
        );
        snap.bars.insert(key, series(flat_bar_zero_volume(0, 100.0), None));
        assert_eq!(
            price(&snap, "binance", "ETHUSDT", Compare::Above),
            Some(102.0),
            "a bar folded from mids is never read: the print"
        );
        *prices.trade.lock().unwrap() = None;
        assert_eq!(price(&snap, "binance", "ETHUSDT", Compare::Above), Some(104.0), "the ask");
        assert_eq!(price(&snap, "binance", "ETHUSDT", Compare::Below), Some(103.0), "the bid");

        // The review's example: a forex venue's mid bar (here even one with volume) is never the price.
        *prices.top.lock().unwrap() = Some((1.08495, 1.08505));
        let mut fx = published();
        fx.bars.insert(
            ("ctrader".into(), "EURUSD".into(), "1m".into()),
            series(flat_bar_unit_volume(0, 1.08500), None),
        );
        assert_eq!(
            price(&fx, "ctrader", "EURUSD", Compare::Above),
            Some(1.08505),
            "the ask, not the mid"
        );
    }
}
