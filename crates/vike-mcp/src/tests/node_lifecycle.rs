//! The node-lifecycle write tools, the settings write and its gates, and the per-call node reads.

use super::write_args::every_write_tools_arguments;
use super::*;
use crate::node_reads::{node_read_answer, node_read_failure};
use crate::node_writes::{LIFECYCLE_TOOLS, control_rejection};
use vike_client::first_frame::is_node_frame;
use vike_tradehub_client::ControlRejected;
use vike_tradehub_client::wire::CoreSnapshotWire;

/// COMPOSED, not spelled. `crates/vike-config/tests/policy_is_consumed.rs` used to read every
/// non-comment line under `src/` for a section-qualified policy key as evidence that the file READS
/// that setting, test modules included, so a fixture spelling one turned `main` red (#1685 lifted
/// these fixtures here from `lib.rs` and `Policy::max_leverage` was `Consumed::No`, as it was then). It now reads the
/// PRODUCTION view only (comments, `#[cfg(test)]` items and test files are skipped), so the
/// composition is no longer required; it is kept because it is harmless.
/// Composing it is the same trick `crates/vike-venues/src/credential_keys.rs` uses to keep its
/// near-miss fixtures out of the settings-registry literal harvest.
const POLICY_KEY_FIXTURE: &str = concat!("policy.", "max_leverage");

// ---- the node-lifecycle write tools + the two per-call node reads ------------------------

/// [`LIFECYCLE_TOOLS`] is a SUBSET of [`WRITE_TOOLS`], never a second roster.
///
/// ⚠ The direction that matters is this one: a lifecycle tool missing from `WRITE_TOOLS` loses
/// the mandatory preview gate, the `destructiveHint` and the `read-only` withholding while
/// still LOOKING like a gated tool — it would be routed by `call_tool`'s catch-all as an
/// unknown tool, which is at least loud, but the annotations pin would go quiet the moment
/// somebody "fixed" that by adding an arm.
#[test]
fn the_lifecycle_tools_are_a_subset_of_the_write_roster() {
    for tool in LIFECYCLE_TOOLS {
        assert!(
            WRITE_TOOLS.contains(&tool),
            "{tool} is a lifecycle tool but not on WRITE_TOOLS — it would carry no preview gate"
        );
        assert!(is_write_tool(tool), "{tool} must route as a write");
    }
    // …and each one is really BUILDABLE by the SHARED construction site, so this is not three
    // names agreeing with three names while `tool_verbs::wire_command_for` answers `no wire command
    // for …` — the state this file's own lifecycle builder existed to avoid, and which the
    // lift into `tool_verbs` would silently restore if one name were left behind there.
    for tool in LIFECYCLE_TOOLS {
        assert!(
            tool_verbs::wire_command_for(tool, &every_write_tools_arguments("c1")).is_ok(),
            "{tool}: the shared site must build a command from the shared argument union"
        );
    }
}

/// Each lifecycle tool inherits the MANDATORY PREVIEW: a first call sends nothing and hands
/// back the token that would confirm it. The roster loops elsewhere prove this for
/// `WRITE_TOOLS` entire; this is the same claim aimed at the three that are new, so a
/// regression names them rather than "some write tool".
#[test]
fn a_lifecycle_write_previews_before_it_executes() {
    for tool in LIFECYCLE_TOOLS {
        let mut args = every_write_tools_arguments("c1");
        args["confirm"] = json!(true);
        let sc = &call(tool, args)["result"]["structuredContent"];
        assert_eq!(sc["will_execute"], json!(false), "{tool}: confirm alone must not execute");
        assert!(sc["preview_token"].is_string(), "{tool}: a preview carries its token: {sc}");
        assert_eq!(sc["verified_by_node"], json!(false), "{tool}: no node, so no verdict");
    }
}

