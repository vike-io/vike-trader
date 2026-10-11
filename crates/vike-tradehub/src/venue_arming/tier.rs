//! Which network a venue's accounts and its one feed dial — asked of the registry row.

use std::collections::{HashMap, HashSet};
use std::sync::{LazyLock, Mutex, PoisonError};

use vike_bridge_core::venue_mount::Tier;
use vike_config::{ArmingBlock, VenueArming, VenueMode};
use vike_mount::MountPolicy;

#[cfg(doc)]
use super::arming::with_other_live_accounts;
use crate::CexVenue;

// -------------------------------------------------------------------------------------------
// Which network a venue's accounts mount on — asked of the REGISTRY ROW, nowhere else
// -------------------------------------------------------------------------------------------

/// **The tier one account's exec side dials**, as the venue's registry row answered for it, or
/// `None` when that account stays on the paper book.
///
/// `row` is a [`VenueArming`] out of `vike_mount::venue_account_arming`: the projection that calls
/// each bridge's pure `VenueMount::resolve` (the arming probe the mount itself acts on) over that
/// account's own tier (its active `account` row) and keys, and the very rows
/// `vike_mount::accounts_to_mount` selects the mounted accounts from. None of the network functions
/// below re-reads an account row, a variable or a key.
#[must_use]
pub(crate) fn exec_tier(row: &VenueArming) -> Option<Tier> {
    match row.effective {
        VenueMode::Live => Some(Tier::Live),
        VenueMode::Demo => Some(Tier::Demo),
        VenueMode::Paper => None,
    }
}

/// **The tier the ACCOUNT selected for itself** — [`exec_tier`] where the row armed, and, where it
/// stayed paper, the tier the arm reached for and did not find a key at.
///
/// A paper account dials nothing, but the venue's FEED still has to dial somewhere, and it must not
/// move when a key goes missing: an ACTIVE `live`-tier account row with no LIVE key set is still a
/// mainnet feed. The registry names that case — and only that case — with
/// [`ArmingBlock::LiveCredentialsAbsent`] (`vike_bridge_core::venue_mount::PaperCause`'s doc: the
/// account's `live` tier permits the live tier, the arm could reach it, and it does NOT fall back to
/// demo — a live account never trades demo). Every other paper block (`PaperTier`,
/// `AccountInactive`, `NoAccountRow`, `TierConflict`, `NoCredentials`, …) names no tier, and a feed
/// with nothing to follow dials the DEMO network, which is what any tier below `live` selects.
/// ⚠ Deliberately a positive test for the one live-naming block rather than a match over the
/// others: a paper cause added later defaults to the lower network, never to mainnet.
#[must_use]
pub fn selected_tier(row: &VenueArming) -> Tier {
    match exec_tier(row) {
        Some(tier) => tier,
        None if row.block == ArmingBlock::LiveCredentialsAbsent => Tier::Live,
        None => Tier::Demo,
    }
}

/// One venue's arming rows — default account first — from the registry this daemon mounts through.
///
/// `policy` is the mount's OWN [`MountPolicy`]: the `account` table (each account's tier and
/// `active` switch) and the `venue_setting` snapshot, the one value `live_mount_with` builds ahead of its plan loop and later
/// moves into the `NodeConfig` the mount is built from. That is what keeps this projection from
/// becoming a second opinion — a bridge's `resolve` that one day reads `MountInputs::settings` or
/// `MountInputs::accounts` answers here from the same snapshot it answers from at mount, instead of
/// from an empty one.
fn rows_for(venue: &str, vars: &HashMap<String, String>, policy: &MountPolicy) -> Vec<VenueArming> {
    vike_mount::venue_account_arming(crate::registry::REGISTRY, venue, vars, Some(policy))
}

