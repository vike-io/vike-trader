//! The leverage table and its pure rules (decision 0182).
//!
//! | test | what would be true without it |
//! |---|---|
//! | [`the_first_release_opens_demo_only_on_the_posting_venues`] | a `live` tier, or deribit, or aster, opened by an edit nobody meant |
//! | [`a_closed_or_unlisted_row_is_never_honoured`] | the engine or the lane acting on a figure planted on a closed row |
//! | [`the_refusal_says_why_in_the_operators_words`] | a refusal that names no reason, or the wrong one |
//! | [`the_candidate_is_whole_at_least_one_and_never_above_an_input`] | a fractional figure truncated by the venue while the engine used the fraction, or a ceiling raised |
//! | [`a_nonsense_ceiling_cannot_loosen_the_candidate`] | a NaN or sub-1 ceiling producing a figure above the stated one |

use vike_model::AccountTier;

use super::*;

fn lev(x: f64) -> AccountLeverage {
    AccountLeverage::new(x).expect("a usable figure")
}

#[test]
fn the_first_release_opens_demo_only_on_the_posting_venues() {
    let hl = leverage_venue("hyperliquid").expect("listed");
    assert_eq!(hl.means, LeverageMeans::Capped);
    for tier in [AccountTier::Paper, AccountTier::Demo, AccountTier::Live] {
        assert!(hl.allows(tier), "decision 0181 accepted every tier and still does: {tier:?}");
    }
    assert!(!hl.whole_only);
    for venue in ["bybit", "okx", "binance"] {
        let row = leverage_venue(venue).expect("listed");
        assert_eq!(row.means, LeverageMeans::Posted, "{venue}");
        assert!(row.allows(AccountTier::Demo), "{venue}: demo is open");
        assert!(!row.allows(AccountTier::Live), "{venue}: live is CLOSED by the table");
        assert!(!row.allows(AccountTier::Paper), "{venue}: a paper account has no venue to read");
        assert!(row.whole_only, "{venue}: the wire takes a whole number");
    }
    let aster = leverage_venue("aster").expect("coded");
    assert!(aster.tiers.is_empty(), "aster's row is closed: no demo account exists");
    assert!(leverage_venue("deribit").is_none(), "deribit has no leverage number");
}

#[test]
fn a_closed_or_unlisted_row_is_never_honoured() {
    assert!(leverage_venue_at("bybit", AccountTier::Demo).is_some());
    for (venue, tier) in [
        ("bybit", AccountTier::Live),
        ("okx", AccountTier::Live),
        ("binance", AccountTier::Live),
        ("aster", AccountTier::Demo),
        ("aster", AccountTier::Live),
        ("deribit", AccountTier::Demo),
        ("oanda", AccountTier::Demo),
        ("bybit", AccountTier::Paper),
    ] {
        assert!(leverage_venue_at(venue, tier).is_none(), "{venue} at {tier:?}");
    }
}

#[test]
fn the_refusal_says_why_in_the_operators_words() {
    // Allowed: nothing to say.
    assert_eq!(leverage_refusal("bybit", AccountTier::Demo, 10.0), None);
    assert_eq!(
        leverage_refusal("hyperliquid", AccountTier::Live, 2.5),
        None,
        "hl takes a fraction"
    );
    // deribit: its own sentence.
    let why = leverage_refusal("deribit", AccountTier::Demo, 5.0).expect("refused");
    assert!(why.contains("no leverage number"), "{why}");
    // An unlisted venue names the listed ones.
    let why = leverage_refusal("oanda", AccountTier::Demo, 5.0).expect("refused");
    assert!(why.contains("oanda") && why.contains("bybit") && !why.contains("aster"), "{why}");
    // A live account on a demo-only venue.
    let why = leverage_refusal("bybit", AccountTier::Live, 5.0).expect("refused");
    assert!(why.contains("demo") && why.contains("`live`"), "{why}");
    // aster: open at no tier.
    let why = leverage_refusal("aster", AccountTier::Live, 5.0).expect("refused");
    assert!(why.contains("not open at any tier"), "{why}");
    // A fraction on a whole-number venue.
    let why = leverage_refusal("binance", AccountTier::Demo, 2.5).expect("refused");
    assert!(why.contains("whole number") && why.contains("2.5"), "{why}");
}

#[test]
fn the_candidate_is_whole_at_least_one_and_never_above_an_input() {
    let ceilings = [None, Some(1.0), Some(3.0), Some(7.9), Some(50.0), Some(f64::INFINITY)];
    let mut stated = 1.0;
    while stated <= 125.0 {
        for policy in ceilings {
            for profile in ceilings {
                let got = lev(stated).candidate(policy, profile);
                assert_eq!(got.fract(), 0.0, "{stated} {policy:?} {profile:?}: whole");
                assert!(got >= 1.0, "{stated} {policy:?} {profile:?}: at least 1x");
                assert!(
                    got <= stated,
                    "{stated} {policy:?} {profile:?}: not above the stated figure"
                );
                for ceiling in [policy, profile].into_iter().flatten() {
                    assert!(
                        got <= ceiling.max(1.0),
                        "{stated} {policy:?} {profile:?}: not above a ceiling"
                    );
                }
            }
        }
        stated += 0.37;
    }
    // Hand-picked rows.
    assert_eq!(lev(10.0).candidate(None, None), 10.0);
    assert_eq!(lev(10.0).candidate(Some(2.5), None), 2.0, "ceiling 2.5 with stated 10 gives 2");
    assert_eq!(lev(10.0).candidate(None, Some(4.0)), 4.0);
    assert_eq!(lev(10.0).candidate(Some(8.0), Some(4.0)), 4.0, "the lowest of the three");
    assert_eq!(lev(2.5).candidate(None, None), 2.0, "a fraction floors");
    assert_eq!(lev(1.0).candidate(Some(1.0), None), 1.0);
}

#[test]
fn a_nonsense_ceiling_cannot_loosen_the_candidate() {
    assert_eq!(lev(5.0).candidate(Some(f64::NAN), None), 5.0, "NaN is ignored by min");
    assert_eq!(lev(5.0).candidate(Some(0.0), None), 1.0, "a sub-1 ceiling degrades to 1x");
    assert_eq!(lev(5.0).candidate(Some(-3.0), Some(f64::NAN)), 1.0);
    assert_eq!(lev(5.0).candidate(Some(f64::INFINITY), Some(f64::INFINITY)), 5.0);
}
