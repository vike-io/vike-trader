//! The node-lifecycle write tools, the settings write and its gates, and the per-call node reads.

use super::*;
use crate::cmd::mcp::node_reads::{node_read_answer, node_read_failure};
use crate::cmd::mcp::node_writes::{LIFECYCLE_TOOLS, control_rejection};
use crate::cmd::trade::is_node_frame;
use vike_tradehub_client::ControlRejected;
use vike_tradehub_client::wire::WireSnapshot;

/// COMPOSED, not spelled. `crates/vike-config/tests/policy_is_consumed.rs` used to read every
/// non-comment line under `src/` for a section-qualified policy key as evidence that the file READS
/// that setting, test modules included, so a fixture spelling one turned `main` red (#1685 lifted
/// these fixtures here from `mcp.rs` and `Policy::max_leverage` is `Consumed::No`). It now reads the
/// PRODUCTION view only (comments, `#[cfg(test)]` items and test files are skipped), so the
/// composition is no longer required; it is kept because it is harmless.
/// Composing it is the same trick `crates/vike-model/src/credential_keys.rs` uses to keep its
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
    // names agreeing with three names while `verbs::wire_command_for` answers `no wire command
    // for …` — the state this file's own lifecycle builder existed to avoid, and which the
    // lift into `verbs` would silently restore if one name were left behind there.
    for tool in LIFECYCLE_TOOLS {
        assert!(
            verbs::wire_command_for(tool, &every_write_tools_arguments()).is_ok(),
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
        let mut args = every_write_tools_arguments();
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
/// went with the file era (the wire's is derived from the key, `crate::cmd::verbs`' job).
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
                .is_some_and(|why| why.contains("no vike-tradehub node configured")),
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
    assert!(err.message.contains("no vike-tradehub node configured"), "{err:?}");
}

/// A CONFIRMED settings write leaves by [`Server::execute_settings_write`] rather than the
/// fire-and-forget worker, and with no node configured that surfaces as the connection error —
/// which is the positive evidence it got PAST the preview gate, the same shape
/// `crates/vike-cli/tests/mcp_transcript.rs` uses for `submit_order`.
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
        text.contains("no vike-tradehub node configured"),
        "a matching token must reach the node step — that is what proves the gate ADMITS: \
             {text}"
    );
}

