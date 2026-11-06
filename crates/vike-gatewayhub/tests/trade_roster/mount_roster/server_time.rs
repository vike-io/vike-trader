//! The clock-row tests that iterate the roster through the REAL registry. They lived in
//! `crates/vike-gatewayhub/src/trade/adapters/server_time_tests.rs`, which keeps the planted-row tests, until the venue
//! mount contract finished (docs/decisions/0096): every clock row is a bridge's declaration now.

use std::collections::HashMap;

use vike_bridge_core::venue_adapter::{ClockAuth, ClockDecl, ClockRisk};
use vike_gatewayhub::registry::trade::REGISTRY;
use vike_gatewayhub::trade::adapters::probe::ServerTimeGap;
use vike_model::AccountTier;
use vike_tradehub_core::preflight::REMEDY_CLOCK;

use crate::local::{clock_policy, clock_policy_of};
use vike_gatewayhub::trade::adapters::server_time::{clock_decl, venue_server_time_ms};

/// THE completeness gate, and the reason no count of wired venues is written down anywhere:
/// every canonical roster venue is either wired or declared, and adding a bridge crate turns
/// this red until the new venue is classified.
#[test]
fn clock_sources_cover_the_roster() {
    for venue in vike_model::VENUES {
        assert!(
            clock_decl(REGISTRY, venue).is_some(),
            "{venue} is on vike_model::VENUES but has no clock row — wire its clock or \
                 declare why it has none"
        );
    }
}

/// A wired row must name the endpoint an operator can curl; a declared row must give a REASON,
/// not a shrug. The length floor is deliberate — "n/a" would pass a non-empty check and teach
/// nobody anything.
#[test]
fn every_row_says_something_useful() {
    for venue in vike_model::VENUES {
        let Some(decl) = clock_decl(REGISTRY, venue) else { continue };
        match decl {
            ClockDecl::Wired { endpoint, auth, .. } => {
                assert!(endpoint.contains('/'), "{venue}'s endpoint names no path: {endpoint}");
                // The label and the declared `auth` must not contradict each other: the label
                // is what an operator reads, the field is what the live smoke branches on, and
                // a row whose two halves disagree is a capability table with a false row.
                let labelled_public = endpoint.contains("(public");
                assert_eq!(
                    labelled_public,
                    auth == ClockAuth::Public,
                    "{venue}'s endpoint label and its declared auth disagree: {endpoint}"
                );
            }
            ClockDecl::NotWired { reason, unmeasured_risk } => {
                assert!(
                    reason.len() >= 60,
                    "{venue}'s NotWired reason is too short to be an explanation: {reason}"
                );
                assert!(!reason.contains("TODO"), "{venue}'s reason is a TODO, not a reason");
                if let Some(at_stake) = unmeasured_risk {
                    assert!(
                        at_stake.len() >= 60,
                        "{venue} declares an unmeasured risk without saying what is at stake"
                    );
                }
            }
        }
    }
}

/// The remedy is per-venue BECAUSE it would otherwise be false: only the recv-window venues may
/// mention a recv window, and only they may claim orders are rejected.
#[test]
fn only_recv_window_venues_claim_orders_are_rejected() {
    for venue in vike_model::VENUES {
        let Some(ClockDecl::Wired { risk, .. }) = clock_decl(REGISTRY, venue) else {
            continue;
        };
        let remedy = risk.remedy();
        assert!(remedy.starts_with("sync the host clock"), "{venue}: {remedy}");
        let claims_recv_window = remedy.contains("recv window");
        assert_eq!(
            claims_recv_window,
            risk == ClockRisk::SignedTimestamp,
            "{venue}'s remedy must mention a recv window iff it signs a timestamp: {remedy}"
        );
        if risk == ClockRisk::NoTimestamp {
            assert!(
                remedy.contains("NOT at risk"),
                "{venue} cannot reject an order over clock drift; say so: {remedy}"
            );
        }
    }
}

/// THE policy rule, over the table: a venue may carry a FAIL threshold — the one that degrades
/// it to paper — IFF a drifted clock can actually get its orders rejected. Everything else is a
/// host-health canary that may warn and nothing more.
#[test]
fn only_order_rejecting_venues_can_fail_their_clock_check() {
    for venue in vike_model::VENUES {
        let Some(ClockDecl::Wired { risk, .. }) = clock_decl(REGISTRY, venue) else {
            assert_eq!(
                clock_policy(REGISTRY, venue),
                None,
                "{venue} is unwired and judges nothing"
            );
            continue;
        };
        let policy = clock_policy_of(risk);
        assert_eq!(clock_policy(REGISTRY, venue), Some(policy), "{venue}");
        assert_eq!(
            policy.fail_ms.is_some(),
            risk == ClockRisk::SignedTimestamp,
            "{venue}: only a venue that REJECTS orders over drift may be degraded to paper by \
                 this leg"
        );
        assert!(policy.warn_ms > 0, "{venue} must warn somewhere");
        assert_ne!(policy.remedy, REMEDY_CLOCK, "{venue} must carry its OWN remedy");
    }
}

/// ③ IS PURE: a declared venue with nothing at stake answers `NotChecked` with its reason and
/// touches NO network — which is what lets the preflight render it without a probe, a timeout
/// or a warning.
#[test]
fn a_declared_venue_reports_not_checked_without_touching_the_network() {
    let vars = HashMap::new();
    for venue in vike_model::VENUES {
        let Some(ClockDecl::NotWired { reason, unmeasured_risk: None }) =
            clock_decl(REGISTRY, venue)
        else {
            continue;
        };
        let gap = venue_server_time_ms(REGISTRY, venue, &vars, AccountTier::Demo)
            .expect_err("declared venues never measure");
        assert_eq!(gap, ServerTimeGap::NotChecked(reason), "{venue}");
    }
}

/// Canary venues warn LATER than recv-window ones: a reading there measures mostly the venue's
/// own clock (hyperliquid: -220..-424 ms across 40 samples from an NTP-disciplined box). Moved here
/// from the unit tests of `server_time` with the judgement: the daemon answers a venue's declared
/// `ClockRisk` and the tradehub's preflight turns it into thresholds (restated in
/// `support/bridge_backend.rs`'s `clock_policy_of`, until the wire carries the policy).
#[test]
fn the_canary_threshold_is_looser_than_the_recv_window_one() {
    let signed = clock_policy_of(ClockRisk::SignedTimestamp);
    let nonce = clock_policy_of(ClockRisk::NonceWindow);
    let none = clock_policy_of(ClockRisk::NoTimestamp);
    assert!(nonce.warn_ms > signed.warn_ms);
    // The two canary risks share THRESHOLDS and differ only in words: same cost, but the
    // operator is told which one they have.
    assert_eq!((nonce.warn_ms, nonce.fail_ms), (none.warn_ms, none.fail_ms));
    assert_ne!(nonce.remedy, none.remedy, "…and they must not say the same thing");
    assert_eq!(signed.fail_ms, Some(vike_tradehub_core::preflight::DEFAULT_CLOCK_FAIL_MS));
    assert_eq!(nonce.fail_ms, None);
}