/// **A settings write PREVIEWS with no `policy_confirm`, a policy key included, and the preview
/// shows the change.** The argument is gone with the retype it carried (`docs/decisions/0086` point
/// 7: no retype confirm, for any key, on any surface), and what the preview carries instead is
/// `change` — the key, the node's CURRENT value and the new one, `old → new`.
///
/// ⚠ With no node configured the old value cannot be READ, and the preview says so rather than
/// inventing one: `old` is null and `old_unread` names why. A fabricated old value is the one field
/// an agent would relay to the owner as fact.
///
/// The calls are the ones the deleted gate treated differently — two policy keys and a non-policy
/// one — and all three now take the same path. Each names only its key: the tool's `file` argument
/// went with the file era (the wire's is derived from the key, `vike_client::verbs`' job).
#[test]
fn a_settings_write_previews_without_a_typed_confirm_and_shows_old_to_new() {
    for (key, value) in [
        ("policy.max_notional_per_order", "250"),
        (POLICY_KEY_FIXTURE, "3"),
        ("config.tradehub_addr", "127.0.0.1:7879"),
    ] {
        let resp = call("set_setting", json!({ "key": key, "value": value }));
        assert_eq!(resp["result"]["isError"], false, "{key}: previewed, never refused: {resp}");
        let sc = &resp["result"]["structuredContent"];
        assert_eq!(sc["will_execute"], json!(false), "{key}: still only a preview: {sc}");
        assert!(sc["preview_token"].is_string(), "{key}: …and it mints its token: {sc}");
        assert_eq!(
            sc["wire_command"]["SetSetting"]["confirm"],
            Value::Null,
            "{key}: nothing rides in the wire's ignored confirm field: {sc}"
        );
        let change = &sc["change"];
        assert_eq!(change["key"], json!(key), "{sc}");
        assert_eq!(change["new"], json!(value), "{sc}");
        assert_eq!(change["old"], Value::Null, "{key}: no node was asked, so no old value: {sc}");
        assert!(
            change["old_unread"]
                .as_str()
                .is_some_and(|why| why.contains("no vike-tradehub configured")),
            "{key}: …and the preview says WHY it has none: {sc}"
        );
        let line = change["line"].as_str().unwrap_or_default();
        assert!(
            line.starts_with(key) && line.ends_with(&format!(" → {value}")),
            "{key}: the one-line `key: old → new`: {line:?}"
        );
    }
}

/// **The preview gate is not the retype, and it outlives it.** On a POLICY write — the call whose
/// extra gate was just deleted, so the one place the shared gate could have gone with it:
/// `confirm: true` alone still only previews; an unknown token is refused; a token minted for one
/// value does not confirm another; and a fresh token reaches the node step, which is the positive
/// half — without it the refusals above would all pass on a gate that refused everything.
#[test]
fn a_policy_write_still_executes_only_against_its_own_preview_token() {
    let mut s = server();
    let args = json!({ "key": "policy.max_notional_per_order", "value": "250" });
    let mut bare = args.clone();
    bare["confirm"] = json!(true);

    let pv = s.call_tool("set_setting", &bare).expect("`confirm: true` alone is a preview");
    assert_eq!(pv["will_execute"], json!(false), "no token, nothing sent: {pv}");
    let token = pv["preview_token"].as_str().expect("a preview mints its token").to_string();

    let mut unknown = bare.clone();
    unknown["preview_token"] = json!("not-a-token-this-server-issued");
    let err = s.call_tool("set_setting", &unknown).expect_err("an unknown token executes nothing");
    assert!(err.refused, "a GATE refusal, which the transcript classifies: {err:?}");
    assert!(err.message.contains("unknown or already used"), "{err:?}");

    let mut other_value = bare.clone();
    other_value["value"] = json!("999");
    other_value["preview_token"] = json!(token);
    let err = s
        .call_tool("set_setting", &other_value)
        .expect_err("a token confirms only the value it previewed");
    assert!(err.message.contains("does not match this command"), "{err:?}");

    let fresh = s.call_tool("set_setting", &args).expect("a fresh preview");
    let mut confirming = bare.clone();
    confirming["preview_token"] = fresh["preview_token"].clone();
    let err = s.call_tool("set_setting", &confirming).expect_err("no node is configured");
    assert!(!err.refused, "the right token is ADMITTED — this is the node step failing: {err:?}");
    assert!(err.message.contains("no vike-tradehub configured"), "{err:?}");
}

/// A CONFIRMED settings write leaves by [`Server::execute_settings_write`] rather than the
/// fire-and-forget worker, and with no node configured that surfaces as the connection error —
/// which is the positive evidence it got PAST the preview gate, the same shape
/// `crates/vike-mcp/tests/shipped_mcp/mcp_transcript.rs` uses for `submit_order`.
#[test]
fn a_confirmed_settings_write_reaches_the_node_step() {
    let mut s = server();
    let args = json!({ "key": "config.tradehub_addr", "value": "127.0.0.1:7879" });
    let preview = s
        .handle(&req(1, "tools/call", json!({ "name": "set_setting", "arguments": args })))
        .unwrap();
    let token = preview["result"]["structuredContent"]["preview_token"]
        .as_str()
        .expect("the preview mints a token")
        .to_string();
    let mut confirming = args.clone();
    confirming["confirm"] = json!(true);
    confirming["preview_token"] = json!(token);
    let sent = s
        .handle(&req(2, "tools/call", json!({ "name": "set_setting", "arguments": confirming })))
        .unwrap();
    assert_eq!(sent["result"]["isError"], true, "no node is configured: {sent}");
    let text = sent["result"]["content"][0]["text"].as_str().unwrap();
    assert!(
        text.contains("no vike-tradehub configured"),
        "a matching token must reach the node step — that is what proves the gate ADMITS: \
             {text}"
    );
}

