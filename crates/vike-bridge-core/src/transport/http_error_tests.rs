//! `VenueApiHttpError` and its helpers, over specs shaped as the three venues declare theirs: each pin is the one the
//! venue's own `rest.rs` held before the merge, so a bridge that moves onto the shared type keeps its words.

use super::*;

static TERSE: VenueApiHttpSpec = VenueApiHttpSpec {
    prefix: "alpaca HTTP",
    network_words: "",
    also_unknown: &[],
    empty_2xx_is_null: true,
    unreadable_2xx_on_write_unknown: false,
    server_error_on_write_unknown: false,
    redirect_on_write_unknown: false,
};
const WORDED_FACTS: VenueApiHttpSpec = VenueApiHttpSpec {
    prefix: "OANDA error",
    network_words: "network error: ",
    also_unknown: &[599],
    empty_2xx_is_null: false,
    unreadable_2xx_on_write_unknown: true,
    server_error_on_write_unknown: false,
    redirect_on_write_unknown: false,
};
static WORDED: VenueApiHttpSpec = WORDED_FACTS;
/// `WORDED` with every owner ruling switched on.
static RULED: VenueApiHttpSpec = VenueApiHttpSpec {
    server_error_on_write_unknown: true,
    redirect_on_write_unknown: true,
    ..WORDED_FACTS
};

fn words(text: &str) -> String {
    field_or_text(text, "errorMessage")
}

/// B-3's knob: a 3xx on a write is unknown under the rule, and the status (a refusal) otherwise and on a read.
#[test]
fn a_redirect_on_a_write_is_unknown_only_under_the_rule() {
    let e = RULED.finish(302, "", true, words).unwrap_err();
    assert_eq!(
        (e.status, e.message.as_str()),
        (VenueApiHttpError::STATUS_OUTCOME_UNKNOWN, "http 302, outcome unknown: ")
    );
    assert_eq!(RULED.finish(302, "", false, words).unwrap_err().status, 302);
    assert_eq!(WORDED.finish(307, "", true, words).unwrap_err().status, 307);
}

