//! [`SessionLink`]: up only when both paths answered and the stream's last line is fresh, down in the lost path's own
//! words (the order session's first), and unknown for everything else.

use std::time::{Duration, Instant};

use super::*;
use crate::transport::IG_HTTP;

fn net_err() -> VenueApiHttpError {
    IG_HTTP.error(0, "network error: connection reset".to_string())
}

/// **Nothing said is not a path that works**: a client whose lanes have not answered, or whose stream alone has, or
/// whose order session alone has, reads unknown.
#[test]
fn a_path_that_has_not_answered_on_both_lanes_is_unknown_never_up() {
    let link = SessionLink::default();
    assert_eq!(link.state(), VenueLinkState::Unknown);
    link.stream_answered();
    assert_eq!(link.state(), VenueLinkState::Unknown, "the order session has not answered");
    let link = SessionLink::default();
    link.rest_answered();
    assert_eq!(link.state(), VenueLinkState::Unknown, "the trade stream has not answered");
}

/// **Up is recent**: both answered reads up, and a stream whose last line is older than the bound is unknown again
/// (its reader is not reading), however recently the order session answered.
#[test]
fn both_answered_is_up_until_the_streams_last_line_is_too_old() {
    let link = SessionLink::default();
    link.rest_answered();
    link.stream_answered();
    let now = Instant::now();
    assert_eq!(link.state_at(now), VenueLinkState::Up);
    assert_eq!(link.state_at(now + FRESH_FOR + Duration::from_secs(1)), VenueLinkState::Unknown);
    assert!(
        FRESH_FOR >= Duration::from_secs(30),
        "at least the read's own idle bound: {FRESH_FOR:?}"
    );
}

/// **A lost order session is down, refuses orders, and outranks the stream**: its login failed, so the exec thread has
/// ended and every order is refused, whatever the stream says.
#[test]
fn a_lost_order_session_is_down_with_orders_refused_and_outranks_the_stream() {
    let link = SessionLink::default();
    link.stream_answered();
    link.rest_login_lost("IG error 403: error.security.api-key-disabled");
    link.stream_lost("stream read: timed out");
    let VenueLinkState::Down(VenueLinkDown { why, effect, .. }) = link.state() else {
        panic!("{:?}", link.state())
    };
    assert_eq!(
        why,
        "the order session's login failed: IG error 403: error.security.api-key-disabled"
    );
    assert_eq!(effect, LOGIN_LOST);
    assert!(effect.contains("refused") && effect.contains("account.active"), "{effect}");
}

/// **A lost trade stream is down, and its words say the orders still go out**; a line read again brings it back.
#[test]
fn a_lost_trade_stream_is_down_with_orders_still_going_out_until_it_reads_again() {
    let link = SessionLink::default();
    link.rest_answered();
    link.stream_lost("stream read: timed out reading body");
    assert_eq!(
        link.state(),
        VenueLinkState::Down(VenueLinkDown {
            why: "the trade stream: stream read: timed out reading body".to_string(),
            effect: STREAM_REDIALING,
            failed_restarts: 0,
            retry_in: Duration::ZERO,
        })
    );
    link.stream_answered();
    assert_eq!(link.state(), VenueLinkState::Up);

    link.stream_gave_up("IG login returned no lightstreamerEndpoint");
    let VenueLinkState::Down(VenueLinkDown { effect, .. }) = link.state() else {
        panic!("{:?}", link.state())
    };
    assert_eq!(effect, STREAM_GAVE_UP, "a thread that ended redials nothing");
}

/// **The order session's word is each call's answer**: any status IG answers with is an answer (a `/confirms` 404 is
/// IG saying "not yet"), but a call IG never answered, a 401 the ladder could not cure and a gateway's ambiguous 5xx
/// are a lost path; the next answered call brings it back.
#[test]
fn each_call_of_the_order_session_states_its_answer() {
    let link = SessionLink::default();
    link.stream_answered();
    for answered in [200_u16, 404, 400] {
        link.rest_heard(&Ok((answered, String::new())));
        assert_eq!(link.state(), VenueLinkState::Up, "HTTP {answered} is IG answering");
    }
    link.rest_heard(&Err(net_err()));
    let VenueLinkState::Down(VenueLinkDown { why, effect, .. }) = link.state() else {
        panic!("{:?}", link.state())
    };
    assert_eq!(
        why,
        "the order session: IG did not answer its last call (network error: connection reset)"
    );
    assert_eq!(effect, CALL_LOST);
    for lost in [401_u16, 503] {
        link.rest_heard(&Ok((200, String::new())));
        link.rest_heard(&Ok((lost, String::new())));
        assert!(matches!(link.state(), VenueLinkState::Down(_)), "HTTP {lost}: {:?}", link.state());
    }
    link.rest_heard(&Ok((200, String::new())));
    assert_eq!(link.state(), VenueLinkState::Up);
}

/// **A re-login that FAILED is said by the login error's own kind** (T-D6, the login's error is the answer): IG
/// refusing it with a status, IG answering it without a usable session, its host never reached, or the login's own
/// words. Never "IG did not answer its last call": IG answered that call, with the 401.
#[test]
fn a_failed_re_login_is_said_by_the_login_errors_kind() {
    let landed = "ig: a re-login landed on the account `B`, not `A` this session trades";
    for (login, said) in [
        (
            IG_HTTP.error(403, "error.security.api-key-disabled".to_string()),
            "IG refused the re-login (HTTP 403: error.security.api-key-disabled)".to_string(),
        ),
        (
            net_err(),
            "IG's login could not be reached for a re-login (network error: connection reset)"
                .to_string(),
        ),
        (
            IG_HTTP.error(200, "login ok but CST / X-SECURITY-TOKEN header missing".to_string()),
            "IG answered the re-login without a usable session (HTTP 200: login ok but CST / \
             X-SECURITY-TOKEN header missing)"
                .to_string(),
        ),
        (IG_HTTP.error(2, landed.to_string()), format!("the re-login failed ({landed})")),
    ] {
        let link = SessionLink::default();
        link.stream_answered();
        link.rest_relogin_failed(&login);
        let VenueLinkState::Down(VenueLinkDown { why, effect, .. }) = link.state() else {
            panic!("{:?}", link.state())
        };
        assert_eq!(why, format!("the order session: {said}"));
        assert!(!why.contains("did not answer"), "{why}");
        assert_eq!(effect, RELOGIN_LOST);
    }
}

/// A new exec client forgets what the last one said, and a venue's long error text is cut, never carried whole.
#[test]
fn a_reset_forgets_both_paths_and_a_long_reason_is_cut() {
    let link = SessionLink::default();
    link.rest_login_lost(&"x".repeat(1000));
    let VenueLinkState::Down(VenueLinkDown { why, .. }) = link.state() else {
        panic!("{:?}", link.state())
    };
    assert!(why.chars().count() < 300, "{}", why.len());
    link.reset();
    assert_eq!(link.state(), VenueLinkState::Unknown);
}

/// The cell is the account's: the same label finds what its lanes wrote, another label finds its own.
#[test]
fn each_account_label_has_its_own_cell() {
    let mine = AccountLabel::parse("IGLINKA").expect("a legal label");
    let other = AccountLabel::parse("IGLINKB").expect("a legal label");
    session_link(&mine).rest_login_lost("refused");
    assert!(matches!(session_link(&mine).state(), VenueLinkState::Down(_)));
    assert_eq!(session_link(&other).state(), VenueLinkState::Unknown);
}