/// **The UNATTENDED gate refuses EVERY settings write** (decision 0040: the owner's rulings of
/// 2026-09-28 for `policy.*` and 2026-09-29 for every other key) — whatever the key's class or
/// spelling — and nothing that is not a settings write.
#[test]
fn the_unattended_gate_refuses_every_settings_write_and_nothing_else() {
    let setting = |key: &str| CommandWire::SetSetting {
        file: verbs::section_of_key(key).to_string(),
        key: key.to_string(),
        value: "1".into(),
        confirm: None,
    };
    for key in [
        "policy.max_notional_per_order",
        POLICY_KEY_FIXTURE,
        "policy.max_account_exposure",
        " policy.max_notional_per_order",
        "flags.tradehub_live",
        "flags.reconcile_off",
        "config.tradehub_addr",
        "preferences.log_level",
        "policies.max_notional_per_order",
    ] {
        let why = unattended_refusal(&setting(key)).expect_err(key);
        assert!(why.contains("vike-cli config set"), "{key:?}: name the CLI route: {why}");
        assert!(why.contains("GUI"), "{key:?}: …and the GUI route: {why}");
    }
    assert_eq!(unattended_refusal(&CommandWire::Cancel("c-1".into())), Ok(()), "not a setting");
}

/// **An unattended SERVER refuses a settings write before any token exists** — the gate sits in the
/// router ahead of the preview, so the command is unconfirmable rather than merely unconfirmed, and
/// the refusal carries the `refused` bit the transcript classifies. The same key previews in an
/// ATTENDED server, so what refuses is the session, not the key.
#[test]
fn an_unattended_server_refuses_a_settings_write_before_minting_a_token() {
    let mut s = Server { unattended: true, ..server() };
    for key in ["policy.max_notional_per_order", "config.tradehub_addr"] {
        let err = s
            .call_tool("set_setting", &json!({ "key": key, "value": "1" }))
            .expect_err("an unattended session changes no setting");
        assert!(err.refused, "`{key}`: a GATE refusal: {err:?}");
    }
    assert_eq!(s.pending.next, 0, "no token may exist for a command nobody can confirm");
    let mut attended = server();
    let pv = attended
        .call_tool("set_setting", &json!({ "key": "config.tradehub_addr", "value": "1:2" }))
        .expect("an attended session previews it");
    assert!(pv["preview_token"].is_string(), "{pv}");
}

/// `--unattended` is a BOOLEAN and takes no value — `--unattended=false` would be a way to write
/// the flag and mean its opposite — and an absent flag is an ATTENDED session.
#[test]
fn the_unattended_flag_parses_and_refuses_a_value() {
    let parsed = |argv: &[&str]| parse_config(argv.iter().map(|s| s.to_string()), None, None);
    assert!(parsed(&["--unattended"]).expect("the bare flag parses").unattended);
    assert!(!parsed(&[]).expect("no flag parses").unattended, "attended is the default");
    assert!(parsed(&["--unattended=false"]).is_err(), "a value is refused, never read");
}

// ⚠ Four PURE-BUILDER tests stood here — the mount XOR, the mount's `params` table,
// `unmount_strategy`'s required id, and `set_setting`'s value rendering. They MOVED to
// `crate::tool_verbs`'s tests with the arms they exercise, unchanged in intent, and they are
// now three of the ten the roster loops there cover rather than three of three. What stays in
// this file is everything with a `Server` in front of it.

/// `node_snapshot` and `vike://node/snapshot` leave the frame's `bars` out, and nothing else: the frame
/// that carries them is untouched, so the wire, the GUI and the order path still have them.
#[test]
fn a_read_leaves_the_bars_out() {
    use vike_tradehub_client::wire::BarSeriesWire;
    let snap = CoreSnapshotWire {
        sequence: 1,
        bars: vec![BarSeriesWire {
            venue: "binance".to_string(),
            symbol: "BTCUSDT".to_string(),
            interval: "1m".to_string(),
            closed: Vec::new(),
            forming: None,
        }],
        ..CoreSnapshotWire::empty()
    };
    let frame = serde_json::to_value(&snap).unwrap();
    assert_eq!(frame["bars"][0]["venue"], "binance", "the frame keeps them: {frame}");
    let answer = node_read_answer(&snap).unwrap();
    assert!(answer.get("bars").is_none(), "{answer}");
    assert_eq!(answer["sequence"], 1);
    assert_eq!(answer["pre_fold"], false);
}

