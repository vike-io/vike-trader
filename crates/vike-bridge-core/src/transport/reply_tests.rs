//! `classify_rest_reply` over each venue shape a spec states, through a real agent and a one-shot peer. The binance
//! shape (`UreqTransport`) has its end-to-end pins in `transport_error_kind_tests.rs`; these pin the other knobs.

use super::super::transport_error_kind_tests::serve;
use super::*;

const CODE: RestReplyCode = RestReplyCode { field: "retCode", msg: "retMsg", string_ok: false };
const STRING_CODE: RestReplyCode = RestReplyCode { field: "code", msg: "msg", string_ok: true };

/// bybit's shape: an integer code, a gateway status unknown on every call before the body.
const ENVELOPE_GATEWAY_FIRST: RestReplySpec = RestReplySpec {
    code: Some(CODE),
    no_verdict_on_5xx: &[],
    gateway_first: RestReplyGatewayFirst::OnEveryCall,
    server_error_on_write_unknown: true,
    redirect_on_write_unknown: false,
    words: None,
    unknown_body_head: 0,
};
/// hyperliquid's shape: no code in the error body.
const STATUS_ONLY: RestReplySpec = RestReplySpec {
    code: None,
    no_verdict_on_5xx: &[],
    gateway_first: RestReplyGatewayFirst::Never,
    server_error_on_write_unknown: true,
    redirect_on_write_unknown: false,
    words: None,
    unknown_body_head: 0,
};
/// vike-ibkr cpapi's shape: no code, the gateway set alone leaves a write unknown, a read keeps its status, a 3xx on a
/// write is unknown.
const GATEWAY_ONLY: RestReplySpec = RestReplySpec {
    code: None,
    no_verdict_on_5xx: &[],
    gateway_first: RestReplyGatewayFirst::OnWrites,
    server_error_on_write_unknown: false,
    redirect_on_write_unknown: true,
    words: None,
    unknown_body_head: 0,
};

