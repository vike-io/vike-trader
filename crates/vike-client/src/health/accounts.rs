//! Which accounts the frame and the directory say exist, and the `engine` rows of [`super::judge`]: one per account,
//! the tier it runs at beside the tier its rows state.

use super::*;

/// Everything the snapshot and the directory say about one account.
pub(super) struct AccountView<'a> {
    pub(super) key: String,
    /// The newest block published under this key.
    pub(super) block: Option<&'a VenueBlockWire>,
    /// The node lists it NOT RUNNING.
    pub(super) stopped: Option<&'a ArmingNotRunningViewWire>,
    /// What its ACTIVE rows state, folded (known only when the directory was read).
    pub(super) row_tier: Option<AccountRowActiveTier>,
}
impl<'a> AccountView<'a> {
    /// The block of an engine that RUNS: a NOT RUNNING account's retained block is not one.
    pub(super) fn running(&self) -> Option<&'a VenueBlockWire> {
        if self.stopped.is_some() { None } else { self.block }
    }

    /// [`Self::row_tier`] in the words a row prints it with.
    pub(super) fn row_words(&self) -> Option<&'static str> {
        match self.row_tier? {
            AccountRowActiveTier::Tier(tier) => Some(tier.as_str()),
            AccountRowActiveTier::Conflict => Some(TWO_TIERS),
            AccountRowActiveTier::NoRow | AccountRowActiveTier::Inactive => None,
        }
    }
}
/// The node's own word for this state (`ArmingBlock::TierConflict`).
const TWO_TIERS: &str = "two active tiers";
fn slot<'m, 'a>(
    map: &'m mut BTreeMap<String, AccountView<'a>>,
    key: String,
) -> &'m mut AccountView<'a> {
    map.entry(key.clone()).or_insert_with(|| AccountView {
        key,
        block: None,
        stopped: None,
        row_tier: None,
    })
}
/// The accounts of the frame, the NOT RUNNING list and the directory, by route key.
pub(super) fn collect_accounts<'a>(
    snap: &'a CoreSnapshotWire,
    directory: Option<&'a AccountDirectoryWire>,
) -> Vec<AccountView<'a>> {
    let mut map = BTreeMap::new();
    // The core's list only grows, so a later block under one key is the newer engine's.
    for block in &snap.venues {
        slot(&mut map, block_key(block)).block = Some(block);
    }
    for stopped in &snap.not_running {
        slot(&mut map, stopped.route_key.clone()).stopped = Some(stopped);
    }
    let rows: &[AccountDirectoryAccountWire] = directory.map_or(&[], |d| d.accounts.as_slice());
    let table = account_table(rows);
    for account in rows {
        slot(&mut map, route_key(&account.venue, account.label.as_deref())).row_tier =
            stated_tier(&table, account);
    }
    // A LANE TWIN (`vike_venues::lane_twin`: binance's `PERP` perpetual lane) is derived from the default
    // account's row while it states a margin leverage and has NO row of its own, so it would read as an
    // engine "no active row states" for ever. It is judged against the row it is derived from: expected,
    // and a WARN only if it runs at another tier than that row. Recognised by the shape the control door uses.
    for block in &snap.venues {
        let Some(twin) = vike_venues::lane_twin(&block.venue) else { continue };
        if !is_lane_twin_block(twin, block) {
            continue;
        }
        let parent = map.get(&block.venue).and_then(|p| p.row_tier);
        let own = slot(&mut map, block_key(block));
        if own.row_tier.is_none() {
            own.row_tier = parent;
        }
    }
    map.into_values().collect()
}
/// The directory's rows as the table the arming rule reads. The wire lists ACTIVE rows only and carries
/// a tier as a word; a word this build does not know states nothing.
fn account_table(rows: &[AccountDirectoryAccountWire]) -> AccountDirectory {
    let known = rows.iter().filter_map(|r| {
        Some(AccountRow {
            id: r.id,
            venue: r.venue.clone(),
            tier: r.tier.parse().ok()?,
            label: r.label.clone(),
            venue_account_id: None,
            parent_id: None,
            active: true,
            last_verified_at: None,
            max_exposure: None,
            leverage: None,
            symbol: None,
            facts: Default::default(),
        })
    });
    AccountDirectory::from_rows(AccountRows::Known(known.collect()), None)
}
/// What the rows of `row`'s account state, by the one rule that decides what it runs at, a venue-scoped
/// tier's conflict included.
fn stated_tier(
    table: &AccountDirectory,
    row: &AccountDirectoryAccountWire,
) -> Option<AccountRowActiveTier> {
    // The STATED tier: the wire carries the rows without their symbols, and this reads what they say.
    match vike_config::account_arming::stated_tier(
        Some(table),
        &row.venue,
        &account_label(row.label.as_deref()),
    ) {
        Ok(tier) => Some(AccountRowActiveTier::Tier(tier)),
        Err(ArmingBlock::TierConflict) => Some(AccountRowActiveTier::Conflict),
        Err(_) => None,
    }
}
pub(super) fn engine_checks(
    accounts: &[AccountView<'_>],
    rows_known: bool,
    down: &[VenueLinkDownWire],
    links: &[TradeLinkDownWire],
) -> Vec<Check> {
    // The directory READ and lists no active row: a node with nothing configured yet, which a fresh server is (the
    // owner, 2026-11-16: it starts EMPTY rather than refusing). Said, not failed; an unread directory still fails.
    if accounts.is_empty() && rows_known {
        return vec![Check::new(
            Status::Warn,
            "engine",
            "no account: the account table lists no active row, so this node runs nothing and no strategy \
             trades until an account is added and a daemon profile names it. A fresh server starts so; the node \
             restarts into its first daemon profile by itself",
        )];
    }
    if accounts.is_empty() {
        return vec![Check::new(
            Status::Fail,
            "engine",
            "no account is known: no engine block, no NOT RUNNING account and no active row",
        )];
    }
    accounts.iter().filter_map(|a| engine_check(a, rows_known, down, links)).collect()
}
/// An engine whose trade lease is closed for good, or whose lease has no live adapter behind it, does not read RUNNING:
/// it stands, no order of it reaches the venue, and nothing brings it back. The `gateway-link` row says why.
fn not_trading_check(
    a: &AccountView<'_>,
    down: &[VenueLinkDownWire],
    links: &[TradeLinkDownWire],
) -> Option<Check> {
    let block = a.running()?;
    let why = if links.iter().any(|l| l.closed && l.route_key == a.key) {
        "its trade lease is closed for good"
    } else if down.iter().any(|d| d.no_adapter && d.route_key == a.key) {
        "the gatewayhub has no live adapter for it"
    } else {
        return None;
    };
    let tier = block.tier.as_deref().unwrap_or("not stated");
    Some(Check::new(
        Status::Warn,
        "engine",
        format!("NOT TRADING ({tier}): {why}, see `gateway-link`"),
    ))
}
/// An engine that RUNS is not healthy while its venue path is down ([`CoreSnapshotWire::venue_link_down`]): the row is
/// never PASS then, and it says why and what to expect, whatever else it already said.
fn degrade_for_venue_path(
    mut check: Check,
    a: &AccountView<'_>,
    down: &[VenueLinkDownWire],
) -> Check {
    let Some(link) = down.iter().find(|l| l.route_key == a.key).filter(|_| a.running().is_some())
    else {
        return check;
    };
    if check.status == Status::Pass {
        check.status = Status::Warn;
    }
    check.detail = format!("{}; {}", check.detail, link.say());
    check
}
fn engine_check(
    a: &AccountView<'_>,
    rows_known: bool,
    down: &[VenueLinkDownWire],
    links: &[TradeLinkDownWire],
) -> Option<Check> {
    if let Some(check) = not_trading_check(a, down, links) {
        return Some(check.scope(&a.key));
    }
    let check = if let Some(n) = a.stopped {
        let tier = n.tier.as_deref().or(a.row_words()).unwrap_or("not stated");
        Check::new(Status::Warn, "engine", format!("NOT RUNNING ({tier}): {}", n.say()))
    } else if let Some(b) = a.block {
        match (b.tier.as_deref(), a.row_tier) {
            (None, _) => Check::new(
                Status::Warn,
                "engine",
                "RUNNING, tier not published (a node that predates the field): treat it as live",
            ),
            (Some(eff), Some(AccountRowActiveTier::Conflict)) => Check::new(
                Status::Warn,
                "engine",
                format!(
                    "RUNNING at tier {eff}, but its rows state {TWO_TIERS}: it keeps running as it \
                     is, and after a restart it does NOT RUN until one is deactivated. Ask: vike-cli \
                     account ls"
                ),
            ),
            (Some(eff), Some(AccountRowActiveTier::Tier(row))) if eff != row.as_str() => {
                Check::new(
                    Status::Warn,
                    "engine",
                    format!("RUNNING at tier {eff}, but its active row states {row}"),
                )
            }
            (Some(eff), Some(AccountRowActiveTier::Tier(_))) => Check::new(
                Status::Pass,
                "engine",
                format!("RUNNING at tier {eff} (the active row agrees)"),
            ),
            (Some(eff), _) if rows_known => Check::new(
                Status::Warn,
                "engine",
                format!(
                    "RUNNING at tier {eff}, but no active row states it (deleted, or just \
                     deactivated): it keeps running as it is"
                ),
            ),
            (Some(eff), _) => Check::new(Status::Pass, "engine", format!("RUNNING at tier {eff}")),
        }
    } else {
        // Only the directory names it: an active row with no engine and no reason.
        let row = a.row_words()?;
        Check::new(
            Status::Fail,
            "engine",
            format!("active row ({row}) but NO engine and NO NOT RUNNING reason on the wire"),
        )
    };
    Some(degrade_for_venue_path(check, a, down).scope(&a.key))
}