/// The frame an agent reads names a resting order's terms (its attributes, its percent trail, its adjustment, the
/// release it rests with) and a held row's price venue and waiting words by the wire's own keys, so nothing between
/// the node and the agent drops them; a row that has none carries no key.
#[test]
fn a_read_names_an_orders_terms_and_a_held_rows_price_venue_and_waiting_words() {
    let held = |order_client_id: &str, release: Value| {
        json!({ "order_client_id": order_client_id, "venue": "ibkr", "symbol": "AAPL.SMART.USD", "side": 1,
                "qty": 1.0, "order_type": "limit", "price": 100.0, "trigger_price": null,
                "parent_order_id": null, "release": release })
    };
    let mut cross = held(
        "h-1",
        json!({ "at": 4_097_217_600_000_i64, "price": { "above": 100_000.0 }, "symbol": "BTCUSDT" }),
    );
    cross["release_venue"] = json!("binance");
    cross["waiting"] = json!("no price of BTCUSDT on binance on this node yet");
    let frame = CoreSnapshotWire {
        sequence: 1,
        orders: serde_json::from_value(json!([{
            "order_client_id": "o-1", "venue": "ibkr", "symbol": "AAPL.SMART.USD", "side": -1,
            "qty": 10.0, "order_type": "stop", "price": null, "trigger_price": 90.0,
            "status": "ACCEPTED", "venue_order_id": null, "filled_qty": 0.0, "average_fill_price": 0.0,
            "attributes": { "all_or_none": true }, "trail_percent": 1.5,
            "adjust": { "at": 110.0, "to": { "stop": { "stop": 100.0 } } },
            "release": { "at": 4_097_217_600_000_i64 }
        }]))
        .expect("the order decodes"),
        held_exits: serde_json::from_value(json!([
            cross,
            held("h-2", json!({ "at": 4_097_217_600_000_i64 }))
        ]))
        .expect("the held rows decode"),
        ..CoreSnapshotWire::empty()
    };
    let answer = node_read_answer(&frame).unwrap();
    let order = &answer["orders"][0];
    assert_eq!(order["attributes"], json!({ "all_or_none": true }), "{order}");
    assert_eq!(order["trail_percent"], 1.5);
    assert_eq!(order["adjust"], json!({ "at": 110.0, "to": { "stop": { "stop": 100.0 } } }));
    assert_eq!(order["release"]["at"], 4_097_217_600_000_i64);
    let rows = &answer["held_exits"];
    assert_eq!(rows[0]["release_venue"], "binance", "{rows}");
    assert_eq!(rows[0]["waiting"], "no price of BTCUSDT on binance on this node yet");
    assert_eq!(rows[0]["release"]["symbol"], "BTCUSDT");
    for key in ["release_venue", "waiting"] {
        assert!(rows[1].get(key).is_none(), "a row with none carries no `{key}`: {rows}");
    }
}

/// `health_node`'s `gatewayhub` says the datahub's answer as numbers, every arm naming the hop, with
/// the keys the same whatever came back, and no budget row in any of them.
#[test]
fn the_gatewayhub_block_says_what_the_datahub_said() {
    use crate::node_reads::gatewayhub_block;
    use vike_client::health::{DatahubAnswer, HubLeg};
    use vike_datahub_client::gateway_status::{
        GatewayBudgetCounter, GatewayStatusReply, GatewayStatusReportWire,
    };
    let block = |answer| gatewayhub_block(&HubLeg { addr: "h:1".to_string(), answer });

    let up = block(DatahubAnswer::Said(GatewayStatusReply {
        reached: true,
        error: None,
        report: Some(GatewayStatusReportWire {
            epoch: 7,
            started_ms: 1,
            uptime_ms: 2,
            connections: 3,
            stream_sessions: 4,
            fetches_running: 5,
            market_data_venues: vec!["binance".to_string()],
            credentialed_data_venues: vec!["okx".to_string()],
            // Carried, and still not shown beside the daemon's health (the expected block has no budget).
            budget: Some(vec![GatewayBudgetCounter {
                venue: "binance".to_string(),
                host: "api".to_string(),
                scope: "ip".to_string(),
                class: "order".to_string(),
                used: 3,
                cap: 50,
                window_ms: 10_000,
                would_throttle: 0,
            }]),
        }),
    }));
    assert_eq!(
        up,
        json!({
            "via": "datahub", "reached": true, "error": null, "uptime_ms": 2, "epoch": 7,
            "connections": 3, "stream_sessions": 4, "fetches_running": 5,
            "market_data_venues": ["binance"], "credentialed_data_venues": ["okx"],
        })
    );

    let empty = json!({
        "via": "datahub", "reached": false, "error": null, "uptime_ms": null, "epoch": null,
        "connections": null, "stream_sessions": null, "fetches_running": null,
        "market_data_venues": null, "credentialed_data_venues": null,
    });
    let with_error = |error: &str| {
        let mut want = empty.clone();
        want["error"] = json!(error);
        want
    };
    let down = block(DatahubAnswer::Said(GatewayStatusReply {
        reached: false,
        error: Some("gatewayhub h:2: refused".to_string()),
        report: None,
    }));
    assert_eq!(down, with_error("gatewayhub h:2: refused"));
    assert_eq!(
        block(DatahubAnswer::NoAnswer("refused".to_string())),
        with_error("the datahub at h:1 does not answer: refused")
    );
    assert_eq!(
        block(DatahubAnswer::Refused("bad key".to_string())),
        with_error("the datahub at h:1 refused this box: bad key")
    );
    assert_eq!(block(DatahubAnswer::TooOld), with_error("the datahub at h:1 is too old to say"));
}

