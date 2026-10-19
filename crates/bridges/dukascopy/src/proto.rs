//! The JSON-lines stdio protocol between `DukascopyExecutionClient` and the JForex sidecar.
//!
//! One compact JSON object per line. Rust → sidecar: [`Command`] (tag `cmd`).
//! Sidecar → Rust: [`Envelope`] (tag `kind`); the `event` payload is the canonical
//! [`Event`] tagged union, so the Rust side needs no venue mapping at all. Parse
//! failures return `None` — callers log-and-skip (forward compatibility, never a
//! crash on a bad line). Both directions derive Serialize AND Deserialize so the
//! fake test bridge can speak the protocol in reverse.

use serde::{Deserialize, Serialize};
use vike_model::OrderRequest;
use vike_model::events::Event;

/// Rust → sidecar command. `order` is the `OrderRequest` serde shape verbatim —
/// `crates/vike-model/src/orders/order.rs` is the single schema authority for both sides.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "lowercase")]
pub enum Command {
    Submit {
        order: Box<OrderRequest>,
    },
    Cancel {
        client_order_id: String,
    },
    /// Amend a RESTING (`OPENED`) order in place: the new TOTAL quantity in UNITS and/or the new limit price.
    /// JForex has no amend request, it has `IOrder.setRequestedAmount` / `setOpenPrice`, each answered by an
    /// `ORDER_CHANGED_OK` / `ORDER_CHANGED_REJECTED` message that the sidecar maps to `OrderModified` /
    /// `OrderModifyRejected`. An absent field leaves that term alone.
    Modify {
        client_order_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        new_qty: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        new_price: Option<f64>,
        #[serde(default)]
        ts: i64,
    },
    /// Start live quotes for these instruments (canonical six-letter spelling, `EURUSD`): the price lane
    /// (`crate::quotes`). The sidecar answers with `tick` envelopes for them and for no other instrument, and
    /// only after this command, so nothing is priced at login. Additive: an old jar logs the unknown command and
    /// skips it (the client does not send it to a jar that did not announce `ticks`).
    Subscribe {
        symbols: Vec<String>,
    },
    /// Stop the `tick` envelopes of these instruments (the order path's own subscriptions are untouched).
    Unsubscribe {
        symbols: Vec<String>,
    },
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
///
/// `tick` (additive, the price lane): one live quote of an instrument a `subscribe` command asked for. Back-compatible
/// both ways like `position`: a jar emits it only after a `subscribe` an old Rust reader never sends (and an old
/// reader would log-and-skip the unknown kind anyway), and a new reader with an old jar never receives one and
/// never sends the command (the `ticks` feature word of `ready` is what it checks first).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Envelope {
    Ready {
        account: String,
        #[serde(default)]
        balance: f64,
        /// What this jar can do beyond the original protocol (additive): `"modify"` = it understands the
        /// `modify` command. An old jar sends none, which the client reads as "cannot amend" and answers a
        /// modify with an explicit refusal instead of writing a command nobody will answer.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        features: Vec<String>,
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
    Tick {
        /// Canonical symbol (EURUSD form), same as `FillEvent.symbol`.
        symbol: String,
        bid: f64,
        ask: f64,
        /// The tick's OWN time (epoch ms, the venue's): what makes a quote provably current. `0` (absent) is a
        /// tick with no time, which `crate::quotes` never prices from.
        #[serde(default)]
        time: i64,
    },
    /// The sidecar's answer to ONE symbol of a `subscribe` command (additive; announced by the `tick-acks` feature
    /// word of `ready`, `crate::quotes::FEATURE_TICK_ACKS`): `ok` = the instrument is marked and the SDK subscribe was
    /// made, `!ok` = the sidecar declined it (unknown instrument, past its cap, the SDK refused) and `reason` says
    /// why. `symbol` is the string the command carried. Back-compatible both ways: an old jar never sends it (and the
    /// client, reading no `tick-acks`, never waits for it), and an old reader logs-and-skips the unknown kind.
    Subscription {
        symbol: String,
        ok: bool,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        reason: String,
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
            data_delay: None,
            account: None,
            combo_legs: Vec::new(),
            client_order_id: coid.into(),
            venue: "dukascopy".into(),
            symbol: "EURUSD".into(),
            side: 1,
            qty: 1000.0,
            order_type: vike_model::OrderType::Market,
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

    /// The amend command: wire names, an absent term is OMITTED (the Java twin reads a missing key as "keep"), and
    /// both the sparse and the full form read back.
    #[test]
    fn modify_command_round_trips_and_omits_an_absent_term() {
        let price_only = Command::Modify {
            client_order_id: "c1".into(),
            new_qty: None,
            new_price: Some(1.2345),
            ts: 7,
        };
        let line = encode_line(&price_only);
        assert!(line.contains(r#""cmd":"modify""#), "{line}");
        assert!(line.contains(r#""new_price":1.2345"#), "{line}");
        assert!(!line.contains("new_qty"), "an absent term is not sent: {line}");
        assert_eq!(parse_command(&line), Some(price_only));

        let both = Command::Modify {
            client_order_id: "c1".into(),
            new_qty: Some(2000.0),
            new_price: Some(1.2),
            ts: 0,
        };
        assert_eq!(parse_command(&encode_line(&both)), Some(both));
        // a sparse line from another writer still parses
        assert!(matches!(
            parse_command(r#"{"cmd":"modify","client_order_id":"c1","new_qty":1000.0}"#),
            Some(Command::Modify { new_price: None, new_qty: Some(_), .. })
        ));
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
            Some(Envelope::Ready {
                account: "DEMO2cGyrc".into(),
                balance: 100000.0,
                features: vec![]
            })
        );
        // balance is optional (serde default)
        assert_eq!(
            parse_envelope(r#"{"kind":"ready","account":"A"}"#),
            Some(Envelope::Ready { account: "A".into(), balance: 0.0, features: vec![] })
        );
        // the jar announces what it can do; a jar that says nothing (every jar before the amend) reads as []
        assert_eq!(
            parse_envelope(r#"{"kind":"ready","account":"A","features":["modify"]}"#),
            Some(Envelope::Ready {
                account: "A".into(),
                balance: 0.0,
                features: vec!["modify".into()]
            })
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

    /// The price lane's two commands: wire names, the symbols list, and a round trip. The Java twin
    /// (`Proto.symbols`) reads exactly this shape.
    #[test]
    fn subscribe_and_unsubscribe_commands_round_trip_and_use_wire_names() {
        let sub = Command::Subscribe { symbols: vec!["EURUSD".into(), "XAUUSD".into()] };
        let line = encode_line(&sub);
        assert_eq!(line, r#"{"cmd":"subscribe","symbols":["EURUSD","XAUUSD"]}"#);
        assert_eq!(parse_command(&line), Some(sub));
        let unsub = Command::Unsubscribe { symbols: vec!["EURUSD".into()] };
        let line = encode_line(&unsub);
        assert_eq!(line, r#"{"cmd":"unsubscribe","symbols":["EURUSD"]}"#);
        assert_eq!(parse_command(&line), Some(unsub));
    }

    /// The additive `tick` envelope: exactly what the Java emitter writes (Gson property order), the round trip,
    /// and the two safe-direction facts: a missing `time` reads as 0 (a tick with no time, never priced from) and
    /// a tick of another shape is no tick (`None`, logged and skipped by the reader).
    #[test]
    fn tick_envelope_round_trips_and_is_additive() {
        let java =
            r#"{"kind":"tick","symbol":"EURUSD","bid":1.1,"ask":1.1002,"time":1760000000123}"#;
        let env = Envelope::Tick {
            symbol: "EURUSD".into(),
            bid: 1.1,
            ask: 1.1002,
            time: 1_760_000_000_123,
        };
        assert_eq!(parse_envelope(java), Some(env.clone()));
        assert_eq!(encode_line(&env), java);
        assert_eq!(
            parse_envelope(r#"{"kind":"tick","symbol":"EURUSD","bid":1.1,"ask":1.2}"#),
            Some(Envelope::Tick { symbol: "EURUSD".into(), bid: 1.1, ask: 1.2, time: 0 })
        );
        assert_eq!(parse_envelope(r#"{"kind":"tick","symbol":"EURUSD","bid":"x"}"#), None);
    }

    /// The additive `subscription` answer: exactly what the Java emitter writes (Gson property order, an absent
    /// `reason` omitted on an ack), the round trip, and the safe direction of a missing `ok` (no answer, `None`).
    #[test]
    fn subscription_envelope_round_trips_and_is_additive() {
        let ack = r#"{"kind":"subscription","symbol":"EURUSD","ok":true}"#;
        let env =
            Envelope::Subscription { symbol: "EURUSD".into(), ok: true, reason: String::new() };
        assert_eq!(parse_envelope(ack), Some(env.clone()));
        assert_eq!(encode_line(&env), ack);

        let nack =
            r#"{"kind":"subscription","symbol":"NOPEXX","ok":false,"reason":"unknown instrument"}"#;
        let env = Envelope::Subscription {
            symbol: "NOPEXX".into(),
            ok: false,
            reason: "unknown instrument".into(),
        };
        assert_eq!(parse_envelope(nack), Some(env.clone()));
        assert_eq!(encode_line(&env), nack);

        // a line with no verdict is no answer
        assert_eq!(parse_envelope(r#"{"kind":"subscription","symbol":"EURUSD"}"#), None);
    }

    #[test]
    fn bad_lines_parse_to_none() {
        assert_eq!(parse_envelope("not json"), None);
        assert_eq!(parse_envelope(r#"{"kind":"mystery"}"#), None);
        assert_eq!(parse_command(r#"{"cmd":"mystery"}"#), None);
        assert_eq!(parse_command(""), None);
    }
}
