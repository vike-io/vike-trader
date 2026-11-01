//! **The venue half's arming: the registry-driven projection, and the producer of the
//! [`ArmingTable`]** (wave 2, W9a; moved into the gatewayhub by W9b). Everything here asks a bridge's own
//! `VenueAdapter::resolve` through the registry `BridgeTrade` holds: the generic fold in the tradehub's
//! `vike-tradehub-core` never reaches it, it reads the table [`arming_table`] builds, which crosses
//! the wire as `ArmingTable` (`vike_gatewayhub_client::proto`).
//!
//! The functions are the ones `arming.rs` carried before the seam, unchanged in body. The `pub`
//! probes are public for the roster tests in `crates/vike-gatewayhub/tests/trade_roster/mount_roster.rs`,
//! which run where the registry is (docs/decisions/0096).

use std::collections::{BTreeMap, HashMap};

use vike_bridge_core::venue_adapter::ClockDecl;
use vike_config::account_arming::{
    PAPER_RUNS, account_ids, account_tier, known_accounts, venue_tier,
};
use vike_gatewayhub_client::proto::{ArmingRow, ArmingTable, VenueFacts};
use vike_model::accounts::account_keys::AccountLabel;
use vike_secrets::account_credentials::AccountCredentials;

use super::registry::{VenueRow, row_of};
use super::snapshot::TradeSnapshot;

/// PRE-CONNECT live-intent probe: would [`crate::make_engine`] build a LIVE exec client for
/// `venue`'s DEFAULT account, holding `credentials`? Judged PURELY from the account's credentials
/// and the deployment map through the bridge's own `VenueAdapter::resolve` (the answer its start
/// acts on, so the two cannot drift), with NO network I/O or side effects — what
/// lets [`crate::require_live_risk_budget`] refuse BEFORE any venue session exists. A `FeatureAbsent` row
/// and an unknown id probe `false`: a `demo`/`live` account of either does not run.
///
/// INTENT, not outcome: `true` means the operator SUPPLIED this venue's live config, even where the
/// start later does not come up (ctrader/ibkr connect failure, hyperliquid/polymarket declining a
/// bad key: the account then does NOT RUN, `ArmingBlock::AdapterFailed`). An intended-live session
/// must carry a bounded budget; a config the operator never wrote can never refuse a start.
#[must_use]
pub fn would_go_live(
    registry: &'static [VenueRow],
    venue: &str,
    vars: &HashMap<String, String>,
    credentials: AccountCredentials<'_>,
) -> bool {
    would_go_live_under(registry, venue, vars, credentials, vike_model::AccountTier::Live)
}

/// [`venue_account_arming`]'s body.
fn account_arming_in(
    registry: &'static [VenueRow],
    venue: &str,
    vars: &HashMap<String, String>,
    policy: Option<&TradeSnapshot>,
) -> Vec<vike_config::VenueArming> {
    let Some(venue) = vike_model::VENUES.iter().copied().find(|v| *v == venue) else {
        return Vec::new();
    };
    known_accounts(venue, policy.map(|p| &p.accounts))
        .into_iter()
        .map(|label| {
            let (tier, effective, block) = account_arming(registry, venue, &label, vars, policy);
            let account_ids = account_ids(policy.map(|p| &p.accounts), venue, &label);
            vike_config::VenueArming { venue, label, tier, effective, block, account_ids }
        })
        .collect()
}