/// `report`'s two numbers are refused by name before anything is dialled, not dropped: a call with no
/// node at all still answers about the argument.
#[test]
fn report_refuses_a_number_that_is_not_one() {
    for (args, named) in [
        (json!({ "cash": "a lot" }), "cash"),
        (json!({ "periods_per_year": [] }), "periods_per_year"),
    ] {
        let resp = call("report", args);
        assert_eq!(resp["result"]["isError"], true, "{resp}");
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains(&format!("`{named}` must be a number")), "{text}");
    }
}

/// The per-call node reads answer with the CONNECTION problem when there is no node — never
/// with an empty payload, which an agent would summarise as "the node runs nothing" and
/// "the node is configured with nothing".
#[test]
fn a_node_read_without_a_node_is_an_error_and_never_an_empty_answer() {
    for tool in ["strategy_status", "settings_show", "health_node", "report"] {
        let resp = call(tool, json!({}));
        assert_eq!(resp["result"]["isError"], true, "{tool}: got {resp}");
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(
            text.contains("no vike-tradehub configured"),
            "{tool}: the error must name what is missing: {text}"
        );
    }
}

/// The three per-call READ failure diagnoses, which an agent must be able to tell apart: two of
/// them are permanent facts about a reachable box and retrying either forever is the behaviour
/// the split exists to prevent.
#[test]
fn a_node_read_failure_says_whether_retrying_could_ever_help() {
    let unsupported = node_read_failure(
        "strategy_status",
        "1.2.3.4:7777",
        &io::Error::new(io::ErrorKind::Unsupported, "no strategy-verbs capability"),
    );
    assert!(unsupported.contains("upgrade"), "{unsupported}");
    assert!(unsupported.contains("cannot succeed"), "{unsupported}");
    assert!(
        unsupported.contains("no strategy-verbs capability"),
        "the client's own sentence \
             names the capability string and must survive: {unsupported}"
    );

    let denied = node_read_failure(
        "settings_show",
        "1.2.3.4:7777",
        &io::Error::new(io::ErrorKind::PermissionDenied, "bad signature"),
    );
    assert!(denied.contains(hubkeys::OBSERVE_KEY_ENV), "points at the key: {denied}");

    let down = node_read_failure(
        "settings_show",
        "1.2.3.4:7777",
        &io::Error::new(io::ErrorKind::ConnectionRefused, "refused"),
    );
    assert!(down.contains("cannot query"), "{down}");
    assert!(!down.contains("upgrade"), "a refused socket is not an old node: {down}");
}

/// An enqueue refused for want of a NODE CAPABILITY must say so in words. It used to print the
/// bare enum name, which the lifecycle verbs made a live path: they are the first tools on this
/// surface that a current-protocol node can decline to accept at all.
#[test]
fn a_capability_refusal_tells_the_agent_not_to_retry() {
    let text = control_rejection(ControlRejected::UnsupportedByHub);
    assert!(text.contains("REFUSED"), "{text}");
    assert!(text.contains("cannot succeed"), "…and that retrying is pointless: {text}");
    assert!(text.contains("upgrade"), "…and where the fix is: {text}");
    // A bare venue on a node from before the rule is the deploy window's case: the spelling that works is said,
    // or an agent believes it cannot flatten.
    assert!(text.contains("`account`") && text.contains("DEFAULT"), "{text}");
    assert!(
        control_rejection(ControlRejected::Busy).contains("try again"),
        "a full queue IS transient and must read differently"
    );
}

/// The two new instruction clauses are scoped by the SAME [`ToolAccess`] the roster is — the
/// identical property `the_instructions_are_scoped_by_the_same_access_the_roster_is` holds for
/// the clauses that shipped before them.
#[test]
fn the_new_instruction_clauses_are_scoped_with_the_tools_they_describe() {
    let full = instructions(&ToolAccess::full());
    let read_only = instructions(&ToolAccess::new(Profile::ReadOnly, Vec::new()));
    let offline = instructions(&ToolAccess::new(Profile::Offline, Vec::new()));

    assert!(full.contains("strategy_status"), "`full` serves the node reads and may say so");
    assert!(
        read_only.contains("strategy_status"),
        "`read-only` still serves them — they authenticate under the observe scope"
    );
    assert!(
        !offline.contains("strategy_status"),
        "`offline` withholds every network tool, so naming one is advertising a withheld tool"
    );

    // The lifecycle clause carries the OWNER's rule for a settings write (`docs/decisions/0086`
    // point 6: an agent changes a live setting only after the owner said yes in chat) — and it is
    // scoped with the tools it governs, like every other clause here.
    assert!(full.contains("yes in chat"), "`full` serves set_setting and must carry the rule");
    for (name, text) in [("read-only", &read_only), ("offline", &offline)] {
        assert!(
            !text.contains("yes in chat"),
            "`{name}` serves no lifecycle write, so the rule is a door it cannot reach"
        );
    }
    // …and nothing of the retype the rule replaced (point 7), under any profile: an instruction
    // naming `policy_confirm` would send an agent after an argument the schema no longer has.
    for (name, text) in [("full", &full), ("read-only", &read_only), ("offline", &offline)] {
        assert!(!text.contains("policy_confirm"), "`{name}` names a removed argument: {text}");
        assert!(
            !text.to_ascii_lowercase().contains("retyp"),
            "`{name}` describes a removed ceremony: {text}"
        );
    }
}

