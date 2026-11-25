use super::*;
use crate::test_support::assert_round_trips_in_every_form;
use vike_hub_protocol::auth::Scope;
use vike_hub_protocol::frame::{read_frame, write_frame};

/// Every wire message round-trips through EVERY session form (JSON, msgpack with field names, its LZ4 form), and prints
/// the same JSON after each. This also pins that `[u8; 32]` (the `Welcome` nonce) survives a serde round-trip under the
/// derive.
#[test]
fn every_request_and_response_round_trips_through_every_session_form() {
    use crate::wire::CommandWire;
    use vike_model::TradingState;

    let reqs = vec![
        Request::Subscribe { topics: vec![Topic::Orders, Topic::All] },
        // The observe stream's verbs: the newest reader, the shaped subscription with every option, its words.
        Request::SubscribeNewest { topics: vec![Topic::All] },
        Request::SubscribeWith(ObserveSubscriptionWire {
            topics: vec![Topic::Positions, Topic::Events],
            newest: true,
            compressed: true,
            patches: true,
            series: Some(vec![SeriesKeyWire {
                venue: "binance".into(),
                symbol: "BTCUSDT".into(),
                interval: "1m".into(),
            }]),
        }),
        Request::FrameRead,
        Request::ObserveResync,
        Request::ObserveSeries { series: None },
        Request::ObserveSeries { series: Some(Vec::new()) },
        Request::Snapshot,
        // The v4 Command shape, BOTH rationale arms — `None` (every pre-v4 caller's shape) and
        // `Some` (an operator/agent rationale riding beside the command).
        Request::Command { command: CommandWire::Cancel("c-1".into()), reason: None },
        Request::Command {
            command: CommandWire::Cancel("c-2".into()),
            reason: Some("stale quote after the feed gap".into()),
        },
        Request::Preview(CommandWire::SetTradingState(TradingState::Halted)),
        Request::Ping,
        // The B4 strategy verbs: the read as its own request, the write as a Command payload.
        Request::StrategyStatus,
        // The REQ-7 settings read.
        Request::SettingsShow,
        // The Trade window's directory read: a unit variant, like the settings read above.
        Request::Directory,
        Request::Command {
            command: CommandWire::UpdateParams {
                venue: "binance".into(),
                symbol: "BTCUSDT".into(),
                interval: "1m".into(),
                mount_id: None,
                params: serde_json::json!({"SpreadMaker": {"qty": 2.0}}),
            },
            reason: Some("widen ahead of the print".into()),
        },
        // The B5 mount verbs: both source arms (registry name / rhai path) and the unmount.
        Request::Command {
            command: CommandWire::MountStrategy {
                venue: "binance".into(),
                // NAMED here and ABSENT on the mount below, so this round trip carries both
                // wire states of the field rather than only the one that changes no bytes.
                account: Some("ALT".into()),
                symbol: "BTCUSDT".into(),
                interval: "1m".into(),
                controller_id: Some("grid-a".into()),
                file: None,
                name: Some("grid".into()),
                rhai: None,
                params: serde_json::json!({"qty": 1.0}),
            },
            reason: Some("second mount for the session".into()),
        },
        Request::Command {
            command: CommandWire::MountStrategy {
                venue: "binance".into(),
                account: None,
                symbol: "BTCUSDT".into(),
                interval: "1m".into(),
                controller_id: None,
                file: None,
                name: None,
                rhai: Some("strategies/breaker.rhai".into()),
                params: serde_json::json!({}),
            },
            reason: None,
        },
        Request::Command {
            command: CommandWire::UnmountStrategy { controller_id: "grid-a".into() },
            reason: Some("done for the day".into()),
        },
        // The settings WRITE: a confirm-less edit, and the OLDER client's shape (a file NAME and
        // a typed confirm, both ignored by the node since `docs/decisions/0086` point 7). Both
        // keep round-tripping until the two fields are deleted from the wire.
        Request::Command {
            command: CommandWire::SetSetting {
                file: "config.toml".into(),
                key: "config.tradehub_addr".into(),
                value: "127.0.0.1:17879".into(),
                confirm: None,
            },
            reason: None,
        },
        Request::Command {
            command: CommandWire::SetSetting {
                file: "policy.toml".into(),
                key: "policy.max_notional_per_order".into(),
                value: "250".into(),
                confirm: Some("policy.max_notional_per_order".into()),
            },
            reason: Some("tighter cap for the weekend".into()),
        },
        // The operator's read of an account at its venue, both kinds and both account spellings.
        Request::VenueRead {
            venue: "polymarket".into(),
            account: "DEFAULT".into(),
            kind: vike_model::VenueReadKind::Balance,
        },
        Request::VenueRead {
            venue: "hyperliquid".into(),
            account: "TESTNET".into(),
            kind: vike_model::VenueReadKind::OpenOrders,
        },
        Request::VenueRead {
            venue: "polymarket".into(),
            account: "DEFAULT".into(),
            kind: vike_model::VenueReadKind::Positions,
        },
    ];
    for r in &reqs {
        assert_round_trips_in_every_form(r);
    }

    let whole = crate::test_support::observe_frame::full_wire_snapshot();
    let responses = vec![
        // The observe stream's two frame shapes: the whole snapshot, and a patch (a whole one, then a delta).
        Response::SnapshotFrame(Box::new(whole.clone())),
        Response::SnapshotPatch(Box::new(SnapshotPatchWire {
            frame: 1,
            base: None,
            patch: serde_json::to_value(&whole).expect("a tree"),
        })),
        Response::SnapshotPatch(Box::new(SnapshotPatchWire {
            frame: 2,
            base: Some(1),
            patch: serde_json::json!({"fields": {"sequence": {"value": 8}}, "removed": ["fault"]}),
        })),
        Response::VenueRead(Box::new(golden_positions())),
        Response::AuthDenied { reason: "bad mac".into() },
        Response::Ack { order_client_id: "c-1".into(), notes: Vec::new(), emulated: None },
        Response::Ack {
            order_client_id: "c-2".into(),
            notes: vec!["EURUSD: the FX market is closed until Sunday 22:00 UTC".into()],
            emulated: Some("iceberg".into()),
        },
        // Both Preview verdicts: accepted (no reason) and rejected (a reason).
        Response::Preview { accepted: true, reason: None, refusal_id: None, notes: Vec::new() },
        Response::Preview {
            accepted: false,
            reason: Some("rate limited".into()),
            refusal_id: None,
            notes: Vec::new(),
        },
        Response::Preview {
            accepted: false,
            reason: Some("name one of these books".into()),
            refusal_id: Some("tradehub.door.account_not_named".into()),
            notes: Vec::new(),
        },
        Response::Preview {
            accepted: true,
            reason: None,
            refusal_id: None,
            notes: vec!["qty 15 goes out as 20, the venue's step is 10".into()],
        },
        Response::Error("nope".into()),
        Response::Pong,
        Response::StrategyStatus(Box::new(crate::wire::StrategyStatusWire {
            identity: crate::wire::HubIdentityWire {
                name: "the build runner".into(),
                strategy: "spread_maker".into(),
                params: "qty=1".into(),
                live: false,
                build: "test-build".into(),
                advertise_addr: String::new(),
                priced_venues: None,
            },
            effective_params: "qty=1".into(),
            mounts: vec![crate::wire::MountViewWire {
                order_client_id_code: None,
                refusal_id: None,
                strategy: "spread_maker".into(),
                params: "qty=1".into(),
                live: false,
                venue: "binance".into(),
                symbol: "BTCUSDT".into(),
                interval: "1m".into(),
                mount_id: String::new(),
                typed_params: Some(serde_json::json!({"SpreadMaker": {"qty": 1.0}})),
                asset_class: Some("CryptoPerp".into()),
                latched: false,
                route_key: String::new(),
                tier: None,
                not_running: false,
            }],
        })),
        // The REQ-7 settings payload — both the settings-dir arms, a value-carrying row and a
        // redacted one (the sentinel is the VALUE on the wire; redaction happened server-side).
        Response::SettingsShow(Box::new(crate::wire::SettingsShowWire {
            settings_dir: Some("/srv/vike-<unit>/settings".into()),
            rows: vec![
                crate::wire::SettingsShowRowWire {
                    section: "config.toml".into(),
                    key: "config.tradehub_addr".into(),
                    value: "127.0.0.1:17879".into(),
                    origin: "config.toml".into(),
                    read_by: "tradehub".into(),
                },
                crate::wire::SettingsShowRowWire {
                    section: "config.toml".into(),
                    key: "config.bot_token".into(),
                    value: "<set>".into(),
                    origin: "env:ACME_TOKEN".into(),
                    read_by: "NO".into(),
                },
            ],
            refusal: None,
        })),
        Response::SettingsShow(Box::new(crate::wire::SettingsShowWire {
            settings_dir: None,
            rows: Vec::new(),
            refusal: Some("the settings database could not be read (locked)".into()),
        })),
        // The directory reply — a venue the store has named and one it has not (`title: None`, a
        // store no writer has carried yet), an unlabelled unbooked account and a labelled, booked
        // one; and the empty directory a node whose database holds nothing answers.
        Response::Directory(Box::new(crate::wire::AccountDirectoryWire {
            venues: vec![
                crate::wire::AccountDirectoryVenueWire {
                    name: "binance".into(),
                    title: Some("Binance".into()),
                },
                crate::wire::AccountDirectoryVenueWire { name: "okx".into(), title: None },
            ],
            accounts: vec![
                crate::wire::AccountDirectoryAccountWire {
                    id: 3,
                    venue: "binance".into(),
                    label: None,
                    tier: "paper".into(),
                    venue_account_id: None,
                },
                crate::wire::AccountDirectoryAccountWire {
                    id: 7,
                    venue: "binance".into(),
                    label: Some("HEDGE".into()),
                    tier: "live".into(),
                    venue_account_id: Some("U1234567".into()),
                },
            ],
        })),
        Response::Directory(Box::new(crate::wire::AccountDirectoryWire {
            venues: Vec::new(),
            accounts: Vec::new(),
        })),
        // The settings write reply, both arms: `false` = hot-applied, `true` = restart-to-apply.
        Response::SettingsWritten { restart_required: true },
        Response::SettingsWritten { restart_required: false },
        // The present-names reply: the held subset, and the empty answer (nothing asked is held).
        Response::CredentialsHeld(Box::new(crate::wire::CredentialsHeldWire {
            held: vec!["A_KEY".into(), "B_KEY".into()],
        })),
        Response::CredentialsHeld(Box::new(crate::wire::CredentialsHeldWire { held: Vec::new() })),
        Response::VenueRead(Box::new(golden_balance())),
        Response::VenueRead(Box::new(golden_open_orders())),
        // The refusal frame (owner's Q8): a declared id and its text.
        Response::Refused(
            crate::refusals::TradehubRefusal::Starting { progress: "binance up".into() }.into(),
        ),
    ];
    for r in &responses {
        assert_round_trips_in_every_form(r);
    }
}