/// **One account's whole arming answer** — `(tier, effective, block)`: its tier from the table
/// ([`vike_config::account_arming::account_tier`]), then [`account_arming_under`] at that tier. `effective` is `None` when the
/// account does NOT RUN, and `tier` is `None` when no active row states one.
///
/// The order is load-bearing: (1) a LABELLED account on a venue that cannot address one is
/// `NoAccountSupport` whatever its row says (the generic precondition, [`refuses_the_label`] —
/// inside [`account_arming_raw`] too — answers before the tier is read); (2) an account the TABLE
/// does not run carries the table's own cause and never reaches the bridge, and a PAPER tier runs
/// the simulator without reaching it either (`crate::contract`'s `contract_arming` short-circuits);
/// (3) else the bridge's `resolve` under `AdapterInputs::tier = tier`, and (4) the preflight stop and
/// the process-exclusive override ([`account_arming_under`]).
pub(crate) fn account_arming(
    registry: &'static [VenueRow],
    venue: &str,
    label: &AccountLabel,
    vars: &HashMap<String, String>,
    policy: Option<&TradeSnapshot>,
) -> (Option<vike_model::AccountTier>, Option<vike_model::AccountTier>, vike_config::ArmingBlock) {
    match account_tier(policy.map(|p| &p.accounts), venue, label) {
        Ok(tier) => {
            let (effective, block) =
                account_arming_under(registry, venue, label, vars, tier, policy);
            (Some(tier), effective, block)
        }
        // The table runs nothing for it — but the label precondition still answers first: no row
        // could make a labelled account of a one-account venue addressable.
        Err(_) if refuses_the_label(registry, venue, label) => {
            (None, None, vike_config::ArmingBlock::NoAccountSupport)
        }
        Err(why) => (None, None, why),
    }
}

/// **The generic precondition every projection answers BEFORE the tier**: a LABELLED account on a
/// venue whose start cannot address one (a contract row declaring `addresses_accounts: false`, or
/// an id the registry does not carry). A `FeatureAbsent` row is judged by its tier instead, as it
/// always was. `crate::contract`'s `contract_arming` and [`account_arming_raw`] ask the same
/// question in the same place.
fn refuses_the_label(registry: &'static [VenueRow], venue: &str, label: &AccountLabel) -> bool {
    !label.is_default()
        && match row_of(registry, venue) {
            Some(VenueRow::Adapter(row)) => !row.declaration().addresses_accounts,
            Some(VenueRow::FeatureAbsent { .. }) => false,
            None => true,
        }
}

/// [`would_go_live_under`] over a deployment's policy — the spelling every caller that HOLDS a
/// policy uses, so the preflight and the start cannot disagree. A per-caller `policy.map_or(…)`
/// would be the second copy where the `None`-means-`Live` mistake lives. A DEFAULT account the
/// table does not run answers `false`: nothing would be authenticated for it.
pub fn would_go_live_under_policy(
    registry: &'static [VenueRow],
    venue: &str,
    vars: &HashMap<String, String>,
    policy: Option<&TradeSnapshot>,
) -> bool {
    let Some(tier) = venue_tier(policy.map(|p| &p.accounts), venue) else { return false };
    // ⚠ NOT `would_go_live_under` (no policy): the DEFAULT account can DECLINE a
    // process-exclusive resource (`crate::exclusive`'s `holder`), and a preflight must not
    // authenticate for a venue whose default account the start does not run.
    account_arming_under(registry, venue, &AccountLabel::Default, vars, tier, policy)
        .0
        .is_some_and(|t| t != vike_model::AccountTier::Paper)
}

