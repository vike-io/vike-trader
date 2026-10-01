use super::*;

/// A synthetic frame shaped like a real venue envelope: nesting, arrays, mixed leaf types,
/// and sensitive leaves in three casings/types.
fn raw() -> Value {
    json!({
        "e": "executionReport",
        "listenKey": "pqia91ma19a5s61cv6a81va65sdf19v8a65a1a5s61cv6a81va65sdf19v8a65a1",
        "event": {
            "apiKey": "AbC123",
            "user_id": 84789,
            "o": {
                "s": "BTCUSDT",
                "c": "vtr-coid-1",
                "i": 4200421,
                "L": "50123.45000000",
                "l": "0.00012000",
                "m": false,
                "n": null,
                "fills": [ {"t": 998877, "TOKEN": "sess-abc"} ]
            }
        }
    })
}

#[test]
fn structure_is_preserved_verbatim_apart_from_sensitive_leaves() {
    let out = capture_frame("binance", "ws_fill", &raw()).frame;
    // Non-sensitive leaves: names, nesting, casing, TYPES all verbatim.
    assert_eq!(out["e"], json!("executionReport"));
    assert_eq!(out["event"]["o"]["s"], json!("BTCUSDT"));
    assert_eq!(out["event"]["o"]["c"], json!("vtr-coid-1"));
    assert_eq!(out["event"]["o"]["i"], json!(4200421), "venue order id KEPT (number)");
    assert_eq!(out["event"]["o"]["L"], json!("50123.45000000"), "string-typed px KEPT");
    assert_eq!(out["event"]["o"]["m"], json!(false));
    assert_eq!(out["event"]["o"]["n"], Value::Null);
    assert_eq!(out["event"]["o"]["fills"][0]["t"], json!(998877), "trade id KEPT");
}

#[test]
fn sensitive_leaves_redact_to_stable_type_preserving_placeholders() {
    let sf = capture_frame("binance", "ws_fill", &raw());
    let out = &sf.frame;
    assert_eq!(out["listenKey"], json!("[redacted:listenkey]"));
    assert_eq!(out["event"]["apiKey"], json!("[redacted:apikey]"), "case-insensitive key");
    assert_eq!(out["event"]["user_id"], json!(0), "numeric id stays a NUMBER");
    assert_eq!(out["event"]["o"]["fills"][0]["TOKEN"], json!("[redacted:token]"));
    let mut paths = sf.redacted_paths.clone();
    paths.sort();
    assert_eq!(
        paths,
        vec!["/event/apiKey", "/event/o/fills/0/TOKEN", "/event/user_id", "/listenKey",]
    );
}

/// The stability law: two captures differing ONLY in secret values sanitize identically —
/// a re-capture diff shows wire drift, never secret churn.
#[test]
fn output_is_stable_across_rotated_secrets() {
    let mut b = raw();
    b["listenKey"] = json!("ROTATED-KEY-xyz");
    b["event"]["apiKey"] = json!("other");
    b["event"]["user_id"] = json!(123456789);
    b["event"]["o"]["fills"][0]["TOKEN"] = json!("sess-def");
    let a = capture_frame("binance", "ws_fill", &raw()).frame;
    let b = capture_frame("binance", "ws_fill", &b).frame;
    assert_eq!(a, b);
}

/// Substring keys must NOT trip: `positionIdx` contains `id`, `signQty` contains `sign`
/// only as a prefix — exact-key match only.
#[test]
fn non_sensitive_lookalike_keys_are_untouched() {
    let v = json!({"positionIdx": 1, "signQty": "0.5", "sideToken2": "x", "validUntil": 9});
    let out = capture_frame("bybit", "k", &v);
    assert_eq!(out.frame, v);
    assert!(out.redacted_paths.is_empty());
}

/// A sensitive OBJECT value over-redacts to the string placeholder (safe direction).
#[test]
fn sensitive_object_value_is_over_redacted_whole() {
    let v = json!({"token": {"inner": "s3cret"}, "ok": 1});
    let out = capture_frame("x", "k", &v).frame;
    assert_eq!(out["token"], json!("[redacted:token]"));
    assert_eq!(out["ok"], json!(1));
}