/// **`set_setting` takes no retyped key, and says what does guard it.** The `policy_confirm`
/// argument is gone from the schema (`docs/decisions/0086` point 7), no tool text on this surface
/// asks an agent to obtain a retyping, and the tool's own description carries the owner's rule and
/// the `old → new` its preview shows — the two things an agent needs before it confirms one.
#[test]
fn set_setting_takes_no_retyped_key_and_states_the_owners_rule() {
    let spec = tools_spec();
    let tool = spec
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "set_setting")
        .expect("set_setting is served under `full`");
    let properties = tool["inputSchema"]["properties"].as_object().expect("an object schema");
    assert!(
        !properties.contains_key("policy_confirm"),
        "the retype argument must be gone: {properties:?}"
    );
    let desc = tool["description"].as_str().expect("a description");
    assert!(desc.contains("yes in chat"), "the owner's rule must travel with the tool: {desc}");
    assert!(desc.contains("old → new"), "…and what the preview shows: {desc}");
    let whole = spec.to_string();
    assert!(!whole.contains("policy_confirm"), "no tool may name the removed argument");
    assert!(!whole.to_ascii_lowercase().contains("retyp"), "no tool may describe the ceremony");
}

/// A node that takes ONE connection through the handshake, advertising `features`, reads the one frame
/// that follows (none, when the client refuses first) and hands the socket to `then`.
fn one_frame_node(
    features: &[&str],
    then: impl FnOnce(&mut std::net::TcpStream) + Send + 'static,
) -> String {
    use vike_hub_protocol::frame::{read_frame, write_frame};
    use vike_tradehub_client::proto::{NODE_PROTO_VERSION, Request, Response};

    let features: Vec<String> = features.iter().map(ToString::to_string).collect();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let addr = listener.local_addr().expect("local addr").to_string();
    std::thread::spawn(move || {
        let (mut s, _) = listener.accept().expect("accept");
        let _hello: Request = read_frame(&mut s).expect("Hello");
        let welcome =
            Response::Welcome { proto_version: NODE_PROTO_VERSION, nonce: [7u8; 32], features };
        write_frame(&mut s, &welcome).expect("write Welcome");
        let Ok(Request::Auth { scope, .. }) = read_frame(&mut s) else {
            panic!("expected Auth after Welcome")
        };
        write_frame(&mut s, &Response::AuthOk { scope }).expect("write AuthOk");
        let _frame = read_frame::<_, Request>(&mut s);
        then(&mut s);
    });
    addr
}

/// A CONFIRMED `set_setting`, past the gate, against `addr`.
fn execute_setting(addr: String) -> Result<Value, String> {
    let stored: std::collections::HashMap<String, String> =
        [(hubkeys::CONTROL_KEY_ENV.to_string(), "ctl-key".to_string())].into_iter().collect();
    let mut s =
        Server { node_addr: Some(addr), keys: hubkeys::resolve(&stored, None), ..test_server() };
    let cmd = tool_verbs::wire_command_for(
        "set_setting",
        &json!({ "key": "config.tradehub_addr", "value": "1:2" }),
    )
    .expect("a settings write builds");
    s.execute(&cmd, None)
}

/// **A settings write that went out and lost its answer says UNKNOWN, never "NOT applied"**: the node
/// may have committed the row, and an agent told otherwise acts on a value the node no longer holds.
#[test]
fn a_settings_write_whose_answer_was_lost_is_unknown() {
    use vike_tradehub_client::proto::FEATURE_SETTINGS_WRITE;
    let hangs_up = one_frame_node(&[FEATURE_SETTINGS_WRITE], |_| {});
    let lost = execute_setting(hangs_up).expect_err("no answer came back");
    assert!(lost.contains("UNKNOWN") && lost.contains("settings_show"), "{lost}");
    assert!(!lost.contains("NOT applied"), "{lost}");
}