/// [`would_go_live`] at a given account TIER — what [`crate::make_engine_for_account`] would
/// reach for the DEFAULT account at that tier: the tier changes which keys and network it resolves.
/// "Live" means **a venue client**: a real authenticated session at whichever TIER (the budget rule
/// asks whether a session is established for the operator, not which endpoint it dials). `tier ==
/// Paper` is `false` outright ([`venue_arming_under`] short-circuits).
///
/// ⚠ **DERIVED, not a second copy**: the coarse projection of [`venue_arming_under`] (the finer
/// "which TIER, and what holds it back" `vike-backend venues` renders). ONE row set with thin
/// wrappers is the point — every row mirrors its arm's own loader, and a second copy is the drift
/// that prevents. [`would_go_live`] is the wrapper at the `live` tier, the "would these
/// CREDENTIALS arm it" that `report_not_running` needs to say whether the account table held back
/// something the keys would have armed.
///
/// ⚠ **The preflight is NOT a legitimate tier-less caller**
/// ([`crate::startup::run_startup_preflight`] takes [`would_go_live_under_policy`], threaded
/// from `crate::assemble_runtime`): its clock leg's ig
/// row is a CREDENTIALED read (`crates/bridges/ig/src/adapter.rs`'s `IgVenueAdapter::server_time_ms`
/// sends `X-IG-API-KEY`) and its credential leg a SIGNED `fetch_balance` per venue — against an
/// account the operator left at `paper`.
///
/// ⚠ **A `Live` probe with demo-only keys answers `false`**: binance/bybit/okx/hyperliquid refuse
/// to sign mainnet with demo keys (decision 0095), and every other arm that would fall back to its
/// demo tier does not run under the no-downgrade rule (`crate::contract`'s `resolution_to_arming`:
/// a `live` account never trades demo). A caller asking "credentials for SOME tier?" must probe
/// `Demo` too — `crate::paper_fallback`'s `would_mount_under_some_tier`.
///
/// ⚠ **`pub` for `vike-tradehub`'s roster tests** (`crates/vike-gatewayhub/tests/trade_roster/mount_roster.rs`).
/// The live sentinel's pre-assembly set is NOT computed here: `crate::armed_live_venues` walks
/// `venue_account_arming`, the per-ACCOUNT form of the same projection.
#[must_use]
pub fn would_go_live_under(
    registry: &'static [VenueRow],
    venue: &str,
    vars: &HashMap<String, String>,
    credentials: AccountCredentials<'_>,
    tier: vike_model::AccountTier,
) -> bool {
    venue_arming_under(registry, venue, vars, credentials, tier)
        .0
        .is_some_and(|t| t != vike_model::AccountTier::Paper)
}

/// **THE per-venue arming projection: which tier `make_engine` would run `venue`'s DEFAULT account
/// at, given `tier` and `credentials` (by FIELD, decision 0156 item 6), and what stops it when it
/// would not** — PURELY from those credentials, the deployment map and this binary's registry (an
/// uncompiled bridge is a `FeatureAbsent` row), with NO network I/O or side effects. `(None, why)`
/// is NOT RUNNING.
///
/// A row that stops matching its arm's loader fails the probe tests; a NEW live arm without a row
/// is still caught by [`crate::make_engine`]'s post-merge budget backstop. ⚠ An unknown venue (no
/// ROSTER venue today) answers `(None, NoLiveArm)`: a `demo`/`live` account of it does not run.
/// INTENT semantics, as [`would_go_live`]: the projection reports what the start will attempt.
/// `tier == Paper` answers `(Some(Paper), PaperTier)` outright: the paper simulator.
///
/// ⚠ **It threads NO policy**, so the `account` table reads UNREAD: the tier is the CALLER's, and
/// the one row reading more than the tier (dukascopy's) resolves the DEFAULT account without the
/// table, where the start ([`crate::make_engine_for_account`]) reads `env.policy`'s. A caller that
/// HOLDS a policy uses the exact [`would_go_live_under_policy`].
pub fn venue_arming_under(
    registry: &'static [VenueRow],
    venue: &str,
    vars: &HashMap<String, String>,
    credentials: AccountCredentials<'_>,
    tier: vike_model::AccountTier,
) -> (Option<vike_model::AccountTier>, vike_config::ArmingBlock) {
    // No policy: the DEFAULT account holds any process-exclusive resource (`crate::exclusive`'s
    // `holder`), so the override `account_arming_under` adds can never fire here.
    account_arming_raw_with(
        registry,
        venue,
        &AccountLabel::Default,
        vars,
        Some(credentials),
        tier,
        None,
    )
}

