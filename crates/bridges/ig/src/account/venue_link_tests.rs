//! [`SessionLink`] over the account's cell: up only when both paths answered and the stream's last line is fresh, down
//! in the lost path's own words (the order session's first), and unknown for everything else.

use std::time::{Duration, Instant};

use super::*;
use crate::transport::IG_HTTP;

fn net_err() -> VenueApiHttpError {
    IG_HTTP.error(0, "network error: connection reset".to_string())
}

/// A client's link claimed on a cell of its own, and that cell (what the gatewayhub's status verb reads).
fn claimed() -> (SessionLink, VenueLink) {
    let (link, cell) = (SessionLink::default(), VenueLink::default());
    link.claim(&cell);
    (link, cell)
}

/// **Nothing said is not a path that works**: a client whose lanes have not answered, or whose stream alone has, or
/// whose order session alone has, reads unknown.
#[test]
fn a_path_that_has_not_answered_on_both_lanes_is_unknown_never_up() {
    let (link, cell) = claimed();
    assert_eq!(cell.state(), VenueLinkState::Unknown);
    link.stream_answered();
    assert_eq!(cell.state(), VenueLinkState::Unknown, "the order session has not answered");
    let (link, cell) = claimed();
    link.rest_answered();
    assert_eq!(cell.state(), VenueLinkState::Unknown, "the trade stream has not answered");
}

/// **Up is recent**: both answered reads up, and a stream whose last line is older than the bound is unknown again
/// (its reader is not reading), however recently the order session answered.
#[test]
fn both_answered_is_up_until_the_streams_last_line_is_too_old() {
    let (link, cell) = claimed();
    link.rest_answered();
    link.stream_answered();
    assert_eq!(cell.state(), VenueLinkState::Up);
    if let Some(old) = Instant::now().checked_sub(FRESH_FOR + Duration::from_secs(1)) {
        link.stream_answered_at(old);
        assert_eq!(cell.state(), VenueLinkState::Unknown);
    }
    assert!(
        FRESH_FOR >= Duration::from_secs(30),
        "at least the read's own idle bound: {FRESH_FOR:?}"
    );
}

/// **A lost order session is down, refuses orders, and outranks the stream**: its login failed, so the exec thread has
/// ended and every order is refused, whatever the stream says.
#[test]
fn a_lost_order_session_is_down_with_orders_refused_and_outranks_the_stream() {
    let (link, cell) = claimed();
    link.stream_answered();
    link.rest_login_lost("IG error 403: error.security.api-key-disabled");
    link.stream_lost("stream read: timed out");
    let VenueLinkState::Down(VenueLinkDown { why, effect, .. }) = cell.state() else {
        panic!("{:?}", cell.state())
    };
    assert_eq!(
        why,
        "the order session's login failed: IG error 403: error.security.api-key-disabled"
    );
    assert_eq!(effect, LOGIN_LOST);
    assert!(effect.contains("refused") && effect.contains("account.active"), "{effect}");
    link.stream_gave_up("IG login returned no lightstreamerEndpoint");
    let VenueLinkState::Down(VenueLinkDown { effect, .. }) = cell.state() else {
        panic!("{:?}", cell.state())
    };
    assert_eq!(effect, LOGIN_LOST, "a stream that gave up does not outrank the lost order session");
}

/// **A lost trade stream is down, and its words say the orders still go out**; a line read again brings it back.
#[test]
fn a_lost_trade_stream_is_down_with_orders_still_going_out_until_it_reads_again() {
    let (link, cell) = claimed();
    link.rest_answered();
    link.stream_lost("stream read: timed out reading body");
    assert_eq!(
        cell.state(),
        VenueLinkState::Down(VenueLinkDown {
            why: "the trade stream: stream read: timed out reading body".to_string(),
            effect: STREAM_REDIALING,
            failed_restarts: 1,
            retry_in: Duration::ZERO,
        }),
        "a read that failed is the first failure, and no retry is counted"
    );
    link.stream_answered();
    assert_eq!(cell.state(), VenueLinkState::Up);

    link.stream_gave_up("IG login returned no lightstreamerEndpoint");
    let VenueLinkState::Down(VenueLinkDown { effect, .. }) = cell.state() else {
        panic!("{:?}", cell.state())
    };
    assert_eq!(effect, STREAM_GAVE_UP, "a thread that ended redials nothing");
}

