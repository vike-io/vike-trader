//! The trade stream's wire, protocol and `DealRefs` tests.

use super::*;
use std::assert_matches;
use vike_bridge_core::transport::http_error::VenueApiHttpError;
use vike_log::loopback::redirecting_origin;

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
/// `"vike-trader-rust"` — a made-up identifier — while `feed/decode/lightstreamer.rs`, the market-data lane
/// that works, sent the mandated one from its own `LS_CID`.
///
/// This test is the twin of `feed/decode/lightstreamer_tests.rs`'s `request_builders_pin_the_wire_bytes`, and it
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
    assert!(body.contains(&format!("LS_cid={}", vike_utility::percent::percent_encode(LS_CID))));
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
    assert_eq!(LS_PROTOCOL, crate::feed::decode::lightstreamer::LS_PROTOCOL);
    assert_eq!(LS_CID, crate::feed::decode::lightstreamer::LS_CID);
    assert_eq!(
        crate::feed::decode::lightstreamer::LS_WEBSOCKET_SUBPROTOCOL,
        format!("{LS_PROTOCOL}.lightstreamer.com"),
        "the WS subprotocol is the same version wearing the vendor's suffix"
    );
}

#[test]
fn form_percent_encodes_group_and_schema() {
    let body = form(&[("LS_group", "TRADE:ABC123"), ("LS_schema", "CONFIRMS OPU WOU")]);
    assert_eq!(body, "LS_group=TRADE%3AABC123&LS_schema=CONFIRMS%20OPU%20WOU");
}

/// Both directions, from ONE write. The `order_client_id -> dealReference` half is what makes
/// `ExecutionClient::confirm` answerable on this venue at all: `GET /confirms/{ref}` is IG's
/// only per-order status read, and before this map existed nothing in the process could turn a
/// order client id back into the reference it needs.
#[test]
fn deal_refs_answers_in_both_directions_from_one_write() {
    let mut refs = DealRefs::default();
    refs.record("order-client-id-market", "REF-A", true);
    refs.record("order-client-id-working", "REF-B", false);

    assert_eq!(refs.order_client_id_for("REF-A").as_deref(), Some("order-client-id-market"));
    assert_eq!(refs.order_client_id_for("REF-B").as_deref(), Some("order-client-id-working"));
    assert_eq!(
        refs.order_client_id_for("REF-UNKNOWN"),
        None,
        "a foreign confirm is not ours to route"
    );

    // `market` travels with the reference: the re-confirm needs it to decide whether the
    // confirm it fetches carries a fill or only an accept.
    assert_eq!(
        refs.pending_for("order-client-id-market"),
        Some(Pending { deal_ref: "REF-A".into(), market: true })
    );
    assert_eq!(
        refs.pending_for("order-client-id-working"),
        Some(Pending { deal_ref: "REF-B".into(), market: false })
    );
    assert_eq!(
        refs.pending_for("order-client-id-never-submitted"),
        None,
        "no reference means IG never acknowledged the deal — there is nothing to ask about"
    );
}

// ── the three lines IG sent for ONE market buy of GBPUSD and its close (demo gateway, 2026-10-30, raw capture) ──

/// The `dealReference` the node sent for that buy (`vk` plus 16 hex digits of its order client id).
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

fn refs_of(
    order_client_id: &str,
    deal_ref: &str,
    market: bool,
) -> OrderVenueIdRegistryDealReference {
    let mut refs = DealRefs::default();
    refs.record(order_client_id, deal_ref, market);
    Arc::new(Mutex::new(refs))
}

