//! The data deleter (`delete_series`): every guard that made permitting it defensible.

use std::sync::{Arc, Mutex};

use super::*;
use vike_model::StoreKindId;

/// **THE DATA DELETER KEEPS EVERY GUARD.** A POSITIVE gate, and it replaces an EXCLUSION.
///
/// The design that proposed `delete_series` opened by forbidding it — a model must not delete
/// data — and its gate would have been an ABSENCE check over `tools_spec` (no advertised tool
/// may contain `delete`/`remove`/`purge`/…). The owner reversed that on 2026-09-07: a user must
/// be able to delete through both the CLI and this surface. **An absence gate cannot be adapted
/// to a present capability**, so the shape is inverted: the tool exists, and this holds every
/// guard that made permitting it defensible.
///
/// §9.1's grant argument survives the reversal and is what sets the bar: an irreversible delete
/// of market history is a larger grant than an order, and in one way larger than a credential
/// write — a credential can be reissued at the venue, and a deleted tape whose venue no longer
/// serves that window cannot be re-fetched at all. Five evidences, because losing any ONE of
/// them is a different failure:
///
/// 1. **`delete_series` is in [`WRITE_TOOLS`].** Removing it would silently drop the mandatory
///    preview, the `destructiveHint`, the `read-only` withholding and the transcript's write
///    classification IN ONE EDIT. That is the failure this evidence exists for, and it is why
///    the roster membership is asserted rather than inferred from the annotations.
/// 2. **A call without `confirm: true` mutates NOTHING** — asserted by driving the real
///    [`Server`] against a real datahub over a RECORDING store, and checking the store saw no
///    delete. NOT by inspecting the returned text: a preview that SAYS it changed nothing while
///    having changed something is exactly what a text assertion cannot catch.
/// 3. **A token bound to a DIFFERENT deletion is refused** — the binding exercised rather than
///    assumed, and over `produced_by` as well as the selector, because the assertion decides
///    what actually goes.
/// 4. **`produced_by` is required by the schema**, and a call omitting it is refused BEFORE any
///    store is opened.
/// 5. **It is absent from `tools/list` under both `read-only` and `offline`**, asserted against
///    the real [`ToolAccess`] rings rather than against a hand-written expectation.
#[test]
fn the_data_deleter_keeps_every_guard() {
    // ---- 1. the roster membership, which is the four other guards in one line ---------------
    assert!(is_write_tool("delete_series"), "delete_series must route as a write");
    assert!(WRITE_TOOLS.contains(&"delete_series"), "…and be ON the roster that says so");
    let spec = tools_spec();
    let tool = spec
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "delete_series")
        .expect("delete_series is advertised");
    assert_eq!(tool["annotations"]["destructiveHint"], json!(true));
    assert_eq!(tool["annotations"]["readOnlyHint"], json!(false));
    assert_eq!(
        tool["annotations"]["idempotentHint"],
        json!(true),
        "the underlying delete IS idempotent, and saying so is honest: a retry after a dropped \
             reply does not double-delete"
    );
    assert_eq!(
        tool["annotations"]["openWorldHint"],
        json!(true),
        "it reaches a datahub over a socket — which is also what withholds it from `offline`"
    );

    // ---- 4. the schema requires the assertion, and the router refuses without it ------------
    let required: Vec<&str> = tool["inputSchema"]["required"]
        .as_array()
        .expect("delete_series declares required properties")
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(
        required.contains(&"produced_by"),
        "produced_by must be REQUIRED by the schema, unconditionally: {required:?}"
    );
    let resp = call("delete_series", json!({ "kind": "bar", "venue": "binance" }));
    assert_eq!(resp["result"]["isError"], true, "a call with no produced_by must be refused");
    let text = resp["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("REQUIRED"), "the refusal must say so: {text}");
    assert!(
        resp["result"]["structuredContent"].is_null(),
        "a refusal is not a preview and must mint no token: {resp}"
    );

    // ---- 5. the rings withhold it, off the REAL ToolAccess ---------------------------------
    for profile in [Profile::ReadOnly, Profile::Offline] {
        let access = ToolAccess::new(profile, Vec::new());
        assert!(!access.admits("delete_series"), "{profile:?} must withhold the deleter",);
        assert!(
            !advertised_names(&access).iter().any(|n| n == "delete_series"),
            "{profile:?} must not ADVERTISE it either"
        );
    }
}