/// …and one that cannot have been written still says "NOT applied". The handshake row is why the
/// client marks the error: its socket fails with the same kinds as the row above.
#[test]
fn a_settings_write_that_never_reached_the_node_or_was_refused_is_not_applied() {
    use vike_hub_protocol::frame::write_frame;
    use vike_tradehub_client::proto::{FEATURE_SETTINGS_WRITE, Response};

    let refused = one_frame_node(&[FEATURE_SETTINGS_WRITE], |s| {
        write_frame(s, &Response::Error("the loader objects".into())).expect("write the refusal");
    });
    let denied = one_frame_node(&[FEATURE_SETTINGS_WRITE], |s| {
        write_frame(s, &Response::AuthDenied { reason: "no such scope".into() })
            .expect("write the denial");
    });
    let older_node = one_frame_node(&[], |_| {});
    let hung_up_in_the_handshake = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().expect("local addr").to_string();
        std::thread::spawn(move || drop(listener.accept()));
        addr
    };
    for (case, addr, says) in [
        ("the node's refusal", refused, "the loader objects"),
        ("a denial", denied, "no such scope"),
        ("a capability it lacks", older_node, FEATURE_SETTINGS_WRITE),
        ("a dial refused", "127.0.0.1:1".to_string(), "NOT applied"),
        ("a hang-up in the handshake", hung_up_in_the_handshake, "NOT applied"),
    ] {
        let err = execute_setting(addr).expect_err(case);
        assert!(err.contains("NOT applied") && err.contains(says), "{case}: {err}");
        assert!(!err.contains("UNKNOWN"), "{case}: {err}");
    }
}

#[test]
fn node_snapshot_without_node_is_a_clean_error() {
    let resp = call("node_snapshot", json!({}));
    assert_eq!(resp["result"]["isError"], true);
}

/// The condition `node_snapshot`'s first-frame wait ends on, row by row: the client's own
/// placeholder never satisfies it, and EITHER piece of node evidence alone does — a stamped
/// `identity` (an idle node at `seq: 0`, the row the old `seq > 0` condition sat out) or a fold
/// (`seq > 0` from a node that stamps nothing).
#[test]
fn only_a_frame_the_node_pushed_ends_the_first_frame_wait() {
    let placeholder = CoreSnapshotWire::empty();
    assert!(!is_node_frame(&placeholder), "the client's placeholder is never the node's answer");

    let identity = vike_tradehub_client::wire::HubIdentityWire {
        name: "idle".to_string(),
        strategy: "spread_maker".to_string(),
        params: String::new(),
        live: false,
        build: "test".to_string(),
        advertise_addr: String::new(),
    };
    let idle_stamped = CoreSnapshotWire { identity: Some(identity), ..CoreSnapshotWire::empty() };
    assert!(is_node_frame(&idle_stamped), "an idle node's stamped seq-0 frame is a real answer");

    let folded_unstamped = CoreSnapshotWire { sequence: 1, ..CoreSnapshotWire::empty() };
    assert!(is_node_frame(&folded_unstamped), "a fold is node evidence without any identity");
}

/// **`node_snapshot` needs no code to show the hub links: it serializes the frame, and this pins that it stays so.**
/// A lost trade link, a closed lease, a lost price lane and the cause of `adapter failed` are all in the answer an agent
/// reads, under the frame's own keys; a healthy frame writes none of them.
#[test]
fn a_read_shows_the_hub_links_and_the_cause_of_a_refused_start() {
    use vike_tradehub_client::wire::{
        ArmingNotRunningViewWire, PriceLinkDownWire, TradeLinkDownWire,
    };
    let healthy =
        node_read_answer(&CoreSnapshotWire { sequence: 1, ..CoreSnapshotWire::empty() }).unwrap();
    for key in ["trade_link_down", "price_link_down", "not_running"] {
        assert!(healthy.get(key).is_none(), "{key}: {healthy}");
    }

    let snap = CoreSnapshotWire {
        sequence: 1,
        trade_link_down: vec![
            TradeLinkDownWire {
                route_key: "binance".to_string(),
                why: "the gatewayhub is stopping".to_string(),
                down_ms: 1_000,
                closed: false,
            },
            TradeLinkDownWire {
                route_key: "bybit".to_string(),
                why: "taken over by key-1".to_string(),
                down_ms: 2_000,
                closed: true,
            },
        ],
        price_link_down: vec![PriceLinkDownWire {
            venue: "okx".to_string(),
            why: "connection refused".to_string(),
            down_ms: 3_000,
            of: 1,
        }],
        not_running: vec![ArmingNotRunningViewWire {
            venue: "ibkr".to_string(),
            account: None,
            route_key: "ibkr".to_string(),
            tier: None,
            reason: "adapter failed".to_string(),
            detail: Some("the lease is held by key-9".to_string()),
            block_word: None,
        }],
        ..CoreSnapshotWire::empty()
    };
    let answer = node_read_answer(&snap).unwrap();
    assert_eq!(answer["trade_link_down"][0]["why"], "the gatewayhub is stopping");
    assert_eq!(answer["trade_link_down"][1]["closed"], true);
    assert_eq!(answer["price_link_down"][0]["venue"], "okx");
    assert_eq!(answer["not_running"][0]["detail"], "the lease is held by key-9");
}

