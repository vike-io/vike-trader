//! **A venue's market feed dials the network its exec side dials — and the ANSWER comes from the
//! venue's registry row over the `account` table, not from a second reading of any tier.**
//!
//! # The rule this specifies (decision 0119)
//!
//! An account trades at exactly its own `account.tier` while a row of it is ACTIVE (the owner's
//! no-downgrade ruling: a `live` account never trades demo). The hyperliquid FEED is one per venue,
//! so its network is decided over every account of the venue, by
//! `crates/vike-tradehub/src/venue_arming/tier.rs`'s `feed_tier_from_rows`:
//!
//! * the ARMED accounts agree → their network;
//! * none is armed → the DEFAULT account's selected tier: MAINNET only for an active `live` row
//!   whose keys are absent (`LiveCredentialsAbsent ⇒ Live`, the one paper block that names the live
//!   tier), TESTNET for everything else — no row, a `paper` row, an inactive row, two conflicting
//!   active tiers, a `demo` row;
//! * they straddle both networks → MAINNET WINS, the start is not refused, and the straddle comes
//!   back with the tier so the daemon can say it.
//!
//! This file was a diff against the venue-line CEILING (`min(venue line, account line)`), which no
//! longer exists; what follows is a new specification and is read as one (contract R12).
//!
//! # What is pinned
//!
//! 1. [`unlabelled_boxes_choose_the_feed_network_of_their_one_account`] — a box with ONE account
//!    (the production box): every row state x every key set, for the five venues whose plan carries
//!    a tier-derived verdict, against literal answers.
//! 2. [`every_other_roster_venue_plans_the_same_at_every_tier`] — the other roster venues' plans do
//!    not read the account table at all, so no row state moves them.
//! 3. [`a_feed_is_never_on_testnet_while_an_armed_account_is_on_mainnet`] — the invariant itself,
//!    over the whole two-account grid, measured from `vike_tradehub_core::venue_account_arming` (the
//!    selection `make_engine_accounts` mounts from), not from the code under test.
//! 4. [`the_feed_network_grid_is_exactly_this`] — the WHOLE grid, DEFAULT x ALT row state x DEFAULT
//!    x ALT keys, against [`expected`], the rule above written out per account; the straddle flag
//!    included.
//! 5. [`a_straddling_venue_keeps_the_mainnet_feed_and_the_start_is_not_refused`] — the straddle
//!    cells, named, with the account ids the straddle names. What the daemon SAYS about them is the
//!    business of the standalone `crates/vike-tradehub/tests/feed_straddle_warning.rs`, which needs
//!    a collector of its own.
//! 6. [`the_plan_loop_asks_the_registry_over_the_mounts_own_policy`] — the production call site is
//!    handed the policy the mount is built from (the one carrying the `account` table), not a bare
//!    one.
//!
//! No process-global state is touched: every test builds its own `vars` and `AdapterPolicy`, so this
//! is a plain member of the `daemon` group.

use std::collections::HashMap;

use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;
use vike_tradehub::HlNetwork;
use vike_tradehub::feeds::venue_feed_plan;
use vike_tradehub::{CexVenue, VenuePlan};
use vike_tradehub_core::{AdapterPolicy, MakerConfig};
use vike_venues::venue_wiring::wired_symbol;

/// The value every fixture credential carries — asserted ABSENT from every refusal message.
const FAKE_SECRET: &str = "fixture-credential-value-that-must-never-be-echoed";

/// The five venues whose plan carries a verdict derived from the account's tier.
const TIER_SENSITIVE: [&str; 5] = ["binance", "bybit", "okx", "aster", "hyperliquid"];

/// Whether a fixture account holds its venue's complete credential set. A credential carries no tier
/// (decision 0156 item 6): the ROW's tier is the network, so "which tier's keys" is no question.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Keys {
    Nothing,
    Held,
}

const ALL_KEYS: [Keys; 2] = [Keys::Nothing, Keys::Held];

/// What the `account` table holds for ONE account of the venue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Row {
    /// No row at all (`NoAccountRow`).
    Absent,
    /// One active `paper` row (`PaperTier`).
    Paper,
    /// One active `demo` row.
    Demo,
    /// One active `live` row.
    Live,
    /// One INACTIVE `live` row — the off switch (`AccountInactive`): it arms nothing and, unlike an
    /// active `live` row, names no live tier for the feed.
    Inactive,
    /// Two active rows, `demo` and `live` (`TierConflict`): paper, no automatic pick.
    Conflict,
}

const ALL_ROWS: [Row; 6] =
    [Row::Absent, Row::Paper, Row::Demo, Row::Live, Row::Inactive, Row::Conflict];

fn alt() -> AccountLabel {
    AccountLabel::parse("ALT").expect("a legal label")
}