/// **An older client never meets the frame** (decision 0112's third leg): the node's reply to a client that did not
/// say it reads the frame is the `Error` of the same text, and to one that did, the frame itself; any other reply is
/// unchanged either way.
#[test]
fn the_refusal_frame_reaches_only_a_client_that_reads_it() {
    let frame = Response::Refused(
        crate::refusals::TradehubRefusal::Starting { progress: "binance up".into() }.into(),
    );
    assert_eq!(frame.older_form(false), Some(Response::Error(starting_error("binance up"))));
    assert_eq!(frame.older_form(true), None, "a client that reads the frame is sent it");
    assert_eq!(Response::Pong.older_form(false), None);
}

/// The wallet read of the 2026-10-27 order test: pUSD with the allowance each exchange holds.
fn golden_balance() -> vike_model::VenueRead {
    use vike_model::{VenueRead, VenueReadAllowance, VenueReadBalance};
    VenueRead::Balance(VenueReadBalance {
        asset: "pUSD".into(),
        balance: 20.0,
        allowances: vec![VenueReadAllowance {
            spender: "0xE111180000d2663C0091e4f400237545B87B996B".into(),
            name: "CTF exchange".into(),
            amount: 0.0,
            unlimited: true,
        }],
        other_assets: Vec::new(),
    })
}

