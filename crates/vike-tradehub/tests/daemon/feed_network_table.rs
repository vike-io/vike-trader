//! **A venue's market feed dials the network its exec side dials — and the ANSWER comes from the
//! venue's registry row, not from a second reading of the ceiling.**
//!
//! # The defect this pins
//!
//! `venue_feed_plan` chose the hyperliquid feed's network from the VENUE line of the arming ceiling
//! (`venues.get("hyperliquid") == Live`). The exec side does not: each ACCOUNT's ceiling is
//! `min(venue line, its own line)`, and the bridge's `resolve` — the arming probe `vike-mount`'s
//! projection calls, and the function `mount` acts on — turns that ceiling plus the keys it finds
//! into a tier. So a LABELLED account capped below the venue line traded the lower network while the
//! one feed per venue sat on the venue line's, and the comment that said "the feed follows the SAME
//! network the exec side dials" was false for it. Nothing failed: the box mounted, quoted, and
//! priced a testnet account off the mainnet book.
//!
//! # What is pinned
//!
//! 1. [`unlabelled_boxes_choose_the_same_feed_network_as_before`] — every box with NO labelled
//!    account (the production box, and every other box today) gets exactly the plan it got before:
//!    every ceiling x every key set, for the five venues whose plan carries a ceiling-derived
//!    verdict. The BEFORE side is [`before`], the two removed derivations transcribed verbatim, so
//!    the comparison is against the old code's answers and not against a restatement of the new
//!    rule.
//! 2. [`every_other_roster_venue_plans_the_same_at_every_ceiling`] — the other nine roster venues'
//!    plans do not read the ceiling at all, so no ceiling and no labelled account moves them.
//! 3. [`a_feed_is_never_on_testnet_while_an_armed_account_is_on_mainnet`] — the invariant itself,
//!    over every labelled-account scenario, measured from `vike_mount::venue_account_arming` (the
//!    selection `make_engine_accounts` mounts from), not from the code under test: where the armed
//!    accounts agree the feed is their network, and where they straddle both networks MAINNET WINS —
//!    the harm is asymmetric (a mainnet feed under a testnet account costs testnet fidelity; a testnet
//!    feed under a live account prices REAL orders off the testnet book), and refusing the start
//!    instead would stop every venue and every mount.
//! 4. [`the_changed_rows_are_exactly_these`] — the whole grid, labelled accounts included, diffed
//!    against [`before`]: the cells that move are listed, and nothing else moves.
//! 5. [`a_straddling_venue_keeps_the_mainnet_feed_and_the_start_is_not_refused`] — the straddle
//!    cells, named. What the daemon SAYS about them is the business of the standalone
//!    `crates/vike-tradehub/tests/feed_straddle_warning.rs`, which needs a collector of its own.
//!
//! 6. [`the_plan_loop_asks_the_registry_over_the_mounts_own_policy`] — the production call site is
//!    handed the policy the mount is built from, not a bare one that only carries the ceilings.
//!
//! No process-global state is touched: every test builds its own `vars` and `MountPolicy`, so this
//! is a plain member of the `daemon` group.

use std::collections::HashMap;

use vike_bridge_core::venue_mount::Tier;
use vike_config::{VenueMode, VenuePolicy};
use vike_hyperliquid::config::{Env, Network};
use vike_model::accounts::account_keys::{AccountLabel, account_key};
use vike_mount::{MakerMountConfig, MountPolicy};
use vike_tradehub::feeds::venue_feed_plan;
use vike_tradehub::registry::REGISTRY;
use vike_tradehub::venue_plan::wired_symbol_for;
use vike_tradehub::{CexVenue, VenuePlan};

/// The value every fixture credential carries — asserted ABSENT from every refusal message.
const FAKE_SECRET: &str = "fixture-credential-value-that-must-never-be-echoed";

const CEILINGS: [VenueMode; 3] = [VenueMode::Paper, VenueMode::Demo, VenueMode::Live];

