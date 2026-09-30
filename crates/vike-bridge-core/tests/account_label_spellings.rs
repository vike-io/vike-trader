//! **The account-LABEL grammar is spelled ONCE and ENFORCED twice, and this is the only place that
//! can compare the two enforcements.**
//!
//! `vike_model::account_keys::AccountLabel::parse` is the AUTHORITY — it is what every
//! operator-facing surface validates through, and its `AccountKeyError` names the rule that was
//! broken. `vike_secrets::normalized_account_label` is the STORE's own floor under it: a second
//! PREDICATE, deliberately, so that a write reaching the store without having gone through the
//! parser still cannot put a row in the `label` column that no policy address can name.
//!
//! ⚠ **The two CONSTANTS were also spelled twice, and are not any more.** `vike-secrets` declared
//! `ACCOUNT_LABEL_MAX_LEN` and `RESERVED_ACCOUNT_LABEL` beside its predicate, for the one reason
//! its doc gave: *"this crate declares no `vike-*` dependency at all … so the label grammar cannot
//! be imported here."*
//! `docs/decisions/0072-vike-secrets-takes-one-vike-edge-and-is-not-split.md` (accepted 2026-09-20)
//! made that false — the manifest declares `vike-model` — so `crates/vike-secrets/src/db.rs`
//! imports `MAX_LABEL_LEN` and `RESERVED_DEFAULT_LABEL` and declares neither. The test that held
//! the two numbers equal went with them: with ONE declaration it is an assertion that cannot fail,
//! which is worse than no test because it reads like coverage. That is the disposition
//! `settings_dir_spellings.rs` records for the same merge, and its argument is the one this file
//! still rests on.
//!
//! ⚠ **What did NOT collapse is the PREDICATE, and the drift is still the hazard.** A store that
//! accepted a label the model refuses would let a row exist that no policy address can name; a
//! store that refused one the model accepts would make a GUI's own validator promise something the
//! write then denies. Neither shows up as a compile error even now that both crates compile the
//! same constant, because the two are different FUNCTIONS over it — so the equality still has to be
//! asserted from a third crate that depends on both, exactly as `settings_dir_spellings.rs` puts
//! it: *"neither crate can see the other, so it could only ever live in the one crate that depends
//! on both."* `vike-bridge-core` is still that crate.

use vike_model::account_keys::{AccountLabel, MAX_LABEL_LEN, RESERVED_DEFAULT_LABEL};
use vike_secrets::normalized_account_label;

/// The two PREDICATES answer identically over every input that separates them: the empty string,
/// the reserved label, an over-long label, lowercase (refused rather than repaired — by BOTH), the
/// separator, punctuation, whitespace, non-ASCII, and the ordinary shapes that must pass.
///
/// ⚠ The lowercase case is the one worth naming. Both refuse `alt` rather than uppercasing it, and
/// they refuse it for the same stated reason: a spelling the program fixes on the operator's behalf
/// is a spelling nobody learns, and the label they then write into `policy.accounts.<venue>.<LABEL>`
/// would match no row. A "helpful" repair added to either copy alone is exactly the drift this file
/// exists to catch.
#[test]
fn the_two_label_predicates_agree_on_every_shape() {
    let long = "A".repeat(MAX_LABEL_LEN + 1);
    let at_cap = "A".repeat(MAX_LABEL_LEN);
    let cases: [&str; 16] = [
        "",
        "ALT",
        "HEDGE2",
        "7",
        &at_cap,
        &long,
        RESERVED_DEFAULT_LABEL,
        "alt",
        "Alt",
        "A_B",
        "A-B",
        "A B",
        " ALT",
        "ALT ",
        "ÄLT",
        "A\nB",
    ];
    for case in cases {
        let model = AccountLabel::parse(case).is_ok();
        let store = normalized_account_label(case).is_some();
        assert_eq!(
            model, store,
            "the two label spellings disagree about {case:?}: vike-model says {model}, \
             vike-secrets says {store}. One of them has been edited without the other — the model's \
             parser is the authority and the store's predicate is the floor under it, and a floor \
             that is not the same shape as the thing above it is not a floor."
        );
    }
}

/// The store's predicate REPAIRS nothing — a label it accepts comes back byte-identical, and a
/// label it refuses comes back as `None` rather than as a trimmed or uppercased near-miss.
///
/// This is the half a shared constant cannot buy: the two could agree on every yes/no answer while
/// one of them silently rewrote its input, and the row would then carry a label the operator did
/// not type.
#[test]
fn the_store_predicate_returns_what_it_was_given() {
    for ok in ["ALT", "HEDGE2", "7", "A1B2C3"] {
        assert_eq!(
            normalized_account_label(ok).as_deref(),
            Some(ok),
            "{ok} must come back unchanged — a label the store rewrote is a label that does not \
             match the one written into policy.toml"
        );
    }
    for bad in [" ALT", "alt", "ALT "] {
        assert!(
            normalized_account_label(bad).is_none(),
            "{bad:?} must be REFUSED rather than repaired into a legal label the operator did not \
             choose"
        );
    }
}
