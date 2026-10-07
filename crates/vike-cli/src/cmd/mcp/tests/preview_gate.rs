//! The mandatory-preview gate: what a preview carries, and the token that alone confirms it.

use super::*;
use crate::cmd::mcp::node_writes::{CHECKED_BY_NODE, CHECKED_BY_NONE};

/// A minimal DELETE intent — this test only needs *an* intent to hang a token on; what is
/// under test is the EPOCH the token carries, not the intent.
fn any_intent() -> PreviewIntent {
    PreviewIntent::Delete(DeleteIntent {
        kind: "bar".to_string(),
        venue: "binance".to_string(),
        symbol: None,
        group: None,
        interval: None,
        produced_by: "test".to_string(),
    })
}

/// **A preview STAMPS the account epoch it saw**, which is what a confirm compares against. The
/// three checks that already guarded a confirm ask *is this token ours*, *is it still warm*,
/// and *was it issued for this command* — none of them asks whether the NODE is still the node
/// that was previewed.
#[test]
fn a_preview_stamps_the_account_epoch_it_saw() {
    let mut store = PendingPreviews::default();
    let token = store.issue(any_intent(), Some(7));
    let taken = store.take(&token).expect("the token was just issued");
    assert_eq!(
        taken.accounts_epoch,
        Some(7),
        "the epoch at preview time is what a confirm compares"
    );
}

/// ⚠ **The complement, and it is what keeps the refusal off every older node:** a node that
/// reports no epoch stamps `Some(0)`, and `Some(0) == Some(0)` compares EQUAL. A node that
/// cannot report an account set cannot have moved one either, so refusing on that would break
/// every confirm against such a node while proving nothing.
#[test]
fn a_node_that_reports_no_epoch_stamps_zero() {
    let mut store = PendingPreviews::default();
    let token = store.issue(any_intent(), Some(0));
    let taken = store.take(&token).expect("the token was just issued");
    assert_eq!(
        taken.accounts_epoch,
        Some(0),
        "two answered zeroes are the pre-field behaviour exactly"
    );
}

/// ⚠ **A node that could not be ASKED stamps `None`, which is a THIRD state and not the
/// answered zero above.** This is the distinction the guard turns on: an unreadable epoch used
/// to be folded into `0`, so a dropped read connection read as an account set that had moved
/// to zero — and the resulting refusal told the agent an account "may have been mounted,
/// unmounted, or relabelled" when the truth was that the link died.
///
/// The assertion is deliberately `ne` against `Some(0)` rather than `eq` against `None`: what
/// must hold is that the two are DISTINGUISHABLE, which is the property a future `unwrap_or`
/// creeping back would destroy while an `eq` on `None` alone still passed.
#[test]
fn a_node_that_could_not_be_asked_stamps_a_state_of_its_own() {
    let mut store = PendingPreviews::default();
    let unreadable = store.issue(any_intent(), None);
    let answered_zero = store.issue(any_intent(), Some(0));
    assert_ne!(
        store.take(&unreadable).expect("issued").accounts_epoch,
        store.take(&answered_zero).expect("issued").accounts_epoch,
        "\"could not ask\" and \"the node answered 0\" must not be the same value — folding \
             them is what let a lost link be reported as a changed account set"
    );
}

/// Two previews taken across a CHANGED set carry different stamps — the property the refusal
/// reads. Without this the two tests above are satisfied by a field nothing ever varies.
#[test]
fn two_previews_across_a_changed_set_carry_different_stamps() {
    let mut store = PendingPreviews::default();
    let before = store.issue(any_intent(), Some(11));
    let after = store.issue(any_intent(), Some(12));
    assert_ne!(
        store.take(&before).expect("issued").accounts_epoch,
        store.take(&after).expect("issued").accounts_epoch,
        "a moved account set must be visible to the confirm that compares them"
    );
}

