//! [`WebhookSink`]: a Telegram / generic-JSON POST through an injected, secret-redacting transport.

use std::time::Duration;

#[cfg(doc)]
use super::QueuedSink;
use super::{
    AlertSink, FiredAlert, telegram_bot_url, telegram_send_message_body, telegram_send_message_url,
};

/// **Why a webhook POST did not land**, NON-secret by construction: a status, or that the transport failed; never
/// the url and never the raw transport error (which can echo the token-bearing url). Its `Display` is the coarse
/// category a log line carries (`http 401`, `transport error`); a caller that decides (the OnFailure pager's retry)
/// asks [`Self::is_transient`], never the words (decision 0301).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebhookFailure {
    /// The target answered with this status, outside `2xx` (a `3xx` too: no webhook agent follows a redirect).
    Status(u16),
    /// No answer: the request failed in the transport (connect, TLS, timeout).
    Transport,
}

impl WebhookFailure {
    /// Is another attempt worth it? Everything except a definite `4xx` answer (`401`: a bad token, `400`: a bad
    /// chat id), which a retry would only repeat; `408` and `429` are transient, as curl's `--retry` reads them.
    #[must_use]
    pub fn is_transient(self) -> bool {
        match self {
            WebhookFailure::Status(408 | 429) | WebhookFailure::Transport => true,
            WebhookFailure::Status(status) => !(400..500).contains(&status),
        }
    }
}

impl std::fmt::Display for WebhookFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WebhookFailure::Status(status) => write!(f, "http {status}"),
            WebhookFailure::Transport => f.write_str("transport error"),
        }
    }
}

/// The blocking HTTP POST a [`WebhookSink`] performs, abstracted so the delivery seam is testable
/// with a fake sender. Implementations MUST keep any secret embedded in `url` (a Telegram token, a
/// Discord/Slack path secret) out of their logs; the return value cannot carry one ([`WebhookFailure`]).
pub trait WebhookTransport: Send + Sync {
    /// POST `body` (a JSON string) to `url`. Return `Ok(())` on a 2xx, else why not.
    fn post_json(&self, url: &str, body: &str) -> Result<(), WebhookFailure>;
}

/// The real transport: a blocking `ureq` agent (rustls, no OpenSSL — the one workspace HTTP stack).
pub struct UreqTransport {
    agent: ureq::Agent,
}

impl UreqTransport {
    pub fn new() -> Self {
        // The network layer's timed no-redirect agent (decision 0299): a 4xx/5xx is a RESPONSE, `10 s` bounds one
        // whole request. No redirect: the url is the secret and a followed 307/308 re-POSTs the alert to another
        // host; a 3xx reads as `http 30x`.
        UreqTransport {
            agent: vike_transport::http::blocking_agent_with_timeout(Duration::from_secs(10)),
        }
    }
}

impl Default for UreqTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl WebhookTransport for UreqTransport {
    fn post_json(&self, url: &str, body: &str) -> Result<(), WebhookFailure> {
        match self.agent.post(url).header("content-type", "application/json").send(body.as_bytes())
        {
            Ok(mut response) => {
                let status = response.status().as_u16();
                let _ = response.body_mut().read_to_string(); // drain so the connection can be reused
                if (200..300).contains(&status) {
                    Ok(())
                } else {
                    // a bare HTTP status carries no secret — safe to surface.
                    Err(WebhookFailure::Status(status))
                }
            }
            // Deliberately opaque: a transport error's Display can echo the request target (the
            // token-bearing url), so we NEVER carry it or the url here.
            Err(_) => Err(WebhookFailure::Transport),
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
    /// A generic JSON webhook (Discord / Slack / custom): `POST {content, title, text, rule}` to `url`, where
    /// `content` is the title and the text on two lines, at most 2000 characters (a Discord execute-webhook url
    /// posts it as the message and refuses a body without it; a custom receiver reads the other three).
    /// The URL itself is treated as a SECRET (Discord/Slack webhook URLs embed a token in the path).
    Generic { url: String },
}

/// The most characters a Discord message's `content` may hold: a longer one is refused, so the generic body cuts it.
const DISCORD_CONTENT_CHARS: usize = 2000;

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
    config: WebhookConfig,
    transport: T,
}

impl<T: WebhookTransport> WebhookSink<T> {
    pub fn new(config: WebhookConfig, transport: T) -> Self {
        WebhookSink { config, transport }
    }

    /// **POST `alert` to this target and say whether it landed** — the one delivery path, which
    /// [`deliver`](AlertSink::deliver) calls too, so the URL and body are built in one place.
    ///
    /// It does NOT consult [`accepts`](AlertSink::accepts): a caller that asks for the outcome has
    /// already chosen this target (the OnFailure pager, `vike-backend notify`, builds its own alert
    /// for exactly one target and must exit non-zero when the send did not land). `deliver` keeps
    /// the routing check in front of it.
    ///
    /// # Errors
    /// The transport's [`WebhookFailure`] (`http 401`, `transport error`) — never the url, never the
    /// raw transport error, so the value is safe to log.
    pub fn try_deliver(&self, alert: &FiredAlert) -> Result<(), WebhookFailure> {
        let (url, body) = self.endpoint_and_body(alert);
        self.transport.post_json(&url, &body)
    }

    /// The `(url, json_body)` this alert would POST — the token-bearing endpoint. PRIVATE and used
    /// only inside [`try_deliver`](Self::try_deliver); never logged.
    fn endpoint_and_body(&self, alert: &FiredAlert) -> (String, String) {
        match &self.config.kind {
            WebhookKind::Telegram { token, chat_id } => {
                let url = telegram_send_message_url(&telegram_bot_url(token));
                let text = format!("{}\n{}", alert.title, alert.body);
                let body = telegram_send_message_body(chat_id, &text);
                (url, body)
            }
            WebhookKind::Generic { url } => {
                // `content` is what a Discord execute-webhook url posts (it refuses a body without it), cut to
                // Discord's limit; a custom receiver reads the three keys beside it.
                let content: String = format!("{}\n{}", alert.title, alert.body)
                    .chars()
                    .take(DISCORD_CONTENT_CHARS)
                    .collect();
                let body = serde_json::json!({
                    "content": content,
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
        if let Err(category) = self.try_deliver(alert) {
            // Log the target NAME + coarse category only — NEVER the url (Telegram token) or the
            // raw transport error.
            tracing::warn!(
                target_name = %self.config.name,
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
        alert.targets.webhooks.iter().any(|w| w == &self.config.name)
    }
}