/// A datahub that ADVERTISES the delete verb and RECORDS every `dry_run` flag it is sent,
/// answering each with a fixed one-series plan.
///
/// ⚠ It speaks the wire rather than wrapping a store, and that is what makes the proof
/// possible at all: the property under test is "a preview SENDS a dry run", which is a fact
/// about the REQUEST — and the store this server would delete from lives in another process on
/// another box, so "the series is still on disk" is not a thing this crate can look at. A
/// `HistStore` double would have proved the same thing one layer further away, through eighteen
/// delegating methods.
///
/// Key-LESS on purpose (no `FEATURE_AUTH`, no nonce), so the plain `DatahubClient::connect`
/// reaches it — this fixture's `Server` is built with `datahub_keys: None`, which is the arm
/// that fallback belongs to.
fn spawn_recording_datahub() -> (std::net::SocketAddr, Arc<Mutex<Vec<bool>>>) {
    spawn_datahub(RealPass::Done)
}

/// What the fake answers a request that is NOT a dry run.
#[derive(Clone, Copy)]
enum RealPass {
    Done,
    /// A refusal: the verb says this one deleted nothing (`vike_datahub`'s `delete_series_verb`).
    Refused,
    /// It reads the request and closes: the half-open link's case, rows gone and no reply.
    NoReply,
}

fn spawn_datahub(real_pass: RealPass) -> (std::net::SocketAddr, Arc<Mutex<Vec<bool>>>) {
    spawn_datahub_with(real_pass, usize::MAX)
}

/// [`spawn_datahub`], whose plan turns FOREIGN from its `foreign_from`-th dry run on (counted from 0): one series
/// carrying a commit key the asserted prefix does not match, so the plan's provenance verdict refuses.
fn spawn_datahub_with(
    real_pass: RealPass,
    foreign_from: usize,
) -> (std::net::SocketAddr, Arc<Mutex<Vec<bool>>>) {
    use std::net::TcpListener;
    use vike_data::store::removal::{
        RemovalOutcome, RemovalPlan, RemovalPlanSeries, SeriesSelector,
    };
    use vike_data::store::series::{SeriesCoverage, SeriesId};
    use vike_datahub_client::PROTO_VERSION;
    use vike_datahub_client::proto::{DeleteDone, Request, Response};
    use vike_hub_protocol::frame::{read_frame, write_frame};
    use vike_hub_protocol::handshake::{HandshakeFrame, HandshakeReply};

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let seen: Arc<Mutex<Vec<bool>>> = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&seen);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut s) = stream else { continue };
            let recorded = Arc::clone(&recorded);
            std::thread::spawn(move || {
                let Ok(HandshakeFrame::Hello { .. }) = read_frame::<_, HandshakeFrame>(&mut s)
                else {
                    return;
                };
                let welcome = HandshakeReply::Welcome {
                    proto_version: PROTO_VERSION,
                    features: vec!["delete_series".to_string()],
                    nonce: None,
                };
                if write_frame(&mut s, &welcome).is_err() {
                    return;
                }
                while let Ok(request) = read_frame::<_, Request>(&mut s) {
                    let response = match request {
                        Request::DeleteSeries { dry_run, .. } => {
                            let mut seen = recorded.lock().unwrap();
                            let foreign =
                                seen.iter().filter(|dry| **dry).count() >= foreign_from && dry_run;
                            seen.push(dry_run);
                            drop(seen);
                            let series = if foreign {
                                vec![RemovalPlanSeries::new(
                                    SeriesId::per_symbol(
                                        StoreKindId::Bar,
                                        "binance",
                                        "BTCUSDT",
                                        Some("1h".to_string()),
                                    ),
                                    SeriesCoverage::default(),
                                    vec!["panel_bars:binance:BTCUSDT:1h:0-1".to_string()],
                                    Some("klines:"),
                                )]
                            } else {
                                Vec::new()
                            };
                            let plan = RemovalPlan {
                                selector: SeriesSelector::new(StoreKindId::Bar, "binance"),
                                produced_by: Some("klines:".to_string()),
                                series,
                            };
                            match (dry_run, real_pass) {
                                (false, RealPass::NoReply) => break,
                                (false, RealPass::Refused) => {
                                    Response::Error("provenance REFUSED: a foreign key".to_string())
                                }
                                _ => Response::Deleted(DeleteDone {
                                    plan,
                                    outcome: (!dry_run).then(RemovalOutcome::default),
                                }),
                            }
                        }
                        other => Response::Error(format!("unexpected {other:?}")),
                    };
                    if write_frame(&mut s, &response).is_err() {
                        break;
                    }
                }
            });
        }
    });
    (addr, seen)
}