#[test]
fn submit_order_without_confirm_only_previews_and_touches_no_network() {
    // node_addr is None; a preview must NOT try to connect (it would error). It returns the
    // resolved wire command with will_execute:false.
    let resp = call(
        "submit_order",
        json!({ "venue": "sim", "symbol": "BTCUSDT", "side": 1, "qty": 0.5, "order_type": "limit", "price": 100.0 }),
    );
    assert_eq!(resp["result"]["isError"], false, "a preview succeeds even with no node");
    let sc = &resp["result"]["structuredContent"];
    assert_eq!(sc["will_execute"], false);
    assert_eq!(sc["wire_command"]["Submit"]["symbol"], "BTCUSDT");
    assert_eq!(sc["wire_command"]["Submit"]["qty"], 0.5);
    assert_eq!(sc["guardrail"]["notional"], 50.0); // 0.5 * 100
}

/// Every WRITE tool advertises the optional `reason` (node proto v4) — and never requires it, so
/// an agent that ignores it keeps working exactly as before.
#[test]
fn every_write_tool_advertises_an_optional_reason() {
    let resp = server().handle(&req(2, "tools/list", json!({}))).unwrap();
    let tools = resp["result"]["tools"].as_array().unwrap();
    for name in WRITE_TOOLS {
        let tool = tools.iter().find(|t| t["name"] == name).expect("advertised");
        let props = &tool["inputSchema"]["properties"];
        assert_eq!(props["reason"]["type"], "string", "{name} must offer a `reason` string");
        assert!(
            props["reason"]["description"].as_str().unwrap().contains("audit"),
            "{name}: the reason description must say where it lands"
        );
        let required = tool["inputSchema"]["required"].as_array();
        assert!(
            required.is_none_or(|r| !r.iter().any(|v| v == "reason")),
            "{name}: `reason` must stay OPTIONAL"
        );
    }
    // A READ tool has no rationale to record.
    let snap = tools.iter().find(|t| t["name"] == "node_snapshot").unwrap();
    assert!(snap["inputSchema"]["properties"]["reason"].is_null());
}

/// The preview ECHOES the rationale (so the agent sees what will be filed against the command)
/// while leaving the resolved command untouched — the reason rides beside it, never inside it.
#[test]
fn a_write_preview_echoes_the_reason_without_changing_the_command() {
    let bare = call("cancel_order", json!({ "client_order_id": "c-1" }));
    assert_eq!(bare["result"]["structuredContent"]["reason"], Value::Null);

    let withr = call(
        "cancel_order",
        json!({ "client_order_id": "c-1", "reason": "  stale quote after the feed gap  " }),
    );
    let sc = &withr["result"]["structuredContent"];
    assert_eq!(sc["will_execute"], false);
    assert_eq!(sc["reason"], "stale quote after the feed gap", "trimmed, and echoed back");
    assert_eq!(
        sc["wire_command"], bare["result"]["structuredContent"]["wire_command"],
        "the wire command is byte-identical with and without a reason"
    );
    // A blank rationale is no rationale (never an empty string in the preview).
    let blank = call("cancel_order", json!({ "client_order_id": "c-1", "reason": "   " }));
    assert_eq!(blank["result"]["structuredContent"]["reason"], Value::Null);
}

#[test]
fn submit_order_missing_required_field_is_a_tool_error() {
    let resp = call("submit_order", json!({ "venue": "sim", "side": 1, "qty": 1.0 })); // no symbol
    assert_eq!(resp["result"]["isError"], true);
}

#[test]
fn confirm_alone_no_longer_executes_it_returns_a_preview() {
    // ⚠ THE REGRESSION THIS PINS. `confirm: true` used to be the whole gate, so a FIRST-and-only
    // call executed — while this module's doc promised "only a second call ... sends it". It now
    // fails CLOSED: without a `preview_token` the call is a preview, whatever `confirm` says.
    let resp = call(
        "submit_order",
        json!({ "venue": "sim", "symbol": "BTCUSDT", "side": 1, "qty": 1.0, "confirm": true }),
    );
    assert_eq!(
        resp["result"]["isError"], false,
        "confirm without a token is a PREVIEW, not an error"
    );
    let sc = &resp["result"]["structuredContent"];
    assert_eq!(sc["will_execute"], false, "nothing may be sent without a preview_token");
    assert!(sc["preview_token"].is_string(), "a preview must hand back the token that confirms it");
}

