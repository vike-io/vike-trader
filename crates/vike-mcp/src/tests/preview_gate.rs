//! The mandatory-preview gate: what a preview carries, and the token that alone confirms it.

use super::*;
use crate::node_writes::{CHECKED_BY_NODE, CHECKED_BY_NONE};

/// What a read says of its frame: its age always, and past the bound which of the two things an old frame can be. No stamp
/// (a node that predates it) adds nothing, and never a made-up figure.
#[test]
fn a_frame_past_the_bound_says_so_and_which_kind_of_old_it_is() {
    use crate::node_reads::{FRAME_AGE_BOUND_MS, stamp_frame_age};
    use vike_tradehub_client::FrameAge;
    let stamped = |age_ms, arrived_after_ms| {
        let mut answer = json!({});
        stamp_frame_age(&mut answer, Some(FrameAge { age_ms, arrived_after_ms }));
        answer
    };

    let current = stamped(FRAME_AGE_BOUND_MS, 40);
    assert_eq!(current["frame_age_ms"], FRAME_AGE_BOUND_MS);
    assert_eq!(current["frame_arrived_after_ms"], 40);
    assert!(current.get("frame_stale").is_none(), "{current}");

    let behind = stamped(59_500, 59_400);
    let why = behind["frame_stale"].as_str().expect("said");
    assert!(why.contains("59500 ms") && why.contains("link to the node is behind"), "{why}");
    assert!(why.contains("Read again"), "{why}");

    let quiet = stamped(59_500, 40);
    let why = quiet["frame_stale"].as_str().expect("said");
    assert!(why.contains("pushed nothing newer"), "{why}");

    let mut unstamped = json!({ "seq": 1 });
    stamp_frame_age(&mut unstamped, None);
    assert_eq!(unstamped, json!({ "seq": 1 }));
}

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

/// ⚠ **A node that could not be ASKED stamps `None`, which is a THIRD state and not the
/// answered zero** — the `Some(0)` a node that reports no epoch stamps; two of those compare
/// EQUAL, which keeps the refusal off every older node (`PendingPreview`'s `accounts_epoch`).
/// This is the distinction the guard turns on: an unreadable epoch used to be folded into `0`, so
/// a dropped read connection read as an account set that had moved to zero — and the resulting
/// refusal told the agent an account "may have been mounted, unmounted, or relabelled" when the
/// truth was that the link died.
///
/// The assertion is deliberately `ne` against `Some(0)` rather than `eq` against `None`: what
/// must hold is that the two are DISTINGUISHABLE, which is the property a future `unwrap_or`
/// creeping back would destroy while an `eq` on `None` alone still passed.
#[test]
fn a_node_that_could_not_be_asked_stamps_a_state_of_its_own() {
    assert_two_previews_stamp_differently(
        None,
        Some(0),
        "\"could not ask\" and \"the node answered 0\" must not be the same value — folding \
             them is what let a lost link be reported as a changed account set",
    );
}

/// Issue one preview over `first` and one over `second`, and hold that the stamps they carry
/// differ (`why` is the failure).
fn assert_two_previews_stamp_differently(first: Option<u64>, second: Option<u64>, why: &str) {
    let mut store = PendingPreviews::default();
    let a = store.issue(any_intent(), first);
    let b = store.issue(any_intent(), second);
    assert_ne!(
        store.take(&a).expect("issued").accounts_epoch,
        store.take(&b).expect("issued").accounts_epoch,
        "{why}"
    );
}

