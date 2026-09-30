use super::*;

fn label(text: &str) -> AccountLabel {
    AccountLabel::parse(text).expect("a legal label")
}

/// The claim is one per venue per process (was `the_sidecar_is_claimed_once`).
#[test]
fn the_claim_is_taken_once_and_a_kept_claim_outlives_the_guard() {
    let claimed = Mutex::new(Vec::new());
    let held = claim_in(&claimed, "dukascopy").expect("the first caller claims it");
    assert!(claim_in(&claimed, "dukascopy").is_none(), "refused while it is held");
    held.keep();
    assert!(claim_in(&claimed, "dukascopy").is_none(), "a COMMITTED claim outlives the guard");
}

/// THE RELEASE (Review Focus 3): a claim dropped without `keep` frees the venue for the next
/// account (was `a_claim_that_is_not_committed_is_released`).
#[test]
fn a_claim_that_is_not_kept_is_released() {
    let claimed = Mutex::new(Vec::new());
    {
        let _failed = claim_in(&claimed, "dukascopy").expect("claimed");
        assert!(claim_in(&claimed, "dukascopy").is_none(), "held while the start is in flight");
    }
    let second = claim_in(&claimed, "dukascopy").expect("a FAILED start leaves it claimable");
    second.keep();
    assert!(claim_in(&claimed, "dukascopy").is_none(), "…and the one that started keeps it");
}

/// A claim held for one venue does not block another venue's.
#[test]
fn two_venues_claim_independently() {
    let claimed = Mutex::new(Vec::new());
    let a = claim_in(&claimed, "dukascopy").expect("the first claim wins");
    assert!(claim_in(&claimed, "fxcm").is_some(), "another venue is unaffected");
    a.keep();
}

/// The holder rule (was `the_policy_names_the_sidecar_holder_and_the_default_yields`).
#[test]
fn the_policy_names_the_holder_and_the_default_yields() {
    assert_eq!(pick_holder(&[]), AccountLabel::Default);
    assert_eq!(pick_holder(&[label("3716974")]), label("3716974"));
    assert_eq!(
        pick_holder(&[label("3709890"), label("3716974")]),
        label("3709890"),
        "the pick must be deterministic, or two starts of one box would trade two brokers"
    );
}