/// **A confirmed delete whose reply was lost may have deleted**, so it is never reported as a failure
/// to delete: rows may be gone and a plan taken before it describes a store that no longer exists. A
/// refusal from the datahub is the other case: that one deleted nothing, and says the datahub's words.
#[test]
fn a_confirmed_delete_whose_reply_is_lost_is_unknown_and_a_refusal_is_not() {
    let args = json!({
        "kind": "bar", "venue": "binance", "symbol": "BTCUSDT", "interval": "1h",
        "produced_by": "klines:"
    });
    let answer_to_the_confirm = |real_pass| {
        let (addr, sent) = spawn_datahub(real_pass);
        let mut s = Server { datahub_addr: addr.to_string(), ..test_server() };
        let params = json!({ "name": "delete_series", "arguments": args });
        let plan = s.handle(&req(1, "tools/call", params)).unwrap();
        let token = plan["result"]["structuredContent"]["preview_token"].as_str().unwrap();
        let mut confirming = args.clone();
        confirming["confirm"] = json!(true);
        confirming["preview_token"] = json!(token);
        let params = json!({ "name": "delete_series", "arguments": confirming });
        let resp = s.handle(&req(2, "tools/call", params)).unwrap();
        assert_eq!(resp["result"]["isError"], true, "{resp}");
        assert_eq!(*sent.lock().unwrap(), [true, true, false], "the real pass went out");
        resp["result"]["content"][0]["text"].as_str().unwrap().to_string()
    };

    let lost = answer_to_the_confirm(RealPass::NoReply);
    assert!(lost.contains("UNKNOWN") && lost.contains("may be gone"), "{lost}");
    assert!(lost.contains("fresh plan"), "the next step is a new plan: {lost}");

    let refused = answer_to_the_confirm(RealPass::Refused);
    assert!(refused.contains("provenance REFUSED"), "{refused}");
    assert!(!refused.contains("UNKNOWN"), "a refusal deleted nothing: {refused}");
}

