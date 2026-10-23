//! `venues`: the build's capability columns beside the server's answer, never merged into one.
use super::*;

/// The matrix is the canonical roster, every row of it, and nothing typed here. A venue added
/// to `vike_model::VENUES` appears with no edit to this file — and a row this file invented
/// would fail the second half.
#[test]
fn every_roster_venue_has_a_row_and_no_row_names_a_venue_off_the_roster() {
    let rows = matrix();
    assert_eq!(rows.len(), vike_model::VENUES.len());
    for (row, venue) in rows.iter().zip(vike_model::VENUES) {
        assert_eq!(&row.venue, venue, "the matrix must be the roster, in its order");
    }
    // Anti-vacuity: the roster is not empty, and the matrix is not uniformly blank — at least
    // one venue declares a live lane and at least one declares a backfill kind, so the
    // renderers below are exercised on real values rather than on a table of dashes.
    assert!(!rows.is_empty());
    assert!(rows.iter().any(|r| !r.live.is_empty()), "no venue declares a live lane");
    assert!(rows.iter().any(|r| r.backfill_bars), "no venue declares a bar backfill");
}

/// Every lane the model declares is NAMED here. The completeness half is the compiler's — see
/// [`live_lanes`] — and this pins the half it cannot check: that the names are distinct and
/// that an all-on row yields all of them.
#[test]
fn every_live_lane_the_model_declares_has_a_distinct_name() {
    let all = LiveDataCaps { bars: true, quotes: true, trades: true, book: true, depth: true };
    let mut names = live_lanes(&all);
    assert_eq!(names.len(), 5, "an all-on row must name every lane: {names:?}");
    names.sort_unstable();
    let before = names.len();
    names.dedup();
    assert_eq!(before, names.len(), "two lanes share a name: {names:?}");
    // The control: nothing on names nothing, so the assertion above is not passing because
    // this function returns a constant.
    assert!(live_lanes(&LiveDataCaps::NONE).is_empty());
}

fn venue_row(venue: &'static str, live: &[&'static str], bars: bool, ticks: bool) -> VenueRow {
    VenueRow { venue, live: live.to_vec(), backfill_bars: bars, backfill_ticks: ticks }
}

/// **§8.1's demand, as a test.** The build's columns and the server's column are three
/// independent statements, and the rendering must let a reader see WHICH side said no. So a
/// venue this build declares four live lanes for, against a server that advertises none, still
/// shows those four lanes AND a `not served` cell — and the skew line names it.
#[test]
fn the_build_column_and_the_server_column_are_never_merged_into_one_verdict() {
    let rows = vec![venue_row("binance", &["bars", "trades"], true, false)];
    let view = ServerView::Answered(vec!["inventory".to_string()]);
    let lines = venues_lines(&rows, &view, "127.0.0.1:7878");
    let body = lines.join("\n");
    assert!(body.contains("bars,trades"), "the build's lanes survive: {body}");
    assert!(body.contains("not served"), "the server's answer is its own cell: {body}");
    assert!(body.contains("SKEW"), "the difference is named: {body}");
    assert!(body.contains("that datahub advertises none"), "{body}");
    // ⚠ ...and it names NO ROUTE. The sentence this replaced told the operator that a
    // `data realtime watch` on a skewed venue "is refused by the server, not by this binary".
    // When it was struck, BOTH halves were false because the route did not exist — that group
    // was refused on this binary's own usage rung. `crate::cmd::data::realtime`'s `watch` ships
    // now, so the first half is true; the SECOND half is still false, and this fixture is
    // exactly the case that shows it. `ServerView::Answered(vec!["inventory"])` advertises no
    // market-data plane, so `vike_datahub_client::DatahubClient::md_subscribe` refuses on the
    // capability LOCALLY — "nothing was sent" — and the refusal that sentence attributed to the
    // server never reaches it. Which side said no is the one thing this verb keeps legible.
    assert!(
        !body.contains("data realtime"),
        "the skew line may not attribute a refusal to a side that did not make it: {body}"
    );
    // The control: a server that DOES advertise it renders the same build columns with a
    // different server cell and NO skew line.
    let served = ServerView::Answered(vec![vike_datahub_client::md_venue_feature("binance")]);
    let ok = venues_lines(&rows, &served, "127.0.0.1:7878").join("\n");
    assert!(ok.contains("bars,trades"), "{ok}");
    // ⚠ `contains("served")` would also match `not served`, so the control asserts the
    // NEGATIVE cell is absent — an assertion that cannot pass for the wrong reason.
    assert!(!ok.contains("not served"), "{ok}");
    assert!(ok.contains("served"), "{ok}");
    assert!(!ok.contains("SKEW"), "nothing differs, so nothing is reported: {ok}");
}

