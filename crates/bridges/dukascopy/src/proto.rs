//! The JSON-lines stdio protocol between `DukascopyExecutionClient` and the JForex sidecar.
//!
//! One compact JSON object per line. Rust → sidecar: [`Command`] (tag `cmd`).
//! Sidecar → Rust: [`Envelope`] (tag `kind`); the `event` payload is the canonical
//! [`Event`] tagged union, so the Rust side needs no venue mapping at all. Parse
//! failures return `None` — callers log-and-skip (forward compatibility, never a
//! crash on a bad line). Both directions derive Serialize AND Deserialize so the
//! fake test bridge can speak the protocol in reverse.

use serde::{Deserialize, Serialize};
use vike_model::events::Event;
use vike_model::OrderRequest;

/// Rust → sidecar command. `order` is the `OrderRequest` serde shape verbatim —
/// `crates/vike-model/src/order.rs` is the single schema authority for both sides.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "lowercase")]
pub enum Command {
    Submit { order: Box<OrderRequest> },
    Cancel { client_order_id: String },
    Shutdown,
}

/// Sidecar → Rust envelope. Exactly one of `ready` | `fatal` opens the stream
/// (the handshake); `event` lines carry the canonical [`Event`].
///
/// `position` (additive, netting-truth law A7): the sidecar's AUTHORITATIVE per-instrument net
/// position — signed size in units and the signed-amount-weighted average of its remaining
/// per-order open prices — emitted right after every fill it reports. JForex is
/// position-per-order and the sidecar realizes netted closes at EACH order's own entry, so the
/// Rust blended fold can drift in (realized, avg) while net size agrees; this line carries the
/// venue truth the exec client re-anchors against (see `netting.rs`). Back-compatible both
/// directions: an old Rust reader sees an unknown `kind` and logs-and-skips; a new reader with
/// an old jar simply never receives the line (no re-anchor — pre-fix behavior).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Envelope {
    Ready {
        account: String,
        #[serde(default)]
        balance: f64,
    },
    Event {
        event: Box<Event>,
    },
    Position {
        /// Canonical symbol (EURUSD form), same as `FillEvent.symbol`.
        symbol: String,
        /// Signed net size in UNITS (not JForex millions): >0 long, <0 short, 0 flat.
        size: f64,
        /// Signed-amount-weighted average open price of the remaining orders; 0.0 when flat.
        avg_px: f64,
        #[serde(default)]
        ts: i64,
    },
    Fatal {
        reason: String,
    },
}

/// Compact one-line JSON (no trailing newline — writers add it).
pub fn encode_line<T: Serialize>(msg: &T) -> String {
    serde_json::to_string(msg).expect("protocol types always serialize")
}

/// Parse a sidecar stdout line; `None` = unparseable/unknown (caller logs + skips).
pub fn parse_envelope(line: &str) -> Option<Envelope> {
    serde_json::from_str(line).ok()
}