/// The credential FIELDS that arm `venue` — aster holds an agent-wallet pair, hyperliquid one
/// private key, okx adds a passphrase. A misspelling here arms nothing and turns
/// `the_feed_network_grid_is_exactly_this` red.
fn fields_for(venue: &str) -> &'static [&'static str] {
    match venue {
        "aster" => &["USER", "PRIVATE_KEY"],
        "hyperliquid" => &["PRIVATE_KEY"],
        "okx" => &["API_KEY", "API_SECRET", "API_PASSPHRASE"],
        _ => &["API_KEY", "API_SECRET"],
    }
}

/// Hand the account row of `label` (planted as `row`) its venue's complete credential set when
/// `keys` says so. Only a single ACTIVE `demo` or `live` row can hold credentials: a missing row,
/// a `paper` one, an inactive one and a tier conflict have no account for a credential to belong to
/// (the mount reads none of them), so nothing is planted there.
fn plant_keys(
    policy: AdapterPolicy,
    venue: &str,
    label: &AccountLabel,
    row: Row,
    keys: Keys,
) -> AdapterPolicy {
    let tier = match (keys, row) {
        (Keys::Held, Row::Demo) => AccountTier::Demo,
        (Keys::Held, Row::Live) => AccountTier::Live,
        _ => return policy,
    };
    let pairs: Vec<(&str, &str)> = fields_for(venue).iter().map(|f| (*f, FAKE_SECRET)).collect();
    policy.with_credentials(venue, label, tier, &pairs)
}

/// Plant `row` for `label` of `venue`. The inactive rows carry fixed ids far above the ones
/// `AdapterPolicy::with_account` hands out (`max + 1`), so no two rows of one table share an id.
fn plant(policy: AdapterPolicy, venue: &str, label: &AccountLabel, row: Row) -> AdapterPolicy {
    match row {
        Row::Absent => policy,
        Row::Paper => policy.with_account(venue, label, AccountTier::Paper),
        Row::Demo => policy.with_account(venue, label, AccountTier::Demo),
        Row::Live => policy.with_account(venue, label, AccountTier::Live),
        Row::Conflict => policy.with_account(venue, label, AccountTier::Demo).with_account(
            venue,
            label,
            AccountTier::Live,
        ),
        Row::Inactive => policy.with_account_row(vike_secrets::Account {
            id: if label.is_default() { 9_001 } else { 9_002 },
            venue: venue.to_string(),
            tier: AccountTier::Live,
            label: label.text().map(str::to_string),
            venue_account_id: None,
            parent_id: None,
            active: false,
            last_verified_at: None,
            max_exposure: None,
        }),
    }
}

/// The mount's policy for one venue: the DEFAULT account's row state and credentials, and ALT's.
fn policy_for(
    venue: &str,
    default: Row,
    default_keys: Keys,
    alt_row: Row,
    alt_keys: Keys,
) -> AdapterPolicy {
    let policy = plant(AdapterPolicy::default(), venue, &AccountLabel::Default, default);
    let policy = plant_keys(policy, venue, &AccountLabel::Default, default, default_keys);
    let policy = plant(policy, venue, &alt(), alt_row);
    plant_keys(policy, venue, &alt(), alt_row, alt_keys)
}

/// The mount a profile row for `venue` lowers to. Hyperliquid's market is `assemble_runtime`'s hardcoded
/// `BTC`; the others sit on their wired symbol, and a venue with none can only be refused anyway.
fn maker_cfg(venue: &str) -> MakerConfig {
    let symbol = if venue == "hyperliquid" { "BTC" } else { wired_symbol(venue).unwrap_or("BTC") };
    MakerConfig::crypto(venue, symbol, 0.5, 0.001)
}

