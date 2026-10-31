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
    assert!(!is_conerr(r#"U,1,1,{"reason":"CONERR"}||"#));
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

// ── the three lines IG sent for ONE market buy of GBPUSD and its close (demo gateway, 2026-10-30, raw capture) ──

/// The `dealReference` the node sent for that buy (`vk` plus 16 hex digits of its coid).
const REF: &str = "vk363e1f3543c55a4b";
/// The `OPU` of the position the buy opened: field 1, `status` `OPEN`, `timestamp` UTC with milliseconds.
const OPU_OPENED: &str = r##"U,1,1,#|{"dealReference":"vk363e1f3543c55a4b","dealId":"DIAAAAYR83UUCBV","direction":"BUY","epic":"CS.D.GBPUSD.MINI.IP","status":"OPEN","dealStatus":"ACCEPTED","level":1.33962,"size":0.04,"currency":"USD","timestamp":"2026-10-30T19:47:04.546","channel":"PublicRestOTC","dealIdOrigin":"DIAAAAYR83UUCBV","expiry":"-","openLevel":1.33962,"stopLevel":null,"limitLevel":null,"guaranteedStop":false}|"##;
/// The `OPU` of its CLOSE: `DELETED`, size 0, and the ENTRY order's `dealReference` (the close has its own).
const OPU_CLOSED: &str = r##"U,1,1,#|{"dealReference":"vk363e1f3543c55a4b","dealId":"DIAAAAYR83UUCBV","direction":"BUY","epic":"CS.D.GBPUSD.MINI.IP","status":"DELETED","dealStatus":"ACCEPTED","level":1.33962,"size":0,"currency":"USD","timestamp":"2026-10-30T19:47:16.785","channel":"PublicRestOTC","dealIdOrigin":"DIAAAAYR83UUCBV","expiry":"-","openLevel":1.33962,"stopLevel":null,"limitLevel":null,"guaranteedStop":false}|"##;
/// The `CONFIRMS` of a LIMIT order's creation (another order, resting at 1.286): `status` `OPEN`, `affectedDeals`
/// `OPENED`, a level and a size, exactly what a position's open states.
const CONFIRMS_LIMIT_CREATED: &str = r##"U,1,1,{"date":"2026-10-30T19:46:48.354","limitDistance":null,"reason":"SUCCESS","limitLevel":null,"level":1.286,"dealId":"DIAAAAYR82EEVBM","channel":"PublicRestOTC","epic":"CS.D.GBPUSD.MINI.IP","dealReference":"vk2614ec4d4954e591","dealStatus":"ACCEPTED","trailingStop":false,"size":0.04,"stopLevel":null,"stopDistance":null,"profitCurrency":null,"expiry":"-","profit":null,"affectedDeals":[{"dealId":"DIAAAAYR82EEVBM","status":"OPENED"}],"guaranteedStop":false,"direction":"BUY","status":"OPEN"}|#|"##;
/// 2026-10-30T19:47:04.546Z, the `OPU_OPENED` timestamp.
const OPENED_AT: i64 = 1_793_389_624_546;
const HEARD: i64 = 1_700_000_000_123;

fn refs_of(coid: &str, deal_ref: &str, market: bool) -> DealRefMap {
    let mut refs = DealRefs::default();
    refs.record(coid, deal_ref, market);
    Arc::new(Mutex::new(refs))
}

/// **A position's CLOSE is not an order cancel** (N07, the "cancel after the fill" of both IG coids on 2026-10-28).
/// The close of a position sends an `OPU` `DELETED` that carries the ENTRY order's `dealReference`; the lane read
/// every `OPU` as a deal confirm and made `OrderCanceled` of it, for an order that had FILLED. The captured line
/// is fed with the entry's reference recorded: no event.
///
/// MUTATION PROOF: decode the `OPU` whatever its `status` (drop the `OPEN` test in `events_for_line`) - this
/// line is `OrderCanceled` for `c-entry`; so did the old pipe-after-item parse, which put the `OPU` in field 0.
#[test]
fn a_position_close_is_not_an_order_cancel() {
    let deal_refs = refs_of("c-entry", REF, true);
    assert!(events_for_line(OPU_CLOSED, &deal_refs, HEARD).is_empty());
}

/// **A confirm of a working order's CREATION is not a fill.** The lane must not turn to the `CONFIRMS` field
/// to repair the grammar: a limit order's create states `status` `OPEN` with a level and a size, and decoded as
/// an execution it books a fill for an order that merely rests. (The same line, in the old parse, was lost with
/// the whole first field.)
#[test]
fn a_confirm_of_a_working_orders_creation_is_not_a_fill() {
    let deal_refs = refs_of("c-limit", "vk2614ec4d4954e591", false);
    assert!(events_for_line(CONFIRMS_LIMIT_CREATED, &deal_refs, HEARD).is_empty());
}

/// **A position OPENED is the fill pair, stamped with IG's own time.** The `OPU` `timestamp` is UTC (the same
/// instant as the box's clock when read on 2026-10-30), milliseconds kept; the fill is the execution the sync
/// confirm reports too (`confirm_trade_id`: the `dealId`), so the engine's trade-id dedup absorbs the overlap.
/// A `timestamp` the lane cannot read falls back to when the line was heard, never 0.
///
/// MUTATION PROOF: stamp with `now` again - the first assertion fails (HEARD, not the timestamp).
#[test]
fn a_position_opened_is_the_fill_pair_stamped_with_its_own_utc_timestamp() {
    let deal_refs = refs_of("c-entry", REF, true);

    let events = events_for_line(OPU_OPENED, &deal_refs, HEARD);

    assert_eq!(events.len(), 2, "the dual-publish pair: {events:?}");
    for ev in &events {
        match ev {
            Event::Fill(f) => {
                assert_eq!((f.ts, f.trade_id.as_str()), (OPENED_AT, "DIAAAAYR83UUCBV"), "{ev:?}");
                assert_eq!((f.last_qty, f.last_px, f.side), (0.04, 1.33962, 1), "{ev:?}");
            }
            Event::OrderFilled(w) => {
                assert_eq!((w.ts, w.fill.ts), (OPENED_AT, OPENED_AT), "{ev:?}")
            }
            other => panic!("expected the fill pair, got {other:?}"),
        }
    }

    let unreadable = OPU_OPENED.replace("2026-10-30T19:47:04.546", "soon");
    let events = events_for_line(&unreadable, &deal_refs, HEARD);
    assert!(matches!(events.first(), Some(Event::Fill(f)) if f.ts == HEARD), "{events:?}");
    let no_timestamp = OPU_OPENED.replace(r#""timestamp":"2026-10-30T19:47:04.546","#, "");
    assert!(matches!(
        events_for_line(&no_timestamp, &deal_refs, HEARD).first(),
        Some(Event::Fill(f)) if f.ts == HEARD
    ));
    // a fraction of fewer digits keeps its place: `.5` is half a second
    let half = OPU_OPENED.replace(".546", ".5");
    assert!(matches!(
        events_for_line(&half, &deal_refs, HEARD).first(),
        Some(Event::Fill(f)) if f.ts == OPENED_AT - 46
    ));
}

/// A line that is not ours names no event: another deal's reference, a control line, a line of another
/// subscription.
#[test]
fn a_line_that_is_not_ours_names_no_event() {
    let deal_refs = refs_of("c-entry", REF, true);
    assert!(events_for_line(&OPU_OPENED.replace(REF, "STRANGER"), &deal_refs, HEARD).is_empty());
    assert!(events_for_line("PROBE", &deal_refs, HEARD).is_empty());
    assert!(events_for_line(&OPU_OPENED.replace("U,1,1", "U,2,1"), &deal_refs, HEARD).is_empty());
}

/// **A fill that reaches the stream while the order's submit is in flight goes out AFTER the order's own
/// answer** (N07, the first GBPUSD order of 2026-10-28: `FillEvent` and `OrderFilled` ahead of `OrderAccepted`,
/// then `terminal event dropped on a live order (out-of-order)`). The order of the capture: the exec thread
/// records the reference, the stream hears the `OPU` of the position the buy opened, and only then the exec thread
/// has read `/confirms` and sends its answer.
///
/// MUTATION PROOF: drop the `SubmitInFlight` line from the exec thread's submit arm (or `begin_submit`) - the
/// pair comes out of `events_for_line` at once, before the answer, as the control below shows.
#[test]
fn the_streams_fill_of_an_order_in_flight_waits_behind_the_orders_own_answer() {
    use vike_exec::Ingest;
    use vike_model::events::OrderSubmitted;
    let own = || Event::OrderSubmitted(OrderSubmitted { client_order_id: "c-entry".into(), ts: 1 });
    let (tx, mut rx) = vike_exec::event_channel(16);
    let mut got = || {
        let mut seen = Vec::new();
        while let Ok(Ingest::Event(ev)) = rx.try_recv() {
            seen.push(ev);
        }
        seen
    };
    let kind = |ev: &Event| match ev {
        Event::OrderSubmitted(_) => "own answer",
        Event::Fill(_) => "fill",
        Event::OrderFilled(_) => "filled",
        other => panic!("{other:?}"),
    };

    // the control: no submit in flight (a fill of a working order, long after its accept) - sent at once
    let deal_refs = refs_of("c-entry", REF, true);
    assert_eq!(events_for_line(OPU_OPENED, &deal_refs, HEARD).len(), 2);

    // the race: the OPU is heard between the reference being recorded and the answer being sent
    let deal_refs: DealRefMap = Arc::new(Mutex::new(DealRefs::default()));
    {
        let _in_flight = crate::exec::SubmitInFlight::begin(&deal_refs, &tx, "c-entry");
        deal_refs.lock().unwrap().record("c-entry", REF, true);
        assert!(events_for_line(OPU_OPENED, &deal_refs, HEARD).is_empty(), "held, not sent");
        // another order's line is not held by this one
        deal_refs.lock().unwrap().record("c-other", "vkOTHER", true);
        let other = OPU_OPENED.replace(REF, "vkOTHER");
        assert_eq!(events_for_line(&other, &deal_refs, HEARD).len(), 2);
        assert!(got().is_empty(), "nothing has reached the lane yet");
        tx.blocking_send(own()).unwrap(); // the exec thread's answer
    } // the submit arm ends: the held pair goes out behind it
    let order: Vec<&str> = got().iter().map(kind).collect();
    assert_eq!(order, ["own answer", "fill", "filled"]);

    // the window is closed: the next line for the order is sent at once
    assert_eq!(events_for_line(OPU_OPENED, &deal_refs, HEARD).len(), 2);
    assert!(deal_refs.lock().unwrap().release("c-entry").is_empty());
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

/// A loopback origin answering every request with a 302 to a SECOND loopback listener, plus that
/// listener's hit counter: the stand-in for the host a followed redirect would replay credentials
/// to.
fn redirecting_origin() -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
    use std::io::{Read, Write};
    use std::sync::{Arc, atomic::AtomicUsize, atomic::Ordering::SeqCst};
    fn serve(reply: String) -> (u16, Arc<AtomicUsize>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let port = listener.local_addr().expect("local addr").port();
        let hits = Arc::new(AtomicUsize::new(0));
        let seen = hits.clone();
        std::thread::spawn(move || {
            for mut conn in listener.incoming().flatten() {
                seen.fetch_add(1, SeqCst);
                let _ = conn.set_read_timeout(Some(std::time::Duration::from_millis(300)));
                let mut buf = [0u8; 8192];
                for _ in 0..2 {
                    let _ = conn.read(&mut buf); // a POST's head and body can be two writes
                }
                let _ = conn.write_all(reply.as_bytes());
            }
        });
        (port, hits)
    }
    let (target, target_hits) =
        serve("HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}".into());
    let (origin, _) = serve(format!(
        "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:{target}/stolen\r\n\
         Content-Length: 0\r\nConnection: close\r\n\r\n"
    ));
    (format!("http://127.0.0.1:{origin}"), target_hits)
}

/// **A 3xx on the credentialed `create_session` POST surfaces as a response and the `Location`
/// is never dialled.** The form body carries the CST/XST pair as `LS_password`; ureq's default
/// follows ten redirects (a 307/308 would re-POST that body) and replays every header but
/// `Authorization`/`Cookie`.
///
/// MUTATION PROOF: delete `.max_redirects(0)` from `trade_stream_agent` — the 302 is followed and
/// the second listener sees the replayed request.
#[test]
fn the_trade_stream_agent_never_follows_a_redirect() {
    let (base, target_hits) = redirecting_origin();
    let resp = trade_stream_agent()
        .post(&format!("{base}/lightstreamer/create_session.txt"))
        .header("Content-Type", "application/x-www-form-urlencoded")
        .send(b"LS_user=acct&LS_password=CST-test%7CXST-test");
    assert_eq!(target_hits.load(std::sync::atomic::Ordering::SeqCst), 0, "the target was dialled");
    assert_eq!(resp.expect("a 302 is a response").status().as_u16(), 302);
}