#[test]
fn write_all_stamps_provenance_and_round_trips_in_emission_order() {
    let dir =
        std::env::temp_dir().join(format!("vike-capture-test-{}-{}", std::process::id(), line!()));
    let _ = std::fs::remove_dir_all(&dir);

    let mut cap = FrameCapture::new("binance", "unit-test double, not a venue");
    cap.capture("ws_fill", &json!({"seq": 1, "listenKey": "a"}));
    cap.capture("ws_accepted", &json!({"x": "NEW"}));
    cap.capture("ws_fill", &json!({"seq": 2}));
    assert_eq!(cap.count("ws_fill"), 2);

    // 2026-07-22T00:00:00Z = 1784678400000 ms — pins the no-chrono UTC formatter too.
    let written = cap.write_all(&dir, 1_784_678_400_000).unwrap();
    assert_eq!(written.len(), 2, "one file per kind");

    let fx = load_captured(&dir, "ws_fill").expect("ws_fill.json");
    assert_eq!(fx.frames.len(), 2);
    assert_eq!(fx.frames[0]["seq"], json!(1), "emission order preserved");
    assert_eq!(fx.frames[1]["seq"], json!(2));
    assert_eq!(fx.frames[0]["listenKey"], json!("[redacted:listenkey]"));
    let p = &fx.provenance;
    assert_eq!(p["captured_at_utc"], json!("2026-07-22T00:00:00Z"));
    assert_eq!(p["venue"], json!("binance"));
    assert_eq!(p["kind"], json!("ws_fill"));
    assert_eq!(p["sanitizer"], json!(SANITIZER_VERSION));
    assert_eq!(p["frame_count"], json!(2));
    assert_eq!(p["redacted_paths"], json!(["/listenKey"]));

    assert!(load_captured(&dir, "absent_kind").is_none(), "absent file → None");

    // Deterministic apart from captured_at: a second write with the same clock is byte-equal.
    let body1 = std::fs::read_to_string(dir.join("ws_fill.json")).unwrap();
    cap.write_all(&dir, 1_784_678_400_000).unwrap();
    let body2 = std::fs::read_to_string(dir.join("ws_fill.json")).unwrap();
    assert_eq!(body1, body2, "stable bytes for stable input");

    let _ = std::fs::remove_dir_all(&dir);
}

/// The mutation enumerator: non-empty on a nested frame, known members present, never the
/// identity, and deterministic (same frame in, same list out).
#[test]
fn frame_mutations_enumerates_structural_near_misses() {
    let frame = json!({"a": {"b": "x"}, "arr": [1, 2]});
    let muts = frame_mutations(&frame);
    assert!(!muts.is_empty());
    assert!(muts.contains(&json!({"a": {}, "arr": [1, 2]})), "key removal missing");
    assert!(muts.contains(&json!({"a": {"b": "x"}, "arr": []})), "array truncation missing");
    assert!(muts.contains(&json!({"a": {"b": 0}, "arr": [1, 2]})), "string→number flip missing");
    assert!(muts.iter().all(|m| m != &frame), "a mutation equals the original frame");
    assert_eq!(muts, frame_mutations(&frame), "the enumeration must be deterministic");
}

/// Number leaves get the numeric edge set; a scalar root (no parent) still mutates.
#[test]
fn frame_mutations_number_edges_and_scalar_root() {
    let frame = json!({"q": 5});
    let muts = frame_mutations(&frame);
    assert!(muts.contains(&json!({})), "removal of the only key");
    assert!(muts.contains(&json!({"q": null})));
    assert!(muts.contains(&json!({"q": "5"})), "number→string flip");
    assert!(muts.contains(&json!({"q": 1e308})));
    assert!(muts.contains(&json!({"q": -1})));
    assert!(muts.contains(&json!({"q": 0})));

    let root = json!(true);
    let root_muts = frame_mutations(&root);
    assert!(root_muts.contains(&json!(null)));
    assert!(root_muts.iter().all(|m| m != &root));
}

/// **⚠ THE PARENT ACCOUNT ID IS REDACTED, IN EVERY SPELLING A CEX USES.**
///
/// The seven original identity keys all answer *which account am I*. A CEX answers a second
/// question beside it — *whose sub-account is this* — and matching here is EXACT on the
/// lowercased key, never a substring, so `parentUid` matched nothing before 2026-09-20.
///
/// That field is precisely what the blind-CEX work goes looking for (bybit and okx name no
/// account in the credential store, so only an authenticated call can, and the parent id is
/// what separates a sub-account from its master). The first capture taken to ANSWER that
/// question would have been the one that committed it in the clear — into a fixtures directory
/// that ships to the public mirror.
#[test]
fn the_parent_account_spellings_are_redacted() {
    let frame = json!({
        "result": {
            "userID": 592123,
            "parentUid": "770044",
            "mainUid": "770044",
            "masterUid": "770044",
            "subAccount": "sub-7",
            "sub_account": "sub-7",
            "parent_uid": "770044",
            "main_uid": "770044",
            "master_uid": "770044",
            "subAcct": "sub-7",
        }
    });
    let out = capture_frame("bybit", "query-api", &frame);
    let r = &out.frame["result"];

    // Every parent/master spelling is gone. A STRING leaf becomes the keyed placeholder…
    for key in [
        "parentUid",
        "mainUid",
        "masterUid",
        "subAccount",
        "sub_account",
        "parent_uid",
        "main_uid",
        "master_uid",
        "subAcct",
    ] {
        let got = r[key].as_str().unwrap_or_default();
        assert!(
            got.starts_with("[redacted:"),
            "{key} survived the sanitizer as {:?} — a real venue id would ship to the public \
                 mirror",
            r[key]
        );
    }
    // …and the NUMBER leaf becomes `0`, which is why `redacted_paths` and not the value is what
    // a reviewer reads: `0` is indistinguishable from a venue that genuinely answered zero.
    assert_eq!(r["userID"], json!(0), "a numeric id is zeroed, not placeholder-stringed");

    // The audit trail names every one of them, which is the half a human actually checks
    // before committing a captured file.
    for key in ["userID", "parentUid", "subAcct"] {
        let p = format!("/result/{key}");
        assert!(out.redacted_paths.contains(&p), "{p} missing from redacted_paths");
    }
}

