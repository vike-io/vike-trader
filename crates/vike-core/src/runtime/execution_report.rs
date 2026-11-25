//! **What the OMS states after each fold** (the execution stream to the datahub, the FIX drop copy's
//! ExecutionReport): which folds owe a report, whether the order is THIS install's, its account, mount and strategy
//! code, and the [`ExecutionReport`] built from the event and the engine's record AFTER the fold. The journal write is
//! `journaling.rs`'s `CoreThread::journal_execution_report`.
//!
//! - **Which folds owe one.** A fill or a fee correction naming a non-empty order client id, whatever the verdict (a
//!   fill the engine dropped is still the account's execution); an `OrderDenied`, always (the denied order is never
//!   registered, so the report is built from the request in hand); any other lifecycle event the engine APPLIED; an
//!   order the engine just registered. An event that names no order (the latency gate's empty-id fill, a balance, a
//!   funding payment) owes none, and its fold pays [`CoreThread::owes_execution_report`]'s one bool and one length.
//! - **Ours or foreign.** OURS = a live registry holds the id (only this process registers orders), or one of this
//!   install's order client id sessions, current or past (`CoreConfig::order_client_id_past_sessions`), minted it
//!   (`vike_model::orders::client_id::order_client_id_minted_by`). Everything else is FOREIGN: another install's id,
//!   even with an equal strategy code; a venue-UI order; a reconcile `EXT-…` id. A foreign report names the account
//!   whose stream carried it (the gateway ring, or the account whose reconcile pass found it) and nothing else.
//! - **Attribution, from the id and the record, never from memory.** The mount is the one whose
//!   `Mount::order_client_id_code` equals the id's strategy code (a cancel-then-new replacement keeps its stem, so its
//!   code); a code no current mount carries leaves `mount_id` `None` and `strategy_code` set, for the publisher to name
//!   from the durable row. A v1 id carries no code: its mount is the live `order_client_id_mount` entry while the
//!   order rests. The account is the ring's, else the engine's that folded it, else the engine whose
//!   `Engine::account_code` the id names.
//!
//! Per ORDER event, never per market message: every function here but [`CoreThread::owes_execution_report`] is cold.

use super::*;
use vike_model::accounts::account_keys::label_of_route_key;
use vike_model::orders::client_id::{
    OrderClientIdAccountCode, OrderClientIdReading, OrderClientIdStrategyCode,
    order_client_id_minted_by, parse_order_client_id,
};
use vike_model::orders::{
    ExecutionOwnership, ExecutionReport, ExecutionReportFeeCorrection, ExecutionReportFill,
    ExecutionReportOrder,
};

/// What a report states the fold of.
pub(crate) enum ExecutionReportCause<'a> {
    /// `event`, folded with verdict `fold`; `request` is the order in hand when the engine never registered it (a
    /// deny).
    Event { event: &'a Event, fold: EventBusFold, request: Option<&'a OrderRequest> },
    /// The order the engine just registered under this id.
    Registered(&'a str),
}

/// Who an execution belongs to: [`ExecutionReport`]'s ownership, account, mount and strategy code.
struct ExecutionAttribution {
    ownership: ExecutionOwnership,
    account: Option<String>,
    mount_id: Option<String>,
    strategy_code: Option<OrderClientIdStrategyCode>,
}

impl<C: ExecutionClient> CoreThread<C> {
    /// **The one test a fold site pays**: a journaling core and an event naming an order.
    #[inline(always)]
    pub(crate) fn owes_execution_report(&self, event: &Event) -> bool {
        self.execution_reports && event.order_client_id().is_some_and(|id| !id.is_empty())
    }

    /// The copy of `event` a fold site keeps for its report, taken before the bus consumes the event: `None` (one
    /// bool, one length) unless it [owes one](Self::owes_execution_report).
    #[inline(always)]
    pub(crate) fn kept_for_report(&self, event: &Event) -> Option<Box<Event>> {
        if self.owes_execution_report(event) { Some(keep_for_report(event)) } else { None }
    }

    /// Fold one in-process lifecycle event the core made up (a deny, a capability reject, a cancel, a watchdog
    /// reject) into engine `index`, and journal its report: [`Self::publish_to`] plus the report. `request` is the
    /// order a deny refused, which the engine never registered.
    pub(crate) fn publish_synthesized(
        &mut self,
        index: usize,
        event: Event,
        request: Option<&OrderRequest>,
    ) -> EventBusFold {
        self.publish_reported(index, event, request, None)
    }

