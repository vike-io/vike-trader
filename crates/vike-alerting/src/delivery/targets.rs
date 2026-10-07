//! The webhook target builders: a caller-supplied credential map -> configs -> raw or queued sinks.

use std::collections::HashMap;

use super::{
    AlertSink, QueuedSink, QueuedSinkStop, UreqTransport, WebhookConfig, WebhookKind, WebhookSink,
};

/// **Every credential name [`webhook_configs_from_env`] can look up** — the declaration a SCOPED
/// credential reader needs in order to ask for these three and nothing else.
///
/// Owner ruling 2026-09-16: restrict what a process materialises.
/// `crates/vike-datahub/src/recorder.rs` mounts alerting on a daemon that trades nothing; it
/// declares THIS constant as its scope. Living here rather than at that call site is the point — a
/// scope restated in the consuming crate could drift from the reader, and the drift's symptom would
/// be alerting silently delivering nowhere.
/// `the_declared_webhook_keys_are_the_ones_the_reader_uses` holds the two together.
pub const WEBHOOK_KEYS: [&str; 3] =
    ["VIKE_ALERT_TELEGRAM_TOKEN", "VIKE_ALERT_TELEGRAM_CHAT_ID", "VIKE_ALERT_WEBHOOK_URL"];

/// Build the available webhook targets from a caller-supplied credential map — the ONLY entry
/// point, and `Layer::Injected` by construction (the same `&HashMap<String, String>` shape
/// `vike_tradehub::reconcile_config` and every venue `config.rs` loader take). A target is built ONLY
/// when its credentials are present, so an operator who configures nothing gets an empty list and
/// every webhook-targeted rule delivers nowhere — byte-identical to the alerting engine being off.
///
/// There is deliberately NO convenience twin that opens the credential store itself: the read
/// belongs to the calling binary — where the rule puts it — and a twin would cost this crate a
/// `vike-bridge-core` edge (this crate's `Cargo.toml` says why there is none).
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

/// Build boxed [`WebhookSink`]s (real `ureq` transport) from a set of [`WebhookConfig`]s — the
/// RAW registration of every configured webhook target on an engine.
///
/// ⚠ **These sinks BLOCK the caller for as long as the endpoint takes**, up to
/// [`UreqTransport`]'s 10 s global timeout, once per alert per target, inline in
/// `AlertEngine::dispatch`. The one caller left on this twin is vike-tradehub's summary thread
/// (`crates/vike-tradehub/src/alerts.rs`'s `maybe_mount`), and it survives here only because that
/// thread is DEDICATED — its dispatch parks nothing but itself — and its join is already a bounded,
/// COUNTED task inside the daemon's teardown deadline, so a stall there costs a share of a budget
/// rather than a stop flag going unread. It is NOT acceptable on a loop that also polls a stop
/// flag, which is what [`queued_ureq_webhook_sinks`] is for. Reach for that one unless you can
/// write down, as tradehub's call site does, why this one is safe where you are calling it.
pub fn ureq_webhook_sinks(configs: Vec<WebhookConfig>) -> Vec<Box<dyn AlertSink>> {
    configs
        .into_iter()
        .map(|cfg| Box::new(WebhookSink::new(cfg, UreqTransport::new())) as Box<dyn AlertSink>)
        .collect()
}

/// The BOUNDED twin of [`ureq_webhook_sinks`]: every configured webhook target as a
/// [`QueuedSink`], plus the stop handle its owner must shut down.
///
/// This is what a caller on a LOOP wants — a daemon tick, a repaint, anything that also has to
/// observe a stop flag — because the sinks it returns cannot park that loop on the wire. The plain
/// twin above stays for a caller that has already decided a blocking POST is acceptable where it
/// stands, and its own doc says which is which.
///
/// The two `Vec`s are positionally paired by target and both preserve `configs`' order, so a
/// caller that logs an outcome can name the target from either.
pub fn queued_ureq_webhook_sinks(
    configs: Vec<WebhookConfig>,
    capacity: usize,
) -> (Vec<Box<dyn AlertSink>>, Vec<QueuedSinkStop>) {
    let mut sinks: Vec<Box<dyn AlertSink>> = Vec::with_capacity(configs.len());
    let mut stops = Vec::with_capacity(configs.len());
    for cfg in configs {
        let name = cfg.name.clone();
        let inner: Box<dyn AlertSink> = Box::new(WebhookSink::new(cfg, UreqTransport::new()));
        let (sink, stop) = QueuedSink::spawn(name, capacity, inner);
        sinks.push(Box::new(sink));
        stops.push(stop);
    }
    (sinks, stops)
}
