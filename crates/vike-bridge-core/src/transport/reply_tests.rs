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
    plain_text_errors: false,
    verdict_on_5xx: &[],
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
    plain_text_errors: false,
    verdict_on_5xx: &[],
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
    plain_text_errors: false,
    verdict_on_5xx: &[],
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
    ask_carrying(spec, write, answer, &[])
}

/// [`ask`] for a request that carried `credentials`.
fn ask_carrying(
    spec: &RestReplySpec,
    write: bool,
    answer: &'static str,
    credentials: &[&str],
) -> (Result<serde_json::Value, VenueApiError>, Vec<String>) {
    let url = format!("{}/x", serve(answer));
    let agent = vike_transport::http::blocking_agent();
    let sent = if write { agent.post(&url).send_empty() } else { agent.get(&url).call() };
    let mut notes = Vec::new();
    let got = classify_rest_reply(sent, write, spec, credentials, |note| {
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

/// The envelope: the venue's own code and message win; a body with no code is the status and the body's summary; an
/// unreadable body is the status and "http error". Each raises one note with the body it parsed.
#[test]
fn a_refusal_carries_the_venues_envelope_or_the_status() {
    let (got, notes) =
        ask(&ENVELOPE_GATEWAY_FIRST, true, reply(400, r#"{"retCode":10001,"retMsg":"bad"}"#));
    assert_eq!(got.unwrap_err(), VenueApiError { code: 10001, msg: "bad".into() });
    assert_eq!(notes, ["refused 400 true"]);
    let e = ask(&ENVELOPE_GATEWAY_FIRST, true, reply(400, r#"{"note":"x"}"#)).0.unwrap_err();
    assert_eq!(e, VenueApiError { code: 400, msg: "12-byte body not shown".into() });
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
    let busy = VenueApiError { code: 503, msg: "4-byte body not shown".into() };
    assert_eq!(e, busy, "a read keeps its status");
    let never =
        RestReplySpec { gateway_first: RestReplyGatewayFirst::Never, ..ENVELOPE_GATEWAY_FIRST };
    assert_eq!(code(&never, true, coded), 10001, "read first, the venue's code is its word");
    assert_eq!(code(&never, false, coded), 10001);
}

/// No code in the error body: a write's 5xx is decided before the read (no note); a read is the status and the body's
/// summary.
#[test]
fn a_status_only_venue_decides_a_writes_5xx_before_the_body() {
    let (got, notes) = ask(&STATUS_ONLY, true, reply(500, "oops"));
    assert_eq!(got.unwrap_err().code, E_TIMEOUT_AMBIGUOUS);
    assert!(notes.is_empty(), "{notes:?}");
    let (got, notes) = ask(&STATUS_ONLY, false, reply(500, "oops"));
    assert_eq!(got.unwrap_err(), VenueApiError { code: 500, msg: "4-byte body not shown".into() });
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

/// The typed-verdict knob (`verdict_on_5xx`): a write's 5xx whose body holds a declared `(field, value)` is the
/// venue's refusal (the status is the code: NOT NOW), read after the body; a body without it, with another value, a
/// non-string value or no JSON stays unknown, and so does every body where nothing is declared (decided before the
/// read, as before) or where a gateway status answers first. A read keeps its status either way.
#[test]
fn a_declared_typed_5xx_body_is_the_venues_verdict_on_a_write() {
    use vike_utility::errors::FailureOutcome::NotNow;
    const TYPED: &[(&str, &str)] = &[("code", "post_only_mode")];
    let typed = RestReplySpec { verdict_on_5xx: TYPED, ..STATUS_ONLY };
    let post_only = reply(503, r#"{"error":"post-only","code":"post_only_mode"}"#);
    let (got, notes) = ask(&typed, true, post_only);
    let e = got.unwrap_err();
    assert_eq!(e, VenueApiError { code: 503, msg: "post-only".into() });
    assert_eq!(e.kind().failure_outcome(), NotNow);
    assert_eq!(notes, ["refused 503 true"]);
    for body in [
        r#"{"error":"order timed out"}"#,
        r#"{"error":"post-only","code":"cancel_only_mode"}"#,
        r#"{"error":"post-only","code":1}"#,
        "post_only_mode",
    ] {
        assert_eq!(code(&typed, true, reply(500, body)), E_TIMEOUT_AMBIGUOUS, "{body}");
    }
    let (got, notes) = ask(&STATUS_ONLY, true, post_only);
    assert_eq!(got.unwrap_err().code, E_TIMEOUT_AMBIGUOUS, "nothing declared: today's verdict");
    assert!(notes.is_empty(), "{notes:?}");
    let gateway_first = RestReplySpec { verdict_on_5xx: TYPED, ..GATEWAY_ONLY };
    assert_eq!(code(&gateway_first, true, post_only), E_TIMEOUT_AMBIGUOUS);
    assert_eq!(code(&typed, false, post_only), 503);
}

#[test]
fn a_send_failure_is_classified_and_noted() {
    let sent = vike_transport::http::blocking_agent().post("http://127.0.0.1:1/x").send_empty();
    let mut notes = 0;
    let e = classify_rest_reply(sent, true, &STATUS_ONLY, &[], |n| {
        assert!(matches!(n, RestReplyNote::SendFailed(_)));
        notes += 1;
    })
    .unwrap_err();
    assert_eq!((e.code, notes), (0, 1), "{e}");
}

/// A venue's own words: each verdict the engine writes goes through `words` (the code stays the engine's) with the
/// body's summary (`body_summary`: the venue's `{"error": …}` sentence, else the size; of the head on a verdict made
/// before the body is read), and a venue with no `words` keeps the shared ones.
#[test]
fn a_venues_own_words_carry_every_verdict_and_the_summary() {
    fn mine(word: &RestReplyWord<'_>) -> String {
        match word {
            RestReplyWord::SendFailed(_) => "sent nothing".to_string(),
            RestReplyWord::Redirect { status, summary } => format!("redirect {status}: {summary}"),
            RestReplyWord::Gateway { status, summary } => format!("gateway {status}: {summary}"),
            RestReplyWord::BodyUnread(_) => "unread".to_string(),
            RestReplyWord::BadJson { status, summary, .. } => format!("bad {status}: {summary}"),
            RestReplyWord::Refused { status, summary, not_json } => {
                format!("refused {status} json={}: {summary}", not_json.is_none())
            }
        }
    }
    let busy = r#"{"error":"busy"}"#;
    let spec = RestReplySpec { words: Some(mine), unknown_body_head: 4, ..GATEWAY_ONLY };
    let got = |write, answer| ask(&spec, write, answer).0.unwrap_err();
    let moved = VenueApiError {
        code: E_TIMEOUT_AMBIGUOUS,
        msg: "redirect 302: 4-byte body not shown".into(),
    };
    assert_eq!(got(true, reply(302, " moved on ")), moved);
    // Four bytes cut out of a JSON body are no JSON.
    assert_eq!(got(true, reply(503, busy)).msg, "gateway 503: 4-byte body not shown");
    assert_eq!(got(true, reply(200, "<html>")).msg, "bad 200: 6-byte body not shown");
    let unread = VenueApiError { code: 0, msg: "bad 200: 6-byte body not shown".into() };
    assert_eq!(got(false, reply(200, "<html>")), unread);
    let refused = VenueApiError { code: 503, msg: "refused 503 json=true: busy".into() };
    assert_eq!(got(false, reply(503, busy)), refused);
    assert_eq!(got(true, reply(400, "busy")).msg, "refused 400 json=false: 4-byte body not shown");
    let sent = vike_transport::http::blocking_agent().post("http://127.0.0.1:1/x").send_empty();
    let e = classify_rest_reply(sent, true, &spec, &[], |_| {}).unwrap_err();
    assert_eq!((e.code, e.msg.as_str()), (0, "sent nothing"));
    // `u64::MAX` reads the whole body, decided before the read or after it (an envelope venue reads first).
    let whole = RestReplySpec { unknown_body_head: u64::MAX, ..spec };
    assert_eq!(ask(&whole, true, reply(502, busy)).0.unwrap_err().msg, "gateway 502: busy");
    let late = RestReplySpec { words: Some(mine), ..STATUS_ONLY };
    let late =
        RestReplySpec { code: Some(CODE), gateway_first: RestReplyGatewayFirst::Never, ..late };
    assert_eq!(ask(&late, true, reply(502, busy)).0.unwrap_err().msg, "gateway 502: busy");
    let shared = RestReplySpec { unknown_body_head: 4, ..GATEWAY_ONLY };
    assert_eq!(
        ask(&shared, true, reply(503, "busy")).0.unwrap_err().msg,
        "network error: gateway http 503, outcome unknown"
    );
}

/// **A venue that declares plain-text errors shows its own short refusal; nothing else does** (`body_summary`): one
/// line of at most 160 characters with no `<` is the venue's words, and a page, several lines, a longer body or a
/// venue that declares nothing is the size alone. A JSON body's `error` sentence is shown for every venue.
#[test]
fn a_declared_plain_text_venue_shows_its_own_short_refusal() {
    let plain = RestReplySpec { plain_text_errors: true, ..STATUS_ONLY };
    let refused =
        |spec: &RestReplySpec, body: &str| ask(spec, false, reply(400, body)).0.unwrap_err().msg;
    let minimum = "Order must have minimum value of $10.";
    assert_eq!(refused(&plain, &format!(" {minimum}\n")), minimum);
    assert_eq!(refused(&STATUS_ONLY, minimum), "37-byte body not shown", "it declared nothing");
    let long = "x".repeat(161);
    for body in ["<html>no</html>", "two\nlines", long.as_str(), ""] {
        assert_eq!(
            refused(&plain, body),
            format!("{}-byte body not shown", body.len()),
            "{body:?}"
        );
    }
    assert_eq!(refused(&STATUS_ONLY, r#"{"error":"Bad Gateway"}"#), "Bad Gateway");
}

/// **No secret in an error's text (plan S10).** A proxy or CDN page in front of the venue can echo the request,
/// credential headers included, and so can a declared plain-text venue's short line or a JSON `error` sentence. Over
/// every spec shape (plain-text declared or not), verdict, call and head, and with words that print every field they
/// are handed, the error holds none of the values the request carried; the venue's own words still come through,
/// redacted.
#[test]
fn an_error_built_from_a_body_echoing_credential_headers_holds_none_of_them() {
    fn every_field(word: &RestReplyWord<'_>) -> String {
        format!("{word:?}")
    }
    // The last is short (an okx passphrase can be): redacted where it stands as a word.
    const ECHOED: [&str; 4] = ["key-7f3a", "pass-9b2c", "0x5d1e9a7c", "Pw4z"];
    let [key, passphrase, signature, short] = ECHOED;
    let echo = format!(
        "POLY_API_KEY: {key} POLY_PASSPHRASE: {passphrase} POLY_SIGNATURE: {signature} \
         OK-ACCESS-PASSPHRASE: {short}"
    );
    let bodies = [
        format!("<html><body>502 Bad Gateway<pre>{echo}</pre></body></html>"),
        serde_json::json!({ "error": "Bad Gateway", "request": echo }).to_string(),
        serde_json::json!({ "headers": echo }).to_string(),
        serde_json::json!({ "error": format!("refused: {echo}") }).to_string(),
        echo.clone(),
    ];
    // binance's shape: an integer code, the body read before a write's 5xx is judged.
    let read_first = RestReplySpec {
        gateway_first: RestReplyGatewayFirst::Never,
        redirect_on_write_unknown: true,
        ..ENVELOPE_GATEWAY_FIRST
    };
    let plain = RestReplySpec { plain_text_errors: true, ..STATUS_ONLY };
    let specs = [
        ENVELOPE_GATEWAY_FIRST,
        read_first,
        STATUS_ONLY,
        GATEWAY_ONLY,
        plain,
        RestReplySpec { words: Some(every_field), unknown_body_head: u64::MAX, ..read_first },
        RestReplySpec { words: Some(every_field), unknown_body_head: u64::MAX, ..STATUS_ONLY },
        RestReplySpec { words: Some(every_field), unknown_body_head: u64::MAX, ..GATEWAY_ONLY },
        RestReplySpec { words: Some(every_field), unknown_body_head: u64::MAX, ..plain },
    ];
    for spec in &specs {
        for body in &bodies {
            for status in [200, 302, 400, 502] {
                for write in [true, false] {
                    let answer = reply(status, body);
                    let Err(e) = ask_carrying(spec, write, answer, &ECHOED).0 else { continue };
                    let (shown, debugged) = (e.to_string(), format!("{e:?}"));
                    for secret in ECHOED {
                        // secret-absent: a value the request carried never reaches the error's text
                        assert!(
                            !shown.contains(secret) && !debugged.contains(secret),
                            "{status} write {write}: {debugged}"
                        );
                    }
                }
            }
        }
    }
    let shown = |body: &str| ask_carrying(&plain, false, reply(400, body), &ECHOED).0.unwrap_err();
    let redacted = "POLY_API_KEY: [redacted] POLY_PASSPHRASE: [redacted] POLY_SIGNATURE: [redacted] \
                    OK-ACCESS-PASSPHRASE: [redacted]";
    assert_eq!(shown(&echo), VenueApiError { code: 400, msg: redacted.into() });
    assert_eq!(shown(&bodies[1]).msg, "Bad Gateway");
    assert_eq!(shown(&bodies[3]).msg, format!("refused: {redacted}"));
}