/// One held, resolved, losing leg: the first row of the funded wallet's real data-api body.
fn golden_positions() -> vike_model::VenueRead {
    use vike_model::{VenueRead, VenueReadPosition};
    VenueRead::Positions {
        positions: vec![VenueReadPosition {
            symbol: "1484359444963781501765067677755406951259550728919736807361564486189514136251"
                .into(),
            title: "Solana Up or Down - June 9, 3:40AM-3:45AM ET".into(),
            outcome: "Down".into(),
            qty: 11.11,
            average_price: Some(0.2099),
            price: Some(0.0),
            value: Some(0.0),
            redeemable: true,
        }],
        truncated: false,
    }
}

/// One resting order, the shape the CLOB's own list gives it (no client id: placed elsewhere).
fn golden_open_orders() -> vike_model::VenueRead {
    use vike_model::{VenueRead, VenueReadOpenOrder};
    VenueRead::OpenOrders {
        orders: vec![VenueReadOpenOrder {
            venue_order_id: "0xabc".into(),
            order_client_id: None,
            symbol: "6710259892".into(),
            side: 1,
            price: Some(0.2),
            trigger_price: None,
            order_type: String::new(),
            qty: 5.0,
            filled_qty: 0.0,
            status: "ACCEPTED".into(),
            timestamp_ms: 1_700_000_000_000,
        }],
        truncated: false,
    }
}

