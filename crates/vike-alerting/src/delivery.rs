//! The delivery seam: where a [`FiredAlert`] goes. An [`AlertSink`] is a fire-and-forget consumer;
//! `AlertEngine` (feature `core`) broadcasts each fired alert to every registered sink, and each
//! sink SELF-FILTERS on the alert's `AlertTargets` (so routing lives in the alert, not the
//! dispatcher).
//!
//! Two sinks ship here:
//! - [`InProcessSink`] — an in-memory buffer the (future) GUI drains for toasts / OS notifications
//!   (the `vike-app` window — compile-checked but untested in CI — is the explicit follow-up).
//! - [`WebhookSink`] — an optional Telegram / generic-webhook POST over the existing `ureq` + rustls
//!   stack (no second HTTP/TLS crate). The blocking POST is done by an injected [`WebhookTransport`]
//!   so the seam is testable with a fake sender and never touches the network in CI.
//!
//! **Secrets** (`WebhookKind`'s bot token, a Discord/Slack webhook URL) never reach `Debug` (manual
//! redacting impl) and never reach a log: a failed [`WebhookSink`] delivery logs the target NAME and
//! a coarse error CATEGORY only — never the endpoint URL (a Telegram URL embeds the token) nor the
//! raw transport error (which can echo the request target). Built from a CALLER-SUPPLIED credential
//! map via [`webhook_configs_from_env`], the same absent-credentials-is-the-gate idiom the venues
//! use — this crate never opens the credential store itself, so nothing here can load a Telegram
//! token its caller did not hand it.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::rule::AlertTargets;

/// One fired alert — the immutable output of the evaluator, handed to every [`AlertSink`]. Carries
/// the rule's delivery `targets` so the dispatcher needs no back-reference to the rule.
///
/// It lives HERE, not in the (feature-`core`) evaluator that produces it, because it is the one
/// type the crate's two halves share: the sinks below take it, so a vike-free consumer — the
/// standalone recorder watchdog this crate was split out for — must be able to build and deliver
/// one without `vike-core` in its graph.
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
pub trait AlertSink: Send + Sync {
    fn deliver(&self, alert: &FiredAlert);
}

/// An in-memory buffer of fired alerts — the future toast / OS-notification seam. Cheap to clone
/// (an `Arc` handle to the shared inbox); the GUI holds one clone and [`drain`](Self::drain)s it on
/// repaint, the engine holds another as a boxed sink.
#[derive(Clone, Default)]
pub struct InProcessSink {
    inbox: Arc<Mutex<Vec<FiredAlert>>>,
}

impl InProcessSink {
    pub fn new() -> Self {
        Self::default()
    }

    /// A shared handle onto the same inbox — hand this to the GUI so it drains what the engine's
    /// boxed sink writes.
    pub fn handle(&self) -> Arc<Mutex<Vec<FiredAlert>>> {
        self.inbox.clone()
    }

    /// Take + clear every buffered alert (the GUI's per-repaint drain).
    pub fn drain(&self) -> Vec<FiredAlert> {
        std::mem::take(&mut *self.inbox.lock().unwrap())
    }