/// The five venues whose plan carries a verdict derived from the ceiling.
const CEILING_SENSITIVE: [&str; 5] = ["binance", "bybit", "okx", "aster", "hyperliquid"];

/// Which tiers' key sets a fixture account holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Keys {
    Nothing,
    Demo,
    Live,
    Both,
}

const ALL_KEYS: [Keys; 4] = [Keys::Nothing, Keys::Demo, Keys::Live, Keys::Both];

impl Keys {
    fn holds(self, tier: Tier) -> bool {
        matches!(
            (self, tier),
            (Keys::Both, _) | (Keys::Demo, Tier::Demo) | (Keys::Live, Tier::Live)
        )
    }
}

fn alt() -> AccountLabel {
    AccountLabel::parse("ALT").expect("a legal label")
}

/// The key NAMES that arm `venue` at `tier`, spelled `{VENUE}_{TIER}{SUFFIX}` the way the loaders
/// read them — aster spells its non-live tier TESTNET and holds an agent-wallet pair, hyperliquid
/// holds one private key, okx adds a passphrase. ⚠ Composed here rather than through
/// `vike_model::credential_keys`' builders on purpose: `crates/vike-ops/tests/settings/settings_registry.rs`
/// pins the set of files that call those builders, and a fixture is not a second composing site. A
/// misspelling here arms nothing and turns `the_changed_rows_are_exactly_these` red.
fn key_names(venue: &str, tier: Tier) -> Vec<String> {
    let live = tier == Tier::Live;
    let (token, suffixes): (&str, &[&str]) = match venue {
        "aster" => (if live { "LIVE" } else { "TESTNET" }, &["_USER", "_PRIVATE_KEY"]),
        "hyperliquid" => (if live { "LIVE" } else { "DEMO" }, &["_PRIVATE_KEY"]),
        "okx" => {
            (if live { "LIVE" } else { "DEMO" }, &["_API_KEY", "_API_SECRET", "_API_PASSPHRASE"])
        }
        _ => (if live { "LIVE" } else { "DEMO" }, &["_API_KEY", "_API_SECRET"]),
    };
    suffixes.iter().map(|suffix| format!("{}_{token}{suffix}", venue.to_uppercase())).collect()
}

fn add_keys(vars: &mut HashMap<String, String>, venue: &str, label: &AccountLabel, keys: Keys) {
    for tier in [Tier::Demo, Tier::Live] {
        if keys.holds(tier) {
            for base in key_names(venue, tier) {
                vars.insert(account_key(&base, label), FAKE_SECRET.to_string());
            }
        }
    }
}

/// A credential map holding the default account's keys and, optionally, ALT's.
fn vars_for(venue: &str, default: Keys, alt_keys: Option<Keys>) -> HashMap<String, String> {
    let mut vars = HashMap::new();
    add_keys(&mut vars, venue, &AccountLabel::Default, default);
    if let Some(keys) = alt_keys {
        add_keys(&mut vars, venue, &alt(), keys);
    }
    vars
}

/// A policy with the venue line at `line` and, optionally, ALT's own stated line.
fn policy_for(venue: &str, line: VenueMode, alt_line: Option<VenueMode>) -> VenuePolicy {
    let mut policy = VenuePolicy::default().declare(venue, line);
    if let Some(mode) = alt_line {
        policy = policy.declare_account(venue, &alt(), mode);
    }
    policy
}

/// The mount a profile row for `venue` lowers to. Hyperliquid's market is `build_node`'s hardcoded
/// `BTC`; the others sit on their wired symbol, and a venue with none can only be refused anyway.
fn mount_cfg(venue: &str) -> MakerMountConfig {
    let symbol =
        if venue == "hyperliquid" { "BTC" } else { wired_symbol_for(venue).unwrap_or("BTC") };
    MakerMountConfig::crypto(venue, symbol, 0.5, 0.001)
}