/// A server NEWER than this binary — it serves a venue this roster does not carry — is the
/// other direction of the same skew, and it is the one an operator can otherwise diagnose only
/// by reading two build logs.
#[test]
fn a_venue_the_server_serves_and_this_roster_does_not_name_is_reported_as_a_version_skew() {
    let rows = matrix();
    let mut features: Vec<String> =
        rows.iter().map(|r| vike_datahub_client::md_venue_feature(r.venue)).collect();
    features.push(vike_datahub_client::md_venue_feature("nextvenue"));
    let view = ServerView::Answered(features);
    let s = skew(&rows, &view).expect("the server answered");
    assert_eq!(s.served_there_unknown_here, vec!["nextvenue".to_string()]);
    assert!(
        s.declared_here_unserved_there.is_empty(),
        "every roster venue is served in this fixture: {s:?}"
    );
    let body = venues_lines(&rows, &view, "127.0.0.1:7878").join("\n");
    assert!(body.contains("OLDER than that server"), "{body}");
    assert!(body.contains("nextvenue"), "{body}");
}

/// **§8.1's other demand.** With no datahub the local columns are the whole answer and the
/// verb still answers: every roster venue is rendered, the server column says UNASKED rather
/// than unserved, and the document carries `null` rather than an empty list.
///
/// ⚠ **The fixture is an `io::Error`'s OWN text, and it is built that way because the hand-typed
/// one drifted.** It read `cannot connect to datahub at 127.0.0.1:1` — [`connect`]'s prefix,
/// which [`ask_the_server`] deliberately does NOT add (the address is already on that line, and
/// the prefix printed it twice). So this test showed a reader the doubled shape the fix had
/// removed, and stayed green because it only looked for `NOT REACHED`. The message now comes
/// from the same place production's does — an `io::Error`, stringified — and the count
/// assertion below is what would fail if the prefix ever came back.
#[test]
fn an_unreachable_datahub_still_renders_every_local_row_and_says_the_column_is_unasked() {
    let rows = matrix();
    let view =
        ServerView::Unreachable(io::Error::from(io::ErrorKind::ConnectionRefused).to_string());
    let body = venues_lines(&rows, &view, "127.0.0.1:1").join("\n");
    for r in &rows {
        assert!(body.contains(r.venue), "{} is missing: {body}", r.venue);
    }
    assert!(body.contains("NOT REACHED"), "{body}");
    assert_eq!(
        body.matches("127.0.0.1:1").count(),
        1,
        "the address is named ONCE — this view carries the client's own sentence, without the \
             `cannot connect to datahub at {{addr}}` prefix `connect` adds: {body}"
    );
    assert!(body.contains("unasked, which is not the same as unserved"), "{body}");
    assert!(!body.contains("not served"), "nothing may claim the server refused: {body}");
    assert!(skew(&rows, &view).is_none(), "there is no difference to state");
    // ...and `?` is not `false` in the document either.
    assert_eq!(view.serves(FEATURE_BACKFILL), None);
    assert_eq!(ServerView::Answered(Vec::new()).serves(FEATURE_BACKFILL), Some(false));
}

/// **A server that ANSWERED and said no is not an absent server**, and this is the case the
/// two-variant [`ServerView`] could not express.
///
/// ⚠ The defect it pins is measured rather than hypothetical: `crate::cmd::data`'s [`connect`]
/// folds a denied mac, a keyed server with no keys in the store, and a PROTO_VERSION skew into
/// the same `CliError::connect` sentence, so `execute_venues` rendered every one of them as
/// `NOT REACHED` with `"reachable": false` — in the verb whose own module doc says it exists
/// to surface a version skew. [`ask_the_server`] reads the `io::ErrorKind` instead, and this
/// holds the three renderings apart.
#[test]
fn a_server_that_answered_and_refused_is_not_rendered_as_an_absent_one() {
    let rows = matrix();
    let refused = ServerView::Refused(
        "datahub protocol version mismatch: client speaks 9, server speaks 10".to_string(),
    );
    let body = venues_lines(&rows, &refused, "127.0.0.1:7878").join("\n");
    assert!(body.contains("REACHED, and it REFUSED"), "{body}");
    assert!(
        !body.contains("NOT REACHED"),
        "a server that answered may not be reported as absent: {body}"
    );
    assert!(body.contains("PROTOCOL VERSION SKEW"), "the cause an operator acts on: {body}");
    assert_eq!(
        body.matches("127.0.0.1:7878").count(),
        1,
        "the address is named ONCE here too — same view, same client sentence: {body}"
    );
    // ⚠ …and the `?` column is attributed to THIS side. The sentence this replaced said the
    // server "was reached and never asked", which is false for the commonest served refusal:
    // a keyed datahub met with no keys advertised its venues in the `Welcome` and this binary
    // discarded them. See the arm in `venues_lines`.
    assert!(
        !body.contains("never asked"),
        "this side's own discard may not be reported as the server saying nothing: {body}"
    );
    assert!(body.contains("not the server's silence"), "…and it says whose choice it is: {body}");
    // The build's own columns are untouched — the whole verb still answers.
    for r in &rows {
        assert!(body.contains(r.venue), "{} is missing: {body}", r.venue);
    }
    // ⚠ [`SERVER_VERBS`] is the ANSWERED arm's alone, and this is the assertion that holds it
    // there: with no advertisement to read, [`ServerView::serves`] is `None` for every one of
    // them and the loop renders `was not asked for` — the same false claim about the far side
    // wearing a spelling the ban above does not match.
    //
    // ⚠ It replaces `!body.contains("not served")`, a string `venues_lines` renders in NO arm
    // (the answered one says `does NOT serve`), so that assertion could not fail for its stated
    // reason — and its message, "nothing was asked, so nothing was refused", was the deleted
    // claim itself, three lines under the assertion that forbids it.
    assert!(
        !body.contains("was not asked for"),
        "a refused server's capability rows may not be rendered, least of all as unasked: \
             {body}"
    );

    // The DOCUMENT: three tokens, because `reachable` was a boolean answering a three-state
    // question — and for this state it answered `false` about a server that was reached.
    assert_eq!(refused.state(), "refused");
    assert_eq!(ServerView::Unreachable("closed".to_string()).state(), "unreachable");
    assert_eq!(ServerView::Answered(Vec::new()).state(), "answered");
    let args = parse_of(&["venues", "--json"]).expect("parses");
    let doc: serde_json::Value =
        serde_json::from_str(&venues_json(&args, &rows, &refused)).expect("one document");
    assert_eq!(doc["server"]["state"], "refused");
    assert!(doc["server"]["features"].is_null(), "a discarded handshake advertised nothing");
    assert!(doc["skew"].is_null(), "there is no difference to state: {doc}");
}

