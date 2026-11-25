//! The leverage table and its pure rules (decision 0182).
//!
//! | test | what would be true without it |
//! |---|---|
//! | [`the_first_release_opens_demo_only_on_the_posting_venues`] | a `live` tier of bybit or okx, or deribit, or aster, opened by an edit nobody meant; binance's `live` (the owner's ruling) lost |
//! | [`a_closed_or_unlisted_row_is_never_honoured`] | the engine or the lane acting on a figure planted on a closed row |
//! | [`the_refusal_says_why_in_the_operators_words`] | a refusal that names no reason, or the wrong one |
//! | [`the_verb_refuses_a_labelled_live_row_and_takes_the_twins_parent`] | a figure on a labelled live row of a lane-twin venue accepted and then silently ignored |
//!
//! The stated figure's own rule (`AccountLeverage::candidate`) is tested where it lives:
//! `crates/vike-secrets/src/db/accounts/leverage_candidate_tests.rs`.

use vike_model::AccountTier;

use super::*;

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
        // The owner opened ONLY binance at `live` (2026-10-19, for its lane twin): bybit and okx stay
        // closed by the table.
        assert_eq!(
            row.allows(AccountTier::Live),
            venue == "binance",
            "{venue}: live is open for binance alone"
        );
        assert!(!row.allows(AccountTier::Paper), "{venue}: a paper account has no venue to read");
        assert!(row.whole_only, "{venue}: the wire takes a whole number");
    }
    assert_eq!(
        leverage_venue("binance").expect("listed").tiers,
        &[AccountTier::Demo, AccountTier::Live],
        "binance: demo and live, nothing else"
    );
    let aster = leverage_venue("aster").expect("coded");
    assert!(aster.tiers.is_empty(), "aster's row is closed: no demo account exists");
    assert!(leverage_venue("deribit").is_none(), "deribit has no leverage number");
}

#[test]
fn a_closed_or_unlisted_row_is_never_honoured() {
    assert!(leverage_venue_at("bybit", AccountTier::Demo).is_some());
    // binance at `live` is OPEN now (and pinned open below); bybit and okx are still closed there.
    assert!(leverage_venue_at("binance", AccountTier::Live).is_some());
    assert!(leverage_venue_at("binance", AccountTier::Paper).is_none());
    for (venue, tier) in [
        ("bybit", AccountTier::Live),
        ("okx", AccountTier::Live),
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
    // binance at `live`: allowed (whole numbers); a fraction and a `paper` row are still refused.
    assert_eq!(leverage_refusal("binance", AccountTier::Live, 5.0), None);
    let why = leverage_refusal("binance", AccountTier::Live, 2.5).expect("refused");
    assert!(why.contains("whole number") && why.contains("2.5"), "{why}");
    let why = leverage_refusal("binance", AccountTier::Paper, 5.0).expect("refused");
    assert!(why.contains("demo or live") && why.contains("`paper`"), "{why}");
    // aster: open at no tier.
    let why = leverage_refusal("aster", AccountTier::Live, 5.0).expect("refused");
    assert!(why.contains("not open at any tier"), "{why}");
    // A fraction on a whole-number venue.
    let why = leverage_refusal("binance", AccountTier::Demo, 2.5).expect("refused");
    assert!(why.contains("whole number") && why.contains("2.5"), "{why}");
}

/// **`set-leverage` refuses a labelled LIVE row of a lane-twin venue, in words** and takes what it took:
/// the default live row (the twin's parent), any demo row, and nothing a closed venue or a fraction would be.
#[test]
fn the_verb_refuses_a_labelled_live_row_and_takes_the_twins_parent() {
    use row_leverage_refusal as refusal;
    let (live, demo) = (AccountTier::Live, AccountTier::Demo);
    assert_eq!(refusal("binance", live, None, 5.0), None, "the default live row feeds the twin");
    assert_eq!(
        refusal("binance", demo, Some("ALT"), 5.0),
        None,
        "a labelled demo row is as before"
    );
    assert_eq!(refusal("binance", demo, None, 5.0), None);
    let why = refusal("binance", live, Some("ALT"), 5.0).expect("a labelled live row is refused");
    for want in ["DEFAULT", "binance#PERP", "`ALT`", "ignored"] {
        assert!(why.contains(want), "{want:?} missing: {why}");
    }
    assert!(refusal("bybit", live, None, 5.0).is_some(), "bybit stays closed at live");
    assert!(refusal("binance", live, None, 2.5).is_some(), "a fraction is refused as before");
}
