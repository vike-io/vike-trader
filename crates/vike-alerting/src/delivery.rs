//! The delivery seam: where a [`FiredAlert`] goes. An [`AlertSink`] is a fire-and-forget consumer;
//! `AlertEngine` broadcasts each fired alert to every registered sink, and each
//! sink SELF-FILTERS on the alert's `AlertTargets` (so routing lives in the alert, not the
//! dispatcher).
//!
//! Two sinks ship here:
//! - [`InProcessSink`] — an in-memory buffer a GUI could drain for toasts / OS notifications. None
//!   does: the desktop mounts no alert engine, so its callers today are tests.
//! - [`WebhookSink`] — an optional Telegram / generic-webhook POST over the existing `ureq` + rustls
//!   stack (no second HTTP/TLS crate). The blocking POST is done by an injected [`WebhookTransport`]
//!   so the seam is testable with a fake sender and never touches the network in CI.
//!
//! …and one DECORATOR, which is where this seam's only BOUND lives:
//! - [`QueuedSink`] — a bounded queue plus one delivery thread in front of a slow sink, so
//!   `deliver` becomes an enqueue and the caller's loop never waits on the wire. Register a
//!   wire-touching sink through it, not raw: without it the [`AlertSink`] contract's "must NEVER
//!   block the caller for long" is a request rather than a property, and a venue outage parked a
//!   whole daemon inside `AlertEngine::dispatch` for half an hour. The measurement, the overflow
//!   policy and what this deliberately does NOT do are on [`QueuedSink`]'s own doc.
//!
//! **Secrets** (`WebhookKind`'s bot token, a Discord/Slack webhook URL) never reach `Debug` (manual
//! redacting impl) and never reach a log: a failed [`WebhookSink`] delivery logs the target NAME and
//! a coarse error CATEGORY only — never the endpoint URL (a Telegram URL embeds the token) nor the
//! raw transport error (which can echo the request target). Built from a CALLER-SUPPLIED credential
//! map via [`webhook_configs_from_env`], the same absent-credentials-is-the-gate idiom the venues
//! use — this crate never opens the credential store itself, so nothing here can load a Telegram
//! token its caller did not hand it.

mod in_process;
mod queued;
mod targets;
mod webhook;

use super::rule::AlertTargets;

pub use in_process::InProcessSink;
pub use queued::{
    DEFAULT_QUEUE_CAPACITY, QueuedSink, QueuedSinkOutcome, QueuedSinkStop, STOP_POLL,
};
pub use targets::{
    WEBHOOK_KEYS, queued_ureq_webhook_sinks, ureq_webhook_sinks, webhook_configs_from_env,
};
pub use webhook::{UreqTransport, WebhookConfig, WebhookKind, WebhookSink, WebhookTransport};

/// One fired alert — the immutable output of the evaluator, handed to every [`AlertSink`]. Carries
/// the rule's delivery `targets` so the dispatcher needs no back-reference to the rule.
///
/// It lives HERE, not in the evaluator that produces it, because it is the one type the evaluator
/// and the sinks share: the sinks below take it, so any consumer can build and deliver one without
/// the evaluator.
#[derive(Debug, Clone, PartialEq)]
pub struct FiredAlert {
    /// the rule that fired (correlation id).
    pub rule_id: String,
    /// the rule's label (its `name`, or `id` when unnamed) — the message title.
    pub title: String,
    /// the specifics that fired it (numbers/context) — the message body.
    pub body: String,
    /// wall-clock ms of the fire (the `now_ms` the caller passed).
    pub ts_ms: i64,
    /// delivery targets, copied from the rule.
    pub targets: AlertTargets,
}

/// A fire-and-forget alert consumer. `deliver` must NEVER block the caller for long and must NEVER
/// panic (an alert delivery failure is logged, not propagated) — the engine calls it inline while
/// draining rules off the hot fold.
///
/// ⚠ **"NEVER block the caller for long" is a CONTRACT this trait cannot enforce, and
/// [`WebhookSink`] does not keep it.** That sink performs a BLOCKING HTTP POST whose only bound is
/// [`UreqTransport`]'s 10 s global timeout, `AlertEngine::dispatch` calls every sink inline and
/// serially for every fired alert, and nothing between the two batches or defers. Put a
/// wire-touching sink behind [`QueuedSink`] and the contract becomes a property of the type rather
/// than a hope about the endpoint.
pub trait AlertSink: Send + Sync {
    fn deliver(&self, alert: &FiredAlert);

    /// Would this sink do anything at all with `alert`? The ROUTING predicate, hoisted out of
    /// `deliver` so a decorator can ask before spending a resource on an alert the sink will
    /// discard — [`QueuedSink`] checks it before taking a queue slot, so a target that is not
    /// routed cannot crowd out one that is.
    ///
    /// The default is "yes", which is right for a sink that consumes everything (a log sink, a
    /// test tap). An implementation that filters MUST answer here and have `deliver` consult the
    /// same predicate rather than re-spelling it: two copies of a routing rule is exactly the
    /// duplication that lets a decorator and its inner sink disagree.
    fn accepts(&self, _alert: &FiredAlert) -> bool {
        true
    }
}

#[cfg(test)]
mod tests;
