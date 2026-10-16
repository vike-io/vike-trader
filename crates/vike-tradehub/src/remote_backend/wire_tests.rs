//! The conversions: unknown words never arm, the unavailable table is the paper table re-worded by
//! exactly one block, and the `data_only` overlay answers what a credential-stripped in-process
//! backend answered.

use vike_model::accounts::account_keys::AccountLabel;
use vike_tradehub_core::AdapterPolicy;

use super::*;
use crate::arming_fixtures_tests::{armed_fields, keyed_account};

fn wire_row(venue: &str, tier: Option<AccountTier>, word: &str) -> proto::ArmingRow {
    proto::ArmingRow {
        venue: venue.to_string(),
        label: None,
        tier,
        effective: tier,
        block_word: word.to_string(),
        account_ids: vec![7],
        book: Some("spot".to_string()),
    }
}

fn wire_facts() -> proto::VenueFacts {
    proto::VenueFacts {
        adapter_present: true,
        addresses_accounts: true,
        takes_recon_trigger: true,
        grid_source: proto::GridSource::PerSymbolFetch,
        process_exclusive: Some("jforex-sidecar".to_string()),
        clock_decl_word: "wired".to_string(),
    }
}

/// A word this build does not know is `adapter failed`, and a refusal that names a block which
/// RUNS is `adapter failed` too: neither can be the way an account gets armed.
#[test]
fn an_unknown_word_and_a_running_refusal_both_read_as_adapter_failed() {
    assert_eq!(block_of_word("a_cause_from_the_future", "test"), ArmingBlock::AdapterFailed);
    assert_eq!(block_of_word("no_credentials", "test"), ArmingBlock::NoCredentials);
    assert_eq!(refusal_block("a_cause_from_the_future", "test"), ArmingBlock::AdapterFailed);
    for running in [ArmingBlock::None, ArmingBlock::PaperTier, ArmingBlock::OrdersHeld] {
        assert_eq!(
            refusal_block(running.word(), "test"),
            ArmingBlock::AdapterFailed,
            "{running:?}"
        );
    }
    assert_eq!(refusal_block("sidecar_held_elsewhere", "test"), ArmingBlock::SidecarHeldElsewhere);
}

/// The grid mirror converts both ways without loss, every variant.
#[test]
fn the_grid_source_converts_both_ways() {
    for source in [GridSource::InHand, GridSource::PerSymbolFetch, GridSource::NoGrid] {
        assert_eq!(grid_source_of_wire(grid_source_to_wire(source)), source);
    }
}

/// **The wire table becomes the fold's table**, row for row in the daemon's order, facts by venue.
/// A row for a venue this build's roster lacks is skipped; a row whose word is unknown does NOT RUN
/// whatever tier the daemon claimed for it.
#[test]
fn a_wire_table_becomes_the_folds_table() {
    let table = proto::ArmingTable {
        facts: [("bybit".to_string(), wire_facts())].into_iter().collect(),
        rows: vec![
            wire_row("bybit", Some(AccountTier::Demo), "none"),
            wire_row("a-venue-from-the-future", Some(AccountTier::Demo), "none"),
            wire_row("okx", Some(AccountTier::Demo), "a_cause_from_the_future"),
            wire_row("binance", Some(AccountTier::Paper), "paper_tier"),
        ],
    };
    let folded = table_of_wire(&table);
    let rows = folded.rows();
    assert_eq!(rows.len(), 3, "the unknown venue's row is skipped: {rows:?}");
    assert_eq!(rows[0].arming.venue, "bybit");
    assert_eq!(rows[0].arming.effective, Some(AccountTier::Demo));
    assert_eq!(rows[0].arming.account_ids, vec![7]);
    assert_eq!(rows[0].book.as_deref(), Some("spot"));
    // The unknown word does not arm, and does not keep the tier it was sent with.
    assert_eq!(rows[1].arming.block, ArmingBlock::AdapterFailed);
    assert_eq!(rows[1].arming.effective, None);
    assert!(!rows[1].arming.runs());
    assert_eq!(rows[2].arming.block, ArmingBlock::PaperTier);

    let facts = folded.facts_of("bybit").expect("the facts cross");
    assert!(facts.adapter_present && facts.addresses_accounts && facts.takes_recon_trigger);
    assert_eq!(facts.grid_source, GridSource::PerSymbolFetch);
    assert_eq!(facts.process_exclusive.as_deref(), Some("jforex-sidecar"));
    assert_eq!(facts.clock_decl_word, "wired");
    assert!(folded.facts_of("okx").is_none(), "no facts were sent for okx");
}

/// A labelled account's text becomes its label, and a label this build cannot parse skips the row.
#[test]
fn a_labelled_row_keeps_its_label() {
    let mut labelled = wire_row("binance", Some(AccountTier::Demo), "none");
    labelled.label = Some("ALT".to_string());
    let mut bad = wire_row("binance", Some(AccountTier::Demo), "none");
    bad.label = Some("not a legal label!".to_string());
    let folded =
        table_of_wire(&proto::ArmingTable { facts: Default::default(), rows: vec![labelled, bad] });
    assert_eq!(folded.rows().len(), 1);
    assert_eq!(folded.rows()[0].arming.label.text(), Some("ALT"));
}

fn accounts_of(policy: &AdapterPolicy) -> AccountDirectory {
    policy.accounts.clone()
}

