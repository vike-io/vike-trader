//! Webhook delivery tests: the declared key set, both POST shapes, failure swallowing, redaction.

use super::*;
use std::assert_matches;
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
    let sink = WebhookSink::new(telegram_config("123:ABC", "9001"), fake.clone());
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
    let sink = WebhookSink::new(generic_config("https://example.test/hook/secret"), fake.clone());
    sink.deliver(&alert(&["webhook"], false));
    let calls = fake.calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    let (url, body) = &calls[0];
    assert_eq!(url, "https://example.test/hook/secret");
    let v: serde_json::Value = serde_json::from_str(body).unwrap();
    assert_eq!(v["rule"], "r1");
    assert_eq!(v["title"], "BTC breakout");
    assert_eq!(v["text"], "binance BTCUSDT crossed above 100000");
}

/// **A Discord execute-webhook url gets the `content` it requires**: the title and the text on two lines, cut on a
/// character boundary to Discord's 2000 characters (a longer message is refused).
#[test]
fn the_generic_body_carries_a_discord_content_within_its_limit() {
    let fake = FakeTransport::default();
    let sink = WebhookSink::new(
        generic_config("https://discord.com/api/webhooks/111/AAA-secret-BBB"),
        fake.clone(),
    );
    sink.deliver(&alert(&["webhook"], false));
    let mut long = alert(&["webhook"], false);
    long.body = "é".repeat(3000); // two bytes each: a byte cut would split one
    sink.deliver(&long);
    let calls = fake.calls.lock().unwrap();
    let content = |i: usize| {
        let v: serde_json::Value = serde_json::from_str(&calls[i].1).unwrap();
        v["content"].as_str().unwrap().to_string()
    };
    assert_eq!(content(0), "BTC breakout\nbinance BTCUSDT crossed above 100000");
    let cut = content(1);
    assert_eq!(cut.chars().count(), 2000);
    assert_eq!(cut, format!("BTC breakout\n{}", "é".repeat(2000 - "BTC breakout\n".len())));
}

#[test]
fn webhook_delivery_failure_is_swallowed_not_propagated() {
    // A failing transport must not panic or propagate — deliver returns normally (the error is
    // logged with the target name + coarse category, never the url).
    let fake = FakeTransport { calls: Arc::new(Mutex::new(Vec::new())), fail: true };
    let sink = WebhookSink::new(telegram_config("t", "c"), fake.clone());
    sink.deliver(&alert(&["telegram"], true)); // must not panic
    assert_eq!(fake.calls.lock().unwrap().len(), 1, "the POST was attempted");
}

#[test]
fn webhook_config_debug_redacts_the_secret() {
    let telegram = telegram_config("123456:SUPER-SECRET-TOKEN", "9001");
    let dbg = format!("{telegram:?}");
    assert!(!dbg.contains("SUPER-SECRET-TOKEN"), "token must never reach Debug: {dbg}");
    assert!(!dbg.contains("123456:SUPER-SECRET-TOKEN"));
    assert!(dbg.contains("<redacted>"));
    assert!(dbg.contains("9001"), "the non-secret chat_id may show for diagnostics");

    let generic = generic_config("https://discord.com/api/webhooks/111/AAA-secret-BBB");
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
    let configs = webhook_configs_from_env(&full);
    assert_eq!(configs.len(), 2);
    assert_eq!(configs[0].name, "telegram");
    assert_matches!(configs[0].kind, WebhookKind::Telegram { .. });
    assert_eq!(configs[1].name, "webhook");
    assert_matches!(configs[1].kind, WebhookKind::Generic { .. });
    // and even the built config never leaks its token through Debug.
    assert!(!format!("{:?}", configs[0]).contains("ABC"));
}

/// `try_deliver` is the outcome-reporting door the OnFailure pager needs: the SAME POST
/// `deliver` makes (one URL/body builder), the transport's category handed back on failure, and
/// no routing check — the caller chose the target.
#[test]
fn try_deliver_posts_the_same_request_and_reports_the_outcome() {
    let ok = FakeTransport::default();
    let sink = WebhookSink::new(telegram_config("123:ABC", "9001"), ok.clone());
    // Not routed to "telegram", and still POSTed: routing is `deliver`'s, not this door's.
    assert_eq!(sink.try_deliver(&alert(&[], false)), Ok(()));
    sink.deliver(&alert(&["telegram"], false));
    let calls = ok.calls.lock().unwrap();
    assert_eq!(calls.len(), 2, "one POST from each door");
    assert_eq!(calls[0], calls[1], "both doors build the identical request");
    assert_eq!(calls[0].0, "https://api.telegram.org/bot123:ABC/sendMessage");

    let failing = FakeTransport { calls: Arc::new(Mutex::new(Vec::new())), fail: true };
    let sink = WebhookSink::new(telegram_config("123:ABC", "9001"), failing.clone());
    let err = sink.try_deliver(&alert(&["telegram"], false)).unwrap_err();
    assert_eq!(err, WebhookFailure::Status(503), "the transport's failure comes back unchanged");
    assert_eq!(failing.calls.lock().unwrap().len(), 1);
}

/// **A webhook POST never follows a redirect.** The URL is a secret (a Telegram token, a
/// Discord/Slack path secret) and the body is the alert; a followed 307/308 would re-POST both to
/// a host the operator never named, and a followed 301/302/303 would silently turn the POST into a
/// GET that delivers nothing. The 3xx comes back as the coarse `http 302` category instead.
///
/// MUTATION PROOF: build `UreqTransport::new`'s agent with `vike_transport::http::public_get_agent` instead of
/// `blocking_agent_with_timeout` — the 302 is followed and the second listener is dialled.
#[test]
fn a_webhook_post_never_follows_a_redirect() {
    let (base, target_hits) = vike_log::loopback::redirecting_origin();
    let err = UreqTransport::new()
        .post_json(&format!("{base}/hook/secret-path"), r#"{"text":"x"}"#)
        .expect_err("a 302 is not a delivery");
    assert_eq!(target_hits.load(std::sync::atomic::Ordering::SeqCst), 0, "the target was dialled");
    assert_eq!(err, WebhookFailure::Status(302));
}

/// **The pager's retry reads the failure's type**: a definite `4xx` is not asked again, `408`, `429`, a `5xx` and a
/// transport failure are; the log words are the coarse category, unchanged.
#[test]
fn a_definite_refusal_is_not_transient_and_the_words_are_the_category() {
    for (failure, transient) in [
        (WebhookFailure::Transport, true),
        (WebhookFailure::Status(500), true),
        (WebhookFailure::Status(429), true),
        (WebhookFailure::Status(408), true),
        (WebhookFailure::Status(302), true),
        (WebhookFailure::Status(400), false),
        (WebhookFailure::Status(403), false),
    ] {
        assert_eq!(failure.is_transient(), transient, "{failure}");
    }
    assert_eq!(WebhookFailure::Status(401).to_string(), "http 401");
    assert_eq!(WebhookFailure::Transport.to_string(), "transport error");
}