/// **GOLDEN FRAMES of the venue read** (feature `venue-read`): the exact bytes of the request and of the
/// three replies a client meets. They are the contract an older or newer peer is held to: the kind is the
/// plain unit-variant word, the account is a string (`DEFAULT` or a label), a refusal is the ordinary
/// `Error`, and the payload mirrors the gatewayhub's lease frame field for field
/// (`fixtures/gatewayhub_wire/v1_trade_down.jsonl`'s `VenueReadReply` rows), so a field added on one side
/// shows up here as a red test before it shows up as a live decode failure.
#[test]
fn the_venue_read_frames_are_pinned_byte_for_byte() {
    let ask = Request::VenueRead {
        venue: "polymarket".into(),
        account: "DEFAULT".into(),
        kind: vike_model::VenueReadKind::OpenOrders,
    };
    assert_eq!(
        serde_json::to_string(&ask).unwrap(),
        r#"{"VenueRead":{"venue":"polymarket","account":"DEFAULT","kind":"OpenOrders"}}"#
    );

    assert_eq!(
        serde_json::to_string(&Response::VenueRead(Box::new(golden_balance()))).unwrap(),
        r#"{"VenueRead":{"Balance":{"asset":"pUSD","balance":20.0,"allowances":[{"spender":"0xE111180000d2663C0091e4f400237545B87B996B","name":"CTF exchange","amount":0.0,"unlimited":true}]}}}"#
    );
    assert_eq!(
        serde_json::to_string(&Response::VenueRead(Box::new(golden_open_orders()))).unwrap(),
        r#"{"VenueRead":{"OpenOrders":{"orders":[{"venue_order_id":"0xabc","order_client_id":null,"symbol":"6710259892","side":1,"price":0.2,"qty":5.0,"filled_qty":0.0,"status":"ACCEPTED","timestamp_ms":1700000000000}],"truncated":false}}}"#
    );
    assert_eq!(
        serde_json::to_string(&Response::Error(
            "not offered by binance: its bridge has no account-wide open-orders read".into()
        ))
        .unwrap(),
        r#"{"Error":"not offered by binance: its bridge has no account-wide open-orders read"}"#
    );
    // The third kind: the same frame with a NEW name in it (which is why it owes its own capability word),
    // and the answer is the gatewayhub's golden row byte for byte.
    assert_eq!(
        serde_json::to_string(&Request::VenueRead {
            venue: "polymarket".into(),
            account: "DEFAULT".into(),
            kind: vike_model::VenueReadKind::Positions,
        })
        .unwrap(),
        r#"{"VenueRead":{"venue":"polymarket","account":"DEFAULT","kind":"Positions"}}"#
    );
    assert_eq!(
        serde_json::to_string(&Response::VenueRead(Box::new(golden_positions()))).unwrap(),
        r#"{"VenueRead":{"Positions":{"positions":[{"symbol":"1484359444963781501765067677755406951259550728919736807361564486189514136251","title":"Solana Up or Down - June 9, 3:40AM-3:45AM ET","outcome":"Down","qty":11.11,"average_price":0.2099,"price":0.0,"value":0.0,"redeemable":true}],"truncated":false}}}"#
    );

    // …and the words an operator types are the kind's `word`, which the gatewayhub's own kind uses.
    assert_eq!(vike_model::VenueReadKind::Balance.word(), "balance");
    assert_eq!(vike_model::VenueReadKind::OpenOrders.word(), "open-orders");
    assert_eq!(vike_model::VenueReadKind::Positions.word(), "positions");
    assert_eq!(golden_balance().kind(), vike_model::VenueReadKind::Balance);
    assert_eq!(golden_open_orders().kind(), vike_model::VenueReadKind::OpenOrders);
    assert_eq!(golden_positions().kind(), vike_model::VenueReadKind::Positions);
}