#[test]
fn a_preview_with_no_node_says_it_was_not_verified() {
    // An ABSENT node verdict must never read as an approving one — the client-side guardrail
    // cannot price a market order at all, and market is the default order type.
    let resp =
        call("submit_order", json!({ "venue": "sim", "symbol": "BTCUSDT", "side": 1, "qty": 1.0 }));
    let sc = &resp["result"]["structuredContent"];
    assert_eq!(sc["verified_by_node"], false);
    assert_eq!(sc["node_verdict"], Value::Null);
    assert!(
        sc["note"].as_str().unwrap().contains("UNVERIFIED CLIENT-SIDE ESTIMATE"),
        "the note must SAY the verdict is unverified: {}",
        sc["note"]
    );
}

#[test]
fn a_node_that_could_not_be_asked_is_not_a_verified_preview() {
    // ⚠ THE BUG THIS PINS. `verified` used to be `node.is_some()`, and `Server::node_preview`
    // returns `Some` on its FAILURE path too — a `checked_by: "none"` row carrying the
    // transport error, so the preview can name the fault. So a node that was dialled and
    // refused (unreachable, `AuthDenied`, `Response::Error`) came back `verified_by_node: true`
    // wearing the note that calls the verdict "the one that counts" — while `node_verdict`
    // right beside it said the node was never asked. The prompts this server serves teach an
    // agent to read that flag and confirm on it; for a MARKET order the client-side guardrail
    // is vacuous too, so nothing at all would have checked the order.
    let cmd = verbs::wire_command_for(
        "submit_order",
        &json!({ "venue": "sim", "symbol": "BTCUSDT", "side": 1, "qty": 1.0 }),
    )
    .unwrap();
    let unreachable = json!({
        "checked_by": CHECKED_BY_NONE,
        "accepted": Value::Null,
        "reason": "the node could not be asked: connection refused",
    });
    let pv = preview_of(
        "submit_order",
        &cmd,
        None,
        verbs::GuardrailCaps::default(),
        "pv-1",
        Some(unreachable),
        VENUE_CHECK_UNVERIFIED,
        // Not a settings write, so no `change` rides the preview.
        None,
    );
    assert_eq!(pv["verified_by_node"], false, "a fault is not a verdict: {pv}");
    assert!(pv.get("change").is_none(), "only a settings write's preview carries `change`: {pv}");
    assert!(
        pv["note"].as_str().unwrap().contains("UNVERIFIED CLIENT-SIDE ESTIMATE"),
        "the note must match the flag: {}",
        pv["note"]
    );
    // ...and the answering case still reads as verified, or the fix would have disarmed the
    // distinction rather than corrected it.
    let answered = json!({
        "checked_by": CHECKED_BY_NODE,
        "accepted": true,
        "reason": Value::Null,
    });
    let ok = preview_of(
        "submit_order",
        &cmd,
        None,
        verbs::GuardrailCaps::default(),
        "pv-2",
        Some(answered),
        VENUE_CHECK_MOUNTED,
        None,
    );
    assert_eq!(ok["verified_by_node"], true, "an answered dry-run IS a verdict: {ok}");
}

