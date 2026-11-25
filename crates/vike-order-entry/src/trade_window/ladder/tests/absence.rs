//! The no-book classifier's tests: every line STATE reads the cause its FEED badge reads.

use super::super::absence::{
    TradeWindowLadderAbsenceCause, depth_absence, depth_absence_headline, depth_absence_next_step,
};
use vike_model::feed_status::DataClientConnectionState;

/// A venue with NO status producer is not the same statement as a producer that reports no state,
/// and the two must not collapse: the first means "the datahub serves no feed here", the second a
/// line that knows nothing yet. And the silent line reads as the window's FEED badge reads it
/// (`FEED ?`, `crate::trade_window::instrument_bar::link_of`), not as "MARKET-DATA LINK DOWN" over
/// the ladder beside a badge that says it knows nothing.
#[test]
fn a_missing_producer_is_distinct_from_a_silent_one() {
    assert_eq!(depth_absence(None), TradeWindowLadderAbsenceCause::NoFeed);
    assert_eq!(
        crate::trade_window::instrument_bar::link_of(None),
        DataClientConnectionState::Unknown
    );
    assert_eq!(
        depth_absence(Some(DataClientConnectionState::Unknown)),
        TradeWindowLadderAbsenceCause::NotServed
    );
}

/// Each state `market_data_session`'s `refresh_statuses` sets picks its own cause: a venue whose
/// streams flow but whose BOOK has not arrived is awaiting a snapshot, one subscribing is dialling,
/// a link idle on purpose is down, a failing one a FAULT (never a reason to wait), and a venue
/// refused, wanting nothing or waiting for a holder is not being served.
#[test]
fn each_line_state_has_its_own_cause() {
    for (state, cause) in [
        (DataClientConnectionState::Connected, TradeWindowLadderAbsenceCause::NoSnapshot),
        (DataClientConnectionState::Connecting, TradeWindowLadderAbsenceCause::Dialling),
        (DataClientConnectionState::Disconnected, TradeWindowLadderAbsenceCause::LinkDown),
        (DataClientConnectionState::Error, TradeWindowLadderAbsenceCause::LinkFault),
        (DataClientConnectionState::Unknown, TradeWindowLadderAbsenceCause::NotServed),
    ] {
        assert_eq!(depth_absence(Some(state)), cause, "{state:?}");
    }
}

/// Every variant answers with its own words, no two share a line, and no next step sends the
/// operator to the WRONG SOCKET. A `_` arm in either table, or a copy-pasted row, is what this
/// catches.
#[test]
fn every_absence_has_its_own_words_and_names_the_right_socket() {
    let all = [
        TradeWindowLadderAbsenceCause::NoFeed,
        TradeWindowLadderAbsenceCause::LinkDown,
        TradeWindowLadderAbsenceCause::LinkFault,
        TradeWindowLadderAbsenceCause::Dialling,
        TradeWindowLadderAbsenceCause::NoSnapshot,
        TradeWindowLadderAbsenceCause::NotServed,
    ];
    let heads: Vec<&str> = all.iter().copied().map(depth_absence_headline).collect();
    let steps: Vec<&str> = all.iter().copied().map(depth_absence_next_step).collect();
    for (i, h) in heads.iter().enumerate() {
        assert!(!h.is_empty(), "{:?} has an empty headline", all[i]);
        assert!(!steps[i].is_empty(), "{:?} has an empty next step", all[i]);
        let lower = steps[i].to_lowercase();
        assert!(
            !lower.contains("tradehub observe") && !lower.contains("reconnect the backend"),
            "{:?} points the operator at the observe socket, which carries no book: {}",
            all[i],
            steps[i]
        );
    }
    let mut h = heads.clone();
    h.sort_unstable();
    h.dedup();
    assert_eq!(h.len(), heads.len(), "two absences share a headline: {heads:?}");
    let mut s = steps.clone();
    s.sort_unstable();
    s.dedup();
    assert_eq!(s.len(), steps.len(), "two absences share a next step: {steps:?}");
}