/// What a plan decided, reduced to what this file compares.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Chose {
    Hyperliquid(Network),
    Cex { venue: &'static str, mainnet: bool },
    Fixed(&'static str),
    Refused,
}

fn chose(plan: Result<VenuePlan, String>) -> Chose {
    match plan {
        Ok(VenuePlan::Hyperliquid(network)) => Chose::Hyperliquid(network),
        Ok(VenuePlan::Cex { venue, mainnet }) => Chose::Cex { venue: venue.slug(), mainnet },
        Ok(VenuePlan::Deribit) => Chose::Fixed("deribit"),
        #[cfg(feature = "polymarket")]
        Ok(VenuePlan::Polymarket) => Chose::Fixed("polymarket"),
        Ok(_) => Chose::Fixed("credentialed-data"),
        Err(_) => Chose::Refused,
    }
}

/// The mount's policy for a set of ceilings: the account table and the `venue_setting` snapshot are
/// empty, which is every box without a settings database and every fixture here.
fn mount_policy(venues: &VenuePolicy) -> MountPolicy {
    MountPolicy { venues: venues.clone(), ..MountPolicy::default() }
}

fn plan_of(venue: &str, vars: &HashMap<String, String>, venues: &VenuePolicy) -> Chose {
    chose(venue_feed_plan(&mount_cfg(venue), vars, &mount_policy(venues)))
}

/// **BEFORE** — the two derivations this change removed, transcribed from `venue_feed_plan`'s
/// hyperliquid arm and `venue_arming::cex_mainnet_enabled` as they stood, and nothing else. Both
/// read the VENUE line of the ceiling; aster additionally reads the credential store through the
/// bridge's own tier chain.
fn before(venue: &str, vars: &HashMap<String, String>, venues: &VenuePolicy) -> Chose {
    let live_permitted = venues.get(venue) == VenueMode::Live;
    match venue {
        "hyperliquid" => Chose::Hyperliquid(Env::for_ceiling(live_permitted).network()),
        "binance" | "bybit" | "okx" => {
            Chose::Cex { venue: cex(venue).slug(), mainnet: live_permitted }
        }
        "aster" => Chose::Cex {
            venue: "aster",
            mainnet: matches!(
                vike_aster::signing::mountable_tier_for_account(
                    &AccountLabel::Default,
                    vars,
                    live_permitted
                ),
                Some((vike_bridge_core::credentials::Environment::Live, _))
            ),
        },
        other => panic!("`before` knows only the ceiling-sensitive venues, not {other}"),
    }
}

fn cex(venue: &str) -> CexVenue {
    CexVenue::ALL.into_iter().find(|v| v.slug() == venue).expect("a CEX slug")
}

// ---------------------------------------------------------------------------------------------
// 1. NO behaviour change for a box without a labelled account
// ---------------------------------------------------------------------------------------------

/// Every ceiling x every key set for each ceiling-sensitive venue, on a box with ONE account: the
/// plan is exactly what the removed derivations answered. The production box is one cell of this
/// grid (aster and polymarket at `live`, hyperliquid at `demo`, no labelled account).
#[test]
fn unlabelled_boxes_choose_the_same_feed_network_as_before() {
    let mut cells = 0;
    for venue in CEILING_SENSITIVE {
        for line in CEILINGS {
            for keys in ALL_KEYS {
                let venues = policy_for(venue, line, None);
                let vars = vars_for(venue, keys, None);
                assert_eq!(
                    plan_of(venue, &vars, &venues),
                    before(venue, &vars, &venues),
                    "{venue} at ceiling {line:?} holding {keys:?} keys: the feed plan moved on a \
                     box with no labelled account"
                );
                cells += 1;
            }
        }
    }
    assert_eq!(cells, 5 * 3 * 4, "the grid walked every cell");
}

/// The production box, spelled out: aster and polymarket `live`, the other twelve venues `demo`,
/// no labelled account, hyperliquid on testnet — and hyperliquid's keys absent or present changes
/// nothing about its feed.
#[test]
fn the_production_ceilings_keep_hyperliquid_on_testnet() {
    let mut venues = VenuePolicy::default();
    for venue in vike_model::VENUES {
        let line = if matches!(*venue, "aster" | "polymarket") {
            VenueMode::Live
        } else {
            VenueMode::Demo
        };
        venues = venues.declare(venue, line);
    }
    for keys in ALL_KEYS {
        let vars = vars_for("hyperliquid", keys, None);
        assert_eq!(
            plan_of("hyperliquid", &vars, &venues),
            Chose::Hyperliquid(Network::Testnet),
            "hyperliquid at `demo` is the testnet feed whatever it holds ({keys:?})"
        );
    }
    for venue in ["binance", "bybit", "okx"] {
        assert_eq!(
            plan_of(venue, &vars_for(venue, Keys::Demo, None), &venues),
            Chose::Cex { venue: cex(venue).slug(), mainnet: false },
            "{venue} at `demo` announces DEMO"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// 2. The other roster venues do not read the ceiling at all
// ---------------------------------------------------------------------------------------------

/// What a venue outside [`CEILING_SENSITIVE`] plans on an EMPTY credential map: deribit's keyless
/// feed, polymarket's (under its feature), and a refusal for every other — the credentialed-data
/// venues refuse an empty store and the rest have no feed arm.
fn fixed_baseline(venue: &str) -> Chose {
    match venue {
        "deribit" => Chose::Fixed("deribit"),
        #[cfg(feature = "polymarket")]
        "polymarket" => Chose::Fixed("polymarket"),
        _ => Chose::Refused,
    }
}

#[test]
fn every_other_roster_venue_plans_the_same_at_every_ceiling() {
    let others: Vec<&str> = vike_model::VENUES
        .iter()
        .copied()
        .filter(|venue| !CEILING_SENSITIVE.contains(venue))
        .collect();
    assert_eq!(others.len(), vike_model::VENUES.len() - CEILING_SENSITIVE.len());
    for venue in others {
        for line in CEILINGS {
            for alt_line in [None, Some(VenueMode::Paper), Some(VenueMode::Demo)] {
                let venues = policy_for(venue, line, alt_line);
                assert_eq!(
                    plan_of(venue, &HashMap::new(), &venues),
                    fixed_baseline(venue),
                    "{venue} at ceiling {line:?} with ALT stated {alt_line:?}: this venue's plan \
                     is not supposed to read the ceiling"
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// 3. The invariant, over every labelled-account scenario
// ---------------------------------------------------------------------------------------------

/// The accounts of `venue` the registry projection ARMS, and the tier each one dials — read from
/// `vike_mount::venue_account_arming`, the selection `make_engine_accounts` mounts from, and never
/// from the code under test.
fn armed_accounts(
    venue: &str,
    vars: &HashMap<String, String>,
    venues: &VenuePolicy,
) -> Vec<(String, Tier)> {
    vike_mount::venue_account_arming(REGISTRY, venue, vars, Some(&mount_policy(venues)))
        .into_iter()
        .filter_map(|row| match row.effective {
            VenueMode::Live => Some((row.subject(), Tier::Live)),
            VenueMode::Demo => Some((row.subject(), Tier::Demo)),
            VenueMode::Paper => None,
        })
        .collect()
}

fn network_of(tier: Tier) -> Network {
    match tier {
        Tier::Live => Network::Mainnet,
        Tier::Demo => Network::Testnet,
    }
}

/// Every cell of the labelled-account grid for hyperliquid: venue line x ALT's stated line (absent
/// is "named by nobody") x default keys x ALT keys.
fn labelled_grid() -> Vec<(VenueMode, Option<VenueMode>, Keys, Keys)> {
    let mut grid = Vec::new();
    for line in CEILINGS {
        for alt_line in [None, Some(VenueMode::Paper), Some(VenueMode::Demo), Some(VenueMode::Live)]
        {
            for default in ALL_KEYS {
                for alt_keys in ALL_KEYS {
                    grid.push((line, alt_line, default, alt_keys));
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
/// flatten a position. Red against a feed that keys on the venue line (a labelled account capped
/// below it and armed on its own tier dials the lower network while the line says the higher) and
/// against one that refuses a straddle.
#[test]
fn a_feed_is_never_on_testnet_while_an_armed_account_is_on_mainnet() {
    let venue = "hyperliquid";
    for (line, alt_line, default, alt_keys) in labelled_grid() {
        let venues = policy_for(venue, line, alt_line);
        let vars = vars_for(venue, default, Some(alt_keys));
        let armed = armed_accounts(venue, &vars, &venues);
        let cell = format!(
            "venue line {line:?}, ALT stated {alt_line:?}, default holds {default:?}, ALT holds \
             {alt_keys:?} (armed: {armed:?})"
        );
        let Chose::Hyperliquid(network) = plan_of(venue, &vars, &venues) else {
            panic!("hyperliquid must always plan — a straddle is not a refusal — {cell}");
        };
        let any = |tier: Tier| armed.iter().any(|(_, t)| *t == tier);
        if any(Tier::Live) {
            assert_eq!(
                network,
                Network::Mainnet,
                "a feed on testnet under an armed mainnet account prices REAL orders off the \
                 testnet book — {cell}"
            );
        } else if any(Tier::Demo) {
            assert_eq!(
                network,
                network_of(Tier::Demo),
                "every armed account is on testnet, so the feed is — {cell}"
            );
        } else {
            // Nothing armed: no exec side to follow, the venue line's network stands, as before.
            assert_eq!(network, Env::for_ceiling(venues.get(venue) == VenueMode::Live).network());
        }
    }
}

// ---------------------------------------------------------------------------------------------
// 4. The cells that change
// ---------------------------------------------------------------------------------------------

/// The WHOLE labelled-account grid diffed against [`before`]. Exactly the cells where the venue line
/// is `live`, ALT is capped below it (stated `demo`), ALT is armed on its own demo keys AND the
/// default account is NOT armed live (it has no live keys) move: the only exec side is ALT's
/// testnet, so the feed dials testnet — it used to dial mainnet because the venue line said so.
///
/// Every other cell is byte-identical to before — including the four where the default account IS
/// armed live beside the testnet ALT: the accounts straddle both networks, MAINNET WINS (it is the
/// feed the line always gave), and the daemon says so loudly instead of refusing to start.
#[test]
fn the_changed_rows_are_exactly_these() {
    let venue = "hyperliquid";
    let mut changed = Vec::new();
    for (line, alt_line, default, alt_keys) in labelled_grid() {
        let venues = policy_for(venue, line, alt_line);
        let vars = vars_for(venue, default, Some(alt_keys));
        let now = plan_of(venue, &vars, &venues);
        if now != before(venue, &vars, &venues) {
            changed.push((line, alt_line, default, alt_keys, now));
        }
    }
    let live = VenueMode::Live;
    let capped = Some(VenueMode::Demo);
    let testnet = Chose::Hyperliquid(Network::Testnet);
    let expected = [
        (live, capped, Keys::Nothing, Keys::Demo, testnet.clone()),
        (live, capped, Keys::Nothing, Keys::Both, testnet.clone()),
        (live, capped, Keys::Demo, Keys::Demo, testnet.clone()),
        (live, capped, Keys::Demo, Keys::Both, testnet),
    ];
    assert_eq!(changed, expected, "the set of moved cells is not the set this change declares");
}

/// A labelled account never moves a CEX venue's announcement: `cex_arming` speaks for the DEFAULT
/// account (the one every mount trades through), and `with_other_live_accounts` adds the others.
#[test]
fn a_labelled_account_does_not_move_a_cex_announcement() {
    for venue in ["binance", "bybit", "okx", "aster"] {
        for line in CEILINGS {
            for alt_line in [None, Some(VenueMode::Paper), Some(VenueMode::Demo), Some(line)] {
                for default in ALL_KEYS {
                    for alt_keys in ALL_KEYS {
                        let venues = policy_for(venue, line, alt_line);
                        let vars = vars_for(venue, default, Some(alt_keys));
                        assert_eq!(
                            plan_of(venue, &vars, &venues),
                            before(venue, &vars, &venues),
                            "{venue} at {line:?}, ALT stated {alt_line:?}, default {default:?}, \
                             ALT {alt_keys:?}"
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
/// refused: the default account armed on live keys and ALT (capped to demo) armed on demo keys, in
/// every key combination that arms both. The daemon's words about it are the standalone
/// `feed_straddle_warning.rs`'s business.
#[test]
fn a_straddling_venue_keeps_the_mainnet_feed_and_the_start_is_not_refused() {
    let venue = "hyperliquid";
    let venues = policy_for(venue, VenueMode::Live, Some(VenueMode::Demo));
    for default in [Keys::Live, Keys::Both] {
        for alt_keys in [Keys::Demo, Keys::Both] {
            let vars = vars_for(venue, default, Some(alt_keys));
            assert_eq!(
                plan_of(venue, &vars, &venues),
                Chose::Hyperliquid(Network::Mainnet),
                "default holds {default:?}, ALT holds {alt_keys:?}: the default dials mainnet and \
                 ALT testnet — mainnet wins, and the plan is not refused"
            );
        }
    }
}

// ---------------------------------------------------------------------------------------------
// 6. The rule, pinned as literals (so no assertion above can be satisfied by the oracle alone)
// ---------------------------------------------------------------------------------------------

/// A planted arming row — the shape `vike_mount::venue_account_arming` produces — for the rules
/// that are pure over rows.
fn row(
    label: AccountLabel,
    effective: VenueMode,
    block: vike_config::ArmingBlock,
) -> vike_config::VenueArming {
    vike_config::VenueArming { venue: "hyperliquid", label, ceiling: effective, effective, block }
}

/// The unlabelled hyperliquid table, as literals: the venue line decides, whatever keys are held.
#[test]
fn an_unlabelled_hyperliquid_box_follows_its_line_whatever_it_holds() {
    for keys in ALL_KEYS {
        for (line, want) in [
            (VenueMode::Paper, Network::Testnet),
            (VenueMode::Demo, Network::Testnet),
            (VenueMode::Live, Network::Mainnet),
        ] {
            let venues = policy_for("hyperliquid", line, None);
            let vars = vars_for("hyperliquid", keys, None);
            assert_eq!(
                plan_of("hyperliquid", &vars, &venues),
                Chose::Hyperliquid(want),
                "ceiling {line:?}, keys {keys:?}"
            );
        }
    }
}

/// **The default account paper, a labelled account armed** — the case the feed is per venue for.
/// The armed account is the exec side the feed serves, so the feed dials ITS network; and where
/// nothing is armed the feed keeps the network the venue line always gave it.
#[test]
fn a_paper_default_account_does_not_make_the_feed_paper() {
    let venue = "hyperliquid";
    let live = VenueMode::Live;
    let demo = VenueMode::Demo;
    // (venue line, ALT's stated line, ALT's keys) with the default account holding NO keys.
    for (line, alt_line, alt_keys, want) in [
        // ALT armed live: the only exec side is mainnet.
        (live, Some(live), Keys::Live, Network::Mainnet),
        // ALT capped to demo and armed on demo keys: the only exec side is TESTNET, and the venue
        // line no longer drags the feed onto mainnet. THE changed row.
        (live, Some(demo), Keys::Demo, Network::Testnet),
        // ALT capped to demo but holding only live keys: it arms nothing, so nothing is followed
        // and the live line's mainnet feed stands (a missing key moves no feed).
        (live, Some(demo), Keys::Live, Network::Mainnet),
        // ALT stated paper: unarmed, same.
        (live, Some(VenueMode::Paper), Keys::Both, Network::Mainnet),
        // ALT named by nobody: AccountNotNamed, unarmed.
        (live, None, Keys::Both, Network::Mainnet),
        (demo, Some(demo), Keys::Demo, Network::Testnet),
    ] {
        let venues = policy_for(venue, line, alt_line);
        let vars = vars_for(venue, Keys::Nothing, Some(alt_keys));
        assert_eq!(
            plan_of(venue, &vars, &venues),
            Chose::Hyperliquid(want),
            "line {line:?}, ALT stated {alt_line:?}, ALT holds {alt_keys:?}, default holds nothing"
        );
    }
}

/// `cex_mainnet_enabled` against literals, aster included — aster's verdict is "a LIVE key set
/// under a `live` ceiling", the one CEX verdict that reads the store.
#[test]
fn the_cex_mainnet_verdict_is_the_registry_rows_tier() {
    use vike_tradehub::venue_arming::cex_mainnet_enabled;
    for venue in ["binance", "bybit", "okx"] {
        for (line, want) in
            [(VenueMode::Paper, false), (VenueMode::Demo, false), (VenueMode::Live, true)]
        {
            for keys in ALL_KEYS {
                let venues = policy_for(venue, line, None);
                assert_eq!(
                    cex_mainnet_enabled(
                        cex(venue),
                        &vars_for(venue, keys, None),
                        &mount_policy(&venues)
                    ),
                    want,
                    "{venue} at {line:?} holding {keys:?}: the ceiling is the network, keys or not"
                );
            }
        }
    }
    for (line, keys, want) in [
        (VenueMode::Live, Keys::Live, true),
        (VenueMode::Live, Keys::Both, true),
        (VenueMode::Live, Keys::Demo, false),
        (VenueMode::Live, Keys::Nothing, false),
        (VenueMode::Demo, Keys::Live, false),
        (VenueMode::Demo, Keys::Both, false),
        (VenueMode::Paper, Keys::Live, false),
    ] {
        let venues = policy_for("aster", line, None);
        assert_eq!(
            cex_mainnet_enabled(
                cex("aster"),
                &vars_for("aster", keys, None),
                &mount_policy(&venues)
            ),
            want,
            "aster at {line:?} holding {keys:?}: mainnet is a LIVE key set under a `live` ceiling"
        );
    }
}

/// The rules that need no store, over planted rows — including the two that a projection over real
/// keys cannot reach: a paper block this build does not know yet, and the registry's "armed on the
/// demo pair under a live ceiling" answer (aster's shape), whose block names the LIVE tier while the
/// row is armed on DEMO.
#[test]
fn the_feed_rule_over_planted_rows() {
    use vike_config::ArmingBlock as Block;
    use vike_tradehub::venue_arming::{FeedChoice, feed_tier_from_rows, selected_tier};
    let quiet = |tier| FeedChoice { tier, straddle: None };
    let default = AccountLabel::Default;
    let paper = VenueMode::Paper;

    // An armed row's tier beats whatever its block says.
    let armed_demo_under_live = row(default.clone(), VenueMode::Demo, Block::LiveCredentialsAbsent);
    assert_eq!(selected_tier(&armed_demo_under_live), Tier::Demo);
    assert_eq!(feed_tier_from_rows(&[armed_demo_under_live]), quiet(Tier::Demo));

    // Paper names the live tier in exactly one case.
    assert_eq!(
        feed_tier_from_rows(&[row(default.clone(), paper, Block::LiveCredentialsAbsent)]),
        quiet(Tier::Live)
    );
    for block in [Block::Disarmed, Block::NoCredentials, Block::AccountNotNamed, Block::NoLiveArm] {
        assert_eq!(
            feed_tier_from_rows(&[row(default.clone(), paper, block)]),
            quiet(Tier::Demo),
            "a paper account whose block is {block:?} names no live tier: the feed stays on the \
             lower network"
        );
    }

    // No rows at all (a venue the roster does not carry) is the lower network, not a panic.
    assert_eq!(feed_tier_from_rows(&[]), quiet(Tier::Demo));

    // Agreement says nothing.
    let alt_live = row(alt(), VenueMode::Live, Block::None);
    let alt_demo = row(alt(), VenueMode::Demo, Block::None);
    let default_live = row(default.clone(), VenueMode::Live, Block::None);
    let default_paper = row(default, paper, Block::NoCredentials);
    assert_eq!(feed_tier_from_rows(&[default_live.clone(), alt_live]), quiet(Tier::Live));
    assert_eq!(
        feed_tier_from_rows(&[default_paper, alt_demo.clone()]),
        quiet(Tier::Demo),
        "a paper default does not make the feed paper: the armed labelled account is followed, \
         and a single network is not a straddle"
    );

    // The straddle: MAINNET WINS, nothing is refused, and the data to say it comes back with the
    // tier — subjects and settings keys only.
    let choice = feed_tier_from_rows(&[default_live, alt_demo]);
    assert_eq!(choice.tier, Tier::Live, "mainnet and testnet armed on one venue: MAINNET WINS");
    let straddle = choice.straddle.expect("both networks armed: the choice carries the straddle");
    assert_eq!(straddle.venue, "hyperliquid");
    assert_eq!(straddle.mainnet_accounts, ["hyperliquid"]);
    assert_eq!(straddle.testnet_accounts, ["hyperliquid account `ALT`"]);
    assert_eq!(straddle.testnet_keys, ["policy.accounts.hyperliquid.ALT"]);
    let words = straddle.message();
    for needle in ["hyperliquid", "ALT", "MAINNET", "TESTNET", "policy.accounts.hyperliquid.ALT"] {
        assert!(words.contains(needle), "the message must name {needle}: {words}");
    }
}

// ---------------------------------------------------------------------------------------------
// 7. The call site
// ---------------------------------------------------------------------------------------------

/// **The plan loop asks the registry over the mount's OWN policy.** `venue_feed_plan` takes a
/// `&MountPolicy`, and the compiler is happy with `&MountPolicy::default()` — which would silently
/// restore a second opinion (a bridge's `resolve` that reads the `account` table or a `venue_setting`
/// row would answer from an empty snapshot here and from the real one at mount). `live_mount_with`
/// is a function that spawns a core and opens sockets, so nothing can call it; this is the text gate
/// the sibling wiring pins are (`account_badge_wiring_pin.rs`), over code lines only so a comment
/// cannot satisfy it: the policy is built BEFORE the one production call, that call is handed it, and
/// the same binding is what the `NodeConfig` is then built with.
#[test]
fn the_plan_loop_asks_the_registry_over_the_mounts_own_policy() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/tradehub_cli.rs");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let code: String =
        text.lines().filter(|l| !l.trim_start().starts_with("//")).collect::<Vec<_>>().join("\n");
    assert_eq!(
        code.matches("venue_feed_plan(").count(),
        1,
        "exactly one production call of `venue_feed_plan` in tradehub_cli.rs"
    );
    let call = code
        .find("venue_feed_plan(&m.cfg, &vars, &mount_policy)?")
        .expect("the plan loop must hand `venue_feed_plan` the mount's own `mount_policy`");
    let built = code
        .find("let mount_policy = vike_mount::MountPolicy {")
        .expect("`live_mount_with` builds its `mount_policy` here");
    assert!(built < call, "the policy must be built BEFORE the plan loop that asks the registry");
    assert!(
        code.contains("policy: mount_policy,"),
        "the SAME binding must be what the `NodeConfig` mounts with"
    );
}