/// **The UNATTENDED gate refuses EVERY settings write** (decision 0040: the owner's rulings of
/// 2026-09-28 for `policy.*` and 2026-09-29 for every other key) — whatever the key's class or
/// spelling — and nothing that is not a settings write.
#[test]
fn the_unattended_gate_refuses_every_settings_write_and_nothing_else() {
    let setting = |key: &str| WireCommand::SetSetting {
        file: verbs::section_of_key(key).to_string(),
        key: key.to_string(),
        value: "1".into(),
        confirm: None,
    };
    for key in [
        "policy.max_notional_per_order",
        POLICY_KEY_FIXTURE,
        "policy.venues.binance",
        "policy.accounts.binance.ALT",
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
    assert_eq!(unattended_refusal(&WireCommand::Cancel("c-1".into())), Ok(()), "not a setting");
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
// `crate::cmd::verbs`'s tests with the arms they exercise, unchanged in intent, and they are
// now three of the ten the roster loops there cover rather than three of three. What stays in
// this file is everything with a `Server` in front of it.

/// The two per-call node reads answer with the CONNECTION problem when there is no node — never
/// with an empty payload, which an agent would summarise as "the node runs nothing" and
/// "the node is configured with nothing".
#[test]
fn a_node_read_without_a_node_is_an_error_and_never_an_empty_answer() {
    for tool in ["strategy_status", "settings_show"] {
        let resp = call(tool, json!({}));
        assert_eq!(resp["result"]["isError"], true, "{tool}: got {resp}");
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(
            text.contains("no vike-tradehub node configured"),
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
    assert!(denied.contains(nodekeys::OBSERVE_KEY_ENV), "points at the key: {denied}");

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
    let text = control_rejection(ControlRejected::UnsupportedByNode);
    assert!(text.contains("REFUSED"), "{text}");
    assert!(text.contains("cannot succeed"), "…and that retrying is pointless: {text}");
    assert!(text.contains("upgrade"), "…and where the fix is: {text}");
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
    let props = tool["inputSchema"]["properties"].as_object().expect("an object schema");
    assert!(!props.contains_key("policy_confirm"), "the retype argument must be gone: {props:?}");
    let desc = tool["description"].as_str().expect("a description");
    assert!(desc.contains("yes in chat"), "the owner's rule must travel with the tool: {desc}");
    assert!(desc.contains("old → new"), "…and what the preview shows: {desc}");
    let whole = spec.to_string();
    assert!(!whole.contains("policy_confirm"), "no tool may name the removed argument");
    assert!(!whole.to_ascii_lowercase().contains("retyp"), "no tool may describe the ceremony");
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
    let placeholder = WireSnapshot::empty();
    assert!(!is_node_frame(&placeholder), "the client's placeholder is never the node's answer");

    let identity = vike_tradehub_client::wire::WireNodeIdentity {
        name: "idle".to_string(),
        strategy: "spread_maker".to_string(),
        params: String::new(),
        live: false,
        build: "test".to_string(),
        advertise_addr: String::new(),
    };
    let idle_stamped = WireSnapshot { identity: Some(identity), ..WireSnapshot::empty() };
    assert!(is_node_frame(&idle_stamped), "an idle node's stamped seq-0 frame is a real answer");

    let folded_unstamped = WireSnapshot { seq: 1, ..WireSnapshot::empty() };
    assert!(is_node_frame(&folded_unstamped), "a fold is node evidence without any identity");
}

/// A read tool MARKS every placeholder frame — the client's own and the node's stamped pre-fold one
/// alike, since both carry `venues: []` and epoch `0` that are not readings — and marks nothing
/// else. `pre_fold` is present on every answer, so `false` can be relied on too.
#[test]
fn a_read_marks_a_frame_with_nothing_built_as_pre_fold_and_only_that() {
    let identity = vike_tradehub_client::wire::WireNodeIdentity {
        name: "pre-fold".to_string(),
        strategy: "spread_maker".to_string(),
        params: String::new(),
        live: false,
        build: "test".to_string(),
        advertise_addr: String::new(),
    };
    let node_placeholder = WireSnapshot { identity: Some(identity), ..WireSnapshot::empty() };
    for (what, snap) in [("client", WireSnapshot::empty()), ("node", node_placeholder)] {
        let answer = node_read_answer(&snap).expect("serializes");
        assert_eq!(answer["pre_fold"], true, "the {what}'s placeholder is marked: {answer}");
        let note = answer["pre_fold_note"].as_str().expect("…with the note");
        assert!(note.contains("does NOT mean nothing is mounted"), "{note}");
        assert_eq!(answer["seq"], 0, "the frame itself is served unchanged: {answer}");
    }

    let built = WireSnapshot { seq: 7, ..WireSnapshot::empty() };
    let answer = node_read_answer(&built).expect("serializes");
    assert_eq!(answer["pre_fold"], false, "a built frame is not marked: {answer}");
    assert!(answer.get("pre_fold_note").is_none(), "…and carries no note: {answer}");
    assert_eq!(answer["seq"], 7, "{answer}");
}

/// The two placeholders `pre_fold` marks are two different FACTS, and the note says which one an
/// answer is. The NODE's (stamped `identity`, `seq: 0`) is a frame the node really published before
/// its first fold: the node was reached and has built nothing. The CLIENT's own
/// (`WireSnapshot::empty()`: `identity: null`, `seq: 0`) is what a fresh connection holds until a
/// node frame arrives — the read's deadline passed before one did, and nothing in it came from the
/// node. The note used to call both "the placeholder a node publishes before its first fold", which
/// was false of the second. `pre_fold` itself is unchanged: `true` on both.
#[test]
fn the_pre_fold_note_names_whose_placeholder_the_frame_is() {
    let identity = vike_tradehub_client::wire::WireNodeIdentity {
        name: "pre-fold".to_string(),
        strategy: "spread_maker".to_string(),
        params: String::new(),
        live: false,
        build: "test".to_string(),
        advertise_addr: String::new(),
    };
    let node =
        node_read_answer(&WireSnapshot { identity: Some(identity), ..WireSnapshot::empty() })
            .expect("serializes");
    let client = node_read_answer(&WireSnapshot::empty()).expect("serializes");
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