#[test]
fn a_preview_whose_node_could_not_be_asked_is_not_verified() {
    // A node IS configured and a control key IS present, but the dry-run cannot reach it
    // (`127.0.0.1:1` refuses immediately). `node_preview` answers with a `checked_by: "none"`
    // object rather than `None`, and `preview_of` used to label THAT `verified_by_node: true`
    // with the "it is the verdict that counts" note — a dropped tunnel mid-incident read as
    // the node's own approval. A fault is not a verdict.
    let env: std::collections::HashMap<String, String> =
        [(nodekeys::CONTROL_KEY_ENV.to_string(), "ctl-key".to_string())].into_iter().collect();
    // Over `test_server()` rather than a second field list — the same argument the `server()`
    // helper's own doc makes: a hand-written list here is a second place to forget a field, and
    // the two would then disagree about what "a fresh server" is.
    let mut s = Server {
        node_addr: Some("127.0.0.1:1".to_string()),
        keys: nodekeys::resolve(&env, &std::collections::HashMap::new(), None),
        ..test_server()
    };
    let resp = s
        .handle(&req(
            1,
            "tools/call",
            json!({ "name": "cancel_order", "arguments": { "client_order_id": "c-1" } }),
        ))
        .unwrap();
    assert_eq!(resp["result"]["isError"], false, "a preview succeeds even unreachable");
    let sc = &resp["result"]["structuredContent"];
    assert_eq!(sc["will_execute"], false);
    assert_eq!(sc["node_verdict"]["checked_by"], "none", "{}", sc["node_verdict"]);
    assert_eq!(sc["node_verdict"]["accepted"], Value::Null);
    assert_eq!(sc["verified_by_node"], false, "a transport fault is not a verdict");
    assert!(
        sc["note"].as_str().unwrap().contains("UNVERIFIED CLIENT-SIDE ESTIMATE"),
        "the note must SAY the verdict is unverified: {}",
        sc["note"]
    );
}

#[test]
fn a_preview_token_fires_at_most_once() {
    let mut s = server();
    let args = json!({ "venue": "sim", "symbol": "BTCUSDT", "side": 1, "qty": 1.0 });
    let pv = s.call_tool("submit_order", &args).unwrap();
    let token = pv["preview_token"].as_str().unwrap().to_string();
    let mut confirming = args.clone();
    confirming["confirm"] = json!(true);
    confirming["preview_token"] = json!(token.clone());
    // First confirm consumes the token; with no node configured it fails at the CONNECTION,
    // which is proof it got PAST the gate.
    let first = s.call_tool("submit_order", &confirming).unwrap_err();
    assert!(first.message.contains("node"), "should reach the node step, got: {first:?}");
    assert!(!first.refused, "reaching the node is a FAILURE, not a gate refusal: {first:?}");
    // Second gets nothing — the token was removed before the caller executed.
    let second = s.call_tool("submit_order", &confirming).unwrap_err();
    assert!(
        second.message.contains("unknown or already used"),
        "a token must fire at most once, got: {second:?}"
    );
    assert!(second.refused, "a spent token is a GATE refusal, and the transcript reads that bit");
}

#[test]
fn intent_ignores_the_minted_id_but_nothing_else() {
    // ⚠ THE BUG THIS PINS. An exact compare rejected EVERY confirm, because
    // `fill_client_order_id` mints a fresh id on every call — so a preview and its confirm can
    // never carry the same one, and submit_order was unusable through this surface. CI caught
    // it; this keeps it caught.
    let base = verbs::wire_command_for(
        "submit_order",
        &json!({ "venue": "sim", "symbol": "BTCUSDT", "side": 1, "qty": 1.0 }),
    )
    .unwrap();
    let mut a = base.clone();
    let mut b = base.clone();
    if let (WireCommand::Submit(x), WireCommand::Submit(y)) = (&mut a, &mut b) {
        x.client_order_id = "c-1".into();
        y.client_order_id = "c-2".into();
    }
    let node = |c: &WireCommand| PreviewIntent::Node(Box::new(c.clone()));
    assert!(same_intent(&node(&a), &node(&b)), "a differing minted id must NOT break the binding");

    // …but every other field still binds.
    let other = verbs::wire_command_for(
        "submit_order",
        &json!({ "venue": "sim", "symbol": "BTCUSDT", "side": 1, "qty": 50.0 }),
    )
    .unwrap();
    assert!(!same_intent(&node(&a), &node(&other)), "a different qty MUST break the binding");
    // ⚠ …and two DIFFERENT KINDS of intent never match, whatever they carry: the discriminant
    // is part of the compare, which is what stops a node token confirming a deletion.
    assert!(!same_intent(
        &node(&a),
        &PreviewIntent::Delete(DeleteIntent {
            kind: "bar".into(),
            venue: "sim".into(),
            symbol: None,
            group: None,
            interval: None,
            produced_by: "klines:".into(),
        })
    ));
}