/// **The unavailable table is the paper table with ONE block re-worded.** A `paper` account keeps its
/// row (it needs no venue); an account at `demo`/`live` is NOT RUNNING with `gatewayhub
/// unavailable` — never a paper stand-in; an account the table itself does not run keeps its cause;
/// and every roster venue's facts are present so the paper accounts still get their engines.
#[test]
fn the_unavailable_table_is_the_paper_table_with_one_block_reworded() {
    let policy = AdapterPolicy::default()
        .with_account("bybit", &AccountLabel::Default, AccountTier::Demo)
        .with_account("binance", &AccountLabel::Default, AccountTier::Paper)
        .with_account("okx", &AccountLabel::Default, AccountTier::Live);
    let accounts = accounts_of(&policy);
    let table = unavailable_table(&accounts);
    let paper = vike_tradehub_core::PaperBackend::new(&accounts).arming_table().unwrap();

    assert_eq!(table.rows().len(), paper.rows().len(), "the same accounts");
    for (got, was) in table.rows().iter().zip(paper.rows()) {
        let want = if was.arming.block == ArmingBlock::LiveSwitchOff {
            ArmingBlock::GatewayUnavailable
        } else {
            was.arming.block
        };
        assert_eq!(got.arming.block, want, "{}", got.arming.route_key());
        assert_eq!(
            (got.arming.tier, got.arming.effective),
            (was.arming.tier, was.arming.effective)
        );
    }
    let row_of = |venue: &str| {
        table
            .row_of_account(venue, &AccountLabel::Default)
            .expect("the account has a row")
            .arming
            .clone()
    };
    assert_eq!(row_of("binance").block, ArmingBlock::PaperTier);
    assert_eq!(row_of("binance").effective, Some(AccountTier::Paper), "paper runs");
    for venue in ["bybit", "okx"] {
        let r = row_of(venue);
        assert_eq!(r.block, ArmingBlock::GatewayUnavailable, "{venue}");
        assert_eq!(r.effective, None, "{venue}: NOT RUNNING, no paper stand-in");
        assert!(!r.runs());
    }
    for venue in vike_model::VENUES {
        assert!(table.facts_of(venue).is_some_and(|f| f.adapter_present), "{venue}");
    }
}

/// An unreadable or empty account table makes no non-paper row, so nothing is re-worded and the
/// table is exactly the paper-only one.
#[test]
fn a_box_with_no_non_paper_account_has_no_gateway_unavailable_row() {
    let accounts = accounts_of(&AdapterPolicy::default());
    let table = unavailable_table(&accounts);
    assert!(table.rows().iter().all(|r| r.arming.block != ArmingBlock::GatewayUnavailable));
}

fn row_in<'a>(table: &'a ArmingTable, venue: &str) -> &'a ArmingRow {
    table.row_of_account(venue, &AccountLabel::Default).expect("a row")
}

/// **The `data_only` overlay answers what the credential-stripped in-process backend answered**:
/// the row of a declared venue, at `demo` and at `live`, compared with a `LocalBackend` table over
/// the same account whose credentials were withheld the way the mount withholds them.
#[test]
fn the_data_only_overlay_matches_a_backend_over_withheld_credentials() {
    use std::collections::HashMap;
    for tier in [AccountTier::Demo, AccountTier::Live] {
        let armed = keyed_account(
            AdapterPolicy::default(),
            "alpaca",
            &AccountLabel::Default,
            tier,
            armed_fields("alpaca"),
        );
        let full = crate::scripted_arming_tests::plan_table(&HashMap::new(), &armed);
        let was = row_in(&full, "alpaca").arming.clone();
        if !(was.runs() && was.effective == Some(tier)) {
            // Only the live tier may be one this venue's arm does not reach with these fields; the
            // demo tier is the premise of the whole test.
            assert_eq!(tier, AccountTier::Live, "the fixture must arm alpaca at demo: {was:?}");
            continue;
        }

        let (kept, withheld) = armed.accounts.without_venue_credentials("alpaca");
        assert!(withheld > 0, "the fixture must have credentials to withhold");
        let stripped = AdapterPolicy { accounts: kept, ..armed.clone() };
        let want = crate::scripted_arming_tests::plan_table(&HashMap::new(), &stripped);

        let got = with_data_only(&full, &["alpaca".to_string()].into_iter().collect());
        let (g, w) = (&row_in(&got, "alpaca").arming, &row_in(&want, "alpaca").arming);
        assert_eq!(
            (g.tier, g.effective, g.block),
            (w.tier, w.effective, w.block),
            "tier {tier}: the overlay must say what a credential-less backend says"
        );
        // …and a venue that is not declared keeps its row exactly.
        assert_eq!(row_in(&with_data_only(&full, &HashSet::new()), "alpaca").arming, was);
    }
}

/// The overlay touches only the rows it should: a `paper` account of a declared venue, an account
/// that does not run for its own reason, and every other venue are left as they are.
#[test]
fn the_data_only_overlay_leaves_paper_and_other_venues_alone() {
    let table = table_of_wire(&proto::ArmingTable {
        facts: Default::default(),
        rows: vec![
            wire_row("alpaca", Some(AccountTier::Paper), "paper_tier"),
            wire_row("oanda", Some(AccountTier::Demo), "none"),
            wire_row("ig", Some(AccountTier::Demo), "none"),
            {
                let mut r = wire_row("ctrader", Some(AccountTier::Demo), "account_inactive");
                r.effective = None;
                r
            },
        ],
    });
    let declared: HashSet<String> =
        ["alpaca", "oanda", "ctrader"].into_iter().map(String::from).collect();
    let got = with_data_only(&table, &declared);
    assert_eq!(row_in(&got, "alpaca").arming.block, ArmingBlock::PaperTier);
    assert_eq!(row_in(&got, "ctrader").arming.block, ArmingBlock::AccountInactive);
    assert_eq!(row_in(&got, "ig").arming.block, ArmingBlock::None, "ig is not declared");
    let oanda = &row_in(&got, "oanda").arming;
    assert_eq!((oanda.effective, oanda.block), (None, ArmingBlock::NoCredentials));
}