/// **The order session's word is each call's answer**: any status IG answers with is an answer (a `/confirms` 404 is
/// IG saying "not yet"), but a call IG never answered, a 401 the ladder could not cure and a gateway's ambiguous 5xx
/// are a lost path; the next answered call brings it back.
#[test]
fn each_call_of_the_order_session_states_its_answer() {
    let (link, cell) = claimed();
    link.stream_answered();
    for answered in [200_u16, 404, 400] {
        link.rest_heard(&Ok((answered, String::new())));
        assert_eq!(cell.state(), VenueLinkState::Up, "HTTP {answered} is IG answering");
    }
    link.rest_heard(&Err(net_err()));
    let VenueLinkState::Down(VenueLinkDown { why, effect, .. }) = cell.state() else {
        panic!("{:?}", cell.state())
    };
    assert_eq!(
        why,
        "the order session: IG did not answer its last call (network error: connection reset)"
    );
    assert_eq!(effect, CALL_LOST);
    for lost in [401_u16, 503] {
        link.rest_heard(&Ok((200, String::new())));
        link.rest_heard(&Ok((lost, String::new())));
        assert!(matches!(cell.state(), VenueLinkState::Down(_)), "HTTP {lost}: {:?}", cell.state());
    }
    link.rest_heard(&Ok((200, String::new())));
    assert_eq!(cell.state(), VenueLinkState::Up);
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
        let (link, cell) = claimed();
        link.stream_answered();
        link.rest_relogin_failed(&login);
        let VenueLinkState::Down(VenueLinkDown { why, effect, .. }) = cell.state() else {
            panic!("{:?}", cell.state())
        };
        // The whole sentence: never "IG did not answer its last call".
        assert_eq!(why, format!("the order session: {said}"));
        assert_eq!(effect, RELOGIN_LOST);
    }
}

/// **Every way a path is lost is a failure** (`Error`, which the tradehub says at once): a failed login, an unanswered
/// call, a 401 the ladder could not cure, a failed re-login, a stream that dropped and one that gave up. Before, the `0`
/// no lane counts read as a routine reopen, said one ask late.
#[test]
fn every_lost_path_reads_error() {
    use vike_model::feed_status::DataClientConnectionState::Error;
    let after = |lose: fn(&SessionLink)| {
        let (link, cell) = claimed();
        link.rest_answered();
        link.stream_answered();
        lose(&link);
        cell.state()
    };
    for state in [
        after(|link| link.rest_login_lost("refused")),
        after(|link| link.rest_heard(&Err(net_err()))),
        after(|link| link.rest_heard(&Ok((401, String::new())))),
        after(|link| link.rest_relogin_failed(&net_err())),
        after(|link| link.stream_lost("stream read: timed out")),
        after(|link| link.stream_gave_up("no Lightstreamer endpoint")),
    ] {
        assert_eq!(state.connection_state(), Error, "{state:?}");
    }
}

/// A new exec client's claim forgets what the last one said, and a venue's long error text is cut, never carried whole.
#[test]
fn a_new_claim_forgets_both_paths_and_a_long_reason_is_cut() {
    let (link, cell) = claimed();
    link.rest_login_lost(&"x".repeat(1000));
    let VenueLinkState::Down(VenueLinkDown { why, .. }) = cell.state() else {
        panic!("{:?}", cell.state())
    };
    assert!(why.chars().count() < 300, "{}", why.len());
    link.claim(&cell);
    assert_eq!(cell.state(), VenueLinkState::Unknown);
}

/// **An older client's lanes write nothing** once a new client has claimed the cell: an exec thread that outlived its
/// bounded join cannot speak over the one that replaced it (the claim's generation).
#[test]
fn an_older_clients_handle_writes_nothing() {
    let (old, cell) = claimed();
    let new = SessionLink::default();
    new.claim(&cell);
    new.rest_answered();
    new.stream_answered();
    old.rest_login_lost("the old client's login failed");
    old.stream_lost("the old client's stream read failed");
    assert_eq!(cell.state(), VenueLinkState::Up, "the new client's word stands");
}

/// The label finds the client's handles and its lent session: the same label finds one link, another label its own.
#[test]
fn each_account_label_has_its_own_link() {
    let mine = AccountLabel::parse("IGLINKA").expect("a legal label");
    let other = AccountLabel::parse("IGLINKB").expect("a legal label");
    let (cell, elsewhere) = (VenueLink::default(), VenueLink::default());
    session_link(&mine).claim(&cell);
    session_link(&other).claim(&elsewhere);
    session_link(&mine).rest_login_lost("refused");
    assert!(matches!(cell.state(), VenueLinkState::Down(_)));
    assert_eq!(elsewhere.state(), VenueLinkState::Unknown);
}