    /// How many alerts are currently buffered (undelivered).
    pub fn len(&self) -> usize {
        self.inbox.lock().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl AlertSink for InProcessSink {
    fn deliver(&self, alert: &FiredAlert) {
        if alert.targets.in_process {
            self.inbox.lock().unwrap().push(alert.clone());
        }
    }
}

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
/// built at runtime from the `.env`, not persisted alongside the rules.
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
        if !alert.targets.webhooks.iter().any(|w| w == &self.cfg.name) {
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
}

/// Build the available webhook targets from a caller-supplied credential map — the ONLY entry
/// point, and `Layer::Injected` by construction (the same `&HashMap<String, String>` shape
/// `vike_ops::reconcile_config` and every venue `config.rs` loader take). A target is built ONLY
/// when its credentials are present, so an operator who configures nothing gets an empty list and
/// every webhook-targeted rule delivers nowhere — byte-identical to the alerting engine being off.
///
/// There is deliberately NO convenience twin that opens the credential store itself. One existed
/// (`webhook_configs_from_workspace_env`, behind a `workspace-env` feature) and was the whole
/// reason this crate carried an optional `vike-bridge-core` edge; its single caller was
/// `vike-tradehub`'s `main.rs`, which already sweeps the process env and resolves the full
/// credential chain for the live mount. Deleting it moved the read to that binary — where the rule
/// puts it — and removed the crate's last non-`core` vike-\* dependency.
///
/// Keys (the workspace credential store):
/// - `VIKE_ALERT_TELEGRAM_TOKEN` + `VIKE_ALERT_TELEGRAM_CHAT_ID` ⇒ a `"telegram"` target.
/// - `VIKE_ALERT_WEBHOOK_URL` ⇒ a `"webhook"` (generic) target.
pub fn webhook_configs_from_env(env: &HashMap<String, String>) -> Vec<WebhookConfig> {
    let get = |k: &str| -> Option<String> {
        let v = env.get(k)?.trim();
        (!v.is_empty()).then(|| v.to_string())
    };
    let mut out = Vec::new();
    if let (Some(token), Some(chat_id)) =
        (get("VIKE_ALERT_TELEGRAM_TOKEN"), get("VIKE_ALERT_TELEGRAM_CHAT_ID"))
    {
        out.push(WebhookConfig {
            name: "telegram".to_string(),
            kind: WebhookKind::Telegram { token, chat_id },
        });
    }
    if let Some(url) = get("VIKE_ALERT_WEBHOOK_URL") {
        out.push(WebhookConfig { name: "webhook".to_string(), kind: WebhookKind::Generic { url } });
    }
    out
}

/// Build boxed [`WebhookSink`]s (real `ureq` transport) from a set of [`WebhookConfig`]s — the one
/// call `vike-app` makes to register every configured webhook target on the engine.
pub fn ureq_webhook_sinks(configs: Vec<WebhookConfig>) -> Vec<Box<dyn AlertSink>> {
    configs
        .into_iter()
        .map(|cfg| Box::new(WebhookSink::new(cfg, UreqTransport::new())) as Box<dyn AlertSink>)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rule::AlertTargets;

    fn alert(webhooks: &[&str], in_process: bool) -> FiredAlert {
        FiredAlert {
            rule_id: "r1".into(),
            title: "BTC breakout".into(),
            body: "binance BTCUSDT crossed above 100000".into(),
            ts_ms: 42,
            targets: AlertTargets {
                in_process,
                webhooks: webhooks.iter().map(|s| s.to_string()).collect(),
            },
        }
    }

    /// A transport double that records every POST (never touches the network) and can be told to
    /// fail, to exercise the swallow-the-error path.
    #[derive(Clone, Default)]
    struct FakeTransport {
        calls: Arc<Mutex<Vec<(String, String)>>>,
        fail: bool,
    }

    impl WebhookTransport for FakeTransport {
        fn post_json(&self, url: &str, body: &str) -> Result<(), String> {
            self.calls.lock().unwrap().push((url.to_string(), body.to_string()));
            if self.fail {
                Err("boom".to_string())
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn in_process_sink_buffers_only_in_process_targeted_alerts() {
        let sink = InProcessSink::new();
        sink.deliver(&alert(&[], true)); // in-process on → buffered
        sink.deliver(&alert(&["telegram"], false)); // in-process off → NOT buffered
        assert_eq!(sink.len(), 1);
        let drained = sink.drain();
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].title, "BTC breakout");
        assert!(sink.is_empty(), "drain clears the buffer");
    }

    #[test]
    fn in_process_handle_shares_the_same_inbox() {
        let sink = InProcessSink::new();
        let handle = sink.handle();
        sink.deliver(&alert(&[], true));
        assert_eq!(handle.lock().unwrap().len(), 1, "the GUI-side handle sees the engine's writes");
    }

    #[test]
    fn webhook_sink_posts_a_telegram_message_for_a_matching_target_only() {
        let fake = FakeTransport::default();
        let sink = WebhookSink::new(
            WebhookConfig {
                name: "telegram".into(),
                kind: WebhookKind::Telegram { token: "123:ABC".into(), chat_id: "9001".into() },
            },
            fake.clone(),
        );
        // routed to "telegram" → one POST.
        sink.deliver(&alert(&["telegram"], true));
        // NOT routed to "telegram" (different name) → no POST.
        sink.deliver(&alert(&["webhook"], true));
        // in-process-only alert → no POST.
        sink.deliver(&alert(&[], true));

        let calls = fake.calls.lock().unwrap();
        assert_eq!(calls.len(), 1, "exactly the one matching-target alert POSTs");
        let (url, body) = &calls[0];
        assert_eq!(url, "https://api.telegram.org/bot123:ABC/sendMessage");
        let v: serde_json::Value = serde_json::from_str(body).unwrap();
        assert_eq!(v["chat_id"], "9001");
        assert!(v["text"].as_str().unwrap().contains("BTC breakout"));
        assert!(v["text"].as_str().unwrap().contains("crossed above 100000"));
    }

    #[test]
    fn webhook_sink_posts_a_generic_json_body() {
        let fake = FakeTransport::default();
        let sink = WebhookSink::new(
            WebhookConfig {
                name: "webhook".into(),
                kind: WebhookKind::Generic { url: "https://example.test/hook/secret".into() },
            },
            fake.clone(),
        );
        sink.deliver(&alert(&["webhook"], false));
        let calls = fake.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        let (url, body) = &calls[0];
        assert_eq!(url, "https://example.test/hook/secret");
        let v: serde_json::Value = serde_json::from_str(body).unwrap();
        assert_eq!(v["rule"], "r1");
        assert_eq!(v["title"], "BTC breakout");
    }

    #[test]
    fn webhook_delivery_failure_is_swallowed_not_propagated() {
        // A failing transport must not panic or propagate — deliver returns normally (the error is
        // logged with the target name + coarse category, never the url).
        let fake = FakeTransport { calls: Arc::new(Mutex::new(Vec::new())), fail: true };
        let sink = WebhookSink::new(
            WebhookConfig {
                name: "telegram".into(),
                kind: WebhookKind::Telegram { token: "t".into(), chat_id: "c".into() },
            },
            fake.clone(),
        );
        sink.deliver(&alert(&["telegram"], true)); // must not panic
        assert_eq!(fake.calls.lock().unwrap().len(), 1, "the POST was attempted");
    }

    #[test]
    fn webhook_config_debug_redacts_the_secret() {
        let tg = WebhookConfig {
            name: "telegram".into(),
            kind: WebhookKind::Telegram {
                token: "123456:SUPER-SECRET-TOKEN".into(),
                chat_id: "9001".into(),
            },
        };
        let dbg = format!("{tg:?}");
        assert!(!dbg.contains("SUPER-SECRET-TOKEN"), "token must never reach Debug: {dbg}");
        assert!(!dbg.contains("123456:SUPER-SECRET-TOKEN"));
        assert!(dbg.contains("<redacted>"));
        assert!(dbg.contains("9001"), "the non-secret chat_id may show for diagnostics");

        let generic = WebhookConfig {
            name: "webhook".into(),
            kind: WebhookKind::Generic {
                url: "https://discord.com/api/webhooks/111/AAA-secret-BBB".into(),
            },
        };
        let dbg = format!("{generic:?}");
        assert!(
            !dbg.contains("secret"),
            "a webhook URL is a secret and must not reach Debug: {dbg}"
        );
        assert!(!dbg.contains("discord.com"));
    }

    #[test]
    fn webhook_configs_from_env_builds_only_present_targets() {
        // empty env → no targets (byte-identical to alerting off).
        assert!(webhook_configs_from_env(&HashMap::new()).is_empty());

        // telegram needs BOTH token and chat_id — token alone yields nothing.
        let mut partial = HashMap::new();
        partial.insert("VIKE_ALERT_TELEGRAM_TOKEN".to_string(), "t".to_string());
        assert!(
            webhook_configs_from_env(&partial).is_empty(),
            "an incomplete telegram config builds no target"
        );

        let mut full = HashMap::new();
        full.insert("VIKE_ALERT_TELEGRAM_TOKEN".to_string(), "123:ABC".to_string());
        full.insert("VIKE_ALERT_TELEGRAM_CHAT_ID".to_string(), "9001".to_string());
        full.insert("VIKE_ALERT_WEBHOOK_URL".to_string(), "https://example.test/h".to_string());
        let cfgs = webhook_configs_from_env(&full);
        assert_eq!(cfgs.len(), 2);
        assert_eq!(cfgs[0].name, "telegram");
        assert!(matches!(cfgs[0].kind, WebhookKind::Telegram { .. }));
        assert_eq!(cfgs[1].name, "webhook");
        assert!(matches!(cfgs[1].kind, WebhookKind::Generic { .. }));
        // and even the built config never leaks its token through Debug.
        assert!(!format!("{:?}", cfgs[0]).contains("ABC"));
    }
}