/// B-1's knob: a 5xx on a write is unknown under the rule, its words led by the status; a read, a 4xx, and a status
/// that already says unknown keep theirs; with the rule off a bare 500 is the venue's refusal.
#[test]
fn a_5xx_on_a_write_is_unknown_only_under_the_rule() {
    let e = RULED.finish(500, r#"{"errorMessage":"boom"}"#, true, words).unwrap_err();
    assert_eq!(e.status, VenueApiHttpError::STATUS_OUTCOME_UNKNOWN);
    assert_eq!(e.message, "http 500, outcome unknown: boom");
    assert!(e.outcome_unknown());
    assert_eq!(
        RULED.finish(500, "boom", false, words).unwrap_err().status,
        500,
        "a read keeps its status"
    );
    assert_eq!(RULED.finish(422, "no", true, words).unwrap_err().status, 422);
    for already in [502, 599] {
        let e = RULED.finish(already, "gw", true, words).unwrap_err();
        assert_eq!(
            (e.status, e.message.as_str()),
            (already, "gw"),
            "its status says unknown already"
        );
    }
    let e = WORDED.finish(500, "boom", true, words).unwrap_err();
    assert_eq!((e.status, e.outcome_unknown()), (500, false), "the rule is off: {e}");
}

#[test]
fn the_display_is_the_venues_prefix_then_the_status_and_message() {
    assert_eq!(
        TERSE.error(403, "request is forbidden").to_string(),
        "alpaca HTTP 403: request is forbidden"
    );
    assert_eq!(WORDED.error(0, "network error: x").to_string(), "OANDA error 0: network error: x");
}

/// `1`, the gateway set and the venue's own extra statuses are unknown; a refusal and a never-sent `0` are not.
#[test]
fn the_outcome_is_unknown_for_status_1_the_gateway_set_and_the_venues_own() {
    for s in [1, 502, 503, 504, 520, 530] {
        assert!(TERSE.error(s, "").outcome_unknown(), "{s}");
    }
    assert!(WORDED.error(599, "").outcome_unknown());
    for s in [0, 2, 200, 400, 401, 403, 422, 500, 599] {
        assert!(!TERSE.error(s, "").outcome_unknown(), "{s}");
    }
}

/// A send failure provably before the write is a clean `0` in the venue's network words; a timeout may have followed
/// the write and is unknown; a read failure after the headers is unknown.
#[test]
fn a_send_failure_is_definite_only_before_the_write() {
    let before = TERSE.write_send_error(&ureq::Error::HostNotFound);
    assert_eq!((before.status, before.outcome_unknown()), (0, false));
    assert_eq!(
        before.message,
        ureq::Error::HostNotFound.to_string(),
        "alpaca's words carry the cause alone"
    );
    let before = WORDED.write_send_error(&ureq::Error::HostNotFound);
    assert_eq!(before.message, format!("network error: {}", ureq::Error::HostNotFound));
    for spec in [&TERSE, &WORDED] {
        let after = spec.write_send_error(&ureq::Error::Timeout(ureq::Timeout::Global));
        assert_eq!(after.status, VenueApiHttpError::STATUS_OUTCOME_UNKNOWN);
        assert!(after.message.starts_with("network error: "), "{after}");
        let read = spec.write_read_error("reset");
        assert_eq!(read.status, VenueApiHttpError::STATUS_OUTCOME_UNKNOWN);
        assert_eq!(read.message, "network error reading the reply: reset");
        assert_eq!(spec.network("down").message, format!("{}down", spec.network_words));
    }
}

#[test]
fn finish_parses_a_2xx_and_words_a_refusal_in_the_venues_grammar() {
    assert_eq!(TERSE.finish(200, r#"{"id":"x"}"#, true, words).unwrap()["id"], "x");
    let e = WORDED.finish(400, r#"{"errorMessage":"bad"}"#, true, words).unwrap_err();
    assert_eq!((e.status, e.message.as_str()), (400, "bad"));
    assert_eq!(WORDED.finish(404, "<html>", false, words).unwrap_err().message, "<html>");
    assert!(WORDED.finish(504, "<html>", true, words).unwrap_err().outcome_unknown());
}

/// The empty body: `null` where the venue answers so, "bad json" elsewhere.
#[test]
fn an_empty_2xx_is_null_only_where_the_spec_says_so() {
    assert!(TERSE.finish(200, " \n", true, words).unwrap().is_null());
    assert!(WORDED.finish(201, "", false, words).unwrap_err().message.starts_with("bad json"));
}

/// B-2's knob: an unreadable 2xx on a write is unknown where the spec switches it on, and keeps its 2xx status (a
/// refusal to the caller) where it does not, and on every read.
#[test]
fn an_unreadable_2xx_on_a_write_is_unknown_only_under_the_rule() {
    let e = WORDED.finish(201, "<html>", true, words).unwrap_err();
    assert_eq!(e.status, VenueApiHttpError::STATUS_OUTCOME_UNKNOWN);
    assert!(e.message.starts_with("bad json"), "{e}");
    let e = WORDED.finish(200, "<html>", false, words).unwrap_err();
    assert_eq!(e.status, 200, "a read keeps its status");
    let e = TERSE.finish(200, "not json", true, words).unwrap_err();
    assert_eq!((e.status, e.outcome_unknown()), (200, false), "the rule is off: {e}");
}

#[test]
fn field_or_text_reads_the_one_field_else_the_body() {
    assert_eq!(field_or_text(r#"{"errorCode":"error.x"}"#, "errorCode"), "error.x");
    assert_eq!(field_or_text(r#"{"errorCode":7}"#, "errorCode"), r#"{"errorCode":7}"#);
    assert_eq!(field_or_text("plain", "errorCode"), "plain");
}