/// The feature is a decode capability with its own string, and the datahub entry can never satisfy it.
#[test]
fn the_venue_read_capability_is_its_own_whole_string() {
    assert_eq!(FEATURE_VENUE_READ, "venue-read");
    assert_ne!(datahub_feature("127.0.0.1:17878"), FEATURE_VENUE_READ);
    for other in [FEATURE_DIRECTORY, FEATURE_STRATEGY_VERBS, FEATURE_RECONCILE_VERBS] {
        assert_ne!(other, FEATURE_VENUE_READ);
    }
}

/// The positions kind owes a word of its own beside `venue-read`: a node with only the older word cannot
/// decode the kind's name, and the two strings are told apart by whole-string equality.
#[test]
fn the_positions_capability_is_its_own_whole_string() {
    assert_eq!(FEATURE_VENUE_READ_POSITIONS, "venue-read-positions");
    assert_ne!(FEATURE_VENUE_READ_POSITIONS, FEATURE_VENUE_READ);
    assert_ne!(datahub_feature("127.0.0.1:17878"), FEATURE_VENUE_READ_POSITIONS);
}

/// The v4 rationale rides BESIDE the command on the wire (its own `reason` field next to `cmd`),
/// never INSIDE the [`CommandWire`] — so the order-write vocabulary is byte-identical whether a
/// rationale is given or not, and the reason can never be mistaken for an order field. Also pins
/// that `Preview` stays a NEWTYPE (it is not audited, so it carries no rationale).
#[test]
fn the_reason_rides_beside_the_command_never_inside_it() {
    use crate::wire::CommandWire;

    let cmd = CommandWire::Cancel("c-1".into());
    let bare = serde_json::to_value(&cmd).unwrap();

    let with = serde_json::to_value(Request::Command {
        command: cmd.clone(),
        reason: Some("flat before the close".into()),
    })
    .unwrap();
    assert_eq!(with["Command"]["command"], bare, "the command itself is untouched by the reason");
    assert_eq!(with["Command"]["reason"], "flat before the close");

    let without = serde_json::to_value(Request::Command { command: cmd, reason: None }).unwrap();
    assert_eq!(without["Command"]["command"], bare, "…and identical with no reason given");
    assert!(without["Command"]["reason"].is_null(), "an absent rationale serializes as null");

    // Preview is deliberately NOT audited, so it stays a newtype (no `reason` to carry).
    let pv = serde_json::to_value(Request::Preview(CommandWire::Cancel("c-1".into()))).unwrap();
    assert_eq!(pv["Preview"], bare, "Preview is still a newtype over the command");
}

