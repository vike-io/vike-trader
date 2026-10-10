//! Delivery tests: the shared fixtures (`alert`, the two target configs) and `FakeTransport`
//! double, and the in-process sink.

use super::*;
use crate::rule::AlertTargets;
use std::sync::{Arc, Mutex};

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

/// The `telegram` target, under the name `webhook_configs_from_env` gives it.
fn telegram_cfg(token: &str, chat_id: &str) -> WebhookConfig {
    WebhookConfig {
        name: "telegram".into(),
        kind: WebhookKind::Telegram { token: token.into(), chat_id: chat_id.into() },
    }
}

/// The generic `webhook` target, under the name `webhook_configs_from_env` gives it.
fn generic_cfg(url: &str) -> WebhookConfig {
    WebhookConfig { name: "webhook".into(), kind: WebhookKind::Generic { url: url.into() } }
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
        if self.fail { Err("boom".to_string()) } else { Ok(()) }
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

#[cfg(test)]
mod queued;
#[cfg(test)]
mod webhook;