    /// [`Self::publish_synthesized`] for an event a RECONCILE pass of engine `index` made up (an adopted order, an
    /// `EXT-DROP-…` fill, an approved confirm): the account whose pass found it is the report's provenance, as a
    /// ring is for a venue event, so a foreign one still names it.
    pub(crate) fn publish_reconciled(&mut self, index: usize, event: Event) -> EventBusFold {
        let provenance = vike_marketdata::Symbol::new(&self.engine(index).route_key);
        self.publish_reported(index, event, None, Some(provenance))
    }

    fn publish_reported(
        &mut self,
        index: usize,
        event: Event,
        request: Option<&OrderRequest>,
        provenance: Option<vike_marketdata::Symbol>,
    ) -> EventBusFold {
        let mut counted = false;
        if let Event::OrderDenied(denied) = &event {
            self.note_denied(index, denied, request.map(|r| (r.venue.as_str(), r.symbol.as_str())));
            // a repeated denial is counted, not reported one by one (`denial_streaks.rs`)
            counted = self.coalesced_denial.as_deref() == Some(denied.order_client_id.as_str());
        }
        let kept = if counted { None } else { self.kept_for_report(&event) };
        let fold = self.publish_to(index, event);
        if let Some(kept) = kept {
            let now_ms = self.engine(index).now_ms;
            let cause = ExecutionReportCause::Event { event: &kept, fold, request };
            self.journal_execution_report(Some(index), cause, provenance, now_ms);
        }
        fold
    }

    /// Journal the report of the order engine `engine_index` just registered under `order_client_id` (its
    /// `INITIALIZED` row; an emulated parent's terminal one).
    pub(crate) fn report_registered(&mut self, engine_index: usize, order_client_id: &str) {
        if self.execution_reports {
            let now_ms = self.engine(engine_index).now_ms;
            let cause = ExecutionReportCause::Registered(order_client_id);
            self.journal_execution_report(Some(engine_index), cause, None, now_ms);
        }
    }

    /// The report `cause` owes on engine `engine_index` (`None`: no engine folded it), `ring` the account whose
    /// stream carried it, `caused_by_sequence` the record whose fold it states and `now_ms` that fold's clock;
    /// `None` when it owes none.
    pub(crate) fn execution_report(
        &self,
        engine_index: Option<usize>,
        cause: &ExecutionReportCause<'_>,
        ring: Option<vike_marketdata::Symbol>,
        caused_by_sequence: u64,
        now_ms: i64,
    ) -> Option<ExecutionReport> {
        let (order_client_id, event, request) = match cause {
            ExecutionReportCause::Event { event, fold, request } => {
                let owed = matches!(
                    event,
                    Event::Fill(_) | Event::FeeAdjustment(_) | Event::OrderDenied(_)
                ) || *fold == EventBusFold::Applied;
                if !owed {
                    return None;
                }
                (event.order_client_id()?, Some(*event), *request)
            }
            ExecutionReportCause::Registered(id) => (*id, None, None),
        };
        let record = match engine_index {
            Some(i) => self.engine(i).registry.get(order_client_id),
            None => (0..=self.extra_engines.len())
                .find_map(|i| self.engine(i).registry.get(order_client_id)),
        };
        // the words of whoever ended or refused it: the gate's for a deny, the venue's for a reject or a cancel, the
        // core's for one it made up
        let reason = match event {
            Some(Event::OrderDenied(e)) => Some(e.reason.to_string()),
            Some(Event::OrderRejected(e)) => Some(e.reason.to_string()),
            Some(Event::OrderCanceled(e)) => Some(e.reason.to_string()),
            Some(Event::OrderCancelRejected(e)) => Some(e.reason.to_string()),
            Some(Event::OrderModifyRejected(e)) => Some(e.reason.to_string()),
            _ => None,
        }
        .filter(|r| !r.is_empty());
        // A deny's order is the request in hand: the engine never registered it, and a record under its id is
        // another order's.
        let order = match (event, request, record) {
            (Some(Event::OrderDenied(_)), Some(req), _) => {
                Some(ExecutionReportOrder::of_request(req, vike_model::OrderStatus::Denied))
            }
            (_, _, Some(record)) => Some(ExecutionReportOrder::of_record(record)),
            (Some(Event::OrderRejected(_)), Some(req), None) => {
                Some(ExecutionReportOrder::of_request(req, vike_model::OrderStatus::Rejected))
            }
            _ => None,
        };
        let engine = engine_index.map(|i| self.engine(i));
        let (venue, mut symbol) = match event {
            Some(Event::Fill(f)) => (f.venue.to_string(), f.symbol.to_string()),
            Some(Event::FeeAdjustment(a)) => (a.venue.to_string(), a.symbol.to_string()),
            _ => match request.or(record.map(|r| &r.request)) {
                Some(r) => (r.venue.clone(), r.symbol.clone()),
                None => (engine.map(|e| e.venue.to_string()).unwrap_or_default(), String::new()),
            },
        };
        if symbol.is_empty()
            && let Some(e) = engine
        {
            symbol = e.symbol.to_string(); // an order naming no symbol trades its engine's own
        }
        // a fee correction is stamped at its FILL's time, as the materializer's correction row was
        let at_ms = match event {
            Some(Event::Fill(f)) => f.timestamp,
            Some(Event::FeeAdjustment(a)) => a.timestamp,
            _ => now_ms,
        };
        let who = self.execution_attribution(
            engine_index,
            order_client_id,
            &venue,
            ring,
            request.is_some(),
        );
        Some(ExecutionReport {
            caused_by_sequence,
            at_ms,
            ownership: who.ownership,
            account: who.account,
            venue,
            symbol,
            order_client_id: order_client_id.to_string(),
            venue_order_id: record.and_then(|r| r.venue_order_id.clone()),
            mount_id: who.mount_id,
            strategy_code: who.strategy_code,
            order,
            fill: match event {
                Some(Event::Fill(f)) => Some(ExecutionReportFill::of_fill(f)),
                _ => None,
            },
            fee_correction: match event {
                Some(Event::FeeAdjustment(a)) => {
                    Some(ExecutionReportFeeCorrection::of_adjustment(a))
                }
                _ => None,
            },
            reason,
        })
    }

