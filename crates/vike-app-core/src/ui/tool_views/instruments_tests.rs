use super::*;
use crate::data::catalog_refresh::RefreshOutcome;
use vike_catalog::{CatalogMode, CatalogSource, VenueStamp};

fn row(venue: &str, source: Option<CatalogSource>, mode: Option<CatalogMode>) -> VenueCatalogRow {
    VenueCatalogRow {
        venue: venue.into(),
        mode,
        source,
        server: Some("127.0.0.1:7878".into()),
        stamp: None,
        baseline: None,
        local: None,
        own_keys: false,
        state: VenueRefreshState::Idle,
    }
}

fn direct(venue: &str) -> VenueCatalogRow {
    row(venue, Some(CatalogSource::Direct), Some(CatalogMode::Enumerable))
}

#[test]
fn an_idle_row_states_why_it_cannot_be_refreshed_and_names_the_kind_of_fact() {
    assert_eq!(status_line(&direct("deribit")), "", "a live row says nothing until pressed");
    assert!(
        status_line(&row("binance", Some(CatalogSource::ServerBacked), None))
            .contains("backend's datahub"),
        "a routed row says where its list comes from"
    );
    // The three facts that are NOT about this build.
    let alpaca = status_line(&row("alpaca", None, None));
    assert!(alpaca.contains("credentials"), "{alpaca}");
    assert!(alpaca.contains("alpaca"), "the sentence names the venue: {alpaca}");
    let ig = status_line(&row("ig", None, None));
    assert!(ig.contains("publishes no bulk instrument list"), "{ig}");
    assert!(ig.contains("nothing to arm and nothing to rebuild"), "{ig}");
    let ibkr = status_line(&row("ibkr", None, None));
    assert!(ibkr.contains("publishes no bulk instrument list"), "{ibkr}");
    // ...and none of them reads as an empty venue.
    for line in [&alpaca, &ig, &ibkr] {
        assert!(!line.contains("0 instruments"), "an empty list is a LIE here: {line}");
    }
}

/// The outcome wins over the note — otherwise the one venue you CAN refresh renders a blank
/// Status cell after a failure, which is the state the operator most needs to see.
#[test]
fn an_outcome_outranks_the_availability_note() {
    let mut r = direct("deribit");
    r.state = VenueRefreshState::Done {
        at_ms: 1,
        outcome: RefreshOutcome::Failed { error: "reset".into(), kept: 412 },
    };
    let line = status_line(&r);
    assert!(line.contains("kept the 412"), "{line}");
    r.state = VenueRefreshState::InFlight { since_ms: 1 };
    assert_eq!(status_line(&r), "fetching…");
}

/// A refusal that came back over the WIRE renders the server's own sentence on the row, not a
/// failure and not a count.
#[test]
fn a_wire_refusal_renders_as_itself() {
    let mut r = row("binance", Some(CatalogSource::ServerBacked), None);
    r.state = VenueRefreshState::Done {
        at_ms: 1,
        outcome: RefreshOutcome::Refused {
            why: "this datahub serves no venue catalog because its operator REFUSED the lane \
                      — `venue_catalog_off = true`"
                .into(),
            kept: 0,
        },
    };
    let line = status_line(&r);
    assert!(line.contains("venue_catalog_off"), "{line}");
    assert!(!line.contains("failed"), "a refusal is not a failure: {line}");
}

#[test]
fn the_summary_counts_only_venues_that_could_be_fetched() {
    let mut fetched = direct("deribit");
    fetched.stamp = Some(VenueStamp { venue: "deribit".into(), last_refreshed_ms: 1, count: 3 });
    assert_eq!(instruments_summary(&[fetched.clone()], 3), "3 instruments across 1 venues");
    assert_eq!(
        instruments_summary(&[fetched.clone(), row("ig", None, None)], 3),
        "3 instruments across 2 venues",
        "a venue that can never be fetched is not an outstanding task"
    );
    assert!(
        instruments_summary(&[fetched, row("binance", Some(CatalogSource::ServerBacked), None)], 3)
            .contains("1 venues never fetched")
    );
}