/// The REQ-2 advertisement round-trip: what [`datahub_feature`] renders,
/// [`advertised_datahub`] reads back verbatim — the one pinned spelling for both sides.
/// The node's NOTES on an accepted order ride on `Ack` as an ADDITIVE key: an `Ack` with nothing to say is
/// byte-identical to the frame from before the field (so an older client reads every ordinary answer as it
/// always did), a frame from an older node (no key) reads back as no notes, and a frame with a key this build
/// does not know is still an `Ack` (what an older client does with `notes`).
#[test]
fn ack_notes_are_an_additive_key() {
    let plain = Response::Ack { order_client_id: "c-1".into(), notes: Vec::new(), emulated: None };
    assert_eq!(serde_json::to_string(&plain).unwrap(), r#"{"Ack":{"order_client_id":"c-1"}}"#);
    assert_eq!(
        serde_json::from_str::<Response>(r#"{"Ack":{"order_client_id":"c-1"}}"#).unwrap(),
        plain
    );

    let noted = Response::Ack {
        order_client_id: "c-2".into(),
        notes: vec!["closed until Sunday".into()],
        emulated: None,
    };
    let wire = serde_json::to_string(&noted).unwrap();
    assert_eq!(wire, r#"{"Ack":{"order_client_id":"c-2","notes":["closed until Sunday"]}}"#);
    assert_eq!(serde_json::from_str::<Response>(&wire).unwrap(), noted);

    assert_eq!(
        serde_json::from_str::<Response>(
            r#"{"Ack":{"order_client_id":"c-3","some_future_key":1}}"#
        )
        .unwrap(),
        Response::Ack { order_client_id: "c-3".into(), notes: Vec::new(), emulated: None },
        "a key this build does not know is ignored, as an older client ignores `notes`"
    );
}

/// The emulated order's KIND word rides the `Ack` beside its one note, the same additive way: a key only when the
/// node emulates the order, and an older client (or an older node's frame) is unaffected.
#[test]
fn ack_emulated_word_is_an_additive_key() {
    let emulated = Response::Ack {
        order_client_id: "c-4".into(),
        notes: vec!["EMULATED: bybit shows no visible size".into()],
        emulated: Some("iceberg".into()),
    };
    let wire = serde_json::to_string(&emulated).unwrap();
    assert_eq!(
        wire,
        r#"{"Ack":{"order_client_id":"c-4","notes":["EMULATED: bybit shows no visible size"],"emulated":"iceberg"}}"#
    );
    assert_eq!(serde_json::from_str::<Response>(&wire).unwrap(), emulated);
}

/// The same for a Preview's notes: a verdict with nothing to say is byte-identical to the frame from before the
/// field, a frame from an older node reads as no notes, and an older client ignores the key.
#[test]
fn preview_notes_are_an_additive_key() {
    let plain =
        Response::Preview { accepted: true, reason: None, refusal_id: None, notes: Vec::new() };
    let old = r#"{"Preview":{"accepted":true,"reason":null}}"#;
    assert_eq!(serde_json::to_string(&plain).unwrap(), old);
    assert_eq!(serde_json::from_str::<Response>(old).unwrap(), plain);

    let noted = Response::Preview {
        accepted: true,
        reason: None,
        refusal_id: None,
        notes: vec!["qty 15 goes out".into()],
    };
    let wire = serde_json::to_string(&noted).unwrap();
    assert_eq!(wire, r#"{"Preview":{"accepted":true,"reason":null,"notes":["qty 15 goes out"]}}"#);
    assert_eq!(serde_json::from_str::<Response>(&wire).unwrap(), noted);
}

/// **A Preview's `refusal_id` is additive both ways** (decision 0112): a verdict without one is the frame from before
/// the field, byte for byte; an older node's frame reads as no id; and an older client, which declares no such key,
/// reads a frame that carries one, in every session form.
#[test]
fn a_preview_refusal_id_is_additive_both_ways() {
    use vike_hub_protocol::frame::{BodyCodec, decode_body, encode_frame};

    /// The Preview as a client that predates the field declares it.
    #[derive(Debug, PartialEq, serde::Deserialize)]
    enum Before {
        Preview { accepted: bool, reason: Option<String> },
    }
    let refused = |refusal_id: Option<&str>| Response::Preview {
        accepted: false,
        reason: Some("name one of these books".into()),
        refusal_id: refusal_id.map(str::to_string),
        notes: Vec::new(),
    };

    let old = r#"{"Preview":{"accepted":false,"reason":"name one of these books"}}"#;
    assert_eq!(serde_json::to_string(&refused(None)).unwrap(), old, "no id: no key");
    assert_eq!(
        serde_json::from_str::<Response>(old).unwrap(),
        refused(None),
        "an older node's frame"
    );

    let with_id = refused(Some("tradehub.door.account_not_named"));
    for codec in [BodyCodec::Json, BodyCodec::Msgpack] {
        let frame = encode_frame(codec, &with_id).expect("encodes");
        let before: Before = decode_body(&frame[4..]).expect("an older client reads it");
        assert_eq!(
            before,
            Before::Preview { accepted: false, reason: Some("name one of these books".into()) },
            "{codec:?}"
        );
        let now: Response = decode_body(&frame[4..]).expect("a node's frame decodes");
        assert_eq!(now, with_id, "{codec:?}");
    }
}

#[test]
fn datahub_feature_round_trips_through_advertised_datahub() {
    let features =
        vec!["observe".to_string(), datahub_feature("127.0.0.1:17878"), "preview".to_string()];
    assert_eq!(advertised_datahub(&features), Some("127.0.0.1:17878".to_string()));
}

/// No `datahub=` entry — including a plain `"datahub"` without the `=` — is no advertisement,
/// and an empty/whitespace value advertises nothing (absence is the answer, the same shape as
/// an unset `config.datahub_addr`).
#[test]
fn a_missing_or_empty_datahub_entry_is_no_advertisement() {
    assert_eq!(advertised_datahub(&[]), None);
    assert_eq!(advertised_datahub(&["observe".to_string(), "preview".to_string()]), None);
    assert_eq!(advertised_datahub(&["datahub".to_string()]), None, "no `=` — not the entry");
    assert_eq!(advertised_datahub(&["datahub=".to_string()]), None, "empty value");
    assert_eq!(advertised_datahub(&["datahub=   ".to_string()]), None, "whitespace value");
}

/// The value-carrying entry can never satisfy a NAMED capability check: every existing check
/// is whole-string equality per feature name, and `datahub=<addr>` equals none of them.
#[test]
fn the_datahub_entry_shadows_no_named_capability() {
    let entry = datahub_feature("127.0.0.1:17878");
    for named in [
        FEATURE_STRATEGY_VERBS,
        FEATURE_SETTINGS_SHOW,
        FEATURE_DIRECTORY,
        FEATURE_MOUNT_VERBS,
        FEATURE_STRATEGY_PARAMS,
        FEATURE_MOUNT_CLASS,
    ] {
        assert_ne!(entry, named);
    }
}

/// The 32-byte nonce is carried as a fixed `[u8; 32]` array; confirm the derive emits a 32-long
/// JSON array (not a base64 string or a truncated form), so the wire shape is unambiguous.
#[test]
fn welcome_nonce_is_a_fixed_32_array() {
    use vike_hub_protocol::handshake::HandshakeReply;
    let w = HandshakeReply::Welcome { proto_version: 1, features: vec![], nonce: Some([9u8; 32]) };
    let v: serde_json::Value = serde_json::to_value(&w).unwrap();
    let arr = v["Welcome"]["nonce"].as_array().expect("nonce is a JSON array");
    assert_eq!(arr.len(), 32);
}

/// The node's handshake moved onto the shared frames (decision 0288) WITHOUT changing a byte: the
/// opener and the shared-key answer are the externally tagged objects this crate's own `Hello`/`Auth`
/// variants wrote, so a node and a client across the move still meet.
#[test]
fn the_shared_handshake_frames_are_the_bytes_the_node_spoke() {
    use vike_hub_protocol::handshake::{HandshakeFrame, HandshakeReply};
    let hello = HandshakeFrame::Hello { proto_version: NODE_PROTO_VERSION };
    assert_eq!(
        serde_json::to_string(&hello).unwrap(),
        format!(r#"{{"Hello":{{"proto_version":{NODE_PROTO_VERSION}}}}}"#)
    );
    let auth = HandshakeFrame::Auth { scope: Scope::Write, mac: vec![1, 2, 3, 4] };
    assert_eq!(
        serde_json::to_string(&auth).unwrap(),
        r#"{"Auth":{"scope":"Write","mac":[1,2,3,4]}}"#
    );
    let ok = HandshakeReply::AuthOk { scope: Scope::Read, admitted: None };
    assert_eq!(serde_json::to_string(&ok).unwrap(), r#"{"AuthOk":{"scope":"Read"}}"#);
    let denied = HandshakeReply::AuthDenied { reason: "bad mac".into() };
    assert_eq!(
        serde_json::to_string(&denied).unwrap(),
        serde_json::to_string(&Response::AuthDenied { reason: "bad mac".into() }).unwrap(),
        "the session's refusal and the handshake's are one encoding"
    );
}

/// **A starting node and a busy core are NOT NOW, never a final no**: nothing was executed and the same
/// verb is taken shortly. Every other text is the node's refusal.
#[test]
fn a_starting_node_and_a_busy_core_read_not_now() {
    let starting = starting_error("starting the accounts; accounts: binance up, okx starting");
    assert_eq!(refusal_outcome(&starting), FailureOutcome::NotNow, "{starting}");
    assert_eq!(refusal_outcome(BUSY_ERROR), FailureOutcome::NotNow);
    for refused in ["no engine for venue `x` on this node", "core is shutting down", "busy"] {
        assert_eq!(refusal_outcome(refused), FailureOutcome::Refused, "{refused}");
    }
}

/// A frame written with the re-exported codec reads back identically — proving the re-export is
/// the SAME framing, and that a `Request` survives the length-prefixed round-trip.
#[test]
fn frame_round_trip_uses_the_reexported_codec() {
    let msg = Request::Ping;
    let mut buffer: Vec<u8> = Vec::new();
    write_frame(&mut buffer, &msg).unwrap();
    let mut cursor = std::io::Cursor::new(buffer);
    let back: Request = read_frame(&mut cursor).unwrap();
    assert_eq!(msg, back);
}