/// What a plan decided, reduced to what this file compares.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Chose {
    Hyperliquid(HlNetwork),
    Cex { venue: &'static str, mainnet: bool },
    Fixed(&'static str),
    Refused,
}

fn chose(plan: Result<VenuePlan, String>) -> Chose {
    match plan {
        Ok(VenuePlan::Hyperliquid(network)) => Chose::Hyperliquid(network),
        Ok(VenuePlan::Cex { venue, mainnet }) => Chose::Cex { venue: venue.slug(), mainnet },
        Ok(VenuePlan::Deribit) => Chose::Fixed("deribit"),
        Ok(VenuePlan::Polymarket) => Chose::Fixed("polymarket"),
        Ok(_) => Chose::Fixed("credentialed-data"),
        Err(_) => Chose::Refused,
    }
}

fn plan_of(venue: &str, policy: &AdapterPolicy) -> Chose {
    chose(venue_feed_plan(
        &maker_cfg(venue),
        &crate::local::table(&HashMap::new(), Some(policy)),
        policy,
    ))
}

fn cex(venue: &str) -> CexVenue {
    CexVenue::ALL.into_iter().find(|v| v.slug() == venue).expect("a CEX slug")
}

// ---------------------------------------------------------------------------------------------
// The rule, written out per account (the specification the grid is held to)
// ---------------------------------------------------------------------------------------------

/// What ONE hyperliquid account's exec side dials under decision 0119: `Some(tier)` when its row
/// state and credentials arm it, `None` when it does not run — and whether that paper NAMES the live tier
/// (an active `live` row with no credentials: `LiveCredentialsAbsent`).
///
/// An account arms only at exactly its row's tier, holding its credentials: a `live` row holding
/// nothing is paper, never demo (decision 0095 for the network; the no-downgrade ruling for the
/// fallback).
fn account_spec(row: Row, keys: Keys) -> (Option<AccountTier>, bool) {
    match (row, keys) {
        (Row::Demo, Keys::Held) => (Some(AccountTier::Demo), false),
        (Row::Live, Keys::Held) => (Some(AccountTier::Live), false),
        (Row::Live, Keys::Nothing) => (None, true),
        _ => (None, false),
    }
}

/// **The specification**: the network hyperliquid's ONE feed dials, and whether the venue's armed
/// accounts straddle both networks — from [`account_spec`] for the DEFAULT account and ALT and the
/// feed rule in this module's doc.
fn expected(default: Row, default_keys: Keys, alt_row: Row, alt_keys: Keys) -> (HlNetwork, bool) {
    let (default_tier, default_names_live) = account_spec(default, default_keys);
    let (alt_tier, _) = account_spec(alt_row, alt_keys);
    let armed = [default_tier, alt_tier];
    let live = armed.contains(&Some(AccountTier::Live));
    let demo = armed.contains(&Some(AccountTier::Demo));
    match (live, demo) {
        (true, false) => (HlNetwork::Mainnet, false),
        (false, true) => (HlNetwork::Testnet, false),
        (true, true) => (HlNetwork::Mainnet, true),
        (false, false) if default_names_live => (HlNetwork::Mainnet, false),
        (false, false) => (HlNetwork::Testnet, false),
    }
}

// ---------------------------------------------------------------------------------------------
// 1. A box without a labelled account
// ---------------------------------------------------------------------------------------------

/// Every row state x every key set for each tier-sensitive venue, on a box with ONE account,
/// against literals:
///
/// * binance/bybit/okx, aster and hyperliquid: the account's TIER is the network (decision 0095), keys or
///   not — an active `live` row is the mainnet verdict even with no key (paper exec, mainnet feed:
///   `LiveCredentialsAbsent ⇒ Live`), and every other state is the testnet/demo verdict;
#[test]
fn unlabelled_boxes_choose_the_feed_network_of_their_one_account() {
    let mut cells = 0;
    for venue in TIER_SENSITIVE {
        for row in ALL_ROWS {
            for keys in ALL_KEYS {
                let mainnet = row == Row::Live;
                let want = if venue == "hyperliquid" {
                    Chose::Hyperliquid(if mainnet {
                        HlNetwork::Mainnet
                    } else {
                        HlNetwork::Testnet
                    })
                } else {
                    Chose::Cex { venue: cex(venue).slug(), mainnet }
                };
                let policy = policy_for(venue, row, keys, Row::Absent, Keys::Nothing);
                assert_eq!(
                    plan_of(venue, &policy),
                    want,
                    "{venue} with a {row:?} row holding {keys:?} keys"
                );
                cells += 1;
            }
        }
    }
    assert_eq!(cells, 5 * ALL_ROWS.len() * ALL_KEYS.len(), "the grid walked every cell");
}

/// The production box, spelled out: aster and polymarket `live`, the other twelve venues `demo`,
/// no labelled account, hyperliquid on testnet — and hyperliquid's keys absent or present changes
/// nothing about its feed.
#[test]
fn the_production_accounts_keep_hyperliquid_on_testnet() {
    let policy = vike_model::VENUES.iter().fold(AdapterPolicy::default(), |p, venue| {
        let tier = if matches!(*venue, "aster" | "polymarket") {
            AccountTier::Live
        } else {
            AccountTier::Demo
        };
        p.with_account(venue, &AccountLabel::Default, tier)
    });
    for keys in ALL_KEYS {
        let held =
            plant_keys(policy.clone(), "hyperliquid", &AccountLabel::Default, Row::Demo, keys);
        assert_eq!(
            plan_of("hyperliquid", &held),
            Chose::Hyperliquid(HlNetwork::Testnet),
            "hyperliquid's `demo` account is the testnet feed whatever it holds ({keys:?})"
        );
    }
    for venue in ["binance", "bybit", "okx"] {
        assert_eq!(
            plan_of(
                venue,
                &plant_keys(policy.clone(), venue, &AccountLabel::Default, Row::Demo, Keys::Held)
            ),
            Chose::Cex { venue: cex(venue).slug(), mainnet: false },
            "{venue}'s `demo` account announces DEMO"
        );
    }
}

/// **The network of Hyperliquid's PRICE-ONLY feed** (`live_runtime`'s `feed_tier` over the rows, the function
/// this plan calls) for the four boxes the account follower cares about: no Hyperliquid account, a `demo`
/// one, a `demo` one beside a `live` ALT (the CI box's `hyperliquid` and `hyperliquid#LIVE`: mainnet wins), a
/// `live` one alone. The first two are TESTNET, which is why a `live` account activated later is not
/// hot-started beside that feed (`account_follow`'s `hyperliquid_feed_refusal`).
#[test]
fn the_price_only_feed_network_follows_the_rows_of_the_four_boxes() {
    // The function `live_runtime_with` calls for the price-only feed (a source pin in
    // `tradehub_cli/tests/deadman.rs` holds the call site to it), over the rows.
    let box_of = |default, default_keys, alt_row, alt_keys| {
        let policy = policy_for("hyperliquid", default, default_keys, alt_row, alt_keys);
        vike_tradehub::feeds::hl_price_network(
            &crate::local::table(&HashMap::new(), Some(&policy)),
            &policy,
        )
    };
    let (no_keys, held) = (Keys::Nothing, Keys::Held);
    assert_eq!(
        box_of(Row::Absent, no_keys, Row::Absent, no_keys),
        HlNetwork::Testnet,
        "no Hyperliquid account"
    );
    assert_eq!(
        box_of(Row::Demo, held, Row::Absent, no_keys),
        HlNetwork::Testnet,
        "a demo account only"
    );
    assert_eq!(
        box_of(Row::Demo, held, Row::Live, held),
        HlNetwork::Mainnet,
        "demo beside live: mainnet wins"
    );
    assert_eq!(
        box_of(Row::Absent, no_keys, Row::Live, held),
        HlNetwork::Mainnet,
        "a live account only"
    );
}

// ---------------------------------------------------------------------------------------------
// 2. The other roster venues do not read the account table at all
// ---------------------------------------------------------------------------------------------

/// What a venue outside [`TIER_SENSITIVE`] plans on an EMPTY credential map: deribit's keyless
/// feed, polymarket's, the credentialed-data venues' (their plans resolve no
/// credential, so an empty store plans the same value at every tier; the refusal is the gatewayhub's),
/// and a refusal for every other — the rest have no feed arm.
fn fixed_baseline(venue: &str) -> Chose {
    match venue {
        "deribit" => Chose::Fixed("deribit"),
        "polymarket" => Chose::Fixed("polymarket"),
        "alpaca" | "ctrader" | "oanda" | "ig" => Chose::Fixed("credentialed-data"),
        _ => Chose::Refused,
    }
}

#[test]
fn every_other_roster_venue_plans_the_same_at_every_tier() {
    let others: Vec<&str> = vike_model::VENUES
        .iter()
        .copied()
        .filter(|venue| !TIER_SENSITIVE.contains(venue))
        .collect();
    assert_eq!(others.len(), vike_model::VENUES.len() - TIER_SENSITIVE.len());
    for venue in others {
        for default in ALL_ROWS {
            for alt_row in [Row::Absent, Row::Paper, Row::Demo] {
                let policy = policy_for(venue, default, Keys::Nothing, alt_row, Keys::Nothing);
                assert_eq!(
                    plan_of(venue, &policy),
                    fixed_baseline(venue),
                    "{venue} with a {default:?} default row and a {alt_row:?} ALT row: this \
                     venue's plan is not supposed to read the account table"
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// 3. The invariant, over every labelled-account scenario
// ---------------------------------------------------------------------------------------------

/// The accounts of `venue` the registry projection ARMS, and the tier each one dials — read from
/// `vike_tradehub_core::venue_account_arming`, the selection `make_engine_accounts` mounts from, and never
/// from the code under test.
fn armed_accounts(venue: &str, policy: &AdapterPolicy) -> Vec<(String, AccountTier)> {
    crate::local::rows(venue, &HashMap::new(), Some(policy))
        .into_iter()
        .filter_map(|row| match row.effective {
            Some(AccountTier::Live) => Some((row.subject(), AccountTier::Live)),
            Some(AccountTier::Demo) => Some((row.subject(), AccountTier::Demo)),
            Some(AccountTier::Paper) | None => None,
        })
        .collect()
}

/// Every cell of the two-account grid for hyperliquid: DEFAULT row x ALT row x DEFAULT keys x ALT
/// keys.
fn labelled_grid() -> Vec<(Row, Row, Keys, Keys)> {
    let mut grid = Vec::new();
    for default in ALL_ROWS {
        for alt_row in ALL_ROWS {
            for default_keys in ALL_KEYS {
                for alt_keys in ALL_KEYS {
                    grid.push((default, alt_row, default_keys, alt_keys));
                }
            }
        }
    }
    grid
}

/// **THE INVARIANT.** Whatever the plan chose, the feed is never on testnet while any armed account
/// is on mainnet (real orders would be priced off the testnet book), and where every armed account
/// is on testnet the feed is on testnet. Where they straddle, MAINNET WINS — and the plan is never
/// refused: a refusal stops every venue and every mount, and a node that will not start cannot
/// flatten a position. With nothing armed, the feed is mainnet exactly when the DEFAULT account is
/// an active `live` row (its keys absent).
#[test]
fn a_feed_is_never_on_testnet_while_an_armed_account_is_on_mainnet() {
    let venue = "hyperliquid";
    for (default, alt_row, default_keys, alt_keys) in labelled_grid() {
        let policy = policy_for(venue, default, default_keys, alt_row, alt_keys);
        let armed = armed_accounts(venue, &policy);
        let cell = format!(
            "default {default:?} holding {default_keys:?}, ALT {alt_row:?} holding {alt_keys:?} \
             (armed: {armed:?})"
        );
        let Chose::Hyperliquid(network) = plan_of(venue, &policy) else {
            panic!("hyperliquid must always plan — a straddle is not a refusal — {cell}");
        };
        let any = |tier: AccountTier| armed.iter().any(|(_, t)| *t == tier);
        if any(AccountTier::Live) {
            assert_eq!(
                network,
                HlNetwork::Mainnet,
                "a feed on testnet under an armed mainnet account prices REAL orders off the \
                 testnet book — {cell}"
            );
        } else if any(AccountTier::Demo) {
            assert_eq!(network, HlNetwork::Testnet, "every armed account is on testnet — {cell}");
        } else {
            let want = if default == Row::Live { HlNetwork::Mainnet } else { HlNetwork::Testnet };
            assert_eq!(
                network, want,
                "nothing armed: only an active `live` default row names mainnet — {cell}"
            );
        }
    }
}

// ---------------------------------------------------------------------------------------------
// 4. The whole grid, against the specification
// ---------------------------------------------------------------------------------------------

/// **THE GRID.** Every cell of DEFAULT row x ALT row x DEFAULT credentials x ALT credentials plans exactly what
/// [`expected`] says, and the registry's own rows report a straddle exactly where [`expected`] does.
/// All disagreements are collected so one run prints the whole table's.
///
/// The counts at the end are the grid's shape, so a fixture that stopped arming (a misspelt key, a
/// row the projection no longer reads) cannot pass by collapsing every cell to one answer.
#[test]
fn the_feed_network_grid_is_exactly_this() {
    use vike_tradehub::venue_arming::feed_tier_from_rows;
    let venue = "hyperliquid";
    let mut wrong = Vec::new();
    let (mut mainnet, mut straddles) = (0usize, 0usize);
    for (default, alt_row, default_keys, alt_keys) in labelled_grid() {
        let policy = policy_for(venue, default, default_keys, alt_row, alt_keys);
        let (want, want_straddle) = expected(default, default_keys, alt_row, alt_keys);
        let got = plan_of(venue, &policy);
        let rows = crate::local::rows(venue, &HashMap::new(), Some(&policy));
        let straddle = feed_tier_from_rows(&rows).straddle.is_some();
        if got != Chose::Hyperliquid(want) || straddle != want_straddle {
            wrong.push(format!(
                "default {default:?}/{default_keys:?}, ALT {alt_row:?}/{alt_keys:?}: expected \
                 {want:?} (straddle {want_straddle}), got {got:?} (straddle {straddle})"
            ));
        }
        mainnet += usize::from(want == HlNetwork::Mainnet);
        straddles += usize::from(want_straddle);
    }
    assert!(wrong.is_empty(), "the feed grid disagrees with the rule:\n{}", wrong.join("\n"));
    let cells = ALL_ROWS.len() * ALL_ROWS.len() * ALL_KEYS.len() * ALL_KEYS.len();
    assert_eq!(labelled_grid().len(), cells, "the grid walked every cell");
    // Straddles: the default's `live` row armed beside ALT's `demo` row armed, and the mirror image —
    // two row pairs, one key cell each (both accounts holding their credentials).
    assert_eq!(straddles, 2, "the straddle cells are exactly the two mixed pairs");
    assert!(mainnet > straddles && mainnet < cells, "a degenerate grid: {mainnet} mainnet cells");
}

/// A labelled account never moves a CEX venue's announcement: `cex_arming` speaks for the DEFAULT
/// account (the one every mount trades through), and `with_other_live_accounts` adds the others.
/// Held against the same box with ALT's row and keys removed.
#[test]
fn a_labelled_account_does_not_move_a_cex_announcement() {
    for venue in ["binance", "bybit", "okx", "aster"] {
        for default in ALL_ROWS {
            for alt_row in ALL_ROWS {
                for default_keys in ALL_KEYS {
                    for alt_keys in ALL_KEYS {
                        let with_alt = plan_of(
                            venue,
                            &policy_for(venue, default, default_keys, alt_row, alt_keys),
                        );
                        let without = plan_of(
                            venue,
                            &policy_for(venue, default, default_keys, Row::Absent, Keys::Nothing),
                        );
                        assert_eq!(
                            with_alt, without,
                            "{venue}: default {default:?}/{default_keys:?}, ALT \
                             {alt_row:?}/{alt_keys:?}"
                        );
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// 5. The straddle
// ---------------------------------------------------------------------------------------------

/// A venue whose armed accounts straddle both networks KEEPS THE MAINNET FEED and the start is not
/// refused: the default account's `live` row armed on its credentials and ALT's `demo` row armed on
/// its own. The straddle names ALT's account id — the `--id` of the verb that moves it. The
/// daemon's words about it are the standalone `feed_straddle_warning.rs`'s business.
#[test]
fn a_straddling_venue_keeps_the_mainnet_feed_and_the_start_is_not_refused() {
    use vike_tradehub::venue_arming::feed_tier_from_rows;
    let venue = "hyperliquid";
    let policy = policy_for(venue, Row::Live, Keys::Held, Row::Demo, Keys::Held);
    assert_eq!(
        plan_of(venue, &policy),
        Chose::Hyperliquid(HlNetwork::Mainnet),
        "the default dials mainnet and ALT testnet — mainnet wins, and the plan is not refused"
    );
    let rows = crate::local::rows(venue, &HashMap::new(), Some(&policy));
    let alt_ids = rows
        .iter()
        .find(|row| row.label == alt())
        .map(|row| row.account_ids.clone())
        .expect("ALT has a row");
    assert_eq!(alt_ids.len(), 1, "ALT's one active row states its tier");
    let straddle = feed_tier_from_rows(&rows).straddle.expect("both networks armed");
    assert_eq!(straddle.testnet_accounts, ["hyperliquid account `ALT`"]);
    assert_eq!(straddle.testnet_account_ids, alt_ids, "the id the remedy's `--id` takes");
}

// ---------------------------------------------------------------------------------------------
// 6. The rule, pinned as literals (so no assertion above can be satisfied by the oracle alone)
// ---------------------------------------------------------------------------------------------

/// A planted arming row — the shape `vike_tradehub_core::venue_account_arming` produces — for the rules
/// that are pure over rows.
fn row(
    label: AccountLabel,
    tier: Option<AccountTier>,
    effective: Option<AccountTier>,
    block: vike_config::ArmingBlock,
    account_ids: Vec<i64>,
) -> vike_config::VenueArming {
    vike_config::VenueArming { venue: "hyperliquid", label, tier, effective, block, account_ids }
}

/// **A gatewayhub that is away does not move the network a `live`-tier account's feed dials** (the review
/// of W11): the unavailable table keeps each row's own tier and blocks it `GatewayUnavailable`, and a
/// feed pricing a live-tier account off the testnet book is the wrong direction to be wrong in. Only a
/// `live` row says so: a `demo` or `paper` row (or none) stays on the lower network, and an unrelated
/// block on a live row (a flag-off `LiveSwitchOff`, `NoLiveArm`) is unchanged.
#[test]
fn a_live_tier_row_blocked_for_want_of_the_gatewayhub_keeps_the_mainnet_feed() {
    use vike_config::ArmingBlock as Block;
    use vike_tradehub::venue_arming::{feed_tier_from_rows, selected_tier};
    let default = AccountLabel::Default;
    let live =
        row(default.clone(), Some(AccountTier::Live), None, Block::GatewayUnavailable, vec![1]);
    assert_eq!(selected_tier(&live), AccountTier::Live);
    assert_eq!(feed_tier_from_rows(&[live]).tier, AccountTier::Live);
    for tier in [Some(AccountTier::Demo), Some(AccountTier::Paper), None] {
        let waiting = row(default.clone(), tier, None, Block::GatewayUnavailable, vec![1]);
        assert_eq!(selected_tier(&waiting), AccountTier::Demo, "a {tier:?} row names no live tier");
    }
    let switched_off = row(default, Some(AccountTier::Live), None, Block::LiveSwitchOff, vec![1]);
    assert_eq!(selected_tier(&switched_off), AccountTier::Demo, "the flag-off block is unchanged");
}

/// **The same holds when the PRICE FEEDS are what is away** (`FeedsUnavailable`, the feed gate's overlay): nothing
/// about the account changed and the overlay keeps the row's tier, so a `live` row still names mainnet and a
/// `demo` row stays demo. `selected_tier` spells the case out; a later reader of the held table must not read a
/// live account's network as demo.
#[test]
fn a_live_tier_row_blocked_for_want_of_its_price_feeds_keeps_the_mainnet_feed() {
    use vike_config::ArmingBlock as Block;
    use vike_tradehub::venue_arming::selected_tier;
    let default = AccountLabel::Default;
    let live =
        row(default.clone(), Some(AccountTier::Live), None, Block::FeedsUnavailable, vec![1]);
    assert_eq!(selected_tier(&live), AccountTier::Live);
    for tier in [Some(AccountTier::Demo), Some(AccountTier::Paper), None] {
        let waiting = row(default.clone(), tier, None, Block::FeedsUnavailable, vec![1]);
        assert_eq!(selected_tier(&waiting), AccountTier::Demo, "a {tier:?} row names no live tier");
    }
}

/// The unlabelled hyperliquid table, as literals: the DEFAULT account's row decides, whatever keys
/// are held — an active `live` row is the mainnet feed (C12: `live` with no keys is still mainnet),
/// and no row, a `paper` row, an inactive `live` row, a tier conflict and a `demo` row are testnet.
#[test]
fn an_unlabelled_hyperliquid_box_follows_its_accounts_tier_whatever_it_holds() {
    for keys in ALL_KEYS {
        for (default, want) in [
            (Row::Absent, HlNetwork::Testnet),
            (Row::Paper, HlNetwork::Testnet),
            (Row::Demo, HlNetwork::Testnet),
            (Row::Live, HlNetwork::Mainnet),
            (Row::Inactive, HlNetwork::Testnet),
            (Row::Conflict, HlNetwork::Testnet),
        ] {
            let policy = policy_for("hyperliquid", default, keys, Row::Absent, Keys::Nothing);
            assert_eq!(
                plan_of("hyperliquid", &policy),
                Chose::Hyperliquid(want),
                "default {default:?}, keys {keys:?}"
            );
        }
    }
}

/// **The default account paper, a labelled account armed** — the case the feed is per venue for.
/// The armed account is the exec side the feed serves, so the feed dials ITS network; and where
/// nothing is armed the feed keeps the network the DEFAULT account's row names.
#[test]
fn a_paper_default_account_does_not_make_the_feed_paper() {
    let venue = "hyperliquid";
    // (default row, ALT's row, ALT's credentials) with the default account holding NO credentials.
    for (default, alt_row, alt_keys, want) in [
        // No default row, ALT armed live: the only exec side is mainnet.
        (Row::Absent, Row::Live, Keys::Held, HlNetwork::Mainnet),
        // No default row, ALT armed demo: the only exec side is testnet.
        (Row::Absent, Row::Demo, Keys::Held, HlNetwork::Testnet),
        // The default's `live` row with no key, ALT armed on demo: the only exec side is ALT's
        // TESTNET, and the unarmed default does not drag the feed onto mainnet.
        (Row::Live, Row::Demo, Keys::Held, HlNetwork::Testnet),
        // …ALT's `demo` row holding nothing arms nothing, so nothing is followed and the default's
        // live row names mainnet (a missing key moves no feed).
        (Row::Live, Row::Demo, Keys::Nothing, HlNetwork::Mainnet),
        // …ALT's `live` row holding nothing: PAPER, never demo (no-downgrade) — so nothing is armed.
        (Row::Live, Row::Live, Keys::Nothing, HlNetwork::Mainnet),
        // ALT `paper`, ALT with no row, ALT inactive: unarmed, same.
        (Row::Live, Row::Paper, Keys::Held, HlNetwork::Mainnet),
        (Row::Live, Row::Absent, Keys::Held, HlNetwork::Mainnet),
        (Row::Live, Row::Inactive, Keys::Held, HlNetwork::Mainnet),
        // A `demo` default with no key beside an armed `demo` ALT: testnet.
        (Row::Demo, Row::Demo, Keys::Held, HlNetwork::Testnet),
    ] {
        let policy = policy_for(venue, default, Keys::Nothing, alt_row, alt_keys);
        assert_eq!(
            plan_of(venue, &policy),
            Chose::Hyperliquid(want),
            "default {default:?} holding nothing, ALT {alt_row:?} holding {alt_keys:?}"
        );
    }
}

/// `cex_mainnet_enabled` against literals, aster included: the tier is the network.
#[test]
fn the_cex_mainnet_verdict_is_the_registry_rows_tier() {
    use vike_tradehub::venue_arming::cex_mainnet_enabled;
    for venue in ["binance", "bybit", "okx", "aster"] {
        for (default, want) in [
            (Row::Absent, false),
            (Row::Paper, false),
            (Row::Demo, false),
            (Row::Live, true),
            (Row::Inactive, false),
            (Row::Conflict, false),
        ] {
            for keys in ALL_KEYS {
                let policy = policy_for(venue, default, keys, Row::Absent, Keys::Nothing);
                assert_eq!(
                    cex_mainnet_enabled(
                        cex(venue),
                        &crate::local::table(&HashMap::new(), Some(&policy)),
                        &policy
                    ),
                    want,
                    "{venue} with a {default:?} row holding {keys:?}: the tier is the network, \
                     keys or not"
                );
            }
        }
    }
}

/// The rules that need no store, over planted rows — including shapes a projection over real keys
/// cannot reach: a paper block this build does not know yet, and an armed row whose block names the
/// LIVE tier (the projection no longer produces it — a `live` account is never armed on DEMO — but
/// the precedence of an armed row's own tier over its block is still the rule).
#[test]
fn the_feed_rule_over_planted_rows() {
    use vike_config::ArmingBlock as Block;
    use vike_tradehub::venue_arming::{FeedChoice, feed_tier_from_rows, selected_tier};
    let quiet = |tier| FeedChoice { tier, straddle: None };
    let default = AccountLabel::Default;
    let (paper, demo, live) =
        (Some(AccountTier::Paper), Some(AccountTier::Demo), Some(AccountTier::Live));

    // An armed row's tier beats whatever its block says.
    let armed_demo = row(default.clone(), demo, demo, Block::LiveCredentialsAbsent, vec![1]);
    assert_eq!(selected_tier(&armed_demo), AccountTier::Demo);
    assert_eq!(feed_tier_from_rows(&[armed_demo]), quiet(AccountTier::Demo));

    // An account that does not run names the live tier in exactly one case: an active `live` row
    // with no live key set.
    assert_eq!(
        feed_tier_from_rows(&[row(
            default.clone(),
            live,
            None,
            Block::LiveCredentialsAbsent,
            vec![1]
        )]),
        quiet(AccountTier::Live)
    );
    for (tier, effective, block) in [
        (paper, paper, Block::PaperTier),
        (None, None, Block::NoAccountRow),
        (None, None, Block::AccountInactive),
        (None, None, Block::TierConflict),
        (demo, None, Block::NoCredentials),
        (live, None, Block::NoLiveArm),
    ] {
        assert_eq!(
            feed_tier_from_rows(&[row(default.clone(), tier, effective, block, vec![])]),
            quiet(AccountTier::Demo),
            "an account that dials no venue, whose block is {block:?}, names no live tier: the \
             feed stays on the lower network"
        );
    }

    // No rows at all (a venue the roster does not carry) is the lower network, not a panic.
    assert_eq!(feed_tier_from_rows(&[]), quiet(AccountTier::Demo));

    // Agreement says nothing.
    let alt_live = row(alt(), live, live, Block::None, vec![2]);
    let alt_demo = row(alt(), demo, demo, Block::None, vec![4242]);
    let default_live = row(default.clone(), live, live, Block::None, vec![1]);
    let default_paper = row(default, None, None, Block::NoAccountRow, vec![]);
    assert_eq!(feed_tier_from_rows(&[default_live.clone(), alt_live]), quiet(AccountTier::Live));
    assert_eq!(
        feed_tier_from_rows(&[default_paper, alt_demo.clone()]),
        quiet(AccountTier::Demo),
        "a default account that does not run does not move the feed: the armed labelled account \
         is followed, and a single network is not a straddle"
    );

    // The straddle: MAINNET WINS, nothing is refused, and the data to say it comes back with the
    // tier — subjects and account ids only.
    let choice = feed_tier_from_rows(&[default_live, alt_demo]);
    assert_eq!(
        choice.tier,
        AccountTier::Live,
        "mainnet and testnet armed on one venue: MAINNET WINS"
    );
    let straddle = choice.straddle.expect("both networks armed: the choice carries the straddle");
    assert_eq!(straddle.venue, "hyperliquid");
    assert_eq!(straddle.mainnet_accounts, ["hyperliquid"]);
    assert_eq!(straddle.testnet_accounts, ["hyperliquid account `ALT`"]);
    assert_eq!(straddle.testnet_account_ids, [4242_i64]);
    let words = straddle.message();
    for needle in ["hyperliquid", "ALT", "MAINNET", "TESTNET", "set-tier", "4242"] {
        assert!(words.contains(needle), "the message must name {needle}: {words}");
    }
    assert!(!words.contains("policy.accounts"), "the deleted settings key must be gone: {words}");
}

// ---------------------------------------------------------------------------------------------
// 7. The call site
// ---------------------------------------------------------------------------------------------

/// **The plan loop asks the registry over the mount's OWN policy.** `venue_feed_plan` takes a
/// `&AdapterPolicy`, and the compiler is happy with `&AdapterPolicy::default()` — which would silently
/// restore a second opinion (a bridge's `resolve` that reads the `account` table or a `venue_setting`
/// row would answer from an empty snapshot here and from the real one at mount). `live_runtime_with`
/// is a function that spawns a core and opens sockets, so nothing can call it; this is the text gate
/// the sibling wiring pins are (`account_badge_wiring_pin.rs`), over code lines only so a comment
/// cannot satisfy it: the policy is built BEFORE the one production call, that call is handed it, and
/// the same binding is what the `RuntimeConfig` is then built with.
#[test]
fn the_plan_loop_asks_the_registry_over_the_mounts_own_policy() {
    // The daemon's composition root and, read after it, its live mount — `live_runtime_with` moved
    // into `tradehub_cli/live_runtime.rs` on 2026-10-06. Both are read so "exactly one call" keeps
    // the scope it had when they were one file: a second call in either would be counted.
    let text = ["src/tradehub_cli.rs", "src/tradehub_cli/live_runtime.rs"]
        .map(|rel| {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
            std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
        })
        .join("\n");
    let code: String =
        text.lines().filter(|l| !l.trim_start().starts_with("//")).collect::<Vec<_>>().join("\n");
    assert_eq!(
        code.matches("venue_feed_plan(").count(),
        1,
        "exactly one production call of `venue_feed_plan` in tradehub_cli.rs + tradehub_cli/live_runtime.rs"
    );
    let call = code
        .find("venue_feed_plan(&m.cfg, &plan_table, &adapter_policy)?")
        .expect("the plan loop must hand `venue_feed_plan` the mount's own `adapter_policy`");
    let built = code
        .find("let mut adapter_policy = vike_tradehub_core::AdapterPolicy {")
        .expect("`live_runtime_with` builds its `adapter_policy` here");
    assert!(built < call, "the policy must be built BEFORE the plan loop that asks the registry");
    assert!(
        code.contains("policy: adapter_policy,"),
        "the SAME binding must be what the `RuntimeConfig` mounts with"
    );
}