/// Two previews taken across a CHANGED set carry different stamps — the property the refusal
/// reads. Without this the tests above are satisfied by a field nothing ever varies.
#[test]
fn two_previews_across_a_changed_set_carry_different_stamps() {
    assert_two_previews_stamp_differently(
        Some(11),
        Some(12),
        "a moved account set must be visible to the confirm that compares them",
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
    // With no node to ask, what one unit of this symbol is worth is not known: never a guessed 50.
    assert_eq!(sc["guardrail"]["notional"], Value::Null);
    assert_eq!(sc["guardrail"]["within_limits"], Value::Null);
    assert!(sc["guardrail"]["unpriced"].as_str().unwrap().contains("--node"), "{sc}");
    assert_eq!(sc["tier"], Value::Null);
    assert!(sc["tier_unread"].as_str().unwrap().contains("--node"), "{sc}");
}

/// **`submit_order_list` sends an id on EVERY leg**, minted by the server before the preview exactly as
/// `submit_order`'s is (so the preview shows the ids that go), and the caller's own ids are kept.
#[test]
fn submit_order_list_previews_an_id_on_every_leg() {
    let ladder = json!({ "venue": "sim", "symbol": "BTCUSDT", "side": 1, "qty": 2.0, "shape": "ladder",
                         "stop_loss": 90.0, "take_profit": "110:1,120:1" });
    let resp = call("submit_order_list", ladder);
    assert_eq!(resp["result"]["isError"], false, "{resp}");
    let legs = resp["result"]["structuredContent"]["wire_command"]["OrderList"]["legs"]
        .as_array()
        .expect("the legs")
        .clone();
    assert_eq!(legs.len(), 4);
    let ids: Vec<&str> = legs.iter().filter_map(|leg| leg["order_client_id"].as_str()).collect();
    assert_eq!(ids.len(), 4, "every leg: {legs:?}");
    assert!(ids.iter().all(|id| vike_model::is_valid_crypto_order_client_id(id)), "{ids:?}");

    let named = json!({ "venue": "sim", "symbol": "BTCUSDT", "side": 1, "qty": 2.0, "shape": "oto",
                        "stop_price": 90.0, "order_client_ids": "e7,s7" });
    let resp = call("submit_order_list", named);
    let legs = &resp["result"]["structuredContent"]["wire_command"]["OrderList"]["legs"];
    assert_eq!(
        (legs[0]["order_client_id"].as_str(), legs[1]["order_client_id"].as_str()),
        (Some("e7"), Some("s7"))
    );
}

/// **`submit_combo` previews the combo it sends, with no id** (the core mints the one) and its net written even
/// when it names none, and the guardrail never reads it as within limits: its legs are sized by the node.
#[test]
fn submit_combo_previews_its_legs_and_its_one_id() {
    let spread = json!({ "venue": "sim", "side": 1, "qty": 2.0,
                         "legs": [{ "symbol": "C1", "ratio": 1 }, { "symbol": "C2", "ratio": -1 }] });
    let resp = call("submit_combo", spread.clone());
    assert_eq!(resp["result"]["isError"], false, "{resp}");
    let sc = &resp["result"]["structuredContent"];
    assert_eq!(sc["will_execute"], false);
    let combo = &sc["wire_command"]["Combo"];
    assert_eq!(combo["legs"][1]["ratio"], -1, "{sc}");
    let minted = combo["order_client_id"].as_str().expect("the server mints the combo's one id");
    assert!(vike_model::is_valid_crypto_order_client_id(minted), "{minted}");
    let mut named = spread;
    named["order_client_id"] = json!("myCombo1");
    let resp = call("submit_combo", named);
    let id = &resp["result"]["structuredContent"]["wire_command"]["Combo"]["order_client_id"];
    assert_eq!(id, "myCombo1", "a named id is the one shown and sent: {resp}");
    assert_eq!(combo["net_limit"], Value::Null, "a market combo writes its net as null: {sc}");
    assert_eq!(sc["guardrail"]["within_limits"], Value::Null, "{sc}");
}

/// **What this server mints opens `M`, and `decode_order_id` reads it back, and a strategy's `S` id**, with no node:
/// the source, the codes and the intent. The names (the account's label, the strategy's) are the node's, so with
/// none they are null; an id this platform never minted is an answer, a missing one a tool error.
#[test]
fn this_servers_ids_open_m_and_decode_order_id_reads_them_back() {
    use vike_model::orders::client_id::{
        OrderClientIdAccountCode, OrderClientIdGenerator, OrderClientIdIntent, OrderClientIdLabels,
        OrderClientIdPlan, OrderClientIdShape, OrderClientIdSource, OrderClientIdStrategyCode,
    };
    let decode = |id: &str| {
        let resp = call("decode_order_id", json!({ "order_client_id": id }));
        assert_eq!(resp["result"]["isError"], false, "{resp}");
        resp["result"]["structuredContent"].clone()
    };

    let resp =
        call("submit_order", json!({ "venue": "sim", "symbol": "BTCUSDT", "side": 1, "qty": 0.5 }));
    let minted = resp["result"]["structuredContent"]["wire_command"]["Submit"]["order_client_id"]
        .as_str()
        .expect("the server mints the id")
        .to_string();
    assert!(minted.starts_with("M0000"), "this server's letter, no strategy: {minted}");
    let mine = decode(&minted);
    assert_eq!((&mine["form"], &mine["source"]), (&json!("v2"), &json!("M")), "{mine}");
    assert_eq!(mine["intent"], "OL", "a buy from flat opens a long: {mine}");

    let strategy = OrderClientIdStrategyCode::new(42).expect("a strategy code");
    let id = OrderClientIdPlan::new(
        OrderClientIdShape::FULL,
        OrderClientIdAccountCode::parse("0B").expect("an account code"),
    )
    .mint(
        &mut OrderClientIdGenerator::new_v2(None),
        OrderClientIdLabels::new(
            OrderClientIdSource::Strategy,
            strategy,
            OrderClientIdIntent::of(-1, true, false, 1.0),
        ),
    );
    let theirs = decode(&id);
    assert_eq!((&theirs["form"], &theirs["source"]), (&json!("v2"), &json!("S")), "{theirs}");
    assert_eq!(theirs["strategy"], json!(strategy.to_string()));
    assert_eq!((&theirs["account"], &theirs["account_number"]), (&json!("0B"), &json!(11)));
    assert_eq!(theirs["intent"], "CL", "a reduce-only sell of a long closes it: {theirs}");
    assert_eq!((&theirs["strategy_name"], &theirs["account_label"]), (&Value::Null, &Value::Null));

    assert_eq!(decode("EXT-1")["form"], "other");
    let missing = call("decode_order_id", json!({}));
    assert_eq!(missing["result"]["isError"], true, "{missing}");
}

/// A frame with one engine per case: `route_key`, its tier, the symbols it says it trades.
fn frame_of(
    blocks: &[(&str, Option<&str>, &[&str])],
) -> vike_tradehub_client::wire::CoreSnapshotWire {
    let mut frame = vike_tradehub_client::wire::CoreSnapshotWire::empty();
    frame.sequence = 1;
    frame.venues = blocks
        .iter()
        .map(|(route_key, tier, symbols)| {
            let (venue, account) = route_key.split_once('#').unzip();
            serde_json::from_value(json!({
                "venue": venue.unwrap_or(route_key), "account": account, "route_key": route_key,
                "tier": tier, "symbols": symbols, "balance": 0.0, "realized_pnl": 0.0,
                "fees_paid": 0.0, "funding_paid": 0.0, "equity": 0.0, "unrealized": 0.0,
                "missing_prices": 0, "margin_used": 0.0, "free_buying_power": 0.0,
                "trading_state": "Active", "positions": [],
            }))
            .expect("a block")
        })
        .collect();
    frame
}

/// `preview_of` for `tool`, over `frame`, with nothing else of the gate in the way.
fn previewed(
    tool: &str,
    args: Value,
    frame: Result<&vike_tradehub_client::wire::CoreSnapshotWire, &str>,
) -> Value {
    let cmd = tool_verbs::wire_command_for(tool, &args).unwrap();
    preview_of(
        tool,
        &cmd,
        None,
        verbs::GuardrailCapabilities { max_qty: None, max_notional: Some(100_000.0) },
        Some("pv-1"),
        None,
        VENUE_CHECK_MOUNTED,
        None,
        frame,
    )
}

/// **The preview names the tier of the account the order would reach, in the node's word.** Hyperliquid
/// and bybit run both tiers under one venue id, so this is the one thing an agent confirming an order
/// could not see, and it may not come from the arguments: the account is named `LIVE` on one block and
/// `DEMO` on another here precisely so a label read as a tier would pass neither row below.
#[test]
fn a_preview_names_the_tier_of_the_account_the_order_reaches() {
    let frame = frame_of(&[
        ("hyperliquid", Some("demo"), &["BTC"]),
        ("hyperliquid#LIVE", Some("demo"), &["BTC"]),
        ("hyperliquid#DEMO", Some("live"), &["BTC"]),
    ]);
    let args = |account: &str| json!({ "venue": "hyperliquid", "account": account, "symbol": "BTC", "side": 1, "qty": 1.0 });
    assert_eq!(previewed("submit_order", args("LIVE"), Ok(&frame))["tier"], "demo");
    assert_eq!(previewed("submit_order", args("DEMO"), Ok(&frame))["tier"], "live");
    let named = previewed("submit_order", args("DEFAULT"), Ok(&frame));
    assert_eq!(named["tier"], "demo");
    assert!(named.get("tier_unread").is_none(), "only an unknown tier says why: {named}");
}

/// A node that does not name the tier, and one that could not be asked: `tier` is null and says why,
/// and the words tell an agent to treat it as live. An absent tier never reads as a demo one.
#[test]
fn a_tier_the_node_does_not_name_reads_as_unknown_and_is_treated_as_live() {
    let args = json!({ "venue": "okx", "symbol": "BTC-USDT-SWAP", "side": 1, "qty": 1.0 });
    let silent = frame_of(&[("okx", None, &["BTC-USDT-SWAP"])]);
    for (frame, why) in
        [(Ok(&silent), "does not name"), (Err("the node is down"), "the node is down")]
    {
        let pv = previewed("submit_order", args.clone(), frame);
        assert_eq!(pv["tier"], Value::Null, "{pv}");
        let unread = pv["tier_unread"].as_str().unwrap();
        assert!(unread.contains(why) && unread.contains("as live"), "{unread}");
    }
}

/// A preview that is no write on ONE account carries no tier: there is none to state.
#[test]
fn a_preview_that_names_no_one_account_carries_no_tier() {
    let frame = frame_of(&[("okx", Some("demo"), &["BTC-USDT-SWAP"])]);
    for (tool, args) in [
        ("set_trading_state", json!({ "state": "halted" })),
        ("market_exit", json!({})),
        ("mass_cancel", json!({})),
        ("unmount_strategy", json!({ "controller_id": "m" })),
    ] {
        let pv = previewed(tool, args, Ok(&frame));
        assert!(pv.get("tier").is_none() && pv.get("tier_unread").is_none(), "{tool}: {pv}");
    }
    let venue_wide = previewed("market_exit", json!({ "venue": "okx" }), Ok(&frame));
    assert_eq!(venue_wide["tier"], "demo", "one venue, one account: {venue_wide}");
}

/// The measured case end to end through the payload: a true $10 inverse limit no longer reads 381,235.
#[test]
fn a_preview_values_an_inverse_contract_by_the_nodes_row() {
    let mut frame = frame_of(&[("deribit", Some("demo"), &["BTC-PERPETUAL"])]);
    frame.venues[0].contract_values = vec![vike_tradehub_client::wire::ContractValueWire {
        symbol: "BTC-PERPETUAL".into(),
        kind: "inverse".into(),
        per_unit: 1.0,
    }];
    let limit = |qty: f64, price: f64| json!({ "venue": "deribit", "symbol": "BTC-PERPETUAL", "side": 1, "qty": qty, "order_type": "limit", "price": price });
    let pv = previewed("submit_order", limit(10.0, 38_123.5), Ok(&frame));
    assert_eq!(pv["guardrail"]["notional"], 10.0, "{pv}");
    assert_eq!(pv["guardrail"]["within_limits"], true, "{pv}");
    // The dangerous direction: `qty x price` read this one as 20,000 and within the ceiling.
    let pv = previewed("submit_order", limit(200_000.0, 0.1), Ok(&frame));
    assert_eq!(pv["guardrail"]["notional"], 200_000.0, "{pv}");
    assert_eq!(pv["guardrail"]["within_limits"], false, "{pv}");
}

/// Every WRITE tool advertises the optional `reason` (node proto v4) — and never requires it, so
/// an agent that ignores it keeps working exactly as before.
#[test]
fn every_write_tool_advertises_an_optional_reason() {
    let resp = server().handle(&req(2, "tools/list", json!({}))).unwrap();
    let tools = resp["result"]["tools"].as_array().unwrap();
    for name in WRITE_TOOLS {
        let tool = tools.iter().find(|t| t["name"] == name).expect("advertised");
        let properties = &tool["inputSchema"]["properties"];
        assert_eq!(properties["reason"]["type"], "string", "{name} must offer a `reason` string");
        assert!(
            properties["reason"]["description"].as_str().unwrap().contains("audit"),
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
    let bare = call("cancel_order", json!({ "order_client_id": "c-1" }));
    assert_eq!(bare["result"]["structuredContent"]["reason"], Value::Null);

    let withr = call(
        "cancel_order",
        json!({ "order_client_id": "c-1", "reason": "  stale quote after the feed gap  " }),
    );
    let sc = &withr["result"]["structuredContent"];
    assert_eq!(sc["will_execute"], false);
    assert_eq!(sc["reason"], "stale quote after the feed gap", "trimmed, and echoed back");
    assert_eq!(
        sc["wire_command"], bare["result"]["structuredContent"]["wire_command"],
        "the wire command is byte-identical with and without a reason"
    );
    // A blank rationale is no rationale (never an empty string in the preview).
    let blank = call("cancel_order", json!({ "order_client_id": "c-1", "reason": "   " }));
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
    let cmd = tool_verbs::wire_command_for(
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
        verbs::GuardrailCapabilities::default(),
        Some("pv-1"),
        Some(unreachable),
        VENUE_CHECK_UNVERIFIED,
        // Not a settings write, so no `change` rides the preview.
        None,
        Err("no frame"),
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
        verbs::GuardrailCapabilities::default(),
        Some("pv-2"),
        Some(answered),
        VENUE_CHECK_MOUNTED,
        None,
        Err("no frame"),
    );
    assert_eq!(ok["verified_by_node"], true, "an answered dry-run IS a verdict: {ok}");
}

/// What the node says the order will go out as is returned in its verdict; a verdict with nothing to say keeps
/// the shape it always had, so a golden of it did not move.
#[test]
fn the_nodes_notes_are_returned_in_its_verdict() {
    let note = "qty 15 goes out as 20, the venue's step is 10".to_string();
    let noted = crate::node_writes::node_verdict(true, None, std::slice::from_ref(&note));
    assert_eq!(noted["notes"], json!([note]));
    assert_eq!(noted["checked_by"], CHECKED_BY_NODE);
    let plain = crate::node_writes::node_verdict(true, None, &[]);
    assert_eq!(plain, json!({ "checked_by": CHECKED_BY_NODE, "accepted": true, "reason": null }));
}

#[test]
fn a_preview_whose_node_could_not_be_asked_is_not_verified() {
    // A node IS configured and a control key IS present, but the dry-run cannot reach it
    // (`127.0.0.1:1` refuses immediately). `node_preview` answers with a `checked_by: "none"`
    // object rather than `None`, and `preview_of` used to label THAT `verified_by_node: true`
    // with the "it is the verdict that counts" note — a dropped tunnel mid-incident read as
    // the node's own approval. A fault is not a verdict.
    let stored: std::collections::HashMap<String, String> =
        [(hubkeys::CONTROL_KEY_ENV.to_string(), "ctl-key".to_string())].into_iter().collect();
    // Over `test_server()` rather than a second field list — the same argument the `server()`
    // helper's own doc makes: a hand-written list here is a second place to forget a field, and
    // the two would then disagree about what "a fresh server" is.
    let mut s = Server {
        node_addr: Some("127.0.0.1:1".to_string()),
        keys: hubkeys::resolve(&stored, None),
        ..test_server()
    };
    let resp = s
        .handle(&req(
            1,
            "tools/call",
            json!({ "name": "cancel_order", "arguments": { "order_client_id": "c-1" } }),
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
    // `fill_order_client_id` mints a fresh id on every call — so a preview and its confirm can
    // never carry the same one, and submit_order was unusable through this surface. CI caught
    // it; this keeps it caught.
    let base = tool_verbs::wire_command_for(
        "submit_order",
        &json!({ "venue": "sim", "symbol": "BTCUSDT", "side": 1, "qty": 1.0 }),
    )
    .unwrap();
    let mut a = base.clone();
    let mut b = base.clone();
    if let (CommandWire::Submit(x), CommandWire::Submit(y)) = (&mut a, &mut b) {
        x.order_client_id = "c-1".into();
        y.order_client_id = "c-2".into();
    }
    let node = |c: &CommandWire| PreviewIntent::Node(Box::new(c.clone()));
    assert!(same_intent(&node(&a), &node(&b)), "a differing minted id must NOT break the binding");

    // …but every other field still binds.
    let other = tool_verbs::wire_command_for(
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

/// **A token is bound to the ACCOUNT it previewed**, because the account rides inside the command
/// the binding compares: a stop previewed for one account of a two-account venue cannot be
/// confirmed onto the other, and the same account confirms (it gets past the gate to the node step,
/// which fails at the connection on this node-less server).
#[test]
fn a_preview_token_is_bound_to_the_account_it_previewed() {
    let mut s = server();
    let order = |account: &str| {
        json!({
            "venue": "hyperliquid", "symbol": "ETH", "side": -1, "qty": 0.01,
            "order_type": "stop", "trigger_price": 1800.0, "account": account,
        })
    };
    let confirming = |mut args: Value, pv: &Value| {
        args["confirm"] = json!(true);
        args["preview_token"] = pv["preview_token"].clone();
        args
    };

    let pv = s.call_tool("submit_order", &order("DEFAULT")).unwrap();
    assert_eq!(pv["wire_command"]["Submit"]["account"], "DEFAULT");
    assert_eq!(pv["wire_command"]["Submit"]["trigger_price"], 1800.0);
    let err = s.call_tool("submit_order", &confirming(order("LIVE"), &pv)).unwrap_err();
    assert!(err.refused, "a token for another account is a GATE refusal: {err:?}");
    assert!(err.message.contains("does not match this command"), "{err:?}");

    let pv = s.call_tool("submit_order", &order("LIVE")).unwrap();
    let err = s.call_tool("submit_order", &confirming(order("LIVE"), &pv)).unwrap_err();
    assert!(!err.refused, "the same account gets past the gate to the node step: {err:?}");
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
    // The verbs-module construction site: the SAME CommandWire::Modify the trade REPL's
    // `modify c-1 --qty 2` builds, preview-gated like every other write tool.
    let resp = call("modify", json!({ "order_client_id": "c-1", "new_qty": 2.0 }));
    assert_eq!(resp["result"]["isError"], false, "a preview succeeds even with no node");
    let sc = &resp["result"]["structuredContent"];
    assert_eq!(sc["will_execute"], false);
    assert_eq!(sc["wire_command"]["Modify"]["order_client_id"], "c-1");
    assert_eq!(sc["wire_command"]["Modify"]["new_qty"], 2.0);
    assert_eq!(sc["wire_command"]["Modify"]["new_price"], Value::Null);
    assert_eq!(sc["guardrail"]["qty"], 2.0, "a new qty IS a size");
    assert_eq!(sc["guardrail"]["within_limits"], Value::Null, "not checked with no frame to read");
}

/// **A server with no settings directory has no store to name.** Launched outside the project folder,
/// `strategy_status` answered that the observe key "is not in the hub-key store" while the real reason
/// went to stderr, which an agent never sees.
#[test]
fn a_missing_key_names_the_missing_settings_directory_when_that_is_the_reason() {
    let at = |keys| Server { node_addr: Some("127.0.0.1:1".to_string()), keys, ..test_server() };
    let empty = std::collections::HashMap::new();

    let nowhere = at(hubkeys::resolve(&empty, None)).tool_strategy_status().unwrap_err();
    assert!(nowhere.contains("resolved no settings directory"), "{nowhere}");
    assert!(nowhere.contains("project folder"), "{nowhere}");
    assert!(!nowhere.contains("is not in the hub-key store"), "{nowhere}");

    let store = std::path::Path::new("/nowhere/settings/db/vike.db");
    let absent = at(hubkeys::resolve(&empty, Some(store))).tool_strategy_status().unwrap_err();
    assert!(absent.contains("is not in the hub-key store"), "a store that lacks it: {absent}");
}

#[test]
fn modify_with_nothing_to_change_is_a_tool_error() {
    let resp = call("modify", json!({ "order_client_id": "c-1" }));
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
