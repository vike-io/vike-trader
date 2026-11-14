use super::ArmingBlock;

/// **The wire words** (`ArmingBlock::word`): one per block, every one distinct and in the
/// `snake_case` a program keys on, each read back to its block by `from_word`, and none of them is
/// the person-facing badge (`as_str`, which holds spaces and is empty for `None`). The table is
/// held to the whole of `ALL`.
#[test]
fn the_wire_words_round_trip_and_are_distinct() {
    let mut seen = std::collections::BTreeSet::new();
    for block in ArmingBlock::ALL {
        let word = block.word();
        assert!(
            !word.is_empty() && word.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'),
            "{block:?}: `{word}` is not snake_case"
        );
        assert!(seen.insert(word), "{block:?}: the word `{word}` is spelled twice");
        assert_eq!(ArmingBlock::from_word(word), Some(block), "{block:?}: `{word}` reads back");
        assert_ne!(word, block.as_str(), "{block:?}: the wire word is not the badge");
    }
    assert_eq!(seen.len(), ArmingBlock::ALL.len());
}

/// A word this build does not know is `None`, never a block: a newer gatewayhub's cause must not
/// read as armed (the consumer maps `None` to `AdapterFailed`). The badge is no wire word either.
#[test]
fn an_unknown_wire_word_is_none_and_a_badge_is_not_a_word() {
    for word in
        ["", "  ", "NONE", "paper tier", "gatewayhub unavailable", "a_cause_from_the_future"]
    {
        assert_eq!(ArmingBlock::from_word(word), None, "`{word}`");
    }
}

/// **The not-connected set, pinned whole** (owner 2026-11-14): exactly these six blocks deactivate a strategy mount
/// instead of refusing the node, and none of them is a block under which an account runs.
#[test]
fn the_venue_not_connected_set_is_pinned() {
    let set: Vec<ArmingBlock> =
        ArmingBlock::ALL.into_iter().filter(|b| b.venue_not_connected()).collect();
    assert_eq!(
        set,
        [
            ArmingBlock::SdkAbsent,
            ArmingBlock::PreflightFailed,
            ArmingBlock::AdapterFailed,
            ArmingBlock::GatewayUnavailable,
            ArmingBlock::FeedsUnavailable,
            ArmingBlock::FxMarketClosed,
        ]
    );
    assert!(set.iter().all(|b| !b.runs()));
}

/// **The wire words, pinned whole.** `the_wire_words_round_trip_and_are_distinct` holds the table
/// self-consistent, which a RENAME would keep: both ends of a wire built from one tree rename
/// together, and an older gatewayhub or tradehub then reads the new word as unknown (`adapter
/// failed`, never armed) for want of a version bump. So each word is written out here, and a change
/// is a visible, deliberate edit to this file and a wire change to negotiate.
#[test]
fn the_wire_word_table() {
    let table = [
        (ArmingBlock::None, "none"),
        (ArmingBlock::PaperTier, "paper_tier"),
        (ArmingBlock::AccountInactive, "account_inactive"),
        (ArmingBlock::NoAccountRow, "no_account_row"),
        (ArmingBlock::TierConflict, "tier_conflict"),
        (ArmingBlock::NoAccountSymbol, "no_account_symbol"),
        (ArmingBlock::FeatureAbsent, "feature_absent"),
        (ArmingBlock::NoLiveArm, "no_live_arm"),
        (ArmingBlock::NoCredentials, "no_credentials"),
        (ArmingBlock::ExecFlagUnset, "exec_flag_unset"),
        (ArmingBlock::LiveSwitchOff, "live_switch_off"),
        (ArmingBlock::SdkAbsent, "sdk_absent"),
        (ArmingBlock::DemoOnlyArm, "demo_only_arm"),
        (ArmingBlock::LiveOnlyArm, "live_only_arm"),
        (ArmingBlock::LiveCredentialsAbsent, "live_credentials_absent"),
        (ArmingBlock::LiveTierNotWired, "live_tier_not_wired"),
        (ArmingBlock::NoAccountSupport, "no_account_support"),
        (ArmingBlock::AccountNotInStore, "account_not_in_store"),
        (ArmingBlock::SidecarHeldElsewhere, "sidecar_held_elsewhere"),
        (ArmingBlock::PreflightFailed, "preflight_failed"),
        (ArmingBlock::AdapterFailed, "adapter_failed"),
        (ArmingBlock::GatewayUnavailable, "gateway_unavailable"),
        (ArmingBlock::OrdersHeld, "orders_held"),
        (ArmingBlock::FeedsUnavailable, "feeds_unavailable"),
        (ArmingBlock::FxMarketClosed, "fx_market_closed"),
    ];
    assert_eq!(table.len(), ArmingBlock::ALL.len(), "one row per block");
    for (block, word) in table {
        assert_eq!(block.word(), word, "{block:?}");
    }
}
