//! The Telegram request shapes, byte for byte: the two callers (the alert sink and the daemon's
//! chat-control surface) each used to spell them inline, so the exact bytes they sent are pinned here.

use super::*;

#[test]
fn the_endpoint_is_the_bot_api_sendmessage_url() {
    let base = telegram_bot_url("123:ABC");
    assert_eq!(base, "https://api.telegram.org/bot123:ABC");
    assert_eq!(telegram_send_message_url(&base), "https://api.telegram.org/bot123:ABC/sendMessage");
}

/// The alert sink holds the chat id as a string, the chat-control surface as an `i64`: each body
/// keeps its own JSON type for it.
#[test]
fn the_body_keeps_the_callers_chat_id_type() {
    assert_eq!(
        telegram_send_message_body("9001".to_string(), "hi\nthere"),
        r#"{"chat_id":"9001","text":"hi\nthere"}"#
    );
    assert_eq!(telegram_send_message_body(-1001_i64, "hi"), r#"{"chat_id":-1001,"text":"hi"}"#);
}
