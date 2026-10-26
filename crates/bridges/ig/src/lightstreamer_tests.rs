//! The TLCP codec's offline tests: the spec's worked example, frame parsing, the wire bytes.

use super::*;
use std::assert_matches;

fn upd(line: &str) -> LsUpdate {
    match parse_frame(line) {
        Some(LsFrame::Update(u)) => u,
        other => panic!("expected Update for {line:?}, got {other:?}"),
    }
}

/// The TLCP 2.1.0 spec's own worked example (chapter 4, "Notification Examples"): a 10-field
/// stock-quote schema driven through the exact update lines the spec prints, asserting the
/// folded state the spec's tables show after each line. This is the codec's conformance
/// fixture — the bytes are the spec's, not ours.
#[test]
fn spec_worked_example_folds_exactly() {
    // schema: timestamp price change min max bid ask open close status
    let mut st = MergeItemState::new(10);

    st.apply(&upd("U,3,1,20:00:33|3.04|0.0|2.41|3.67|3.03|3.04|#|#|$").fields);
    assert_eq!(st.get(0), Some("20:00:33"));
    assert_eq!(st.get_f64(1), Some(3.04));
    assert_eq!(st.get(7), None, "open is null");
    assert_eq!(st.get(8), None, "close is null");
    assert_eq!(st.get(9), Some(""), "status is empty");

    st.apply(&upd("U,3,1,20:00:54|3.07|0.98|||3.06|3.07|||Suspended").fields);
    assert_eq!(st.get_f64(3), Some(2.41), "min unchanged");
    assert_eq!(st.get_f64(5), Some(3.06));
    assert_eq!(st.get(9), Some("Suspended"));

    st.apply(&upd("U,3,1,20:04:16|3.02|-0.65|||3.01|3.02|||$").fields);
    assert_eq!(st.get(9), Some(""), "status back to empty via $");

    st.apply(&upd("U,3,1,20:04:40|^4|3.02|3.03|||").fields);
    assert_eq!(st.get(0), Some("20:04:40"));
    assert_eq!(st.get_f64(1), Some(3.02), "price covered by ^4 run");
    assert_eq!(st.get_f64(6), Some(3.03), "ask updated after the run");

    st.apply(&upd("U,3,1,20:06:10|3.05|0.32|^7").fields);
    assert_eq!(st.get_f64(1), Some(3.05));
    assert_eq!(st.get_f64(6), Some(3.03), "ask inside the trailing ^7 run");

    st.apply(&upd("U,3,1,20:06:49|3.08|1.31|||3.08|3.09|||").fields);
    assert_eq!(st.get_f64(5), Some(3.08));
    assert_eq!(st.get(9), Some(""), "status still empty");
}

#[test]
fn update_head_is_comma_separated_per_spec() {
    let u = upd("U,7,2,1.0855|1.0856");
    assert_eq!((u.sub_id, u.item), (7, 2));
    assert_eq!(
        u.fields,
        vec![FieldDelta::Value("1.0855".into()), FieldDelta::Value("1.0856".into())]
    );
    // A single trailing empty segment is one Unchanged, not zero fields.
    assert_eq!(upd("U,1,1,").fields, vec![FieldDelta::Unchanged]);
}

#[test]
fn update_values_percent_decode_and_run_bounds_hold() {
    let u = upd("U,1,1,a%7Cb|%23leading|c%2Cd");
    assert_eq!(
        u.fields,
        vec![
            FieldDelta::Value("a|b".into()),
            FieldDelta::Value("#leading".into()),
            FieldDelta::Value("c,d".into()),
        ]
    );
    // ^0 and an absurd run are refused (whole line demoted to Other, never a panic).
    assert_matches!(parse_frame("U,1,1,^0|x"), Some(LsFrame::Other(_)));
    assert_matches!(parse_frame("U,1,1,^99999"), Some(LsFrame::Other(_)));
}

#[test]
fn control_frames_parse() {
    assert_eq!(
        parse_frame("CONOK,S1a2b3c,50000,5000,*"),
        Some(LsFrame::Conok {
            session_id: "S1a2b3c".into(),
            request_limit: 50000,
            keepalive_ms: 5000,
            control_link: "*".into(),
        })
    );
    assert_eq!(
        parse_frame("CONERR,2,Requested%20Adapter%20Set%20not%20available"),
        Some(LsFrame::Conerr {
            code: "2".into(),
            message: "Requested Adapter Set not available".into()
        })
    );
    assert_eq!(
        parse_frame("SUBOK,1,1,4"),
        Some(LsFrame::Subok { sub_id: 1, num_items: 1, num_fields: 4 })
    );
    assert_eq!(parse_frame("REQOK,1"), Some(LsFrame::Reqok));
    assert_eq!(
        parse_frame("REQERR,1,17,Data%20Adapter%20not%20found"),
        Some(LsFrame::Reqerr {
            req_id: "1".into(),
            code: "17".into(),
            message: "Data Adapter not found".into()
        })
    );
    assert_eq!(parse_frame("PROBE"), Some(LsFrame::Probe));
    assert_eq!(parse_frame("LOOP,0"), Some(LsFrame::Loop));
    assert_eq!(
        parse_frame("END,31,Session%20closed"),
        Some(LsFrame::End { code: "31".into(), message: "Session closed".into() })
    );
    assert_eq!(
        parse_frame("SERVNAME,Lightstreamer"),
        Some(LsFrame::Other("SERVNAME,Lightstreamer".into()))
    );
    assert_eq!(parse_frame(""), None);
}

#[test]
fn request_builders_pin_the_wire_bytes() {
    assert_eq!(
        create_session_request("ABC123", "CST-c|XST-x"),
        "create_session\r\nLS_adapter_set=DEFAULT&LS_cid=mgQkwtwdysogQz2BJ4Ji%20kOj2Bg\
         &LS_user=ABC123&LS_password=CST-c%7CXST-x"
    );
    // The builder's shape, for each adapter the crate speaks to. (The bytes PRODUCTION sends are pinned
    // end to end by `tests/offline/quote_feed.rs`'s `PRICE_CONTROL`, over the real `Feeds`.)
    assert_eq!(
        subscribe_request(
            1,
            1,
            "Pricing",
            "MERGE",
            "PRICE:ACC1:CS.D.EURUSD.MINI.IP",
            "BIDPRICE1 ASKPRICE1",
            true
        ),
        "control\r\nLS_reqId=1&LS_op=add&LS_subId=1&LS_data_adapter=Pricing\
         &LS_group=PRICE%3AACC1%3ACS.D.EURUSD.MINI.IP&LS_schema=BIDPRICE1%20ASKPRICE1&LS_mode=MERGE\
         &LS_snapshot=true"
    );
    assert_eq!(
        subscribe_request(
            2,
            1,
            "DEFAULT",
            "MERGE",
            "CHART:CS.D.EURUSD.MINI.IP:1MINUTE",
            "UTM",
            false
        ),
        "control\r\nLS_reqId=2&LS_op=add&LS_subId=1&LS_data_adapter=DEFAULT\
         &LS_group=CHART%3ACS.D.EURUSD.MINI.IP%3A1MINUTE&LS_schema=UTM&LS_mode=MERGE\
         &LS_snapshot=false"
    );
}