/// [`venue_arming_under`] for one named ACCOUNT — the projection [`venue_account_arming`] builds its
/// rows from, and the one [`crate::make_engine_accounts`] selects with.
///
/// ⚠ **A LABELLED account is refused outright on a venue declaring `addresses_accounts: false`**,
/// or one the registry does not carry, before `resolve` is consulted ([`account_arming_raw`],
/// `crate::contract`'s `contract_arming`). A bridge that CAN address one threads `label` into the
/// loader its `start` calls, so projection and start read the same account's keys.
///
/// Two box-level answers sit above the bridge's: a venue the startup preflight hard-FAILED
/// (`TradeSnapshot::preflight_failed`) runs none of its `demo`/`live` accounts (`PreflightFailed`),
/// and the process-exclusive override below.
pub(crate) fn account_arming_under(
    registry: &'static [VenueRow],
    venue: &str,
    label: &AccountLabel,
    vars: &HashMap<String, String>,
    tier: vike_model::AccountTier,
    policy: Option<&TradeSnapshot>,
) -> (Option<vike_model::AccountTier>, vike_config::ArmingBlock) {
    use vike_config::ArmingBlock as Block;
    let raw = account_arming_raw(registry, venue, label, vars, tier, policy);
    // THE PREFLIGHT STOP (decision 0156 item 1): a hard preflight FAIL stops the venue's `demo` and
    // `live` accounts with that reason — not the `NoCredentials` its withheld keys would otherwise
    // read as. The label precondition still answers first, and a `paper` account still runs.
    if raw.1 != Block::NoAccountSupport
        && tier != vike_model::AccountTier::Paper
        && policy.is_some_and(|p| p.preflight_failed.contains(venue))
    {
        return (None, Block::PreflightFailed);
    }
    // THE ONE-SIDECAR OVERRIDE, ABOVE every row, only where a venue declares a process-exclusive
    // resource: a fact about which OTHER account gets it, not about this account's credentials.
    // `crate::make_engine_for_account` consults the SAME function. When they disagreed, the
    // projection said `Demo` for both accounts, the mount built paper for one, and a strategy
    // naming it passed `crates/vike-tradehub-core/src/node/accounts.rs`'s `refuse_unarmed_mount_accounts`
    // and ran on paper believing it was armed.
    if raw.0.is_some_and(|t| t != vike_model::AccountTier::Paper)
        && super::contract::is_process_exclusive(row_of(registry, venue))
        && !super::exclusive::holds(registry, venue, label, vars, policy)
    {
        return (None, Block::SidecarHeldElsewhere);
    }
    raw
}

/// [`account_arming_under`] WITHOUT the one-exclusive-resource override: a contract row through its
/// bridge's `resolve`, every other row by the fixed answers below. Besides that wrapper, only
/// `crate::exclusive`'s `holder` calls it: "would this account arm on its own merits", without
/// asking the override again.
///
/// ⚠ `arm_addresses_accounts` (the hand-written list) is gone: each bridge declares
/// `VenueDeclaration::addresses_accounts`, and an unknown venue addresses none.
pub(crate) fn account_arming_raw(
    registry: &'static [VenueRow],
    venue: &str,
    label: &AccountLabel,
    vars: &HashMap<String, String>,
    tier: vike_model::AccountTier,
    policy: Option<&TradeSnapshot>,
) -> (Option<vike_model::AccountTier>, vike_config::ArmingBlock) {
    account_arming_raw_with(registry, venue, label, vars, None, tier, policy)
}

