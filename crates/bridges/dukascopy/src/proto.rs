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
    /// Start live quotes for these instruments (the wire spelling: six letters for a currency pair or a metal,
    /// `EURUSD`, `XAUUSD`, and the SDK's slashed name for a CFD, `COPPER.CMD/USD`): the price lane
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
    /// Ask what the VENUE holds NOW: the sidecar answers with ONE [`Envelope::Recon`] carrying the same `id`, listing
    /// every order the venue shows (with its stop-loss and take-profit) and the net position per instrument
    /// (`ReconSnapshot.java`). The node's reconcile client (`crate::recon_client`) is its one sender. Additive: an old
    /// jar logs the unknown command and skips it, so the client does not send it to a jar that did not announce
    /// `recon` (`crate::exec`'s `FEATURE_RECON`).
    Recon {
        id: u64,
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
        /// `IAccount.getBalance()`: the vendor's Javadoc calls it the balance of the PREVIOUS END-OF-DAY processing, so
        /// it is fresh only at start. Its one consumer is `crate::ready_balance` (the engine's balance, for a USD account
        /// on a USD-quoted mount); a missing key reads 0.0, which that module does not adopt.
        #[serde(default)]
        balance: f64,
        /// What this jar can do beyond the original protocol (additive): `"modify"` = it understands the
        /// `modify` command. An old jar sends none, which the client reads as "cannot amend" and answers a
        /// modify with an explicit refusal instead of writing a command nobody will answer.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        features: Vec<String>,
        /// The account's own currency as the SDK states it (`IAccount.getAccountCurrency()`, an ISO code: `USD`),
        /// the fact the multi-currency money (decision 0183) hangs on. Additive both ways: a jar from before it
        /// sends none (read as "not stated", never as USD: `crate::quotes::QuoteBus::account_currency`), and a
        /// reader from before it ignores the unknown key.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        currency: Option<String>,
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
    /// The sidecar's ADOPTION census (additive; announced by the `adopt` feature word of `ready`, `crate::exec`'s
    /// `FEATURE_ADOPT`): written ONCE per sidecar, right after its `ready` and the `position` lines of the instruments
    /// the venue lists an order on. `orders` are the venue's orders whose label names a client_order_id of ours (the new
    /// sidecar took them up: it can cancel, amend and hear them again), `foreign` counts the orders it could not name
    /// (opened in the platform, left alone). It ENDS the start's position lines, so a symbol the client holds a position
    /// in and got no line for is flat at the venue. Back-compatible both ways: an old jar never sends it, an old reader
    /// logs-and-skips the unknown kind.
    Adopted {
        #[serde(default)]
        orders: Vec<AdoptedOrder>,
        #[serde(default)]
        foreign: u32,
    },
    /// The answer to ONE [`Command::Recon`] (additive; announced by the `recon` feature word of `ready`,
    /// `crate::exec`'s `FEATURE_RECON`): what the venue lists now. `id` is the command's. ⚠ **An absent list is NOT an
    /// empty one**: a jar that cannot read the venue sends `error` (and no lists), and a line with neither is malformed;
    /// both are ERRORS to the reader (`crate::recon_query`), because an empty answer would read as a flat venue and the
    /// node would call every position it holds a phantom. Back-compatible both ways: an old jar never sends it, an old
    /// reader logs-and-skips the unknown kind.
    Recon {
        id: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        orders: Option<Vec<ReconOrder>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        positions: Option<Vec<ReconPosition>>,
        /// How many of `orders` carry no client_order_id of ours (opened in the platform): counted, never touched.
        #[serde(default)]
        foreign: u32,
        /// The sidecar's clock when it read the venue (epoch ms).
        #[serde(default)]
        ts: i64,
        /// Why the sidecar could not read the venue, in words.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// What the VENUE states about one instrument's amounts and price grid (additive; announced by the `terms` feature
    /// word of `ready`, `crate::quotes::FEATURE_TERMS`): written once per instrument, at its first tick, BEFORE that tick.
    /// It is the only thing a metal or a CFD is admitted on (`crate::venue_terms`). Back-compatible both ways: an old
    /// jar never sends it, an old reader logs-and-skips the unknown kind.
    Terms(crate::venue_terms::VenueTerms),
    Fatal {
        reason: String,
    },
}

/// One order of the [`Envelope::Recon`] answer, as the venue lists it. Quantities are in UNITS, a price of `0.0`
/// means "none", `commission` is the venue's own sign (a charge is NEGATIVE: `OrderMap.java`'s `commissionAdvance`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReconOrder {
    /// The JForex order id (the position id once it is filled); empty for an order the platform has not numbered yet.
    #[serde(default)]
    pub order_id: String,
    /// The client_order_id the order's label was made from; absent for an order opened in the platform.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coid: Option<String>,
    /// Canonical symbol (EURUSD form).
    pub symbol: String,
    /// +1 long, -1 short.
    pub side: i32,
    /// `CREATED`, `OPENED` (resting) or `FILLED` (an open position).
    pub state: String,
    /// The SDK's order command (`BUY`, `SELLLIMIT`, ...).
    #[serde(default)]
    pub command: String,
    /// Units filled so far.
    #[serde(default)]
    pub amount: f64,
    /// Units asked for.
    #[serde(default)]
    pub requested: f64,
    /// The open price: a resting order's limit price, a filled order's fill price.
    #[serde(default)]
    pub price: f64,
    /// The stop-loss price the order carries at the venue.
    #[serde(default)]
    pub stop_loss: f64,
    /// The take-profit price the order carries at the venue.
    #[serde(default)]
    pub take_profit: f64,
    /// The order's cumulative commission, in the account's currency, with the venue's sign.
    #[serde(default)]
    pub commission: f64,
    /// When the order was created (epoch ms).
    #[serde(default)]
    pub created: i64,
    /// When the order was filled (epoch ms; `0` = not filled).
    #[serde(default)]
    pub filled: i64,
}