#[test]
fn a_preview_token_is_bound_to_the_command_it_previewed() {
    // ⚠ Preview `qty: 0.5`, confirm `qty: 50` was ACCEPTED before this gate existed.
    let mut s = server();
    let pv = s
        .call_tool(
            "submit_order",
            &json!({ "venue": "sim", "symbol": "BTCUSDT", "side": 1, "qty": 0.5 }),
        )
        .unwrap();
    let token = pv["preview_token"].as_str().unwrap().to_string();
    let err = s
        .call_tool(
            "submit_order",
            &json!({ "venue": "sim", "symbol": "BTCUSDT", "side": 1, "qty": 50.0,
                         "confirm": true, "preview_token": token }),
        )
        .unwrap_err();
    assert!(
        err.message.contains("does not match this command"),
        "a token must not confirm a DIFFERENT command, got: {err:?}"
    );
}

#[test]
fn set_trading_state_preview_maps_the_wire_command() {
    let resp = call("set_trading_state", json!({ "state": "halted" }));
    assert_eq!(resp["result"]["structuredContent"]["wire_command"]["SetTradingState"], "Halted");
}

#[test]
fn market_exit_preview_allows_an_omitted_venue() {
    let resp = call("market_exit", json!({}));
    assert_eq!(resp["result"]["isError"], false);
    assert!(resp["result"]["structuredContent"]["wire_command"]["MarketExit"].is_object());
}

#[test]
fn modify_without_confirm_only_previews_the_shared_wire_command() {
    // The verbs-module construction site: the SAME WireCommand::Modify the trade REPL's
    // `modify c-1 --qty 2` builds, preview-gated like every other write tool.
    let resp = call("modify", json!({ "client_order_id": "c-1", "new_qty": 2.0 }));
    assert_eq!(resp["result"]["isError"], false, "a preview succeeds even with no node");
    let sc = &resp["result"]["structuredContent"];
    assert_eq!(sc["will_execute"], false);
    assert_eq!(sc["wire_command"]["Modify"]["client_order_id"], "c-1");
    assert_eq!(sc["wire_command"]["Modify"]["new_qty"], 2.0);
    assert_eq!(sc["wire_command"]["Modify"]["new_price"], Value::Null);
    assert_eq!(sc["guardrail"]["within_limits"], true, "no order size to check");
}

#[test]
fn modify_with_nothing_to_change_is_a_tool_error() {
    let resp = call("modify", json!({ "client_order_id": "c-1" }));
    assert_eq!(resp["result"]["isError"], true);
    assert!(resp["result"]["content"][0]["text"].as_str().unwrap().contains("new_qty"));
}

#[test]
fn mass_cancel_preview_allows_an_omitted_scope() {
    let resp = call("mass_cancel", json!({}));
    assert_eq!(resp["result"]["isError"], false);
    let sc = &resp["result"]["structuredContent"];
    assert_eq!(sc["will_execute"], false);
    assert_eq!(sc["wire_command"]["MassCancel"]["venue"], Value::Null);
    assert_eq!(sc["wire_command"]["MassCancel"]["symbol"], Value::Null);
}

#[test]
fn mass_cancel_confirm_without_a_token_is_a_preview_not_a_send() {
    let resp = call("mass_cancel", json!({ "venue": "sim", "confirm": true }));
    assert_eq!(resp["result"]["isError"], false, "no token ⇒ preview, never a send");
    assert_eq!(resp["result"]["structuredContent"]["will_execute"], false);
}