/// A read tool MARKS every placeholder frame — the client's own and the node's stamped pre-fold one
/// alike, since both carry `venues: []` and epoch `0` that are not readings — and marks nothing
/// else. `pre_fold` is present on every answer, so `false` can be relied on too.
#[test]
fn a_read_marks_a_frame_with_nothing_built_as_pre_fold_and_only_that() {
    let identity = vike_tradehub_client::wire::HubIdentityWire {
        name: "pre-fold".to_string(),
        strategy: "spread_maker".to_string(),
        params: String::new(),
        live: false,
        build: "test".to_string(),
        advertise_addr: String::new(),
    };
    let node_placeholder =
        CoreSnapshotWire { identity: Some(identity), ..CoreSnapshotWire::empty() };
    for (what, snap) in [("client", CoreSnapshotWire::empty()), ("node", node_placeholder)] {
        let answer = node_read_answer(&snap).expect("serializes");
        assert_eq!(answer["pre_fold"], true, "the {what}'s placeholder is marked: {answer}");
        let note = answer["pre_fold_note"].as_str().expect("…with the note");
        assert!(note.contains("does NOT mean nothing is mounted"), "{note}");
        assert_eq!(answer["sequence"], 0, "the frame itself is served unchanged: {answer}");
    }

    let built = CoreSnapshotWire { sequence: 7, ..CoreSnapshotWire::empty() };
    let answer = node_read_answer(&built).expect("serializes");
    assert_eq!(answer["pre_fold"], false, "a built frame is not marked: {answer}");
    assert!(answer.get("pre_fold_note").is_none(), "…and carries no note: {answer}");
    assert_eq!(answer["sequence"], 7, "{answer}");
}

/// The two placeholders `pre_fold` marks are two different FACTS, and the note says which one an
/// answer is. The NODE's (stamped `identity`, `seq: 0`) is a frame the node really published before
/// its first fold: the node was reached and has built nothing. The CLIENT's own
/// (`CoreSnapshotWire::empty()`: `identity: null`, `seq: 0`) is what a fresh connection holds until a
/// node frame arrives — the read's deadline passed before one did, and nothing in it came from the
/// node. The note used to call both "the placeholder a node publishes before its first fold", which
/// was false of the second. `pre_fold` itself is unchanged: `true` on both.
#[test]
fn the_pre_fold_note_names_whose_placeholder_the_frame_is() {
    let identity = vike_tradehub_client::wire::HubIdentityWire {
        name: "pre-fold".to_string(),
        strategy: "spread_maker".to_string(),
        params: String::new(),
        live: false,
        build: "test".to_string(),
        advertise_addr: String::new(),
    };
    let node = node_read_answer(&CoreSnapshotWire {
        identity: Some(identity),
        ..CoreSnapshotWire::empty()
    })
    .expect("serializes");
    let client = node_read_answer(&CoreSnapshotWire::empty()).expect("serializes");
    assert_eq!((node["pre_fold"].clone(), client["pre_fold"].clone()), (json!(true), json!(true)));
    let node_note = node["pre_fold_note"].as_str().expect("the node's placeholder carries a note");
    let client_note = client["pre_fold_note"].as_str().expect("so does the client's own");

    assert!(
        client_note.contains("client's OWN") && client_note.contains("identity: null"),
        "the client's placeholder must be named as the client's own: {client_note}"
    );
    assert!(
        !client_note.contains("stamped with its identity"),
        "…and never as a frame the node published: {client_note}"
    );
    assert!(
        node_note.contains("stamped with its identity"),
        "the node's placeholder must be named as the frame the node published: {node_note}"
    );
    assert!(!node_note.contains("client's OWN"), "…and never as the client's own: {node_note}");
    for note in [node_note, client_note] {
        assert!(note.contains("does NOT mean nothing is mounted"), "{note}");
    }
}

/// **The write tools wait `config.node_ack_wait_ms`, carried by the server.** A server built with
/// no row waits the row's default — the two seconds the `mcp` constant it replaced always waited —
/// and the wait a caller hands in is the one the server holds, which `offer_command` passes to
/// `TradehubControlHandle::await_outcome`. The sentence an agent reads when it runs out spells the
/// SAME duration (the shared `verbs::wait_seconds_text`), never a rounded-down `0s`.
#[test]
fn the_write_tools_wait_the_configured_ack_wait() {
    use std::time::Duration;
    assert_eq!(
        server().ack_wait,
        Duration::from_millis(u64::from(vike_config::config::DEFAULT_NODE_ACK_WAIT_MS)),
        "a server with no row waits the row's default"
    );
    let s = Server { ack_wait: Duration::from_millis(500), ..server() };
    assert_eq!(s.ack_wait, Duration::from_millis(500));
    assert_eq!(verbs::wait_seconds_text(s.ack_wait), "0.5s");
}
