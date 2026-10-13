//! The Telegram Bot API's PURE request shapes — the endpoint URL and the `sendMessage` body — spelled
//! ONCE, for the two callers that POST one: [`WebhookSink`](super::WebhookSink) (alert delivery) and
//! the trading daemon's chat-control surface (`crates/vike-tradehub/src/telegram/deps.rs`).
//!
//! **This module owns no transport, timeout, retry or error mapping**: each caller keeps its own
//! (the sink's 10 s `ureq` agent and coarse-category errors; the daemon's `AGENT_TIMEOUT` agent and
//! its own log lines), and they differ on purpose.
//!
//! ⚠ **The bot token is a credential.** [`telegram_bot_url`] returns a string that EMBEDS it, so the
//! value is a secret: never `Debug`-format it into a type that derives `Debug`, never log it, never
//! put it on argv. This module defines no struct for it to live in, and nothing here logs.

/// `https://api.telegram.org/bot<token>` — the base every Bot API method hangs off
/// (`<base>/sendMessage`, `<base>/getUpdates`). The result contains `token`: treat it as one.
#[must_use]
pub fn telegram_bot_url(token: &str) -> String {
    format!("https://api.telegram.org/bot{token}")
}

/// The `sendMessage` endpoint under a [`telegram_bot_url`] base.
#[must_use]
pub fn telegram_send_message_url(bot_url: &str) -> String {
    format!("{bot_url}/sendMessage")
}

/// The JSON body of a `sendMessage` POST: `{"chat_id": <chat_id>, "text": <text>}`. `chat_id` is
/// serialised as the caller holds it — a JSON string for the alert sink's `String` ids, a JSON number
/// for the chat-control surface's `i64` — because Telegram accepts both and the two callers have
/// always sent their own.
#[must_use]
pub fn telegram_send_message_body(chat_id: impl serde::Serialize, text: &str) -> String {
    serde_json::json!({ "chat_id": chat_id, "text": text }).to_string()
}