/// Whether this CEX venue's exec binds MAINNET — its DEFAULT account's [`selected_tier`], asked of
/// the venue's registry row: for binance/bybit/okx the account's tier IS the network (decision
/// 0095 as decision 0119 restated it, and their `resolve` answers `LiveCredentialsAbsent` rather
/// than falling back to the demo pair); for aster the credential chain under that tier, which
/// `AsterVenueMount::resolve` runs (`crates/bridges/aster/src/mount.rs`) — a `live`-tier aster
/// account whose bridge would bind its testnet pair mounts PAPER instead (vike-mount's tier
/// interlock), so its feed still follows `live`. Resolved at the venue GATE and carried in
/// [`crate::VenuePlan::Cex`], because `vars` is moved into the `NodeConfig` before the feed block
/// runs.
///
/// ⚠ It used to re-implement each bridge's network choice (`live_permitted` for three, the bridge's
/// `mountable_tier_for_account` for aster) and so could disagree with the mount; it asks the row now,
/// so it cannot. The DEFAULT account only, by design: the announcement it feeds speaks for the
/// account every strategy mount trades through, and the venue's other accounts are said separately
/// ([`with_other_live_accounts`]).
#[must_use]
pub fn cex_mainnet_enabled(
    venue: CexVenue,
    vars: &HashMap<String, String>,
    policy: &MountPolicy,
) -> bool {
    rows_for(venue.slug(), vars, policy)
        .iter()
        .find(|row| row.is_default_account())
        .is_some_and(|row| selected_tier(row) == Tier::Live)
}

/// **Which network a venue's ONE market feed dials**, from the arming rows of every account of the
/// venue — pure over the rows, so the rule is testable without a store.
///
/// A feed is per VENUE and an account is not, so the rule is stated over the set of accounts whose
/// exec side the feed serves — every account the registry ARMED ([`exec_tier`] is `Some`):
///
/// * **they agree** — the feed dials that network. This is the whole of the ordinary case, and the
///   only one a box without a labelled account can reach: one account, one tier.
/// * **none is armed** — there is no exec side to follow, so the feed follows the DEFAULT account's
///   [`selected_tier`]: an active `live`-tier row with no key still gets the mainnet feed, and a
///   `demo` or `paper` tier (or no active row at all) the testnet one. The default account, because
///   it is the one every strategy mount trades through.
/// * **they straddle both networks** — MAINNET WINS, the start is NOT refused, and the straddle is
///   returned alongside the tier so the caller can say it ([`feed_tier`] logs it at `warn!`). One
///   feed cannot be right for both and the two guesses do not cost the same: a mainnet feed under a
///   testnet account costs only testnet fidelity, while a testnet feed under a live account prices
///   REAL orders off the testnet book. Refusing instead would stop every venue and every mount,
///   would fire on the next restart or deploy, and a node that will not start cannot flatten a
///   position (`docs/decisions/0013-degrade-vs-refuse.md`).
///
/// ⚠ **The default account being paper does not make the feed paper.** When the default account
/// stays paper (a missing key, an inactive or paper-tier row) and a labelled one armed, the armed
/// account is the exec side the feed serves and the feed dials ITS network — a keyless `live`
/// default and a `demo` labelled account is a testnet feed, never the mainnet feed the default
/// account alone would have picked.
///
/// What the rule guarantees is exactly this: **a feed is never on testnet while any armed account
/// is on mainnet.** It does NOT guarantee that every armed account shares the feed's network — a
/// testnet account beside a live one is priced off the mainnet book, and [`Straddle`] is how that
/// gets said.
#[must_use]
pub fn feed_tier_from_rows(rows: &[VenueArming]) -> FeedChoice {
    let armed =
        |tier: Tier| rows.iter().filter(|row| exec_tier(row) == Some(tier)).collect::<Vec<_>>();
    let (live, demo) = (armed(Tier::Live), armed(Tier::Demo));
    match (live.is_empty(), demo.is_empty()) {
        (false, true) => FeedChoice::agreed(Tier::Live),
        (true, false) => FeedChoice::agreed(Tier::Demo),
        (true, true) => FeedChoice::agreed(
            rows.iter().find(|row| row.is_default_account()).map_or(Tier::Demo, selected_tier),
        ),
        (false, false) => {
            let subjects = |accounts: &[&VenueArming]| -> Vec<String> {
                accounts.iter().map(|row| row.subject()).collect()
            };
            FeedChoice {
                tier: Tier::Live,
                straddle: Some(Straddle {
                    venue: live[0].venue,
                    mainnet_accounts: subjects(live.as_slice()),
                    testnet_accounts: subjects(demo.as_slice()),
                    testnet_account_ids: demo
                        .iter()
                        .flat_map(|row| row.account_ids.iter().copied())
                        .collect(),
                }),
            }
        }
    }
}