    /// **Is `order_client_id` this install's?** The live registry first (exact), then the sessions that minted
    /// this install's ids, the current one and the past ones.
    pub(crate) fn execution_ownership(&self, order_client_id: &str) -> ExecutionOwnership {
        let held = (0..=self.extra_engines.len())
            .any(|i| self.engine(i).registry.contains_key(order_client_id));
        if held {
            // the registry's answer is exact: no session copy, no id parse
            return ExecutionOwnership::Ours;
        }
        let (session, sequence) = self.order_client_id_gen.state();
        let minted = order_client_id_minted_by(order_client_id, &session, sequence)
            || self
                .config
                .order_client_id_past_sessions
                .iter()
                .any(|(past, last)| order_client_id_minted_by(order_client_id, past, *last));
        if minted { ExecutionOwnership::Ours } else { ExecutionOwnership::Foreign }
    }

    /// Who an execution of `order_client_id` on `venue` belongs to (the module doc's rules). `in_hand`: the report is
    /// built from a request this core is submitting (a deny), which is this process's own order exactly as a
    /// registry entry is, though no registry holds it.
    fn execution_attribution(
        &self,
        engine_index: Option<usize>,
        order_client_id: &str,
        venue: &str,
        ring: Option<vike_marketdata::Symbol>,
        in_hand: bool,
    ) -> ExecutionAttribution {
        let label = |venue: &str, route_key: &str| {
            label_of_route_key(venue, route_key).map(|l| l.to_string())
        };
        let ring_account = ring.and_then(|key| label(venue, key.as_str()));
        let ownership = if in_hand {
            ExecutionOwnership::Ours
        } else {
            self.execution_ownership(order_client_id)
        };
        if ownership == ExecutionOwnership::Foreign {
            return ExecutionAttribution {
                ownership,
                account: ring_account,
                mount_id: None,
                strategy_code: None,
            };
        }
        let parsed = parse_order_client_id(order_client_id);
        let account = ring_account
            .or_else(|| {
                engine_index.map(|i| self.engine(i)).and_then(|e| label(&e.venue, &e.route_key))
            })
            .or_else(|| {
                let Some(OrderClientIdReading::V2(parts)) = parsed else { return None };
                let code = parts.account.filter(|c| *c != OrderClientIdAccountCode::NONE)?;
                (0..=self.extra_engines.len())
                    .map(|i| self.engine(i))
                    .find(|e| e.account_code == code)
                    .and_then(|e| label(&e.venue, &e.route_key))
            });
        let (mount_id, strategy_code) = match parsed {
            Some(OrderClientIdReading::V2(parts))
                if parts.strat != OrderClientIdStrategyCode::NONE =>
            {
                let slot = self.mounts.iter().position(|m| {
                    m.as_ref().is_some_and(|m| m.order_client_id_code == Some(parts.strat))
                });
                (slot.and_then(|i| self.mount_ids.get(i).cloned()), Some(parts.strat))
            }
            Some(OrderClientIdReading::V1 { .. }) => (
                self.order_client_id_mount
                    .get(order_client_id)
                    .and_then(|&i| self.mount_ids.get(i).cloned()),
                None,
            ),
            _ => (None, None),
        };
        ExecutionAttribution { ownership, account, mount_id, strategy_code }
    }
}

/// [`CoreThread::kept_for_report`]'s copy, out of line: only an event that names an order reaches it.
#[cold]
#[inline(never)]
fn keep_for_report(event: &Event) -> Box<Event> {
    Box::new(event.clone())
}
