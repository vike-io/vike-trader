//! The trade stream's wire, protocol and `DealRefs` tests.

use super::*;

#[test]
fn conok_parses_session_and_control_link() {
    let (sid, link) = parse_conok("CONOK,S1a2b3c4d5e6f,50000,5000,myhost.ig.com").unwrap();
    assert_eq!(sid, "S1a2b3c4d5e6f");
    assert_eq!(link, "myhost.ig.com");
    let (sid2, link2) = parse_conok("CONOK,S9,50000,5000,*").unwrap();
    assert_eq!(sid2, "S9");
    assert_eq!(link2, "*");
    assert!(parse_conok("CONERR,2,Wrong credentials").is_none());
}

/// The classification the reconnect loop's recovery hangs off: a `CONERR` first line is the
/// server REFUSING our credentials (re-authenticate), everything else that is not a `CONOK` is
/// a protocol surprise (retry with the same pair). Getting this backwards is invisible at
/// runtime — it just means an expired session never recovers, which is the defect this whole
/// path exists to close.
#[test]
fn conerr_is_classified_as_a_refusal_and_nothing_else_is() {
    assert!(is_conerr("CONERR,2,Wrong credentials"));
    assert!(is_conerr("CONERR,1,Requested Adapter Set not available"));
    // Real TLCP frames can carry leading whitespace after a chunked read.
    assert!(is_conerr("  CONERR,2,x"));
    assert!(!is_conerr("CONOK,S1,50000,5000,*"));
    assert!(!is_conerr("PROBE"));
    assert!(!is_conerr("LOOP,0"));
    assert!(!is_conerr(""));
    // ⚠ Not a substring test: a CONFIRMS payload mentioning the word must not read as a refusal.
    assert!(!is_conerr(r#"U,1,1|{"reason":"CONERR"}"#));
}

/// ⚠ **The `LS_cid` this lane sends is a LICENSING field, not a name for us**, and it is why
/// this stream had almost certainly never worked on any account. TLCP 2.1.0's `create_session`
/// mandates one special string "for all custom developed clients"; anything else is answered
/// `CONERR,71 License not valid for this Client type`, which is a refusal of the CLIENT and has
/// nothing to do with the account, the credentials or the subscription. `stream.rs` sent
/// `"vike-trader-rust"` — a made-up identifier — while `lightstreamer.rs`, the market-data lane
/// that works, sent the mandated one from its own `LS_CID`.
///
/// This test is the twin of `lightstreamer_tests.rs`'s `request_builders_pin_the_wire_bytes`, and it
/// asserts the ENCODED body: the mandated string contains a space, so the wire form carries
/// `%20` (`form_percent_encodes_group_and_schema` below pins that encoding independently).
#[test]
fn the_trade_stream_sends_the_licensed_client_id_the_spec_mandates() {
    let body = create_session_body("ABC123", "CST-tok|XST-tok");

    assert!(
        body.contains("LS_cid=mgQkwtwdysogQz2BJ4Ji%20kOj2Bg"),
        "the create_session body must carry the SPEC's client id; got: {body}"
    );
    assert!(
        !body.contains("vike-trader-rust"),
        "a made-up client id is refused with CONERR,71 whatever the account says; got: {body}"
    );
    // The value is the shared constant, not a second copy that can drift from it again.
    assert!(body.contains(&format!("LS_cid={}", vike_util::percent::percent_encode(LS_CID))));
}

/// The whole body, verbatim, both requests — the market-data lane pins its two the same way.
/// A verbatim pin is what makes an accidental edit to a licensing or protocol field a test
/// failure rather than a silent `CONERR` nobody can attribute.
#[test]
fn trade_stream_request_bodies_pin_the_wire_bytes() {
    assert_eq!(
        create_session_body("ABC123", "CST-abc|XST-def"),
        "LS_adapter_set=DEFAULT&LS_user=ABC123&LS_password=CST-abc%7CXST-def\
             &LS_cid=mgQkwtwdysogQz2BJ4Ji%20kOj2Bg"
    );
    assert_eq!(
        subscribe_body("S1a2b3", "ABC123"),
        "LS_session=S1a2b3&LS_reqId=1&LS_op=add&LS_subId=1&LS_data_adapter=DEFAULT\
             &LS_group=TRADE%3AABC123&LS_schema=CONFIRMS%20OPU%20WOU&LS_mode=DISTINCT\
             &LS_snapshot=true"
    );
}

/// One handshake, one spelling. Both lanes speak TLCP 2.1.0 to the same vendor's server, and
/// `stream.rs` re-spelling the protocol version (and `form`/`urlencode`) as its own local copies
/// is the duplication that let the `LS_cid` above diverge in the first place — the wrong cid was
/// the COST, the duplication was the defect.
#[test]
fn the_two_tlcp_lanes_agree_on_the_protocol_version() {
    assert_eq!(LS_PROTOCOL, crate::lightstreamer::LS_PROTOCOL);
    assert_eq!(LS_CID, crate::lightstreamer::LS_CID);
    assert_eq!(
        crate::lightstreamer::LS_WS_SUBPROTOCOL,
        format!("{LS_PROTOCOL}.lightstreamer.com"),
        "the WS subprotocol is the same version wearing the vendor's suffix"
    );
}

#[test]
fn form_percent_encodes_group_and_schema() {
    let body = form(&[("LS_group", "TRADE:ABC123"), ("LS_schema", "CONFIRMS OPU WOU")]);
    assert_eq!(body, "LS_group=TRADE%3AABC123&LS_schema=CONFIRMS%20OPU%20WOU");
}

/// Both directions, from ONE write. The `coid -> dealReference` half is what makes
/// `ExecutionClient::confirm` answerable on this venue at all: `GET /confirms/{ref}` is IG's
/// only per-order status read, and before this map existed nothing in the process could turn a
/// coid back into the reference it needs.
#[test]
fn deal_refs_answers_in_both_directions_from_one_write() {
    let mut refs = DealRefs::default();
    refs.record("coid-market", "REF-A", true);
    refs.record("coid-working", "REF-B", false);

    assert_eq!(refs.coid_for("REF-A").as_deref(), Some("coid-market"));
    assert_eq!(refs.coid_for("REF-B").as_deref(), Some("coid-working"));
    assert_eq!(refs.coid_for("REF-UNKNOWN"), None, "a foreign confirm is not ours to route");

    // `market` travels with the reference: the re-confirm needs it to decide whether the
    // confirm it fetches carries a fill or only an accept.
    assert_eq!(
        refs.pending_for("coid-market"),
        Some(Pending { deal_ref: "REF-A".into(), market: true })
    );
    assert_eq!(
        refs.pending_for("coid-working"),
        Some(Pending { deal_ref: "REF-B".into(), market: false })
    );
    assert_eq!(
        refs.pending_for("coid-never-submitted"),
        None,
        "no reference means IG never acknowledged the deal — there is nothing to ask about"
    );
}

#[test]
fn rebase_host_keeps_scheme() {
    assert_eq!(
        rebase_host("https://demo-apd.marketdatasystems.com", "push.ig.com"),
        "https://push.ig.com"
    );
}

/// The trade stream's agent carries the exec row's idle bound, observed on the BUILT agent:
/// `recv_body` is the 30 s each read wait of the chunked body may last. The row declares no dial or
/// handshake bound for this lane, so no connect bound may appear on the agent either (a bound the
/// venue was never given would be a retune).
#[test]
fn the_trade_stream_agent_carries_the_exec_rows_idle_bound() {
    let timeouts = trade_stream_agent().config().timeouts();
    assert_eq!(timeouts.recv_body, Some(std::time::Duration::from_secs(30)), "the read-idle bound");
    assert_eq!(timeouts.connect, None, "the row declares no dial bound for this lane");
}

/// `DealIds` keeps both directions in step: a coid re-recorded under a new `dealId` leaves no stale
/// reverse entry, and `forget` drops both.
#[test]
fn deal_ids_keep_both_directions_in_step() {
    let mut ids = DealIds::default();
    ids.record("c1", "D1");
    assert_eq!(ids.deal_id_for("c1").as_deref(), Some("D1"));
    assert_eq!(ids.coid_for("D1").as_deref(), Some("c1"));
    ids.record("c1", "D2");
    assert_eq!(ids.coid_for("D1"), None, "the superseded dealId no longer names the order");
    assert_eq!(ids.coid_for("D2").as_deref(), Some("c1"));
    ids.forget("c1");
    assert_eq!((ids.deal_id_for("c1"), ids.coid_for("D2")), (None, None));
    ids.forget("never-recorded");
}

/// A cancel's reference is taught to the stream ONLY: the submit-time `coid -> dealReference` half
/// (what the confirm watchdog re-asks IG with) must survive it.
#[test]
fn record_stream_only_does_not_touch_the_watchdogs_half() {
    let mut refs = DealRefs::default();
    refs.record("c1", "SUBMIT", false);
    refs.record_stream_only("c1", "CANCEL");
    assert_eq!(refs.coid_for("CANCEL").as_deref(), Some("c1"));
    assert_eq!(refs.coid_for("SUBMIT").as_deref(), Some("c1"));
    assert_eq!(refs.pending_for("c1").map(|p| p.deal_ref).as_deref(), Some("SUBMIT"));
}

/// The map lives as long as the process: past `MAX_DEAL_REFS` orders the OLDEST goes (both
/// directions), the newest stay; a rejected order is forgotten at once.
#[test]
fn deal_refs_is_bounded_and_forgets_a_rejected_order() {
    let mut refs = DealRefs::default();
    for i in 0..MAX_DEAL_REFS + 10 {
        refs.record(&format!("c{i}"), &format!("R{i}"), false);
    }
    assert_eq!(refs.pending_for("c0"), None, "the oldest is evicted");
    assert_eq!(refs.coid_for("R9"), None, "and its reverse half with it");
    assert!(refs.pending_for("c10").is_some());
    assert!(refs.pending_for(&format!("c{}", MAX_DEAL_REFS + 9)).is_some());
    assert!(refs.by_coid.len() <= MAX_DEAL_REFS && refs.by_ref.len() <= MAX_DEAL_REFS);

    refs.record("rej", "RREJ", true);
    refs.record_stream_only("rej", "RCANCEL");
    refs.forget("rej");
    assert_eq!(
        (refs.pending_for("rej"), refs.coid_for("RREJ"), refs.coid_for("RCANCEL")),
        (None, None, None)
    );
    assert!(refs.pending_for("c11").is_some(), "others untouched");
}