/// The `io::ErrorKind` split [`ask_the_server`] turns on, as a table — the kinds the client
/// produces when the far side SPOKE, against the ones that mean nothing answered.
///
/// It is a unit test of the CLASSIFIER rather than of a connection, deliberately: the three
/// served refusals need a server that denies a mac, one that speaks another PROTO_VERSION and
/// one that is not a datahub at all, and `DatahubClient`'s own tests are where those live. What
/// this file owns is the decision made on the kind they each arrive as.
#[test]
fn the_answered_and_refused_kinds_are_the_ones_the_far_side_spoke_on() {
    for kind in [io::ErrorKind::PermissionDenied, io::ErrorKind::InvalidData] {
        assert!(answered_and_refused(kind), "{kind:?} is a served refusal");
    }
    for kind in [
        io::ErrorKind::ConnectionRefused,
        io::ErrorKind::TimedOut,
        io::ErrorKind::WouldBlock,
        io::ErrorKind::NotFound,
        io::ErrorKind::ConnectionReset,
    ] {
        assert!(!answered_and_refused(kind), "{kind:?} is a socket, not an answer");
    }
}

/// Both server-wide rows are rendered with their own verdict and their own cost, and neither
/// is a per-venue column — see [`SERVER_VERBS`].
#[test]
fn the_server_wide_capabilities_are_reported_with_what_their_absence_costs() {
    let rows = matrix();
    let view = ServerView::Answered(vec![FEATURE_BACKFILL.to_string()]);
    let body = venues_lines(&rows, &view, "127.0.0.1:7878").join("\n");
    assert!(body.contains(&format!("serves `{FEATURE_BACKFILL}`")), "{body}");
    assert!(body.contains(&format!("does NOT serve `{FEATURE_VENUE_CATALOG}`")), "{body}");
    for (_, why) in SERVER_VERBS {
        assert!(body.contains(*why), "the cost must be stated: {body}");
    }
}

/// The documents name the group and the verb from the declarations rather than as literals,
/// and the `venues` document keeps the two sources APART — which is the shape a consumer
/// branches on.
#[test]
fn the_documents_derive_their_verb_and_keep_the_two_sources_apart() {
    let args = parse_of(&["venues", "--json"]).expect("parses");
    let rows = matrix();
    let doc: serde_json::Value = serde_json::from_str(&venues_json(
        &args,
        &rows,
        &ServerView::Unreachable("closed".to_string()),
    ))
    .expect("one JSON document");
    assert_eq!(doc["group"], "catalog");
    assert_eq!(doc["verb"], Verb::Venues.as_str());
    assert_eq!(doc["build"]["venues"].as_array().expect("rows").len(), rows.len());
    assert_eq!(doc["server"]["state"], "unreachable");
    assert!(doc["server"]["features"].is_null(), "unasked is null, never []: {doc}");
    assert!(doc["skew"].is_null());
    // There is deliberately NO merged per-venue verdict anywhere in the document.
    assert!(doc["venues"].is_null(), "the two sources must not be flattened: {doc}");
}