/// **A position's CLOSE is not an order cancel** (N07, the "cancel after the fill" of both IG order client ids on 2026-10-28).
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
    for event in &events {
        match event {
            Event::Fill(f) => {
                assert_eq!(
                    (f.timestamp, f.trade_id.as_str()),
                    (OPENED_AT, "DIAAAAYR83UUCBV"),
                    "{event:?}"
                );
                assert_eq!((f.last_qty, f.last_price, f.side), (0.04, 1.33962, 1), "{event:?}");
            }
            Event::OrderFilled(w) => {
                assert_eq!((w.timestamp, w.fill.timestamp), (OPENED_AT, OPENED_AT), "{event:?}")
            }
            other => panic!("expected the fill pair, got {other:?}"),
        }
    }

    let unreadable = OPU_OPENED.replace("2026-10-30T19:47:04.546", "soon");
    let events = events_for_line(&unreadable, &deal_refs, HEARD);
    assert!(matches!(events.first(), Some(Event::Fill(f)) if f.timestamp == HEARD), "{events:?}");
    let no_timestamp = OPU_OPENED.replace(r#""timestamp":"2026-10-30T19:47:04.546","#, "");
    assert!(matches!(
        events_for_line(&no_timestamp, &deal_refs, HEARD).first(),
        Some(Event::Fill(f)) if f.timestamp == HEARD
    ));
    // a fraction of fewer digits keeps its place: `.5` is half a second
    let half = OPU_OPENED.replace(".546", ".5");
    assert!(matches!(
        events_for_line(&half, &deal_refs, HEARD).first(),
        Some(Event::Fill(f)) if f.timestamp == OPENED_AT - 46
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

/// **After a gatewayhub restart a working order the previous process placed fills on the stream under its own order client id.**
/// The start's `DealRefs` is empty, so the `OPU` of the position its fill opened was dropped and only reconcile found
/// it. The re-teaching refills the route with the reference that process sent (`deal_reference` of the order client id), so the
/// line is that order client id's fill pair, keyed on the position's `dealId` like any streamed open (a repeat is the engine's
/// trade-id dedup); an `OPU` of a deal nobody taught stays foreign, and the confirm watchdog's half is not taught.
///
/// MUTATION PROOF: empty `teach_deal_refs`'s loop - the second assertion fails (no event, the pre-fix drop).
#[test]
fn a_reteached_working_orders_streamed_fill_reaches_its_order_client_id() {
    let deal_refs: OrderVenueIdRegistryDealReference = Arc::new(Mutex::new(DealRefs::default()));
    let line = OPU_OPENED.replace(REF, &crate::execution::deal_reference("order-client-id-old"));
    assert!(events_for_line(&line, &deal_refs, HEARD).is_empty(), "before the re-teaching");

    let taught = vike_execution::RestoredOrder {
        venue_order_id: Some("DIAAAAYR82EEVBM".into()),
        ..vike_execution::RestoredOrder::new("order-client-id-old", "CS.D.GBPUSD.MINI.IP")
    };
    crate::execution::teach_deal_refs(&deal_refs, &[taught]);

    assert_matches!(
        events_for_line(&line, &deal_refs, HEARD).as_slice(),
        [Event::Fill(f), Event::OrderFilled(w)]
            if f.order_client_id == "order-client-id-old" && w.order_client_id == "order-client-id-old"
                && f.trade_id == "DIAAAAYR83UUCBV"
    );
    assert!(events_for_line(OPU_OPENED, &deal_refs, HEARD).is_empty(), "a deal nobody taught");
    assert_eq!(
        deal_refs.lock().unwrap().pending_for("order-client-id-old"),
        None,
        "no confirm half"
    );
}

/// **A fill that reaches the stream while the order's submit is in flight goes out AFTER the order's own
/// answer** (N07, the first GBPUSD order of 2026-10-28: `FillEvent` and `OrderFilled` ahead of `OrderAccepted`,
/// then `terminal event dropped on a live order (out-of-order)`). The order of the capture: the exec thread
/// enters the fence and records the reference, the stream hears the `OPU` of the position the buy opened and
/// delivers it through the fence ([`LightstreamerDial::fence`]), and only then the exec thread sends its answer. The fence
/// is the shared one; what this holds is that every event the stream mints for the order is a kind it holds.
///
/// MUTATION PROOF: drop the `fence.enter` line from the exec thread's submit arm - the pair goes out at once,
/// before the answer, as the control below shows.
#[test]
fn the_streams_fill_of_an_order_in_flight_waits_behind_the_orders_own_answer() {
    use vike_execution::Ingest;
    use vike_model::events::OrderSubmitted;
    let own = || {
        Event::OrderSubmitted(OrderSubmitted { order_client_id: "c-entry".into(), timestamp: 1 })
    };
    let (tx, mut rx) = vike_execution::event_channel(16);
    let mut got = || {
        let mut seen = Vec::new();
        while let Ok(Ingest::Event(event)) = rx.try_recv() {
            seen.push(event);
        }
        seen
    };
    let kind = |event: &Event| match event {
        Event::OrderSubmitted(_) => "own answer",
        Event::Fill(_) => "fill",
        Event::OrderFilled(_) => "filled",
        other => panic!("{other:?}"),
    };
    let fence = SubmitFence::new(tx.clone());
    let deliver = |line: &str, deal_refs: &OrderVenueIdRegistryDealReference| {
        for event in events_for_line(line, deal_refs, HEARD) {
            assert!(fence.deliver(event), "the lane is open");
        }
    };

    // the control: no submit in flight (a fill of a working order, long after its accept) - sent at once
    let deal_refs = refs_of("c-entry", REF, true);
    deliver(OPU_OPENED, &deal_refs);
    assert_eq!(got().iter().map(kind).collect::<Vec<_>>(), ["fill", "filled"]);

    // the race: the OPU is heard between the reference being recorded and the answer being sent
    let deal_refs: OrderVenueIdRegistryDealReference = Arc::new(Mutex::new(DealRefs::default()));
    {
        let _fenced = fence.enter("c-entry");
        deal_refs.lock().unwrap().record("c-entry", REF, true);
        deliver(OPU_OPENED, &deal_refs);
        assert_eq!(fence.parked("c-entry"), 2, "held, not sent");
        // another order's line is not held by this one
        deal_refs.lock().unwrap().record("c-other", "vkOTHER", true);
        deliver(&OPU_OPENED.replace(REF, "vkOTHER"), &deal_refs);
        assert_eq!(got().len(), 2, "the other order's pair went at once, this one's is held");
        tx.blocking_send(own()).unwrap(); // the exec thread's answer
    } // the submit arm ends: the held pair goes out behind it
    let order: Vec<&str> = got().iter().map(kind).collect();
    assert_eq!(order, ["own answer", "fill", "filled"]);

    // the window is closed: the next line for the order is sent at once
    deliver(OPU_OPENED, &deal_refs);
    assert_eq!(fence.parked("c-entry"), 0);
    assert_eq!(got().len(), 2);
}

#[test]
fn rebase_host_keeps_scheme() {
    assert_eq!(
        rebase_host("https://demo-apd.marketdatasystems.com", "push.ig.com"),
        "https://push.ig.com"
    );
}

/// The trade stream's agent carries the exec row's idle bound and its dial window, observed on the BUILT
/// agent: `recv_body` is the 30 s each read wait of the chunked body may last, and `connect` the row's 10 s
/// `dial_timeout` (owner ruling B-13). The row declares no handshake bound, so none appears on the agent.
#[test]
fn the_trade_stream_agent_carries_the_execution_rows_idle_bound() {
    let timeouts = trade_stream_agent().config().timeouts();
    assert_eq!(timeouts.recv_body, Some(std::time::Duration::from_secs(30)), "the read-idle bound");
    assert_eq!(timeouts.connect, Some(std::time::Duration::from_secs(10)), "the row's dial window");
}

/// A cancel's reference is taught to the stream ONLY: the submit-time `order_client_id -> dealReference` half
/// (what the confirm watchdog re-asks IG with) must survive it.
#[test]
fn record_stream_only_does_not_touch_the_watchdogs_half() {
    let mut refs = DealRefs::default();
    refs.record("c1", "SUBMIT", false);
    refs.record_stream_only("c1", "CANCEL");
    assert_eq!(refs.order_client_id_for("CANCEL").as_deref(), Some("c1"));
    assert_eq!(refs.order_client_id_for("SUBMIT").as_deref(), Some("c1"));
    assert_eq!(refs.pending_for("c1").map(|p| p.deal_ref).as_deref(), Some("SUBMIT"));
}

/// **A cancel whose `/confirms` did not answer ends on the stream**, as `cancel_working_order` promises when it answers the
/// engine "unconfirmed". Since the lane was narrowed to the `OPU` nothing read that `DELETED`: red on that code, and the
/// order stayed live with nothing left to end it.
#[test]
fn a_streamed_deleted_ends_an_order_whose_cancel_went_unconfirmed() {
    let map: OrderVenueIdRegistryDealReference = Arc::new(Mutex::new(DealRefs::default()));
    map.lock().unwrap().record("c1", "SUBMIT", false);
    map.lock().unwrap().record_stream_only("c1", "CANCEL");
    let line = r##"U,1,1,{"dealReference":"CANCEL","dealId":"D1","status":"DELETED","dealStatus":"ACCEPTED","reason":"SUCCESS"}|#|#"##;
    let events = events_for_line(line, &map, 9);
    assert_matches!(events.as_slice(), [Event::OrderCanceled(c)] if c.order_client_id == "c1");
    assert!(events_for_line(line, &map, 9).is_empty(), "read once: a snapshot repeats it");
}

/// How many times the order `c1` is ended: the stream's `OrderCanceled`s plus the one the exec thread sends for the
/// cancel's `outcome` (`None`: it sends nothing).
fn times_ended(
    streamed: &[Event],
    outcome: impl Into<Option<vike_bridge_core::exec_actor::ExecutionClientActorCancelOutcome>>,
) -> usize {
    let sent = outcome.into().map(|o| vike_bridge_core::exec_actor::cancel_event("c1", o));
    streamed.iter().chain(sent.iter()).filter(|e| matches!(e, Event::OrderCanceled(_))).count()
}

/// **A cancel whose streamed confirm lands before its own `/confirms` answers is ended, once** (read from the code, not
/// seen live: IG answers the DELETE in 60-200 ms and streams its `DELETED` right behind it, while the ladder takes
/// seconds). The reference was taught to the stream only after the ladder FAILED, so the confirm reached a stream that
/// knew nothing of it and was dropped: a local ACCEPTED order over a working order IG had deleted, with nothing left to
/// end it. Whichever answers first ends the order and the other sends nothing: the stream ahead of a ladder that never
/// answers (the ghost), the stream ahead of a ladder that answers, and an answered ladder ahead of the stream's copy.
///
/// MUTATION PROOF: move `record_stream_only` back behind the ladder in `cancel_working_order` - the first case ends the
/// order 0 times.
#[test]
fn a_cancel_the_stream_confirms_before_its_ladder_does_is_ended_once() {
    use crate::execution::lifecycle::cancel_working_order;
    const DELETED: &str = r##"U,1,1,{"dealReference":"CANCEL","dealId":"D1","status":"DELETED","dealStatus":"ACCEPTED","reason":"SUCCESS"}|#|#"##;
    let deleted = || -> Result<serde_json::Value, VenueApiHttpError> {
        Ok(
            serde_json::json!({"dealStatus": "ACCEPTED", "status": "DELETED", "dealReference": "CANCEL"}),
        )
    };
    let lost = || -> Result<serde_json::Value, VenueApiHttpError> {
        Err(crate::transport::IG_HTTP.error(404, "error.service.execution.find"))
    };
    let reference = || Ok(serde_json::json!({"dealReference": "CANCEL"}));

    for (case, answer) in [("the ladder never answers", lost()), ("the ladder answers", deleted())]
    {
        let map = refs_of("c1", "SUBMIT", false);
        let mut streamed = Vec::new();
        let outcome = cancel_working_order(
            &map,
            "c1",
            "D1",
            |_| reference(),
            |_| {
                // IG's streamed confirm, heard before the ladder's first attempt is answered
                streamed.extend(events_for_line(DELETED, &map, 9));
                answer
            },
        );
        assert_eq!(times_ended(&streamed, outcome), 1, "stream first, {case}: {streamed:?}");
    }

    let map = refs_of("c1", "SUBMIT", false);
    let outcome = cancel_working_order(&map, "c1", "D1", |_| reference(), |_| deleted());
    let streamed = events_for_line(DELETED, &map, 9);
    assert_eq!(times_ended(&streamed, outcome), 1, "the ladder first: {streamed:?}");
}

/// Every other `CONFIRMS` stays unread: a deletion of a reference no unconfirmed cancel minted, the submit's own
/// reference, a working order's creation, and the REFUSAL of the very cancel, which ends nothing.
#[test]
fn no_other_confirm_is_read_from_the_stream() {
    let map: OrderVenueIdRegistryDealReference = Arc::new(Mutex::new(DealRefs::default()));
    map.lock().unwrap().record("c1", "SUBMIT", false);
    map.lock().unwrap().record_stream_only("c1", "CANCEL");
    for line in [
        r##"U,1,1,{"dealReference":"STRANGER","status":"DELETED","dealStatus":"ACCEPTED"}|#|#"##,
        r##"U,1,1,{"dealReference":"SUBMIT","status":"DELETED","dealStatus":"ACCEPTED"}|#|#"##,
        r##"U,1,1,{"dealReference":"CANCEL","status":"OPEN","dealStatus":"ACCEPTED","size":1.0,"level":1.1,"dealId":"D1"}|#|#"##,
        r##"U,1,1,{"dealReference":"CANCEL","dealStatus":"REJECTED","reason":"ERROR"}|#|#"##,
    ] {
        assert!(events_for_line(line, &map, 9).is_empty(), "{line}");
    }
    assert_eq!(
        map.lock().unwrap().order_client_id_for("CANCEL").as_deref(),
        Some("c1"),
        "still taught"
    );
}

/// Owner ruling 2026-11-12: a forgotten order's slot in the `MAX_DEAL_REFS` bound is freed AT ONCE. So a bound
/// filled by live orders alone evicts none of them, and one more evicts exactly the oldest live order. Before the
/// ruling the forgotten order's slot counted until it aged out, and the oldest LIVE order went one insert early.
#[test]
fn a_forgotten_orders_slot_is_free_at_once_so_a_live_order_is_not_evicted_early() {
    let mut refs = DealRefs::default();
    refs.record("c0", "R0", false);
    refs.record("rejected", "RREJ", true);
    refs.forget("rejected");
    for i in 1..MAX_DEAL_REFS {
        refs.record(&format!("c{i}"), &format!("R{i}"), false);
    }
    assert!(
        refs.pending_for("c0").is_some(),
        "{MAX_DEAL_REFS} live orders fill the bound: none evicted"
    );
    assert_eq!(
        refs.order_client_id_for("R0").as_deref(),
        Some("c0"),
        "and its reference still routes"
    );

    refs.record("one-more", "RMORE", false);
    assert_eq!(
        (refs.pending_for("c0"), refs.order_client_id_for("R0")),
        (None, None),
        "the oldest live order goes"
    );
    assert!(refs.pending_for("c1").is_some() && refs.pending_for("one-more").is_some());
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
    assert_eq!(refs.order_client_id_for("R9"), None, "and its reverse half with it");
    assert!(refs.pending_for("c10").is_some());
    assert!(refs.pending_for(&format!("c{}", MAX_DEAL_REFS + 9)).is_some());
    assert!(
        refs.by_order_client_id.as_map().len() <= MAX_DEAL_REFS
            && refs.by_ref.len() <= MAX_DEAL_REFS
    );

    refs.record("rej", "RREJ", true);
    refs.record_stream_only("rej", "RCANCEL");
    refs.forget("rej");
    assert_eq!(
        (
            refs.pending_for("rej"),
            refs.order_client_id_for("RREJ"),
            refs.order_client_id_for("RCANCEL")
        ),
        (None, None, None)
    );
    assert!(refs.pending_for("c11").is_some(), "others untouched");
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
    let response = trade_stream_agent()
        .post(&format!("{base}/lightstreamer/create_session.txt"))
        .header("Content-Type", "application/x-www-form-urlencoded")
        .send(b"LS_user=acct&LS_password=CST-test%7CXST-test");
    assert_eq!(target_hits.load(std::sync::atomic::Ordering::SeqCst), 0, "the target was dialled");
    assert_eq!(response.expect("a 302 is a response").status().as_u16(), 302);
}
