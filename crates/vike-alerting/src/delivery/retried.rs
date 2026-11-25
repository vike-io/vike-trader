//! `RetriedWebhook`: a webhook target that asks again after a pause and counts an alert that never landed, over the
//! one shared ladder (`vike_utility::retry::retry_on_pauses`). Every target a daemon registers is one, behind a
//! [`QueuedSink`](super::QueuedSink) ([`queued_ureq_webhook_sinks`](super::queued_ureq_webhook_sinks)), so its pauses
//! sleep on the target's own delivery thread, never the caller's.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use super::{AlertSink, FiredAlert, WebhookConfig, WebhookSink, WebhookTransport};

/// How long a failed delivery waits before each ask again: three asks over about five seconds.
pub(crate) const DELIVERY_RETRY_PAUSES: [Duration; 2] =
    [Duration::from_secs(1), Duration::from_secs(4)];

/// **A webhook target that asks again after a pause, and counts an alert that never landed.** Every failure is asked
/// again, whatever its category: a page sent twice beats one never sent, and a choice by category would be a decision
/// on its text. An alert that fails every ask adds one to `failed` (the caller's counter, shared by its targets) and is
/// said at `warn` with the running total; the line names the target and the coarse category, never the url (a
/// Telegram url holds the token). `pauses` is a parameter so a test asks again without a wait.
pub(crate) struct RetriedWebhook<T: WebhookTransport> {
    name: String,
    sink: WebhookSink<T>,
    pauses: &'static [Duration],
    failed: Arc<AtomicU64>,
}

impl<T: WebhookTransport> RetriedWebhook<T> {
    pub(crate) fn new(
        config: WebhookConfig,
        transport: T,
        pauses: &'static [Duration],
        failed: Arc<AtomicU64>,
    ) -> Self {
        RetriedWebhook {
            name: config.name.clone(),
            sink: WebhookSink::new(config, transport),
            pauses,
            failed,
        }
    }
}

impl<T: WebhookTransport> AlertSink for RetriedWebhook<T> {
    fn deliver(&self, alert: &FiredAlert) {
        if !self.accepts(alert) {
            return;
        }
        let outcome = vike_utility::retry::retry_on_pauses(
            self.pauses,
            |_| self.sink.try_deliver(alert),
            Result::is_ok,
            std::thread::sleep,
        );
        if let Err(category) = outcome {
            let failed_total = self.failed.fetch_add(1, Ordering::Relaxed) + 1;
            tracing::warn!(
                target_name = %self.name, rule = %alert.rule_id, error = %category,
                asks = self.pauses.len() + 1, failed_total,
                "alert NOT delivered: the webhook target failed every ask, so this alert was never sent"
            );
        }
    }

    fn accepts(&self, alert: &FiredAlert) -> bool {
        self.sink.accepts(alert)
    }
}
