//! `AccountLeverage::candidate`, the one figure a posting venue is asked to hold (decision 0182). The table that
//! says which venue posts one is `crates/vike-venues/src/leverage_venues.rs`'s, tested beside it.
//!
//! | test | what would be true without it |
//! |---|---|
//! | [`the_candidate_is_whole_at_least_one_and_never_above_an_input`] | a fractional figure truncated by the venue while the engine used the fraction, or a ceiling raised |
//! | [`a_nonsense_ceiling_cannot_loosen_the_candidate`] | a NaN or sub-1 ceiling producing a figure above the stated one |

use super::*;

fn leverage(x: f64) -> AccountLeverage {
    AccountLeverage::new(x).expect("a usable figure")
}

#[test]
fn the_candidate_is_whole_at_least_one_and_never_above_an_input() {
    let ceilings = [None, Some(1.0), Some(3.0), Some(7.9), Some(50.0), Some(f64::INFINITY)];
    let mut stated = 1.0;
    while stated <= 125.0 {
        for policy in ceilings {
            for profile in ceilings {
                let got = leverage(stated).candidate(policy, profile);
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
    assert_eq!(leverage(10.0).candidate(None, None), 10.0);
    assert_eq!(
        leverage(10.0).candidate(Some(2.5), None),
        2.0,
        "ceiling 2.5 with stated 10 gives 2"
    );
    assert_eq!(leverage(10.0).candidate(None, Some(4.0)), 4.0);
    assert_eq!(leverage(10.0).candidate(Some(8.0), Some(4.0)), 4.0, "the lowest of the three");
    assert_eq!(leverage(2.5).candidate(None, None), 2.0, "a fraction floors");
    assert_eq!(leverage(1.0).candidate(Some(1.0), None), 1.0);
}

#[test]
fn a_nonsense_ceiling_cannot_loosen_the_candidate() {
    assert_eq!(leverage(5.0).candidate(Some(f64::NAN), None), 5.0, "NaN is ignored by min");
    assert_eq!(leverage(5.0).candidate(Some(0.0), None), 1.0, "a sub-1 ceiling degrades to 1x");
    assert_eq!(leverage(5.0).candidate(Some(-3.0), Some(f64::NAN)), 1.0);
    assert_eq!(leverage(5.0).candidate(Some(f64::INFINITY), Some(f64::INFINITY)), 5.0);
}
