//! The exec badge is per ACCOUNT.

use super::*;

// ---------------------------------------------------------------------------------------------
// The exec badge is per ACCOUNT
// ---------------------------------------------------------------------------------------------

/// One arming row, through the same type `vike_mount::venue_arming` produces.
fn arming_row(venue: &'static str, label: Option<&str>) -> vike_config::VenueArming {
    vike_config::VenueArming {
        venue,
        label: match label {
            None => vike_model::accounts::account_keys::AccountLabel::Default,
            Some(l) => {
                vike_model::accounts::account_keys::AccountLabel::parse(l).expect("a legal label")
            }
        },
        tier: vike_config::VenueMode::Live,
        effective: vike_config::VenueMode::Live,
        block: vike_config::ArmingBlock::None,
        account_ids: Vec::new(),
    }
}

/// **THE DEFECT.** The default `bybit` account has no credentials and mounts paper; a LABELLED
/// `bybit` account armed real exec in this same process. The venue-keyed badge answered
/// `live_venues.contains("bybit")` — false — and the daemon announced `exec = PAPER` beside a
/// live authenticated bybit session.
///
/// ⚠ The armed set is spelled through `VenueArming::route_key`, NOT as a `"bybit#ALT"` literal:
/// a fixture that hand-writes the key it is about to look up passes with the renderer broken,
/// which is the seeded-through-the-function-under-test failure this program has hit before.
#[test]
fn a_venue_whose_labelled_account_armed_does_not_announce_paper() {
    let rows = vec![arming_row("bybit", None), arming_row("bybit", Some("ALT"))];
    let live: std::collections::HashSet<String> = [rows[1].route_key()].into_iter().collect();

    let others = other_live_accounts("bybit", &rows, &live);
    assert_eq!(others, vec![rows[1].route_key()], "the ALT account's route key must be found");

    let announced =
        with_other_live_accounts(cex_arming(CexVenue::Bybit, false, false), "bybit", others);
    assert_eq!(announced.exec, EXEC_OTHER_ACCOUNT_LIVE);
    assert_ne!(announced.exec, EXEC_PAPER, "the badge must not read paper for a live venue");
    assert!(
        !announced.exec.starts_with(EXEC_PAPER),
        "…and must not answer an `exec=PAPER` grep either: {}",
        announced.exec
    );
    // The remedy still arms THIS mount, and now says what the paper verdict is about.
    let remedy = announced.remedy.expect("a paper mount still carries its remedy");
    assert!(remedy.contains("ALREADY LIVE"), "{remedy}");
    assert!(remedy.contains(&rows[1].route_key()), "the live account must be NAMED: {remedy}");
    assert!(remedy.contains("BYBIT_DEMO_API_KEY"), "the original remedy survives: {remedy}");
}

/// Both accounts armed: the badge still answers an `exec=LIVE` grep, and says there is more
/// than one book behind it.
#[test]
fn a_venue_with_two_armed_accounts_says_so_and_still_reads_live() {
    let rows = vec![arming_row("bybit", None), arming_row("bybit", Some("ALT"))];
    let live: std::collections::HashSet<String> =
        rows.iter().map(vike_config::VenueArming::route_key).collect();
    let announced = with_other_live_accounts(
        cex_arming(CexVenue::Bybit, true, true),
        "bybit",
        other_live_accounts("bybit", &rows, &live),
    );
    assert_eq!(announced.exec, EXEC_LIVE_MULTI_ACCOUNT);
    assert!(announced.exec.starts_with(EXEC_LIVE), "an `exec=LIVE` grep must still match");
    assert_eq!(announced.other_live, vec![rows[1].route_key()]);
    assert!(announced.remedy.is_none(), "a live mount carries no remedy");
}

/// **A single-account box is BYTE-IDENTICAL.** Every row is a default-account row, so
/// `other_live_accounts` is empty for every venue and `with_other_live_accounts` returns its
/// input field for field — asserted against the UNWRAPPED producer, both armed and not.
#[test]
fn a_single_account_box_announces_exactly_what_it_did_before() {
    let rows: Vec<vike_config::VenueArming> =
        ["binance", "bybit", "okx", "aster"].into_iter().map(|v| arming_row(v, None)).collect();
    for armed_venues in [vec![], vec!["bybit"], vec!["binance", "bybit", "okx", "aster"]] {
        let live: std::collections::HashSet<String> =
            armed_venues.iter().map(|v| (*v).to_string()).collect();
        for venue in [CexVenue::Binance, CexVenue::Bybit, CexVenue::Okx, CexVenue::Aster] {
            let slug = venue.slug();
            assert!(
                other_live_accounts(slug, &rows, &live).is_empty(),
                "{slug} has no second account on this box"
            );
            for mainnet in [false, true] {
                let exec_live = live.contains(slug);
                let bare = cex_arming(venue, mainnet, exec_live);
                let wrapped = with_other_live_accounts(
                    cex_arming(venue, mainnet, exec_live),
                    slug,
                    other_live_accounts(slug, &rows, &live),
                );
                assert_eq!(bare, wrapped, "{slug} mainnet={mainnet} must be untouched");
                assert_eq!(
                    wrapped.exec,
                    if exec_live { EXEC_LIVE } else { EXEC_PAPER },
                    "…and reads exactly the two badges it always read"
                );
            }
        }
    }
    // The credentialed-data producers take the same trip.
    for (bare, slug) in [
        (alpaca_arming(false), "alpaca"),
        (ctrader_arming(true), "ctrader"),
        (oanda_arming(false), "oanda"),
        (deribit_arming(true), "deribit"),
        (ig_arming(false), "ig"),
    ] {
        let exec = bare.exec;
        let wrapped = with_other_live_accounts(bare, slug, Vec::new());
        assert_eq!(wrapped.exec, exec, "{slug} must keep its badge with no second account");
        assert!(wrapped.other_live.is_empty());
    }
}

/// The route key of ANOTHER venue's labelled account never leaks into this venue's answer, and
/// a labelled account that did NOT arm is not reported as live.
#[test]
fn the_answer_is_scoped_to_the_venue_and_to_what_actually_armed() {
    let rows = vec![
        arming_row("bybit", Some("ALT")),
        arming_row("okx", Some("ALT")),
        arming_row("bybit", Some("HEDGE")),
    ];
    // Only okx#ALT armed.
    let live: std::collections::HashSet<String> = [rows[1].route_key()].into_iter().collect();
    assert!(
        other_live_accounts("bybit", &rows, &live).is_empty(),
        "okx's armed account must not appear under bybit"
    );
    assert_eq!(other_live_accounts("okx", &rows, &live), vec![rows[1].route_key()]);
}

/// `exec_badge` is total over its two inputs, and the four strings are distinct — so no state
/// can be mistaken for another by a grep.
#[test]
fn the_four_exec_badges_are_distinct() {
    let all = [
        exec_badge(false, false),
        exec_badge(false, true),
        exec_badge(true, false),
        exec_badge(true, true),
    ];
    let mut dedup = all.to_vec();
    dedup.sort_unstable();
    dedup.dedup();
    assert_eq!(dedup.len(), all.len(), "the four badges must be distinct: {all:?}");
}
