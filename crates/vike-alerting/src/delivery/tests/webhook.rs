//! Webhook delivery tests: the declared key set, both POST shapes, failure swallowing, redaction.

use super::*;
use std::collections::HashMap;

/// **[`WEBHOOK_KEYS`] and [`webhook_configs_from_env`] must not drift apart**, because a
/// SCOPED credential reader declares that constant and the drift's symptom would be alerting
/// silently delivering nowhere on a daemon that scoped its read
/// (`crates/vike-datahub/src/recorder.rs`).
///
/// Both directions, as far as they can be checked from a fixed-name reader:
///
/// * **Nothing else is needed** — a map holding ONLY these three names produces the MAXIMAL
///   result (both targets). A fourth required name added to the reader without joining the
///   constant would stop one of those targets materialising, and this reddens.
/// * **Every declared name is USED** — dropping any one of the three from an otherwise complete
///   map changes the result. A name left in the constant after its read was deleted reddens
///   here rather than quietly widening what a scoped process materialises.
#[test]
fn the_declared_webhook_keys_are_the_ones_the_reader_uses() {
    let target_names = |env: &HashMap<String, String>| -> Vec<String> {
        webhook_configs_from_env(env).into_iter().map(|c| c.name).collect()
    };
    let full: HashMap<String, String> =
        WEBHOOK_KEYS.iter().map(|k| ((*k).to_string(), format!("v-{k}"))).collect();
    let complete = target_names(&full);
    assert_eq!(
        complete,
        vec!["telegram".to_string(), "webhook".to_string()],
        "the three declared names alone must build every target this reader can build — a \
             fourth name the reader needs has appeared and is NOT in WEBHOOK_KEYS"
    );
    for dropped in WEBHOOK_KEYS {
        let mut partial = full.clone();
        partial.remove(dropped);
        assert_ne!(
            target_names(&partial),
            complete,
            "{dropped} is declared in WEBHOOK_KEYS but removing it changes nothing — the \
                 reader no longer uses it, so a scoped process is materialising a credential for \
                 no one"
        );
    }
}

#[test]
fn webhook_sink_posts_a_telegram_message_for_a_matching_target_only() {
    let fake = FakeTransport::default();
    let sink = WebhookSink::new(telegram_cfg("123:ABC", "9001"), fake.clone());
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
    let sink = WebhookSink::new(generic_cfg("https://example.test/hook/secret"), fake.clone());
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
    let sink = WebhookSink::new(telegram_cfg("t", "c"), fake.clone());
    sink.deliver(&alert(&["telegram"], true)); // must not panic
    assert_eq!(fake.calls.lock().unwrap().len(), 1, "the POST was attempted");
}

#[test]
fn webhook_config_debug_redacts_the_secret() {
    let tg = telegram_cfg("123456:SUPER-SECRET-TOKEN", "9001");
    let dbg = format!("{tg:?}");
    assert!(!dbg.contains("SUPER-SECRET-TOKEN"), "token must never reach Debug: {dbg}");
    assert!(!dbg.contains("123456:SUPER-SECRET-TOKEN"));
    assert!(dbg.contains("<redacted>"));
    assert!(dbg.contains("9001"), "the non-secret chat_id may show for diagnostics");

    let generic = generic_cfg("https://discord.com/api/webhooks/111/AAA-secret-BBB");
    let dbg = format!("{generic:?}");
    assert!(!dbg.contains("secret"), "a webhook URL is a secret and must not reach Debug: {dbg}");
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