/// [`account_arming_raw`] with the account's credentials SUPPLIED (`Some`) rather than resolved
/// from `policy`'s directory (`None`) — the policy-less probes' door ([`venue_arming_under`]).
fn account_arming_raw_with(
    registry: &'static [VenueRow],
    venue: &str,
    label: &AccountLabel,
    vars: &HashMap<String, String>,
    credentials: Option<AccountCredentials<'_>>,
    tier: vike_model::AccountTier,
    policy: Option<&TradeSnapshot>,
) -> (Option<vike_model::AccountTier>, vike_config::ArmingBlock) {
    use vike_config::ArmingBlock as Block;
    use vike_model::AccountTier as Mode;
    match row_of(registry, venue) {
        Some(VenueRow::Adapter(row)) => {
            super::contract::contract_arming(*row, label, vars, credentials, tier, policy)
        }
        // A labelled account reaches the tier check too (as the feature-off rows always did).
        Some(VenueRow::FeatureAbsent { .. }) => {
            if tier == Mode::Paper {
                PAPER_RUNS
            } else {
                (None, Block::FeatureAbsent)
            }
        }
        // Not in the registry: a labelled account first (no named account here), then the
        // tier, then "no live arm".
        None => {
            if refuses_the_label(registry, venue, label) {
                (None, Block::NoAccountSupport)
            } else if tier == Mode::Paper {
                PAPER_RUNS
            } else {
                (None, Block::NoLiveArm)
            }
        }
    }
}

/// **The facts of a venue whose bridge this build does not carry** (a `FeatureAbsent` row, or an id the
/// registry holds no row for): no adapter, nothing addressable, no grid, no exclusive resource.
fn absent_facts() -> VenueFacts {
    VenueFacts {
        adapter_present: false,
        addresses_accounts: false,
        takes_recon_trigger: false,
        grid_source: vike_gatewayhub_client::proto::GridSource::NoGrid,
        process_exclusive: None,
        clock_decl_word: "absent".to_string(),
    }
}

/// **The static facts of one venue**, read off its registry row: a `FeatureAbsent` row or an id
/// the registry does not carry answers [`absent_facts`].
fn facts_of(registry: &'static [VenueRow], venue: &str) -> VenueFacts {
    match row_of(registry, venue) {
        Some(VenueRow::Adapter(row)) => {
            let declaration = row.declaration();
            VenueFacts {
                adapter_present: true,
                addresses_accounts: declaration.addresses_accounts,
                takes_recon_trigger: declaration.takes_recon_trigger,
                grid_source: super::symbol_grid::grid_source_of(declaration.grid_source),
                process_exclusive: declaration.process_exclusive.map(|e| e.resource.to_string()),
                clock_decl_word: match declaration.clock {
                    ClockDecl::Wired { .. } => "wired",
                    ClockDecl::NotWired { .. } => "not-wired",
                }
                .to_string(),
            }
        }
        Some(VenueRow::FeatureAbsent { .. }) | None => absent_facts(),
    }
}

/// **Every account row of every roster venue, with the venue BOOK the account trades**, in
/// `vike_model::VENUES` order, a venue's accounts default-first — the order `venue_arming` always
/// returned them in. Each row is [`account_arming_in`]'s verdict: the account's tier from the table,
/// then the bridge's `resolve` at that tier, then the one-sidecar override; the book is the store's
/// recorded id, else the offline derivation, `None` where neither names it.
///
/// ⚠ **`policy` must carry no `preflight_failed` venue**: the startup preflight's stop is the
/// tradehub's overlay over the table, so a stop baked in here could not be told apart from the venue's
/// own answer. `BridgeTrade` clears it.
#[must_use]
pub fn arming_rows(
    registry: &'static [VenueRow],
    vars: &HashMap<String, String>,
    policy: &TradeSnapshot,
) -> Vec<(vike_config::VenueArming, Option<String>)> {
    let directory = crate::trade::adapters::snapshot::directory_of(Some(policy));
    vike_model::VENUES
        .iter()
        .flat_map(|venue| account_arming_in(registry, venue, vars, Some(policy)))
        .map(|arming| {
            let book = arming.effective.and_then(|mode| {
                super::book_identity::book_of_account(
                    registry,
                    arming.venue,
                    &arming.label,
                    mode,
                    directory,
                )
            });
            (arming, book)
        })
        .collect()
}

