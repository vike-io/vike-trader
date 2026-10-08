//! [`WebhookSink`]: a Telegram / generic-JSON POST through an injected, secret-redacting transport.

use std::time::Duration;

#[cfg(doc)]
use super::QueuedSink;
use super::{AlertSink, FiredAlert};

/// The blocking HTTP POST a [`WebhookSink`] performs, abstracted so the delivery seam is testable
/// with a fake sender. Implementations MUST keep any secret embedded in `url` (a Telegram token, a
/// Discord/Slack path secret) out of their return value and their logs.
pub trait WebhookTransport: Send + Sync {
    /// POST `body` (a JSON string) to `url`. Return `Ok(())` on a 2xx, `Err(category)` otherwise —
    /// where `category` is a NON-secret coarse label (never the url, never the raw request).
    fn post_json(&self, url: &str, body: &str) -> Result<(), String>;
}

/// The real transport: a blocking `ureq` agent (rustls, no OpenSSL — the one workspace HTTP stack).
pub struct UreqTransport {
    agent: ureq::Agent,
}

impl UreqTransport {
    pub fn new() -> Self {
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(10)))
            .user_agent("vike-trader-rust")
            .build()
            .new_agent();
        UreqTransport { agent }
    }
}

impl Default for UreqTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl WebhookTransport for UreqTransport {
    fn post_json(&self, url: &str, body: &str) -> Result<(), String> {
        match self.agent.post(url).header("content-type", "application/json").send(body.as_bytes())
        {
            Ok(mut resp) => {
                let status = resp.status().as_u16();
                let _ = resp.body_mut().read_to_string(); // drain so the connection can be reused
                if (200..300).contains(&status) {
                    Ok(())
                } else {
                    // a bare HTTP status carries no secret — safe to surface.
                    Err(format!("http {status}"))
                }
            }
            // Deliberately opaque: a transport error's Display can echo the request target (the
            // token-bearing url), so we NEVER interpolate it or the url here.
            Err(_) => Err("transport error".to_string()),
        }
    }
}

/// What a webhook target IS + where it points. Holds the secret (Telegram bot token / webhook url),
/// so its [`Debug`] is MANUAL and redacting (see the impl below) and it is NEVER serialized — it is
/// built at runtime from the caller's credential map, not persisted alongside the rules.
#[derive(Clone)]
pub enum WebhookKind {
    /// Telegram Bot API: `POST https://api.telegram.org/bot<token>/sendMessage` with
    /// `{chat_id, text}`. `token` is the secret; `chat_id` is a plain conversation id.
    Telegram { token: String, chat_id: String },
    /// A generic JSON webhook (Discord / Slack / custom): `POST {title, text, rule}` to `url`.
    /// The URL itself is treated as a SECRET (Discord/Slack webhook URLs embed a token in the path).
    Generic { url: String },
}

impl std::fmt::Debug for WebhookKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // token redacted; chat_id is not a credential (kept for diagnostics).
            WebhookKind::Telegram { chat_id, .. } => f
                .debug_struct("Telegram")
                .field("token", &"<redacted>")
                .field("chat_id", chat_id)
                .finish(),
            // the whole URL is redacted (it can carry a path secret).
            WebhookKind::Generic { .. } => {
                f.debug_struct("Generic").field("url", &"<redacted>").finish()
            }
        }
    }
}

/// One named webhook target a rule references by [`name`](Self::name) in
/// `AlertTargets::webhooks`. `Debug` is derived but redacts through [`WebhookKind`]'s manual impl.
#[derive(Clone, Debug)]
pub struct WebhookConfig {
    /// the target name rules deliver to (e.g. `"telegram"`, `"webhook"`).
    pub name: String,
    pub kind: WebhookKind,
}

/// A webhook [`AlertSink`]: POSTs a fired alert to its configured target when (and only when) the
/// alert's `AlertTargets::webhooks` name it. Generic over the [`WebhookTransport`] so tests inject
/// a fake sender.
pub struct WebhookSink<T: WebhookTransport> {
    cfg: WebhookConfig,
    transport: T,
}

impl<T: WebhookTransport> WebhookSink<T> {
    pub fn new(cfg: WebhookConfig, transport: T) -> Self {
        WebhookSink { cfg, transport }
    }

    /// The `(url, json_body)` this alert would POST — the token-bearing endpoint. PRIVATE and used
    /// only inside [`deliver`](Self::deliver); never logged.
    fn endpoint_and_body(&self, alert: &FiredAlert) -> (String, String) {
        match &self.cfg.kind {
            WebhookKind::Telegram { token, chat_id } => {
                let url = format!("https://api.telegram.org/bot{token}/sendMessage");
                let text = format!("{}\n{}", alert.title, alert.body);
                let body = serde_json::json!({ "chat_id": chat_id, "text": text }).to_string();
                (url, body)
            }
            WebhookKind::Generic { url } => {
                let body = serde_json::json!({
                    "title": alert.title,
                    "text": alert.body,
                    "rule": alert.rule_id,
                })
                .to_string();
                (url.clone(), body)
            }
        }
    }
}

impl<T: WebhookTransport> AlertSink for WebhookSink<T> {
    fn deliver(&self, alert: &FiredAlert) {
        if !self.accepts(alert) {
            return; // this alert is not routed to this webhook target
        }
        let (url, body) = self.endpoint_and_body(alert);
        if let Err(category) = self.transport.post_json(&url, &body) {
            // Log the target NAME + coarse category only — NEVER the url (Telegram token) or the
            // raw transport error.
            tracing::warn!(
                target_name = %self.cfg.name,
                rule = %alert.rule_id,
                error = %category,
                "alert webhook delivery failed"
            );
        }
    }

    /// Which alerts this target is named by — the routing rule, spelled ONCE (see
    /// [`AlertSink::accepts`]). A [`QueuedSink`] in front of this sink asks it before taking a
    /// queue slot, so a Telegram outage cannot consume the Discord target's queue.
    fn accepts(&self, alert: &FiredAlert) -> bool {
        alert.targets.webhooks.iter().any(|w| w == &self.cfg.name)
    }
}
