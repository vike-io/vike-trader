use super::*;

fn label(text: &str) -> AccountLabel {
    AccountLabel::parse(text).expect("a legal label")
}

fn row(label: AccountLabel, block: ArmingBlock, account_ids: Vec<i64>) -> VenueArming {
    VenueArming {
        venue: "bybit",
        label,
        tier: Some(AccountTier::Live),
        effective: None,
        block,
        account_ids,
    }
}

/// A labelled row names its account, a default row names only the venue.
#[test]
fn a_labelled_row_names_its_account() {
    let labelled = row(label("ALT"), ArmingBlock::None, vec![7]);
    assert!(!labelled.is_default_account());
    assert!(labelled.subject().contains("ALT"), "{}", labelled.subject());
    let default = row(AccountLabel::Default, ArmingBlock::None, vec![3]);
    assert!(default.is_default_account());
    assert_eq!(default.subject(), "bybit");
}

/// Every block renders a non-empty sentence that NAMES its venue — the column's whole job — and
/// only the clear one has an empty badge.
#[test]
fn every_block_explains_itself_and_names_the_venue() {
    for block in ArmingBlock::ALL {
        for account in [AccountLabel::Default, label("ALT")] {
            let why = row(account.clone(), block, vec![]).why();
            assert!(why.contains("bybit") || why.contains("BYBIT"), "{block:?}: {why}");
            assert!(why.len() > 20, "{block:?}: {why}");
            // …and a LABELLED row's sentence NAMES the account for every cause that is about the
            // account itself, so "bybit does not run" can never be read as a statement about the
            // account beside the one it describes.
            if let Some(text) = account.text()
                && matches!(
                    block,
                    ArmingBlock::PaperTier
                        | ArmingBlock::AccountInactive
                        | ArmingBlock::NoAccountRow
                        | ArmingBlock::TierConflict
                        | ArmingBlock::LiveSwitchOff
                        | ArmingBlock::LiveCredentialsAbsent
                        | ArmingBlock::LiveTierNotWired
                        | ArmingBlock::PreflightFailed
                        | ArmingBlock::AdapterFailed
                        | ArmingBlock::GatewayUnavailable
                        | ArmingBlock::OrdersHeld
                        | ArmingBlock::FeedsUnavailable
                )
            {
                assert!(why.contains(text), "{block:?} on a labelled row must name it: {why}");
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
            | ArmingBlock::PaperTier
            | ArmingBlock::AccountInactive
            | ArmingBlock::NoAccountRow
            | ArmingBlock::TierConflict
            | ArmingBlock::FeatureAbsent
            | ArmingBlock::NoLiveArm
            | ArmingBlock::NoCredentials
            | ArmingBlock::ExecFlagUnset
            | ArmingBlock::LiveSwitchOff
            | ArmingBlock::SdkAbsent
            | ArmingBlock::DemoOnlyArm
            | ArmingBlock::LiveOnlyArm
            | ArmingBlock::LiveCredentialsAbsent
            | ArmingBlock::LiveTierNotWired
            | ArmingBlock::NoAccountSupport
            | ArmingBlock::AccountNotInStore
            | ArmingBlock::SidecarHeldElsewhere
            | ArmingBlock::PreflightFailed
            | ArmingBlock::AdapterFailed
            | ArmingBlock::GatewayUnavailable
            | ArmingBlock::OrdersHeld
            | ArmingBlock::FeedsUnavailable => true,
        };
        assert!(covered);
    }
}

/// **Exactly three blocks RUN the account** (decision 0156): at its tier, on the paper simulator
/// for a `paper` row, and at its tier with every order held. Every other block is NOT RUNNING, and
/// its sentence says so in those words — never "stays paper", which would name a substitute.
#[test]
fn exactly_three_blocks_run_and_every_other_says_it_does_not_run() {
    for block in ArmingBlock::ALL {
        let runs =
            matches!(block, ArmingBlock::None | ArmingBlock::PaperTier | ArmingBlock::OrdersHeld);
        assert_eq!(block.runs(), runs, "{block:?}");
        if runs {
            continue;
        }
        let why = row(AccountLabel::Default, block, vec![]).why();
        assert!(
            why.contains("does not run")
                || why.contains("does NOT RUN")
                || why.contains("can never run"),
            "{block:?} must say the account does not run: {why}"
        );
        assert!(
            !why.to_lowercase().contains("stays paper"),
            "{block:?} names no substitute: {why}"
        );
    }
}

/// **The account-table causes name the account verb that clears them, with the row's own id.**
/// The remedy is an `account` verb on the `account` table — never a `config set` of a settings key,
/// which no longer arms anything.
#[test]
fn the_account_table_causes_name_the_account_verb_and_the_id() {
    let cases = [
        (ArmingBlock::PaperTier, "vike-cli secrets account set-tier --id 7"),
        (ArmingBlock::AccountInactive, "vike-cli secrets account activate --id 7"),
        (ArmingBlock::TierConflict, "vike-cli secrets account deactivate --id <one of 7, 9>"),
        (ArmingBlock::LiveCredentialsAbsent, "vike-cli secrets account set-tier --id 7"),
    ];
    for (block, verb) in cases {
        let ids = if block == ArmingBlock::TierConflict { vec![7, 9] } else { vec![7] };
        let why = row(label("ALT"), block, ids).why();
        assert!(why.contains(verb), "{block:?} must name `{verb}`: {why}");
        assert!(!why.contains("config set policy"), "{block:?} names no settings key: {why}");
    }
    // With no id in hand the remedy points at the listing that prints one.
    let why = row(AccountLabel::Default, ArmingBlock::AccountInactive, vec![]).why();
    assert!(why.contains("--id <N>") && why.contains("vike-cli secrets accounts"), "{why}");
    // A missing row is cleared by ADDING one — at `paper` too, since a paper engine exists only for
    // a row at tier `paper` — and it runs at the next restart.
    let why = row(AccountLabel::Default, ArmingBlock::NoAccountRow, vec![]).why();
    assert!(why.contains("vike-cli secrets account add --venue bybit"), "{why}");
    assert!(why.contains("<paper|demo|live>"), "{why}");
    assert!(why.contains("next restart"), "{why}");
}

/// **Two active tiers of one account is its own cause**: the row says it does NOT RUN, says why it
/// did not pick, and the badge is distinct from the paper-tier one.
#[test]
fn a_tier_conflict_does_not_run_and_does_not_pick() {
    let why = row(AccountLabel::Default, ArmingBlock::TierConflict, vec![3, 4]).why();
    assert!(why.contains("does NOT RUN"), "{why}");
    assert!(why.contains("demo") && why.contains("live"), "{why}");
    assert_eq!(ArmingBlock::TierConflict.as_str(), "two active tiers");
    assert_ne!(ArmingBlock::TierConflict.as_str(), ArmingBlock::PaperTier.as_str());
    assert!(!why.contains("of any label"), "an account-scoped venue judges one label: {why}");
}

/// **A VENUE-scoped tier's conflict says "of any label"** (decision 0140): ibkr's rows are judged
/// over every label, so the sentence names the venue-wide rule, says no account of it runs, and
/// still names the labelled subject and the ids to choose between.
#[test]
fn a_venue_scoped_tier_conflict_says_of_any_label() {
    let ibkr = |label: AccountLabel| VenueArming {
        venue: "ibkr",
        ..row(label, ArmingBlock::TierConflict, vec![3, 4])
    };
    for subject in [AccountLabel::Default, label("ALT")] {
        let why = ibkr(subject.clone()).why();
        assert!(why.contains("of any label") && why.contains("nor does any ibkr account"), "{why}");
        assert!(why.contains("does NOT RUN"), "{why}");
        assert!(why.contains("demo") && why.contains("live"), "{why}");
        assert!(why.contains("deactivate --id <one of 3, 4>"), "{why}");
        if let Some(text) = subject.text() {
            assert!(why.contains(text), "a labelled row names its account: {why}");
        }
    }
}

/// **A LIVE-tier key set the arm cannot use is its own cause, and it says so.** It is neither
/// "no credentials" (the store holds some) nor "no live credentials" (that one means the arm COULD
/// use a live set and none is stored): the sentence names the venue, the live tier it will not
/// select, that the account does not run, and the way out — and a LABELLED row names its account.
#[test]
fn a_live_tier_the_arm_cannot_use_is_named_for_what_it_is() {
    let row = |label: AccountLabel| VenueArming {
        venue: "alpaca",
        label,
        tier: Some(AccountTier::Demo),
        effective: None,
        block: ArmingBlock::LiveTierNotWired,
        account_ids: vec![],
    };
    let why = row(AccountLabel::Default).why();
    assert!(why.contains("alpaca"), "names the venue: {why}");
    assert!(why.contains("LIVE"), "names the tier that was found: {why}");
    assert!(why.contains("demo"), "names the tier the arm mounts: {why}");
    assert!(why.contains("does NOT RUN"), "says it does not run: {why}");
    assert!(why.contains("secrets list"), "says where to look: {why}");
    assert!(!why.contains("no usable"), "must not read as the missing-credentials sentence: {why}");
    let labelled = row(label("ALT")).why();
    assert!(labelled.contains("ALT"), "a labelled row names its account: {labelled}");
    assert_eq!(ArmingBlock::LiveTierNotWired.as_str(), "live tier not wired");
    assert!(!ArmingBlock::LiveTierNotWired.is_clear());
}

/// A live account with no live keys does NOT RUN and says it never falls to demo.
#[test]
fn a_live_account_without_live_keys_never_falls_to_demo() {
    let why = row(AccountLabel::Default, ArmingBlock::LiveCredentialsAbsent, vec![5]).why();
    assert!(why.contains("MAINNET"), "{why}");
    assert!(why.contains("does NOT RUN"), "{why}");
    assert!(why.contains("never falls to the demo network"), "{why}");
}

/// **The two box switches are STOPS** (decision 0156 item 4): each names its switch and the command
/// that lifts it, and neither says the account became paper.
#[test]
fn the_box_switches_are_stops_that_name_their_switch() {
    let live = row(label("ALT"), ArmingBlock::LiveSwitchOff, vec![2]).why();
    assert!(live.contains("flags.tradehub_live"), "{live}");
    assert!(live.contains("config set flags.tradehub_live true"), "{live}");
    assert!(live.contains("tier live"), "names the account's own tier: {live}");
    assert!(live.contains("a paper account still does"), "{live}");
    let poly = VenueArming {
        venue: "polymarket",
        ..row(AccountLabel::Default, ArmingBlock::ExecFlagUnset, vec![])
    }
    .why();
    assert!(poly.contains("flags.poly_exec"), "{poly}");
    assert!(poly.contains("does not run"), "{poly}");
}

/// Every badge is its own: two causes that print the same short word are one cause to the
/// operator, whatever the enum says.
#[test]
fn every_badge_is_distinct() {
    let mut seen = std::collections::BTreeSet::new();
    for block in ArmingBlock::ALL {
        assert!(seen.insert(block.as_str()), "{block:?}: badge {:?} is not unique", block.as_str());
    }
}

/// `runs` is strictly "at the tier its row states, under a block that runs" — no row, a projection
/// that names no tier, and a bridge's answer ABOVE the account's tier (refused by the mount) all
/// read NOT RUNNING.
#[test]
fn runs_means_at_the_rows_own_tier() {
    use AccountTier::{Demo, Live, Paper};
    let row = |tier, effective, block| VenueArming {
        venue: "binance",
        label: AccountLabel::Default,
        tier,
        effective,
        block,
        account_ids: vec![],
    };
    assert!(row(Some(Live), Some(Live), ArmingBlock::None).runs());
    assert!(row(Some(Demo), Some(Demo), ArmingBlock::None).runs());
    assert!(row(Some(Paper), Some(Paper), ArmingBlock::PaperTier).runs());
    assert!(row(Some(Live), Some(Live), ArmingBlock::OrdersHeld).runs());
    assert!(!row(Some(Live), None, ArmingBlock::LiveCredentialsAbsent).runs());
    assert!(!row(None, None, ArmingBlock::NoAccountRow).runs());
    assert!(!row(Some(Demo), Some(Live), ArmingBlock::None).runs(), "above the tier: refused");
}

/// The routing key is the account's, and carries no tier: one process mounts one engine per
/// `(venue, label)`.
#[test]
fn the_route_key_is_the_bare_venue_for_the_default_account() {
    let default = row(AccountLabel::Default, ArmingBlock::None, vec![1]);
    assert_eq!(default.route_key(), "bybit");
    let labelled = row(label("ALT"), ArmingBlock::None, vec![2]);
    assert_ne!(labelled.route_key(), "bybit", "a labelled account routes under its own key");
}

/// **The arming screen's word table, pinned whole**: every block's badge, one row each. A badge is
/// what an operator greps the screen and the `venue_mounted` journal for — and the reason word a
/// not-running account carries onto the wire — so a reworded badge is a visible change, made here
/// on purpose rather than in passing. `seen` holds the table to the whole of [`ArmingBlock::ALL`].
#[test]
fn the_arming_screen_word_table() {
    let table = [
        (ArmingBlock::None, ""),
        (ArmingBlock::PaperTier, "paper tier"),
        (ArmingBlock::AccountInactive, "account inactive"),
        (ArmingBlock::NoAccountRow, "no account row"),
        (ArmingBlock::TierConflict, "two active tiers"),
        (ArmingBlock::FeatureAbsent, "feature absent"),
        (ArmingBlock::NoLiveArm, "no live arm"),
        (ArmingBlock::NoCredentials, "no credentials"),
        (ArmingBlock::ExecFlagUnset, "flags.poly_exec off"),
        (ArmingBlock::LiveSwitchOff, "flags.tradehub_live off"),
        (ArmingBlock::SdkAbsent, "SDK not loaded"),
        (ArmingBlock::DemoOnlyArm, "demo-only arm"),
        (ArmingBlock::LiveOnlyArm, "live-only arm"),
        (ArmingBlock::LiveCredentialsAbsent, "no live credentials"),
        (ArmingBlock::LiveTierNotWired, "live tier not wired"),
        (ArmingBlock::NoAccountSupport, "no second-account support"),
        (ArmingBlock::AccountNotInStore, "account not in the store"),
        (ArmingBlock::SidecarHeldElsewhere, "another account holds the session"),
        (ArmingBlock::PreflightFailed, "preflight failed"),
        (ArmingBlock::AdapterFailed, "adapter failed"),
        (ArmingBlock::GatewayUnavailable, "gatewayhub unavailable"),
        (ArmingBlock::OrdersHeld, "orders held"),
        (ArmingBlock::FeedsUnavailable, "price feeds unavailable (datahub)"),
    ];
    assert_eq!(table.len(), ArmingBlock::ALL.len(), "one row per block");
    for (block, badge) in table {
        assert_eq!(block.as_str(), badge, "{block:?}");
        assert!(ArmingBlock::ALL.contains(&block), "{block:?} is in ALL");
    }
}

/// **A held arm mounts AT its tier and says every order is refused.** The row RUNS (its effective
/// tier is the account's), so the word and its sentence are the only things on the row that say
/// orders are held: the sentence names the tier it mounts, that orders are REFUSED and none reaches
/// the venue, that data and reconcile still run, and that no setting lifts it.
#[test]
fn an_orders_held_row_mounts_its_tier_and_refuses_every_order() {
    let held = |label: AccountLabel| VenueArming {
        venue: "ibkr",
        label,
        tier: Some(AccountTier::Live),
        effective: Some(AccountTier::Live),
        block: ArmingBlock::OrdersHeld,
        account_ids: vec![4],
    };
    let row = held(AccountLabel::Default);
    assert!(row.runs(), "a held arm runs at its own tier");
    assert!(!row.block.is_clear(), "and the row still says something is held");
    let why = row.why();
    for needle in ["ibkr", "mounts live", "ORDER HELD", "REFUSES every order", "none reaches"] {
        assert!(why.contains(needle), "expected `{needle}`: {why}");
    }
    assert!(why.contains("data") && why.contains("reconciles"), "{why}");
    assert!(why.contains("No setting lifts"), "{why}");
    let labelled = held(label("ALT")).why();
    assert!(labelled.contains("ibkr account `ALT`"), "{labelled}");
}

/// **The two demo-shaped words stay true of an arm that HAS a live tier** (ibkr): neither may claim
/// the venue has no live arm, and each points at the daemon's log for the rule that refused.
#[test]
fn the_demo_shaped_words_do_not_deny_an_arm_its_live_tier() {
    for block in [ArmingBlock::DemoOnlyArm, ArmingBlock::LiveTierNotWired] {
        let why = VenueArming {
            venue: "ibkr",
            label: AccountLabel::Default,
            tier: Some(AccountTier::Live),
            effective: None,
            block,
            account_ids: vec![2],
        }
        .why();
        assert!(!why.contains("never selects a live one"), "{block:?}: {why}");
        assert!(!why.contains("has no live tier —"), "{block:?}: {why}");
        assert!(why.contains("its live tier"), "{block:?} names the live-tier case: {why}");
        assert!(why.contains("daemon's log"), "{block:?} points at the log: {why}");
    }
}

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

/// **The gatewayhub-down block is a refusal that names the daemon and the config row, and says no
/// paper account stands in** (decision 0156 / 0164 D7); it is not one of the three that run.
#[test]
fn a_gateway_unavailable_row_is_not_running_and_names_the_gatewayhub() {
    let r = VenueArming {
        venue: "bybit",
        label: AccountLabel::Default,
        tier: Some(AccountTier::Demo),
        effective: None,
        block: ArmingBlock::GatewayUnavailable,
        account_ids: vec![4],
    };
    assert!(!r.runs() && !ArmingBlock::GatewayUnavailable.runs());
    let why = r.why();
    for needle in ["gatewayhub", "config.gatewayhub_connect_wait_ms", "NOT RUN", "tier demo"] {
        assert!(why.contains(needle), "expected `{needle}`: {why}");
    }
    assert!(why.contains("not started in its place"), "{why}");
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
    ];
    assert_eq!(table.len(), ArmingBlock::ALL.len(), "one row per block");
    for (block, word) in table {
        assert_eq!(block.word(), word, "{block:?}");
    }
}
