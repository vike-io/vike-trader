//! A reconcile login that never reached IG at the start is made by a later pass: the start keeps its client, the
//! next read logs in and reads, a read that cannot log in fails in words while the reads right behind it ask IG
//! nothing, and a REFUSED login (at the start or later) ends it, because asking again cannot cure a refusal. Over the
//! scripted loopback double (`fake_ig`), never IG.

use serde_json::json;
use vike_execution::recon::ReconcileClient;
use vike_ig::ReconcileClientIg;

use crate::fake_ig::{Route, login_route, serve};

const MOUNTED: &str = "EURUSD";

fn refusal() -> serde_json::Value {
    json!({ "errorCode": "error.security.invalid-details" })
}

/// The start's login is hung up: the client is still built, and the first pass logs in, reads, and keeps the session.
#[test]
fn a_start_login_that_never_reached_ig_is_made_by_the_next_pass() {
    crate::kept_rows::hold();
    let ig = serve(vec![
        login_route().after(1),
        Route::hang_up("POST /session"),
        Route::ok("GET /workingorders", json!({ "workingOrders": [] })),
    ]);
    let client = ReconcileClientIg::connect(&ig.config(), MOUNTED)
        .expect("a login IG never answered keeps the client");
    assert_eq!(ig.count("POST /session"), 1);
    assert!(client.fetch_order_status_reports(0).expect("the pass logs in, then reads").is_empty());
    client.fetch_order_status_reports(0).expect("the session is kept");
    assert_eq!(
        (ig.count("POST /session"), ig.count("GET /workingorders")),
        (2, 2),
        "one login, kept"
    );
}

/// A pass whose login does not reach IG either fails its read in words, and the read right behind it waits the
/// ladder's step instead of logging in again.
#[test]
fn a_pass_that_cannot_log_in_fails_in_words_and_the_read_behind_it_asks_ig_nothing() {
    crate::kept_rows::hold();
    let ig = serve(vec![Route::hang_up("POST /session")]);
    let client = ReconcileClientIg::connect(&ig.config(), MOUNTED).expect("the client is kept");
    let first = client.fetch_position_status_reports().expect_err("not logged in");
    assert!(first.contains("IG was not reached"), "{first}");
    let behind = client.fetch_balance().expect_err("still not logged in");
    assert!(behind.text.contains("the next login is in"), "{behind}");
    assert_eq!(
        ig.count("POST /session"),
        2,
        "the start's login and the first pass's, none behind it"
    );
    assert_eq!(ig.count("GET "), 0);
}

/// IG refusing the start's login leaves no client, as before.
#[test]
fn a_login_ig_refused_at_the_start_leaves_no_client() {
    let ig = serve(vec![Route::status("POST /session", 401, refusal())]);
    assert!(ReconcileClientIg::connect(&ig.config(), MOUNTED).is_none());
    assert_eq!(ig.count("POST /session"), 1);
}

/// A later login IG refuses ends the retries: every later read fails in the refusal's words and logs in nowhere.
#[test]
fn a_later_login_ig_refuses_ends_the_retries() {
    crate::kept_rows::hold();
    let ig = serve(vec![
        Route::status("POST /session", 403, refusal()).after(1),
        Route::hang_up("POST /session"),
    ]);
    let client = ReconcileClientIg::connect(&ig.config(), MOUNTED).expect("the client is kept");
    let refused = client.fetch_order_status_reports(0).expect_err("refused");
    assert!(refused.contains("IG refused the reconcile login"), "{refused}");
    assert_eq!(client.fetch_order_status_reports(0).expect_err("still refused"), refused);
    assert_eq!(ig.count("POST /session"), 2, "no login after the refusal");
}
