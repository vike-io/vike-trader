//! `exclusive`'s unit tests: a claim is taken once, released unless kept, and names its holder.

use super::*;

fn label(text: &str) -> AccountLabel {
    AccountLabel::parse(text).expect("a legal label")
}

/// One claim per resource per process.
#[test]
fn the_claim_is_taken_once_and_a_kept_claim_outlives_the_guard() {
    let claimed = Mutex::new(Vec::new());
    let held = claim_in(&claimed, "JForex sidecar").expect("the first caller claims it");
    assert!(claim_in(&claimed, "JForex sidecar").is_none(), "refused while it is held");
    held.keep();
    assert!(claim_in(&claimed, "JForex sidecar").is_none(), "a COMMITTED claim outlives the guard");
}

/// THE RELEASE (Review Focus 3): a claim dropped without `keep` frees the resource for the next
/// account.
#[test]
fn a_claim_that_is_not_kept_is_released() {
    let claimed = Mutex::new(Vec::new());
    {
        let _failed = claim_in(&claimed, "JForex sidecar").expect("claimed");
        assert!(
            claim_in(&claimed, "JForex sidecar").is_none(),
            "held while the start is in flight"
        );
    }
    let second = claim_in(&claimed, "JForex sidecar").expect("a FAILED start leaves it claimable");
    second.keep();
    assert!(claim_in(&claimed, "JForex sidecar").is_none(), "…and the one that started keeps it");
}

/// A claim held on one resource does not block another resource's.
#[test]
fn two_resources_claim_independently() {
    let claimed = Mutex::new(Vec::new());
    let a = claim_in(&claimed, "JForex sidecar").expect("the first claim wins");
    assert!(claim_in(&claimed, "another venue's resource").is_some(), "another resource is free");
    a.keep();
}

/// Hand `claimed` to a thread that panics while holding its lock, so the mutex is POISONED.
fn poison(claimed: &Mutex<Vec<String>>) {
    std::thread::scope(|s| {
        let joined = s
            .spawn(|| {
                let _held = claimed.lock().expect("not yet poisoned");
                panic!("a thread dies holding the claim set");
            })
            .join();
        assert!(joined.is_err(), "the thread panicked");
    });
    assert!(claimed.is_poisoned(), "precondition: the set is poisoned");
}

/// ⚠ A POISONED set is recovered, not read as "already claimed". Both halves: a claim over it is
/// granted, and a guard dropped over it still releases — otherwise the next account would be
/// refused with a reason that is not true.
#[test]
fn a_poisoned_claim_set_still_claims_and_releases() {
    let claimed = Mutex::new(Vec::new());
    poison(&claimed);
    {
        let _first = claim_in(&claimed, "JForex sidecar").expect("a poisoned set still claims");
        assert!(claim_in(&claimed, "JForex sidecar").is_none(), "…and still refuses a second");
    }
    assert!(
        claim_in(&claimed, "JForex sidecar").is_some(),
        "a guard dropped over a poisoned set still releases its resource"
    );
}

/// The holder rule.
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