/// **A plan whose provenance the store refused gets NO token** (D1 of 2026-11-24: the CLI refuses before its confirm
/// too), and a token taken over a clean plan is refused at the confirm when the store's own plan has turned foreign
/// since: the delete pass never goes out.
#[test]
fn a_refused_provenance_plan_issues_no_token_and_the_confirm_asks_again() {
    let args = json!({
        "kind": "bar", "venue": "binance", "symbol": "BTCUSDT", "interval": "1h",
        "produced_by": "klines:"
    });
    let call_on = |s: &mut Server, id: i64, arguments: &Value| {
        let params = json!({ "name": "delete_series", "arguments": arguments });
        s.handle(&req(id, "tools/call", params)).unwrap()
    };
    // Every plan refuses: a preview, and no token to confirm it with.
    let (addr, sent) = spawn_datahub_with(RealPass::Done, 0);
    let mut s = Server { datahub_addr: addr.to_string(), ..test_server() };
    let resp = call_on(&mut s, 1, &args);
    assert_eq!(resp["result"]["isError"], false, "{resp}");
    let body = &resp["result"]["structuredContent"];
    assert_eq!(body["preview_token"], Value::Null, "{body}");
    assert_eq!(body["provenance_satisfied"], false, "{body}");
    assert!(body["refused"].as_array().is_some_and(|lines| lines.len() == 1), "{body}");
    let note = body["note"].as_str().unwrap();
    assert!(
        note.starts_with("error [data.delete.provenance_refused]: provenance REFUSED: 1 series"),
        "{note}"
    );
    assert!(note.ends_with("and plan again."), "{note}");
    assert_eq!(*sent.lock().unwrap(), [true]);

    // The first plan passes, and the store turns foreign before the confirm.
    let (addr, sent) = spawn_datahub_with(RealPass::Done, 1);
    let mut s = Server { datahub_addr: addr.to_string(), ..test_server() };
    let plan = call_on(&mut s, 1, &args);
    let token = plan["result"]["structuredContent"]["preview_token"]
        .as_str()
        .expect("a clean plan's token");
    let mut confirming = args.clone();
    confirming["confirm"] = json!(true);
    confirming["preview_token"] = json!(token);
    let resp = call_on(&mut s, 2, &confirming);
    assert_eq!(resp["result"]["isError"], true, "{resp}");
    let text = resp["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.starts_with("error [data.delete.provenance_refused]: "), "{text}");
    assert_eq!(*sent.lock().unwrap(), [true, true], "the delete pass never went out");
}

/// **Evidences 2 and 3 of [`the_data_deleter_keeps_every_guard`]**, driven end to end against a
/// real server and a real wire.
///
/// 2. a call WITHOUT `confirm` sends `dry_run: true` and NOTHING else — the mutation gate, and
///    it is asserted on what left the process rather than on what the answer says about itself;
/// 3. a token bound to a DIFFERENT deletion is refused — including one that differs only in
///    `produced_by`, which is the argument that decides what actually goes.
#[test]
fn a_delete_preview_sends_only_a_dry_run_and_its_token_is_bound_to_it() {
    let (addr, sent) = spawn_recording_datahub();
    let mut s = Server { datahub_addr: addr.to_string(), ..test_server() };
    let args = json!({
        "kind": "bar", "venue": "binance", "symbol": "BTCUSDT", "interval": "1h",
        "produced_by": "klines:"
    });

    // ---- 2. the PREVIEW ---------------------------------------------------------------------
    let resp = s
        .handle(&req(1, "tools/call", json!({ "name": "delete_series", "arguments": args })))
        .unwrap();
    let body = &resp["result"]["structuredContent"];
    assert_eq!(body["will_execute"], json!(false), "{resp}");
    let token = body["preview_token"].as_str().expect("a plan mints a token").to_string();
    assert_eq!(
        *sent.lock().unwrap(),
        vec![true],
        "a preview must send EXACTLY ONE request, and it must be a dry run"
    );

    // ---- 3a. the SAME token against a DIFFERENT SELECTOR ------------------------------------
    let mut other = args.clone();
    other["symbol"] = json!("ETHUSDT");
    other["confirm"] = json!(true);
    other["preview_token"] = json!(token);
    let resp = s
        .handle(&req(2, "tools/call", json!({ "name": "delete_series", "arguments": other })))
        .unwrap();
    assert_eq!(resp["result"]["isError"], true, "{resp}");
    let text = resp["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("does not match this deletion"), "{text}");
    assert_eq!(
        *sent.lock().unwrap(),
        vec![true],
        "a refused confirm must send NOTHING — the token is consumed before the socket"
    );

    // ⚠ The token is spent by that attempt (single use), so the next case takes a fresh one.
    let resp = s
        .handle(&req(3, "tools/call", json!({ "name": "delete_series", "arguments": args })))
        .unwrap();
    let token = resp["result"]["structuredContent"]["preview_token"].as_str().unwrap().to_string();

    // ---- 3b. the same SELECTOR under a DIFFERENT ASSERTION ----------------------------------
    let mut reasserted = args.clone();
    reasserted["produced_by"] = json!("pmxt:");
    reasserted["confirm"] = json!(true);
    reasserted["preview_token"] = json!(token.clone());
    let resp = s.handle(&req(
        4,
        "tools/call",
        json!({ "name": "delete_series", "arguments": reasserted }),
    ));
    let resp = resp.unwrap();
    assert_eq!(
        resp["result"]["isError"], true,
        "`produced_by` is part of the INTENT: a token previewed under one assertion must not \
             confirm a delete under another: {resp}"
    );

    // ---- ...and the CONFIRMING call, which is the non-vacuity proof for all of the above ----
    let resp = s
        .handle(&req(5, "tools/call", json!({ "name": "delete_series", "arguments": args })))
        .unwrap();
    let token = resp["result"]["structuredContent"]["preview_token"].as_str().unwrap().to_string();
    let mut confirming = args.clone();
    confirming["confirm"] = json!(true);
    confirming["preview_token"] = json!(token);
    let resp = s.handle(&req(
        6,
        "tools/call",
        json!({ "name": "delete_series", "arguments": confirming }),
    ));
    let resp = resp.unwrap();
    assert_eq!(resp["result"]["isError"], false, "{resp}");
    assert_eq!(resp["result"]["structuredContent"]["will_execute"], json!(true));
    let flags = sent.lock().unwrap().clone();
    assert_eq!(
        flags.last(),
        Some(&false),
        "only a CONFIRMED call may send a non-dry-run request: {flags:?}"
    );
    assert_eq!(
        flags.iter().filter(|d| !**d).count(),
        1,
        "…and exactly one of them did, out of {} requests: {flags:?}",
        flags.len()
    );
}
