//! **The account-LABEL grammar is spelled TWICE, and this is the only place that can compare the
//! two.**
//!
//! `vike_model::account_keys::AccountLabel::parse` is the AUTHORITY — it is what every
//! operator-facing surface validates through, and its `AccountKeyError` names the rule that was
//! broken. `vike_secrets::normalized_account_label` is the STORE's own floor under it, because
//! `vike-secrets` declares no `vike-*` dependency at all (that is what lets `vike-model` and the
//! bridges both reach it without a cycle) and therefore cannot import the grammar.
//!
//! ⚠ **The duplication is deliberate and the drift is the hazard.** A store that accepted a label
//! the model refuses would let a row exist that no policy address can name; a store that refused
//! one the model accepts would make a GUI's own validator promise something the write then denies.
//! Neither shows up as a compile error, and neither crate can see the other — so the equality has
//! to be asserted from a third crate that depends on both. `vike-bridge-core` is that crate, for
//! exactly the reason `settings_dir_spellings.rs` gives about the settings-directory resolver's own
//! two copies: *"neither crate can see the other, so it could only ever live in the one crate that
//! depends on both."*

use vike_model::account_keys::{AccountLabel, MAX_LABEL_LEN, RESERVED_DEFAULT_LABEL};
use vike_secrets::{ACCOUNT_LABEL_MAX_LEN, RESERVED_ACCOUNT_LABEL, normalized_account_label};

/// The two CONSTANTS agree. They are the cells the two predicates disagree about most cheaply — a
/// length bound drifts by one the day somebody widens the column and edits only one file.
#[test]
fn the_two_label_constants_are_equal() {
    assert_eq!(
        ACCOUNT_LABEL_MAX_LEN, MAX_LABEL_LEN,
        "vike_secrets::ACCOUNT_LABEL_MAX_LEN and vike_model::account_keys::MAX_LABEL_LEN are the \
         SAME rule spelled in two crates that cannot see each other. A store that took a longer \
         label than the model's parser accepts would hold a row no policy address can name."
    );
    assert_eq!(
        RESERVED_ACCOUNT_LABEL, RESERVED_DEFAULT_LABEL,
        "the reserved label is the spelling an UNLABELLED key already addresses; the two copies \
         must name the same string or one surface would write a row the other treats as the \
         default account."
    );
}

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