/// **A SHIPPED baseline renders as a third state: a real count, a SHIPPED date, and its
/// qualifier.** `docs/decisions/0066` decision 7.
///
/// ⚠ Mutation proof: make `age_cell` fall through to `refreshed_label` for a baseline row and
/// the second assertion goes red — the cell reads `never` beside a non-zero count, which is
/// the undated-but-authoritative rendering the record refuses. Drop the qualifier from
/// `status_line` and the fourth goes red.
#[test]
fn a_shipped_baseline_shows_its_date_and_its_qualifier_and_never_an_age() {
    let mut r = row("alpaca", None, None);
    r.baseline = Some(vike_catalog::BaselineVenue {
        venue: "alpaca".into(),
        fetched: "2026-09-16".into(),
        qualifier: "demo".into(),
        instruments: vec![],
    });
    assert_eq!(r.count(), 0, "an empty shipped row is a measured zero, not an absence");
    assert_eq!(age_cell(&r, 1), "shipped 2026-09-16");
    assert!(!age_cell(&r, 1).contains("ago"), "a shipped date must never read as an age");
    assert!(!age_cell(&r, 1).contains("never"), "it WAS fetched — by us, on that date");

    let line = status_line(&r);
    assert!(line.contains("ships with vike"), "the source is named: {line}");
    assert!(line.contains("demo"), "the qualifier rides the Status cell: {line}");
    assert!(line.contains("2026-09-16"), "{line}");
    // …and the venue's own refusal sentence survives beside it, because the SERVER still
    // refuses this venue and that fact did not change.
    assert!(line.contains("credentials"), "{line}");
}

/// **A LOCALLY fetched list outranks the shipped one and is spelled differently.**
/// `docs/decisions/0066` decision 9.
///
/// ⚠ Mutation proof: point `age_cell` at `row.baseline` instead of `row.answering_list()` and
/// the second assertion goes red — a venue with a real count renders `never`, the word for
/// "nothing has ever measured this". Drop the `LocalCredentialed` arm from `status_line` and
/// the fourth does: their own list is described as one that ships with vike and "was not
/// fetched with your credentials", which is exactly backwards.
#[test]
fn a_locally_fetched_list_is_spelled_as_theirs_and_never_as_shipped() {
    let mut r = row("alpaca", None, None);
    r.baseline = Some(vike_catalog::BaselineVenue {
        venue: "alpaca".into(),
        fetched: "2026-01-01".into(),
        qualifier: "demo".into(),
        instruments: vec![],
    });
    r.local = Some(vike_catalog::BaselineVenue {
        venue: "alpaca".into(),
        fetched: "2026-09-16".into(),
        qualifier: "demo".into(),
        instruments: vec![],
    });
    assert_eq!(r.answer(), vike_catalog::CatalogAnswer::LocalCredentialed);
    let age = age_cell(&r, 1);
    assert_eq!(age, "yours 2026-09-16", "their own list, their own date");
    assert!(!age.contains("never"), "{age}");
    assert!(!age.contains("ago"), "a fetch DATE is not an age: {age}");
    assert!(!age.contains("shipped"), "it did not ship with the binary: {age}");

    let line = status_line(&r);
    assert!(line.contains("this list is YOURS"), "{line}");
    assert!(line.contains("vike-backend catalog refresh alpaca"), "{line}");
    assert!(
        !line.contains("not fetched with your credentials"),
        "the shipped caveat is a FALSE warning about their own list: {line}"
    );
    // …and the server's refusal still stands beside it, because the SERVER still refuses.
    assert!(line.contains("credentials"), "{line}");
}

/// **The operator's own fetch WINS, and the row stops mentioning the shipped one.**
#[test]
fn an_operator_fetch_outranks_a_shipped_list_even_at_zero() {
    let mut r = row("alpaca", None, None);
    r.baseline = Some(vike_catalog::BaselineVenue {
        venue: "alpaca".into(),
        fetched: "2026-09-16".into(),
        qualifier: "demo".into(),
        instruments: vec![],
    });
    r.stamp = Some(VenueStamp { venue: "alpaca".into(), last_refreshed_ms: 0, count: 0 });
    assert!(age_cell(&r, 1_000).contains("just now"), "{}", age_cell(&r, 1_000));
    assert!(
        !status_line(&r).contains("ships with vike"),
        "a measured venue must not advertise the list it no longer uses: {}",
        status_line(&r)
    );
    assert!(!status_line(&r).contains("this list is YOURS"), "{}", status_line(&r));
}

#[test]
fn the_hover_says_which_box_the_press_spends() {
    assert!(press_hint(&direct("deribit")).contains("from its API"));
    let routed = press_hint(&row("binance", Some(CatalogSource::ServerBacked), None));
    assert!(routed.contains("datahub at 127.0.0.1:7878"), "{routed}");
}