/// B-3's write half: a 3xx on a write is unknown before its body is read (no note) where the spec says so, and the
/// status-as-code otherwise; a read keeps it either way.
#[test]
fn a_redirect_on_a_write_is_unknown_only_where_the_spec_says_so() {
    let moved = reply(302, "");
    let (got, notes) = ask(&GATEWAY_ONLY, true, moved);
    let e = got.unwrap_err();
    assert_eq!(
        (e.code, e.msg.as_str()),
        (E_TIMEOUT_AMBIGUOUS, "network error: redirect http 302, outcome unknown")
    );
    assert!(notes.is_empty(), "{notes:?}");
    assert_eq!(code(&GATEWAY_ONLY, false, moved), 302);
    assert_eq!(code(&STATUS_ONLY, true, moved), 302);
    let coded = reply(307, r#"{"retCode":10001,"retMsg":"m"}"#);
    let ruled = RestReplySpec { redirect_on_write_unknown: true, ..ENVELOPE_GATEWAY_FIRST };
    assert_eq!(code(&ruled, true, coded), E_TIMEOUT_AMBIGUOUS);
    assert_eq!(code(&ruled, false, coded), 10001);
}

fn reply(status: u16, body: &str) -> &'static str {
    Box::leak(
        format!(
            "HTTP/1.1 {status} X\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        )
        .into_boxed_str(),
    )
}

/// One request (`POST` = a write, `GET` = a read) answered `answer`, through `spec`; the notes it raised.
fn ask(
    spec: &RestReplySpec,
    write: bool,
    answer: &'static str,
) -> (Result<serde_json::Value, VenueApiError>, Vec<String>) {
    let url = format!("{}/x", serve(answer));
    let agent = crate::http::blocking_agent();
    let sent = if write { agent.post(&url).send_empty() } else { agent.get(&url).call() };
    let mut notes = Vec::new();
    let got = classify_rest_reply(sent, write, spec, |note| {
        notes.push(match note {
            RestReplyNote::SendFailed(_) => "send".to_string(),
            RestReplyNote::Refused { status, body } => {
                format!("refused {status} {}", body.is_some())
            }
        })
    });
    (got, notes)
}

fn code(spec: &RestReplySpec, write: bool, answer: &'static str) -> i64 {
    ask(spec, write, answer).0.unwrap_err().code
}

#[test]
fn a_2xx_is_the_json_and_an_unreadable_one_is_unknown_on_a_write_only() {
    for spec in [ENVELOPE_GATEWAY_FIRST, STATUS_ONLY, GATEWAY_ONLY] {
        let (got, notes) = ask(&spec, true, reply(200, r#"{"a":1}"#));
        assert_eq!(got.unwrap()["a"], 1);
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(code(&spec, true, reply(200, "<html>")), E_TIMEOUT_AMBIGUOUS);
        let e = ask(&spec, false, reply(200, "<html>")).0.unwrap_err();
        assert_eq!((e.code, e.msg.starts_with("bad json: ")), (0, true), "{e}");
    }
}

/// The envelope: the venue's own code and message win; a body with no code is the status and the body; an unreadable
/// body is the status and "http error". Each raises one note with the body it parsed.
#[test]
fn a_refusal_carries_the_venues_envelope_or_the_status() {
    let (got, notes) =
        ask(&ENVELOPE_GATEWAY_FIRST, true, reply(400, r#"{"retCode":10001,"retMsg":"bad"}"#));
    assert_eq!(got.unwrap_err(), VenueApiError { code: 10001, msg: "bad".into() });
    assert_eq!(notes, ["refused 400 true"]);
    let e = ask(&ENVELOPE_GATEWAY_FIRST, true, reply(400, r#"{"note":"x"}"#)).0.unwrap_err();
    assert_eq!(e, VenueApiError { code: 400, msg: r#"{"note":"x"}"#.into() });
    let e = ask(&ENVELOPE_GATEWAY_FIRST, true, reply(400, r#"{"retCode":"10001"}"#)).0.unwrap_err();
    assert_eq!(e.code, 400, "a string is no integer code for this envelope: {e}");
    let (got, notes) = ask(&ENVELOPE_GATEWAY_FIRST, false, reply(404, "<html>"));
    assert_eq!(got.unwrap_err(), VenueApiError { code: 404, msg: "http error".into() });
    assert_eq!(notes, ["refused 404 false"]);
}

/// okx's string code: present is the envelope; a value that does not read, or reads as the status, is the status and
/// counts as no venue code (so a write's 5xx stays unknown).
#[test]
fn a_string_code_reads_and_falls_back_to_the_status() {
    let spec = RestReplySpec { code: Some(STRING_CODE), ..ENVELOPE_GATEWAY_FIRST };
    assert_eq!(code(&spec, true, reply(400, r#"{"code":"51000","msg":"m"}"#)), 51000);
    assert_eq!(code(&spec, true, reply(400, r#"{"code":51000,"msg":"m"}"#)), 51000);
    let e = ask(&spec, true, reply(400, r#"{"code":true,"msg":"m"}"#)).0.unwrap_err();
    assert_eq!(e, VenueApiError { code: 400, msg: "m".into() });
    assert_eq!(code(&spec, true, reply(500, r#"{"code":"oops","msg":"m"}"#)), E_TIMEOUT_AMBIGUOUS);
    assert_eq!(code(&spec, true, reply(500, r#"{"code":"500","msg":"m"}"#)), E_TIMEOUT_AMBIGUOUS);
    assert_eq!(code(&spec, true, reply(500, r#"{"code":"50001","msg":"m"}"#)), 50001);
    assert_eq!(code(&spec, false, reply(500, r#"{"code":"oops","msg":"m"}"#)), 500);
}

/// Owner ruling T-D7: a body code `0` (an integer, or okx's string `"0"`) is no code. The venue answered, so the refusal
/// is the HTTP status with the venue's message, never `0`, the never-sent sentinel (`ErrorKind::Network`, retried).
#[test]
fn a_body_code_of_zero_is_no_code_and_the_status_is() {
    let okx = RestReplySpec { code: Some(STRING_CODE), ..ENVELOPE_GATEWAY_FIRST };
    for (spec, body) in [
        (&okx, r#"{"code":"0","msg":"m"}"#),
        (&okx, r#"{"code":0,"msg":"m"}"#),
        (&ENVELOPE_GATEWAY_FIRST, r#"{"retCode":0,"retMsg":"m"}"#),
    ] {
        for write in [true, false] {
            let e = ask(spec, write, reply(400, body)).0.unwrap_err();
            assert_eq!(e, VenueApiError { code: 400, msg: "m".into() }, "{body} write {write}");
        }
    }
}

/// The gateway set before the body: on every call (no note, the body is never read), or on writes only.
#[test]
fn a_gateway_status_answers_before_the_body_on_the_calls_the_spec_names() {
    let coded = reply(502, r#"{"retCode":10001,"retMsg":"m"}"#);
    for write in [true, false] {
        let (got, notes) = ask(&ENVELOPE_GATEWAY_FIRST, write, coded);
        assert_eq!(got.unwrap_err().code, E_TIMEOUT_AMBIGUOUS);
        assert!(notes.is_empty(), "{notes:?}");
    }
    assert_eq!(code(&GATEWAY_ONLY, true, reply(503, "busy")), E_TIMEOUT_AMBIGUOUS);
    let e = ask(&GATEWAY_ONLY, false, reply(503, "busy")).0.unwrap_err();
    assert_eq!(e, VenueApiError { code: 503, msg: "busy".into() }, "a read keeps its status");
    let never =
        RestReplySpec { gateway_first: RestReplyGatewayFirst::Never, ..ENVELOPE_GATEWAY_FIRST };
    assert_eq!(code(&never, true, coded), 10001, "read first, the venue's code is its word");
    assert_eq!(code(&never, false, coded), 10001);
}

/// No code in the error body: a write's 5xx is decided before the read (no note); a read is the status and the raw text.
#[test]
fn a_status_only_venue_decides_a_writes_5xx_before_the_body() {
    let (got, notes) = ask(&STATUS_ONLY, true, reply(500, "oops"));
    assert_eq!(got.unwrap_err().code, E_TIMEOUT_AMBIGUOUS);
    assert!(notes.is_empty(), "{notes:?}");
    let (got, notes) = ask(&STATUS_ONLY, false, reply(500, "oops"));
    assert_eq!(got.unwrap_err(), VenueApiError { code: 500, msg: "oops".into() });
    assert_eq!(notes, ["refused 500 false"]);
    assert_eq!(code(&STATUS_ONLY, true, reply(422, "no")), 422);
}

/// B-1's knob: `false` keeps a bare 500 on a write as the venue's refusal (only the gateway set is unknown).
#[test]
fn a_bare_500_on_a_write_is_unknown_only_where_the_spec_says_so() {
    assert_eq!(code(&STATUS_ONLY, true, reply(500, "x")), E_TIMEOUT_AMBIGUOUS);
    assert_eq!(code(&GATEWAY_ONLY, true, reply(500, "x")), 500);
    let coded = RestReplySpec { server_error_on_write_unknown: false, ..ENVELOPE_GATEWAY_FIRST };
    assert_eq!(
        code(&coded, true, reply(500, r#"{"retCode":0,"retMsg":"m"}"#)),
        500,
        "code 0 is no code: the status is"
    );
    assert_eq!(
        code(&ENVELOPE_GATEWAY_FIRST, true, reply(500, r#"{"retCode":0,"retMsg":"m"}"#)),
        E_TIMEOUT_AMBIGUOUS
    );
}

#[test]
fn a_send_failure_is_classified_and_noted() {
    let sent = crate::http::blocking_agent().post("http://127.0.0.1:1/x").send_empty();
    let mut notes = 0;
    let e = classify_rest_reply(sent, true, &STATUS_ONLY, |n| {
        assert!(matches!(n, RestReplyNote::SendFailed(_)));
        notes += 1;
    })
    .unwrap_err();
    assert_eq!((e.code, notes), (0, 1), "{e}");
}

/// A venue's own words: each verdict the engine writes goes through `words` (the code stays the engine's), the body's
/// head (as it came, untrimmed) rides a verdict made before the body is read, and a venue with no `words` keeps the
/// shared ones.
#[test]
fn a_venues_own_words_carry_every_verdict_and_the_head() {
    fn mine(word: &RestReplyWord<'_>) -> String {
        match word {
            RestReplyWord::SendFailed(_) => "sent nothing".to_string(),
            RestReplyWord::Redirect { status, head } => format!("redirect {status}:{head}"),
            RestReplyWord::Gateway { status, body } => format!("gateway {status}:{body}"),
            RestReplyWord::BodyUnread(_) => "unread".to_string(),
            RestReplyWord::BadJson { status, text, .. } => format!("bad {status}: {text}"),
            RestReplyWord::Refused { status, text, parsed } => {
                format!("refused {status} json={}: {text}", parsed.is_ok())
            }
        }
    }
    let spec = RestReplySpec { words: Some(mine), unknown_body_head: 4, ..GATEWAY_ONLY };
    let got = |write, answer| ask(&spec, write, answer).0.unwrap_err();
    let moved = VenueApiError { code: E_TIMEOUT_AMBIGUOUS, msg: "redirect 302: mov".into() };
    assert_eq!(got(true, reply(302, " moved on ")), moved);
    assert_eq!(got(true, reply(503, "busy now")).msg, "gateway 503:busy");
    assert_eq!(got(true, reply(200, "<html>")).msg, "bad 200: <html>");
    let unread = VenueApiError { code: 0, msg: "bad 200: <html>".into() };
    assert_eq!(got(false, reply(200, "<html>")), unread);
    let refused = VenueApiError { code: 503, msg: "refused 503 json=false: busy".into() };
    assert_eq!(got(false, reply(503, "busy")), refused);
    assert_eq!(got(true, reply(400, "{}")).msg, "refused 400 json=true: {}");
    let sent = crate::http::blocking_agent().post("http://127.0.0.1:1/x").send_empty();
    let e = classify_rest_reply(sent, true, &spec, |_| {}).unwrap_err();
    assert_eq!((e.code, e.msg.as_str()), (0, "sent nothing"));
    // `u64::MAX` keeps the whole body, decided before the read or after it (an envelope venue reads first).
    let whole = RestReplySpec { unknown_body_head: u64::MAX, ..spec };
    let body = " the whole body ";
    assert_eq!(
        ask(&whole, true, reply(502, body)).0.unwrap_err().msg,
        format!("gateway 502:{body}")
    );
    let late = RestReplySpec { words: Some(mine), ..STATUS_ONLY };
    let late =
        RestReplySpec { code: Some(CODE), gateway_first: RestReplyGatewayFirst::Never, ..late };
    assert_eq!(
        ask(&late, true, reply(502, body)).0.unwrap_err().msg,
        format!("gateway 502:{body}")
    );
    let shared = RestReplySpec { unknown_body_head: 4, ..GATEWAY_ONLY };
    assert_eq!(
        ask(&shared, true, reply(503, "busy")).0.unwrap_err().msg,
        "network error: gateway http 503, outcome unknown"
    );
}