/// **…and the exactness is NOT relaxed to get there.** The module doc's own example is
/// `positionIdx must not trip on id`, and widening these entries into substring matches would
/// do exactly that — silently blanking order and position fields whose fidelity is the point of
/// a fixture.
#[test]
fn the_new_entries_do_not_start_matching_substrings() {
    let frame = json!({
        "positionIdx": 1,
        "parentUidSuffix": "keep-me",
        "submitAccountKind": "keep-me",
        "orderId": "1234",
        "tradeId": "258544119",
    });
    let out = capture_frame("bybit", "order", &frame);
    assert_eq!(out.frame["positionIdx"], json!(1));
    assert_eq!(out.frame["parentUidSuffix"], json!("keep-me"));
    assert_eq!(out.frame["submitAccountKind"], json!("keep-me"));
    assert_eq!(out.frame["orderId"], json!("1234"));
    assert_eq!(out.frame["tradeId"], json!("258544119"), "fixture fidelity is kept");
    assert!(out.redacted_paths.is_empty(), "nothing here is sensitive: {:?}", out.redacted_paths);
}

/// ⚠ **THE HUMAN half, and it was MEASURED on a live demo account rather than predicted.**
///
/// `private/get_account_summary` with `extended: true` on deribit answered `email`, `username`
/// and `system_name`, and the sanitizer's own audit trail came back EMPTY: the sixteen
/// account-identity entries above this one are all machine ids, and none of these three is one.
/// A capture of that body would have committed an operator's real email address into a fixtures
/// directory that ships to the public mirror.
#[test]
fn the_human_account_names_are_redacted() {
    let frame = json!({
        "result": {
            "email": "somebody@example.com",
            "username": "trader-one",
            "system_name": "trader-one",
            "systemName": "trader-one",
        }
    });
    let out = capture_frame("deribit", "account-summary", &frame);
    let r = &out.frame["result"];

    for key in ["email", "username", "system_name", "systemName"] {
        let got = r[key].as_str().unwrap_or_default();
        assert!(
            got.starts_with("[redacted:"),
            "{key} survived the sanitizer as {:?} — a real account name would ship to the \
                 public mirror",
            r[key]
        );
    }
    for key in ["email", "username", "system_name"] {
        let p = format!("/result/{key}");
        assert!(out.redacted_paths.contains(&p), "{p} missing from redacted_paths");
    }
}

/// ⚠ **THE RESIDUAL, PINNED SO IT CANNOT BE "FIXED" BY ACCIDENT.**
///
/// Deribit's ACCOUNT id is spelled `id` — `result.id`, beside the JSON-RPC envelope's own
/// request `id`. That key must never join [`REDACT_KEYS`]: it is the key every venue order id,
/// trade id and combo leg rides, and those are the fixture's fidelity. Redaction is by KEY and
/// not by PATH, so no spelling catches one `id` and spares the other.
///
/// This test therefore asserts a LEAK is still possible, on purpose. It exists so a later
/// author who adds `"id"` to the list — which looks like closing a hole — is told by a red test
/// what it actually costs, and so the residual is measured rather than assumed away.
#[test]
fn the_account_id_deribit_spells_id_is_deliberately_not_redacted() {
    let frame = json!({ "id": 7, "result": { "id": 84789, "orderId": "1234" } });
    let out = capture_frame("deribit", "account-summary", &frame);
    assert_eq!(out.frame["id"], json!(7), "the JSON-RPC request id is not an account");
    assert_eq!(
        out.frame["result"]["id"],
        json!(84789),
        "deribit's ACCOUNT id survives — reading the file before committing it is what stands \
             in front of this, and the module doc already asks for that"
    );
    assert_eq!(out.frame["result"]["orderId"], json!("1234"), "and this is why `id` may not join");
    assert!(out.redacted_paths.is_empty(), "nothing was redacted: {:?}", out.redacted_paths);
}