/// Parse a Rust command line (used by the fake bridge; the Java twin is `Proto.java`).
pub fn parse_command(line: &str) -> Option<Command> {
    serde_json::from_str(line).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_model::events::OrderAccepted;

    fn order(coid: &str) -> OrderRequest {
        OrderRequest {
            combo_legs: Vec::new(),
            client_order_id: coid.into(),
            venue: "dukascopy".into(),
            symbol: "EURUSD".into(),
            side: 1,
            qty: 1000.0,
            order_type: "market".into(),
            price: None,
            trigger_price: None,
            reduce_only: false,
            time_in_force: Default::default(),
            gtd_expiry: None,
            ts: 42,
            parent_order_id: None,
            linked_order_ids: vec![],
            order_list_id: None,
            contingency_type: None,
            weight: 0.0,
            stop: None,
            trail: None,
            extreme: None,
            on_close: false,
            margin_mode: None,
            trigger_by: None,
        }
    }

    #[test]
    fn command_round_trips_and_uses_wire_names() {
        let cmd = Command::Submit { order: Box::new(order("c1")) };
        let line = encode_line(&cmd);
        // wire contract: tag + real OrderRequest field names (order_type, not "kind")
        assert!(line.contains(r#""cmd":"submit""#), "{line}");
        assert!(line.contains(r#""order_type":"market""#), "{line}");
        assert!(line.contains(r#""client_order_id":"c1""#), "{line}");
        assert!(!line.contains('\n'));
        assert_eq!(parse_command(&line), Some(cmd));

        let cancel = Command::Cancel { client_order_id: "c1".into() };
        let line = encode_line(&cancel);
        assert!(line.contains(r#""cmd":"cancel""#), "{line}");
        assert_eq!(parse_command(&line), Some(cancel));

        assert_eq!(parse_command(&encode_line(&Command::Shutdown)), Some(Command::Shutdown));
    }

    #[test]
    fn envelope_round_trips_with_canonical_event() {
        let env = Envelope::Event {
            event: Box::new(Event::OrderAccepted(OrderAccepted {
                client_order_id: "c1".into(),
                venue_order_id: Some("12345".into()),
                ts: 9,
            })),
        };
        let line = encode_line(&env);
        assert!(line.contains(r#""kind":"event""#), "{line}");
        assert!(line.contains(r#""type":"OrderAccepted""#), "{line}");
        assert_eq!(parse_envelope(&line), Some(env));

        let ready = parse_envelope(r#"{"kind":"ready","account":"DEMO2cGyrc","balance":100000.0}"#);
        assert_eq!(
            ready,
            Some(Envelope::Ready { account: "DEMO2cGyrc".into(), balance: 100000.0 })
        );
        // balance is optional (serde default)
        assert_eq!(
            parse_envelope(r#"{"kind":"ready","account":"A"}"#),
            Some(Envelope::Ready { account: "A".into(), balance: 0.0 })
        );
        let fatal = parse_envelope(r#"{"kind":"fatal","reason":"login failed"}"#);
        assert_eq!(fatal, Some(Envelope::Fatal { reason: "login failed".into() }));
    }

    /// The additive `position` envelope: round-trips, uses the wire names the Java emitter
    /// writes, and its ABSENCE keeps every pre-existing envelope parsing identically — the
    /// serde back-compat pin for the netting-truth protocol extension (both directions: the
    /// old-reader side is pinned by `bad_lines_parse_to_none`'s unknown-kind → `None` law).
    #[test]
    fn position_envelope_round_trips_and_is_additive() {
        let env =
            Envelope::Position { symbol: "EURUSD".into(), size: -1_000_000.0, avg_px: 1.1, ts: 42 };
        let line = encode_line(&env);
        assert!(line.contains(r#""kind":"position""#), "{line}");
        assert!(line.contains(r#""symbol":"EURUSD""#), "{line}");
        assert_eq!(parse_envelope(&line), Some(env));

        // Exactly what the Java emitter writes (Gson property order), ts optional.
        let java = r#"{"kind":"position","symbol":"USDJPY","size":2000.0,"avg_px":155.25,"ts":7}"#;
        assert_eq!(
            parse_envelope(java),
            Some(Envelope::Position {
                symbol: "USDJPY".into(),
                size: 2000.0,
                avg_px: 155.25,
                ts: 7
            })
        );
        assert_eq!(
            parse_envelope(r#"{"kind":"position","symbol":"USDJPY","size":0.0,"avg_px":0.0}"#),
            Some(Envelope::Position { symbol: "USDJPY".into(), size: 0.0, avg_px: 0.0, ts: 0 })
        );
    }

    #[test]
    fn bad_lines_parse_to_none() {
        assert_eq!(parse_envelope("not json"), None);
        assert_eq!(parse_envelope(r#"{"kind":"mystery"}"#), None);
        assert_eq!(parse_command(r#"{"cmd":"mystery"}"#), None);
        assert_eq!(parse_command(""), None);
    }
}
