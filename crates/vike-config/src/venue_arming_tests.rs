use super::*;
use vike_model::VENUES;

fn label(text: &str) -> AccountLabel {
    AccountLabel::parse(text).expect("a legal label")
}

/// The key is the section the writer resolves plus the venue — never a hand-typed literal, so
/// a renamed section moves the confirm and the write together.
#[test]
fn the_key_is_the_one_the_writer_takes() {
    assert_eq!(arming_key("bybit"), "policy.venues.bybit");
    let row = VenueArming {
        venue: "bybit",
        label: AccountLabel::Default,
        ceiling: VenueMode::Paper,
        effective: VenueMode::Paper,
        block: ArmingBlock::Disarmed,
    };
    assert_eq!(row.key(), arming_key("bybit"));
}

/// **A LABELLED row writes the ACCOUNT's key, never the venue's** — the one property that keeps
/// a second account's switch from moving the ceiling of the account beside it.
#[test]
fn a_labelled_row_writes_its_own_account_key() {
    assert_eq!(account_arming_key("bybit", "ALT"), "policy.accounts.bybit.ALT");
    let row = VenueArming {
        venue: "bybit",
        label: label("ALT"),
        ceiling: VenueMode::Demo,
        effective: VenueMode::Demo,
        block: ArmingBlock::None,
    };
    assert_eq!(row.key(), account_arming_key("bybit", "ALT"));
    assert_ne!(row.key(), arming_key("bybit"), "an account switch must not write the venue line");
    assert!(!row.is_default_account());
    assert!(row.subject().contains("ALT"), "{}", row.subject());
}

/// A mount-less build states the FILE and nothing else, for the whole roster — the DEFAULT
/// account only, because it can enumerate no other.
#[test]
fn ceilings_only_states_the_file_and_claims_no_tier() {
    let policy = VenuePolicy::default().declare("bybit", VenueMode::Live);
    let rows = ceilings_only(&policy);
    assert_eq!(rows.len(), VENUES.len(), "one row per roster venue");
    for row in &rows {
        assert_eq!(row.block, ArmingBlock::NoMountInThisBuild);
        assert!(
            row.is_default_account(),
            "{}: a thin build enumerates no second account",
            row.venue
        );
        assert_eq!(
            row.effective, row.ceiling,
            "{}: a build with no mount must not CLAIM a tier",
            row.venue
        );
        assert!(!row.is_capped());
    }
    assert_eq!(
        rows.iter().find(|r| r.venue == "bybit").expect("bybit").ceiling,
        VenueMode::Live,
        "the file's own content is still rendered"
    );
}

/// Every block renders a non-empty sentence that NAMES its venue — the column's whole job —
/// and only the clear one has an empty badge.
#[test]
fn every_block_explains_itself_and_names_the_venue() {
    for block in ArmingBlock::ALL {
        for account in [AccountLabel::Default, label("ALT")] {
            let row = VenueArming {
                venue: "bybit",
                label: account.clone(),
                ceiling: VenueMode::Live,
                effective: VenueMode::Paper,
                block,
            };
            let why = row.why();
            assert!(why.contains("bybit") || why.contains("BYBIT"), "{block:?}: {why}");
            assert!(why.len() > 20, "{block:?}: {why}");
            // …and a LABELLED row's sentence NAMES the account, so "bybit is paper" can never
            // be read as a statement about the account beside the one it describes.
            if let Some(text) = account.text() {
                assert!(
                    why.contains(text) || !matches!(block, ArmingBlock::AccountNotNamed),
                    "{block:?} on a labelled row must name the account: {why}"
                );
            }
            assert_eq!(block.is_clear(), block == ArmingBlock::None);
            assert_eq!(block.as_str().is_empty(), block.is_clear(), "{block:?}");
        }
    }
    // …and `ALL` really is every variant: an exhaustive match with no wildcard, so a new
    // variant fails to compile here rather than silently escaping the loop above.
    for block in ArmingBlock::ALL {
        let covered = match block {
            ArmingBlock::None
            | ArmingBlock::Disarmed
            | ArmingBlock::FeatureAbsent
            | ArmingBlock::NoLiveArm
            | ArmingBlock::NoMountInThisBuild
            | ArmingBlock::NoCredentials
            | ArmingBlock::ExecFlagUnset
            | ArmingBlock::SdkAbsent
            | ArmingBlock::DemoOnlyArm
            | ArmingBlock::LiveOnlyArm
            | ArmingBlock::LiveCredentialsAbsent
            | ArmingBlock::LiveTierNotWired
            | ArmingBlock::AccountNotNamed
            | ArmingBlock::NoAccountSupport
            | ArmingBlock::AccountNotInStore
            | ArmingBlock::SidecarHeldElsewhere => true,
        };
        assert!(covered);
    }
}

/// **A LIVE-tier key set the arm cannot use is its own cause, and it says so.** It is neither
/// "no credentials" (the store holds some) nor "no live credentials" (that one means the arm COULD
/// use a live set and none is stored): the sentence names the venue, the live tier it will not
/// select, that it stays paper, and the way out — and a LABELLED row names its account.
#[test]
fn a_live_tier_the_arm_cannot_use_is_named_for_what_it_is() {
    let row = |label: AccountLabel| VenueArming {
        venue: "alpaca",
        label,
        ceiling: VenueMode::Demo,
        effective: VenueMode::Paper,
        block: ArmingBlock::LiveTierNotWired,
    };
    let why = row(AccountLabel::Default).why();
    assert!(why.contains("alpaca"), "names the venue: {why}");
    assert!(why.contains("LIVE"), "names the tier that was found: {why}");
    assert!(why.contains("demo"), "names the tier the arm mounts: {why}");
    assert!(why.contains("PAPER"), "says where it stays: {why}");
    assert!(why.contains("secrets list"), "says where to look: {why}");
    assert!(!why.contains("no usable"), "must not read as the missing-credentials sentence: {why}");
    let labelled = row(label("ALT")).why();
    assert!(labelled.contains("ALT"), "a labelled row names its account: {labelled}");
    assert_eq!(ArmingBlock::LiveTierNotWired.as_str(), "live tier not wired");
    assert!(!ArmingBlock::LiveTierNotWired.is_clear());
}

/// Every badge is its own: two causes that print the same short word on the arming screen are
/// one cause to the operator, whatever the enum says.
#[test]
fn every_badge_is_distinct() {
    let mut seen = std::collections::BTreeSet::new();
    for block in ArmingBlock::ALL {
        assert!(seen.insert(block.as_str()), "{block:?}: badge {:?} is not unique", block.as_str());
    }
}

/// `is_capped` is strictly "below the ceiling" — a row at its ceiling is not capped even when
/// something else about it is unusual.
#[test]
fn capped_means_below_the_ceiling() {
    let row = |ceiling, effective| VenueArming {
        venue: "binance",
        label: AccountLabel::Default,
        ceiling,
        effective,
        block: ArmingBlock::None,
    };
    assert!(row(VenueMode::Live, VenueMode::Demo).is_capped());
    assert!(row(VenueMode::Live, VenueMode::Paper).is_capped());
    assert!(!row(VenueMode::Demo, VenueMode::Demo).is_capped());
    assert!(!row(VenueMode::Paper, VenueMode::Paper).is_capped());
}