/// **The word an `ArmingBlock` crosses the wire as** (`ArmingRow::block_word`, `AdapterRefusal::block_word`):
/// one function, so there is ONE place that names the spelling: `ArmingBlock::word()` (snake case, a key for a
/// program that never changes with a rewording; "none" for a clear row). The tradehub's `RemoteBackend` reads
/// the words back with `ArmingBlock::from_word`, and the session (`crate::trade::session`'s `block_runs`)
/// reads the table's words the same way: never `ArmingBlock::as_str()`, the badge for a person, which is
/// the empty string for a clear row.
#[must_use]
pub fn wire_word(block: vike_config::ArmingBlock) -> &'static str {
    block.word()
}

/// **THE ARMING TABLE of this registry under `policy`** — the producer of what the tradehub fetches once
/// per assembly (`TradeLane::arming_table`, the wire's `ArmingTable`): the static facts of every
/// roster venue (and of any registry row the roster does not carry) and one row per ACCOUNT of each
/// roster venue ([`arming_rows`]), with `ArmingBlock` as its word. No credential is in it.
#[must_use]
pub fn arming_table(
    registry: &'static [VenueRow],
    vars: &HashMap<String, String>,
    policy: &TradeSnapshot,
) -> ArmingTable {
    let mut facts: BTreeMap<String, VenueFacts> = BTreeMap::new();
    for venue in vike_model::VENUES.iter().copied().chain(registry.iter().map(VenueRow::venue)) {
        facts.entry(venue.to_string()).or_insert_with(|| facts_of(registry, venue));
    }
    let rows = arming_rows(registry, vars, policy)
        .into_iter()
        .map(|(arming, book)| ArmingRow {
            venue: arming.venue.to_string(),
            label: arming.label.text().map(str::to_string),
            tier: arming.tier,
            effective: arming.effective,
            block_word: wire_word(arming.block).to_string(),
            account_ids: arming.account_ids,
            book,
        })
        .collect();
    // `key_changed` is the lane's to fill: it knows what its running adapters started with.
    ArmingTable { facts, rows, key_changed: Vec::new() }
}

/// **The shared-BOOK report for one venue's already-resolved arming rows, resolved against `policy`'s
/// directory** — the registry-taking form of the computation whose per-account result the
/// [`ArmingTable`] carries as [`ArmingRow::book`] (the table builder resolves each running
/// account's book through [`super::book_identity::book_of_account`], and the fold's
/// `crate::shared_books_for` reads those). Kept for the roster tests that plant a DIFFERENT directory
/// beside the rows (the recorded-id half of the resolution): every pair of ACTIVE accounts with the
/// same effective trading book, which `make_engine_accounts` warns about and starts anyway.
///
/// An account whose book [`super::book_identity::book_of_account`] cannot determine contributes no
/// [`vike_config::ArmedBook`], so "warn nothing where you cannot tell" holds by ABSENCE — and so does
/// an account that does not run (no effective tier: it has no book this process trades). Resolution
/// is RECORDED-first ([`super::book_identity::recorded_book`] reads
/// `vike_secrets::Account::venue_account_id` from [`TradeSnapshot::accounts`]).
#[must_use]
pub fn shared_books_under(
    registry: &'static [VenueRow],
    venue: &str,
    rows: &[vike_config::VenueArming],
    policy: Option<&TradeSnapshot>,
) -> Vec<vike_config::SharedBook> {
    let directory = crate::trade::adapters::snapshot::directory_of(policy);
    let books: Vec<vike_config::ArmedBook> = rows
        .iter()
        .filter_map(|r| {
            let effective = r.effective?;
            let book = super::book_identity::book_of_account(
                registry, r.venue, &r.label, effective, directory,
            )?;
            Some(vike_config::ArmedBook {
                venue: r.venue,
                label: r.label.clone(),
                book,
                mode: effective,
            })
        })
        .collect();
    let _ = venue;
    vike_config::shared_books(&books)
}
