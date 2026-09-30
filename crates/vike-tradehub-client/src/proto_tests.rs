use super::*;

/// Every wire message must round-trip byte-stably through JSON (the frame payload codec). This
/// also pins that `[u8; 32]` (the `Welcome` nonce) survives a serde round-trip under the derive.
#[test]
fn request_response_round_trip_through_json() {
    use crate::wire::{WireCommand, WireTradingState};

    let reqs = vec![
        Request::Hello { proto_version: NODE_PROTO_VERSION },
        Request::Auth { scope: Scope::Write, mac: vec![1, 2, 3, 4] },
        Request::Subscribe { topics: vec![Topic::Orders, Topic::All] },
        Request::Snapshot,
        // The v4 Command shape, BOTH rationale arms — `None` (every pre-v4 caller's shape) and
        // `Some` (an operator/agent rationale riding beside the command).
        Request::Command { cmd: WireCommand::Cancel("c-1".into()), reason: None },
        Request::Command {
            cmd: WireCommand::Cancel("c-2".into()),
            reason: Some("stale quote after the feed gap".into()),
        },
        Request::Preview(WireCommand::SetTradingState(WireTradingState::Halted)),
        Request::Ping,
        // The B4 strategy verbs: the read as its own request, the write as a Command payload.
        Request::StrategyStatus,
        // The REQ-7 settings read.
        Request::SettingsShow,
        Request::Command {
            cmd: WireCommand::UpdateParams {
                venue: "binance".into(),
                symbol: "BTCUSDT".into(),
                interval: "1m".into(),
                params: serde_json::json!({"SpreadMaker": {"qty": 2.0}}),
            },
            reason: Some("widen ahead of the print".into()),
        },
        // The B5 mount verbs: both source arms (registry name / rhai path) and the unmount.
        Request::Command {
            cmd: WireCommand::MountStrategy {
                venue: "binance".into(),
                // NAMED here and ABSENT on the mount below, so this round trip carries both
                // wire states of the field rather than only the one that changes no bytes.
                account: Some("ALT".into()),
                symbol: "BTCUSDT".into(),
                interval: "1m".into(),
                controller_id: Some("grid-a".into()),
                name: Some("grid".into()),
                rhai: None,
                params: serde_json::json!({"qty": 1.0}),
            },
            reason: Some("second mount for the session".into()),
        },
        Request::Command {
            cmd: WireCommand::MountStrategy {
                venue: "binance".into(),
                account: None,
                symbol: "BTCUSDT".into(),
                interval: "1m".into(),
                controller_id: None,
                name: None,
                rhai: Some("strategies/breaker.rhai".into()),
                params: serde_json::json!({}),
            },
            reason: None,
        },
        Request::Command {
            cmd: WireCommand::UnmountStrategy { controller_id: "grid-a".into() },
            reason: Some("done for the day".into()),
        },
        // The REQ-7 settings WRITE: a Command payload like every control verb — a confirm-less
        // edit, and the shape an OLDER client still sends: a file NAME and a typed confirm, both
        // ignored by the node since `docs/decisions/0086` (point 7 for the confirm). Both must
        // keep round-tripping until step 2 deletes the two fields.
        Request::Command {
            cmd: WireCommand::SetSetting {
                file: "config.toml".into(),
                key: "config.tradehub_addr".into(),
                value: "127.0.0.1:7879".into(),
                confirm: None,
            },
            reason: None,
        },
        Request::Command {
            cmd: WireCommand::SetSetting {
                file: "policy.toml".into(),
                key: "policy.max_notional_per_order".into(),
                value: "250".into(),
                confirm: Some("policy.max_notional_per_order".into()),
            },
            reason: Some("tighter cap for the weekend".into()),
        },
        // The ruling-16 report verb, BOTH optional arms: absent (the caller has no opinion and
        // the node's own defaults apply) and present. The absent arm is the load-bearing one —
        // a `None` that encoded as anything other than JSON `null` would have the node silently
        // rescale every return ratio in the answer.
        Request::Tearsheet { seed: None, periods_per_year: None },
        Request::Tearsheet { seed: Some(25_000.0), periods_per_year: Some(365.0) },
    ];
    for r in reqs {
        let js = serde_json::to_string(&r).unwrap();
        let back: Request = serde_json::from_str(&js).unwrap();
        assert_eq!(r, back);
    }

    let resps = vec![
        Response::Welcome {
            proto_version: NODE_PROTO_VERSION,
            nonce: [7u8; 32],
            features: vec!["subscribe".into()],
        },
        Response::AuthOk { scope: Scope::Read },
        Response::AuthDenied { reason: "bad mac".into() },
        Response::Ack { coid: "c-1".into() },
        // Both Preview verdicts: accepted (no reason) and rejected (a reason).
        Response::Preview { accepted: true, reason: None },
        Response::Preview { accepted: false, reason: Some("rate limited".into()) },
        Response::Error("nope".into()),
        Response::Pong,
        Response::StrategyStatus(Box::new(crate::wire::WireStrategyStatus {
            identity: crate::wire::WireNodeIdentity {
                name: "the build runner".into(),
                strategy: "spread_maker".into(),
                params: "qty=1".into(),
                live: false,
                build: "test-build".into(),
                advertise_addr: String::new(),
            },
            effective_params: "qty=1".into(),
            mounts: vec![crate::wire::WireMountRow {
                strategy: "spread_maker".into(),
                params: "qty=1".into(),
                live: false,
                venue: "binance".into(),
                symbol: "BTCUSDT".into(),
                interval: "1m".into(),
                typed_params: Some(serde_json::json!({"SpreadMaker": {"qty": 1.0}})),
                asset_class: Some("CryptoPerp".into()),
            }],
        })),
        // The REQ-7 settings payload — both the settings-dir arms, a value-carrying row and a
        // redacted one (the sentinel is the VALUE on the wire; redaction happened server-side).
        Response::SettingsShow(Box::new(crate::wire::WireSettingsShow {
            settings_dir: Some("/srv/vike-<unit>/settings".into()),
            rows: vec![
                crate::wire::WireSettingsRow {
                    section: "config.toml".into(),
                    key: "config.tradehub_addr".into(),
                    value: "127.0.0.1:7879".into(),
                    origin: "config.toml".into(),
                    read_by: "tradehub".into(),
                },
                crate::wire::WireSettingsRow {
                    section: "config.toml".into(),
                    key: "config.bot_token".into(),
                    value: "<set>".into(),
                    origin: "env:ACME_TOKEN".into(),
                    read_by: "NO".into(),
                },
            ],
        })),
        Response::SettingsShow(Box::new(crate::wire::WireSettingsShow {
            settings_dir: None,
            rows: Vec::new(),
        })),
        // The REQ-7 write reply — both arms are LIVE since v2: `false` = the node hot-applied
        // the key, `true` = restart-to-apply. The shape was pinned both ways from v1 so the
        // hot-reload could land without a schema change — and it did.
        Response::SettingsWritten { restart_required: true },
        Response::SettingsWritten { restart_required: false },
        // The ruling-16 report reply: the `LiveTearsheet` document as JSON TEXT, carried
        // verbatim (see the variant's own doc for why this wire is a string).
        Response::Tearsheet(r#"{"trades":3,"sharpe":1.25}"#.into()),
    ];
    for r in resps {
        let js = serde_json::to_string(&r).unwrap();
        let back: Response = serde_json::from_str(&js).unwrap();
        assert_eq!(r, back);
    }
}

/// The v4 rationale rides BESIDE the command on the wire (its own `reason` field next to `cmd`),
/// never INSIDE the [`WireCommand`] — so the order-write vocabulary is byte-identical whether a
/// rationale is given or not, and the reason can never be mistaken for an order field. Also pins
/// that `Preview` stays a NEWTYPE (it is not audited, so it carries no rationale).
#[test]
fn the_reason_rides_beside_the_command_never_inside_it() {
    use crate::wire::WireCommand;

    let cmd = WireCommand::Cancel("c-1".into());
    let bare = serde_json::to_value(&cmd).unwrap();

    let with = serde_json::to_value(Request::Command {
        cmd: cmd.clone(),
        reason: Some("flat before the close".into()),
    })
    .unwrap();
    assert_eq!(with["Command"]["cmd"], bare, "the command itself is untouched by the reason");
    assert_eq!(with["Command"]["reason"], "flat before the close");

    let without = serde_json::to_value(Request::Command { cmd, reason: None }).unwrap();
    assert_eq!(without["Command"]["cmd"], bare, "…and identical with no reason given");
    assert!(without["Command"]["reason"].is_null(), "an absent rationale serializes as null");

    // Preview is deliberately NOT audited, so it stays a newtype (no `reason` to carry).
    let pv = serde_json::to_value(Request::Preview(WireCommand::Cancel("c-1".into()))).unwrap();
    assert_eq!(pv["Preview"], bare, "Preview is still a newtype over the command");
}

/// The REQ-2 advertisement round-trip: what [`datahub_feature`] renders,
/// [`advertised_datahub`] reads back verbatim — the one pinned spelling for both sides.
#[test]
fn datahub_feature_round_trips_through_advertised_datahub() {
    let features =
        vec!["observe".to_string(), datahub_feature("127.0.0.1:7878"), "preview".to_string()];
    assert_eq!(advertised_datahub(&features), Some("127.0.0.1:7878".to_string()));
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
    let entry = datahub_feature("127.0.0.1:7878");
    for named in [
        FEATURE_STRATEGY_VERBS,
        FEATURE_SETTINGS_SHOW,
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
    let w = Response::Welcome { proto_version: 1, nonce: [9u8; 32], features: vec![] };
    let v: serde_json::Value = serde_json::to_value(&w).unwrap();
    let arr = v["Welcome"]["nonce"].as_array().expect("nonce is a JSON array");
    assert_eq!(arr.len(), 32);
}

/// A frame written with the re-exported codec reads back identically — proving the re-export is
/// the SAME framing, and that a `Request` survives the length-prefixed round-trip.
#[test]
fn frame_round_trip_uses_the_reexported_codec() {
    let msg = Request::Hello { proto_version: NODE_PROTO_VERSION };
    let mut buf: Vec<u8> = Vec::new();
    write_frame(&mut buf, &msg).unwrap();
    let mut cursor = std::io::Cursor::new(buf);
    let back: Request = read_frame(&mut cursor).unwrap();
    assert_eq!(msg, back);
}