/// One net position of the [`Envelope::Recon`] answer: an instrument that has a FILLED order. An instrument with none
/// has NO row (the reader states it flat for the symbols it knows).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReconPosition {
    /// Canonical symbol (EURUSD form).
    pub symbol: String,
    /// Signed net size in UNITS: >0 long, <0 short, 0 flat (fully hedged).
    pub size: f64,
    /// Signed-amount-weighted average open price; `0.0` when flat.
    #[serde(default)]
    pub avg_px: f64,
}

/// One order of the [`Envelope::Adopted`] census, as the sidecar read it off the venue. Quantities are in UNITS, a
/// price of `0.0` means "none".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdoptedOrder {
    /// The client_order_id the order's label was made from (`OrderMap.coidOfLabel`).
    pub coid: String,
    /// The JForex order id (the position id once it is filled).
    pub order_id: String,
    /// Canonical symbol (EURUSD form).
    pub symbol: String,
    /// +1 long, -1 short.
    pub side: i32,
    /// `CREATED`, `OPENED` (resting) or `FILLED` (an open position).
    pub state: String,
    /// Units filled so far.
    #[serde(default)]
    pub amount: f64,
    /// Units asked for.
    #[serde(default)]
    pub requested: f64,
    /// The stop-loss price the order carries at the venue.
    #[serde(default)]
    pub stop_loss: f64,
    /// The take-profit price the order carries at the venue.
    #[serde(default)]
    pub take_profit: f64,
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

    /// A stop-loss is the ordinary `submit` with the stop's own fields; `StrategyBridge.java`'s `handleStop` reads
    /// exactly `order_type`, `side`, `qty`, `reduce_only` and `trigger_price`. A stop with no trigger travels as an
    /// explicit `null`, which the Java twin (`Proto.optionalDouble`) reads as "no trigger" and refuses in words.
    #[test]
    fn a_stop_submit_carries_the_fields_the_java_stop_handler_reads() {
        let mut stop = order("s1");
        stop.order_type = vike_model::OrderType::Stop;
        stop.side = -1;
        stop.trigger_price = Some(156.8);
        stop.reduce_only = true;
        let line = encode_line(&Command::Submit { order: Box::new(stop.clone()) });
        for field in [
            r#""order_type":"stop""#,
            r#""side":-1"#,
            r#""qty":1000.0"#,
            r#""reduce_only":true"#,
            r#""trigger_price":156.8"#,
        ] {
            assert!(line.contains(field), "{field} missing from {line}");
        }
        assert_eq!(parse_command(&line), Some(Command::Submit { order: Box::new(stop.clone()) }));

        stop.trigger_price = None;
        let line = encode_line(&Command::Submit { order: Box::new(stop) });
        assert!(line.contains(r#""trigger_price":null"#), "{line}");
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
                features: vec![],
                currency: None
            })
        );
        // balance is optional (serde default)
        assert_eq!(
            parse_envelope(r#"{"kind":"ready","account":"A"}"#),
            Some(Envelope::Ready {
                account: "A".into(),
                balance: 0.0,
                features: vec![],
                currency: None
            })
        );
        // the jar announces what it can do; a jar that says nothing (every jar before the amend) reads as []
        assert_eq!(
            parse_envelope(r#"{"kind":"ready","account":"A","features":["modify"]}"#),
            Some(Envelope::Ready {
                account: "A".into(),
                balance: 0.0,
                features: vec!["modify".into()],
                currency: None
            })
        );
        let fatal = parse_envelope(r#"{"kind":"fatal","reason":"login failed"}"#);
        assert_eq!(fatal, Some(Envelope::Fatal { reason: "login failed".into() }));
    }

    /// The additive account currency of `ready` (decision 0183): exactly what the Java emitter writes (Gson
    /// property order), the round trip, and BOTH directions of skew. A jar from before the field sends none, which
    /// reads as "not stated" (`None`, never a default currency); a reader from before the field ignores the key
    /// (no `deny_unknown_fields`: the line still parses, and an unknown future key does too).
    #[test]
    fn ready_carries_the_account_currency_additively() {
        let java = r#"{"kind":"ready","account":"3757428","balance":10000.0,"features":["modify","ticks","tick-acks","adopt","take-profit"],"currency":"USD"}"#;
        let env = Envelope::Ready {
            account: "3757428".into(),
            balance: 10000.0,
            features: vec![
                "modify".into(),
                "ticks".into(),
                "tick-acks".into(),
                "adopt".into(),
                "take-profit".into(),
            ],
            currency: Some("USD".into()),
        };
        assert_eq!(parse_envelope(java), Some(env.clone()));
        assert_eq!(encode_line(&env), java);

        // an absent currency is omitted on the wire, so a jar-less test double's line is what it always was
        let none =
            Envelope::Ready { account: "A".into(), balance: 1.0, features: vec![], currency: None };
        assert_eq!(encode_line(&none), r#"{"kind":"ready","account":"A","balance":1.0}"#);
        assert_eq!(parse_envelope(&encode_line(&none)), Some(none));

        // a key this reader does not know is ignored, not a parse failure (a newer jar, an older reader)
        assert!(matches!(
            parse_envelope(r#"{"kind":"ready","account":"A","balance":1.0,"future":{"x":1}}"#),
            Some(Envelope::Ready { currency: None, .. })
        ));
        // an explicit null is "not stated" too
        assert!(matches!(
            parse_envelope(r#"{"kind":"ready","account":"A","currency":null}"#),
            Some(Envelope::Ready { currency: None, .. })
        ));
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

    /// The additive `adopted` census: exactly what the Java emitter writes (Gson property order), the round trip, and
    /// the safe direction of every missing key (an empty census, no foreign count).
    #[test]
    fn adopted_envelope_round_trips_and_is_additive() {
        let java = r#"{"kind":"adopted","orders":[{"coid":"abc123","order_id":"1165850288","symbol":"USDJPY","side":1,"state":"FILLED","amount":1000.0,"requested":1000.0,"stop_loss":156.5,"take_profit":0.0}],"foreign":2}"#;
        let env = Envelope::Adopted {
            orders: vec![AdoptedOrder {
                coid: "abc123".into(),
                order_id: "1165850288".into(),
                symbol: "USDJPY".into(),
                side: 1,
                state: "FILLED".into(),
                amount: 1000.0,
                requested: 1000.0,
                stop_loss: 156.5,
                take_profit: 0.0,
            }],
            foreign: 2,
        };
        assert_eq!(parse_envelope(java), Some(env.clone()));
        assert_eq!(encode_line(&env), java);
        assert_eq!(
            parse_envelope(r#"{"kind":"adopted"}"#),
            Some(Envelope::Adopted { orders: vec![], foreign: 0 })
        );
        // an order with no coid is no order of the census
        assert_eq!(parse_envelope(r#"{"kind":"adopted","orders":[{"order_id":"1"}]}"#), None);
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

    /// The additive `recon` command and its answer: the command's exact bytes (the Java twin reads `id` through
    /// `Proto.optionalDouble`), the answer exactly as the Java emitter writes it (`ReconSnapshot.answer`: an order of
    /// ours names its coid and one from the platform carries none), the error form, and the fact that an ABSENT list
    /// stays absent (`None`), which is what lets the reader tell an empty answer from a malformed one.
    #[test]
    fn recon_command_and_answer_round_trip_and_keep_an_absent_list_absent() {
        let ask = Command::Recon { id: 41 };
        assert_eq!(encode_line(&ask), r#"{"cmd":"recon","id":41}"#);
        assert_eq!(parse_command(&encode_line(&ask)), Some(ask));

        let java = r#"{"kind":"recon","id":41,"ts":1700000000000,"orders":[{"order_id":"22","coid":"filled1","symbol":"USDJPY","side":1,"state":"FILLED","command":"BUY","amount":1000.0,"requested":1000.0,"price":156.9,"stop_loss":156.5,"take_profit":0.0,"commission":-0.03,"created":1700000000000,"filled":1700000001000},{"order_id":"23","symbol":"USDJPY","side":-1,"state":"FILLED","command":"SELL","amount":400.0,"requested":400.0,"price":157.0,"stop_loss":0.0,"take_profit":0.0,"commission":-0.03,"created":1,"filled":2}],"positions":[{"symbol":"USDJPY","size":600.0,"avg_px":156.8333}],"foreign":1}"#;
        let Some(Envelope::Recon { id, orders, positions, foreign, ts, error }) =
            parse_envelope(java)
        else {
            panic!("the Java emitter's recon line must parse: {java}");
        };
        assert_eq!((id, foreign, ts, error), (41, 1, 1_700_000_000_000, None));
        let orders = orders.expect("a list");
        assert_eq!(orders.len(), 2);
        assert_eq!(orders[0].coid.as_deref(), Some("filled1"));
        assert_eq!(orders[0].stop_loss, 156.5);
        assert_eq!(orders[0].commission, -0.03);
        assert_eq!(orders[1].coid, None, "an order opened in the platform names no coid");
        assert_eq!(orders[1].side, -1);
        assert_eq!(positions.expect("a list")[0].size, 600.0);

        // the error form carries no list; a line with neither is parsed as such and judged by the reader
        assert!(matches!(
            parse_envelope(
                r#"{"kind":"recon","id":5,"error":"the venue's orders could not be read: x"}"#
            ),
            Some(Envelope::Recon { id: 5, orders: None, positions: None, error: Some(_), .. })
        ));
        assert!(matches!(
            parse_envelope(r#"{"kind":"recon","id":6}"#),
            Some(Envelope::Recon { id: 6, orders: None, positions: None, error: None, .. })
        ));
        // an empty list is an ANSWER (the venue holds nothing), and stays distinct from an absent one
        assert!(matches!(
            parse_envelope(r#"{"kind":"recon","id":7,"orders":[],"positions":[]}"#),
            Some(Envelope::Recon { id: 7, orders: Some(o), positions: Some(p), .. }) if o.is_empty() && p.is_empty()
        ));
        // no id is no answer to anything
        assert_eq!(parse_envelope(r#"{"kind":"recon","orders":[],"positions":[]}"#), None);
    }

    #[test]
    fn bad_lines_parse_to_none() {
        assert_eq!(parse_envelope("not json"), None);
        assert_eq!(parse_envelope(r#"{"kind":"mystery"}"#), None);
        assert_eq!(parse_command(r#"{"cmd":"mystery"}"#), None);
        assert_eq!(parse_command(""), None);
    }
}