/// The answer [`feed_tier_from_rows`] gives: the tier the venue's one feed dials, and — only when the
/// venue's armed accounts dial both networks — the [`Straddle`] that has to be said out loud.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedChoice {
    pub tier: Tier,
    pub straddle: Option<Straddle>,
}

impl FeedChoice {
    /// The ordinary answer: nothing to say.
    fn agreed(tier: Tier) -> Self {
        FeedChoice { tier, straddle: None }
    }
}

/// **A venue whose armed accounts dial both networks**, as data: which accounts are on which
/// network, and the `account.id`s of the rows that put the testnet ones there (the `--id` the
/// account verbs take). Subjects and row ids only — an account label and an integer, never a
/// credential value.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Straddle {
    pub venue: &'static str,
    pub mainnet_accounts: Vec<String>,
    pub testnet_accounts: Vec<String>,
    pub testnet_account_ids: Vec<i64>,
}

impl Straddle {
    /// The testnet rows' ids, comma-joined — the `--id` values the remedy names.
    fn testnet_ids(&self) -> String {
        self.testnet_account_ids.iter().map(i64::to_string).collect::<Vec<_>>().join(", ")
    }

    /// The sentence the operator reads. It says which book the testnet accounts are priced off, why
    /// the feed is on that book anyway, and the two ways out.
    #[must_use]
    pub fn message(&self) -> String {
        format!(
            "⚠ {venue}: this daemon wires ONE market feed per venue, and the venue's armed accounts \
             dial BOTH networks — MAINNET: {mainnet}; TESTNET: {testnet}. The feed dials MAINNET \
             (a testnet feed would price REAL orders off the testnet book), so the TESTNET \
             account(s) {testnet} are priced off the MAINNET book: their orders are real testnet \
             orders against mainnet quotes, and what they fill and earn says nothing about how \
             the strategy does on testnet. To end it, put the accounts on one network — \
             `vike-cli secrets account set-tier --id <N> --tier <paper|demo|live>` (or \
             `vike-cli secrets account deactivate --id <N>`) for the testnet account row(s) \
             {ids} — or run the testnet account in its own process, with its own project folder",
            venue = self.venue,
            mainnet = self.mainnet_accounts.join(", "),
            testnet = self.testnet_accounts.join(", "),
            ids = self.testnet_ids(),
        )
    }
}

/// The straddles [`feed_tier`] has already said in this process.
///
/// A plan is built whenever a venue is mounted, and a venue can be mounted more than once in a
/// process (a second mount of the same venue, a re-plan), so a warning emitted per plan build says
/// the same sentence at every one. The set is keyed by the straddle's CONTENT — the venue, the
/// accounts on each network, the account rows that put them there — so the same straddle is said once
/// and a DIFFERENT one (another testnet account joins, a mainnet account leaves) is a new finding
/// and is said. A restart of the process says it again, which is the right cadence: the operator
/// reads one start's log at a time.
static SAID_STRADDLES: LazyLock<Mutex<HashSet<Straddle>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

/// Whether this is the first time `straddle` is being said in this process.
fn first_sighting(straddle: &Straddle) -> bool {
    SAID_STRADDLES.lock().unwrap_or_else(PoisonError::into_inner).insert(straddle.clone())
}

/// The tier `venue`'s ONE feed dials, from the registry's own rows for it — the answer
/// `venue_feed_plan` builds the hyperliquid feed on. A straddle is logged here, at `warn!`, ONCE PER
/// PROCESS per straddle (a second plan build over the same accounts says nothing more; see
/// `SAID_STRADDLES`) with the venue, the accounts on each network and the `account.id`s of the
/// testnet rows as structured fields; the message carries the same words. The PLAN is the same
/// every time: only the sentence is deduplicated, never the answer.
pub(crate) fn feed_tier(venue: &str, vars: &HashMap<String, String>, policy: &MountPolicy) -> Tier {
    let choice = feed_tier_from_rows(&rows_for(venue, vars, policy));
    if let Some(straddle) = choice.straddle.as_ref().filter(|s| first_sighting(s)) {
        tracing::warn!(
            venue = straddle.venue,
            mainnet_accounts = %straddle.mainnet_accounts.join(", "),
            testnet_accounts = %straddle.testnet_accounts.join(", "),
            testnet_account_ids = %straddle.testnet_ids(),
            "{}",
            straddle.message()
        );
    }
    choice.tier
}
