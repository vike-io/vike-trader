//! The Trade window's accounts: venue and account naming, the tier words, the seed, the rows.

use std::collections::HashMap;

use super::reasons::{NOT_LISTED_WHY, NOT_RUNNING_WHY, not_running_because};
use super::window::WindowSources;
use crate::orders::order_dispatch::TradeAddress;
use vike_exec::{CoreSnapshot, VenueBlock};
use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;
use vike_panels::trade::{AccountRow, StatusKind};
use vike_tradehub_client::wire::{WireDirectory, WireDirectoryAccount};

/// What an unlabelled account is called while its venue holds only one.
const MAIN: &str = "main";

/// A new instrument for a window: the trader's pick, or the window's first-frame seed.
#[derive(Debug, Clone, PartialEq)]
pub struct TradePick {
    pub venue: String,
    /// The account label, `None` for the venue's default account.
    pub account: Option<String>,
    pub symbol: String,
}

/// A venue's name as the settings database spells it (`venue.title`, the owner's ruling of
/// 2026-09-30), else its key. `dir` is `None` before the node's first `Directory` reply and against
/// a node that predates the verb; the key is honest then, and nothing here invents a spelling.
pub fn venue_label<'a>(dir: Option<&'a WireDirectory>, venue: &'a str) -> &'a str {
    dir.and_then(|d| d.venues.iter().find(|v| v.name == venue))
        .and_then(|v| v.title.as_deref())
        .unwrap_or(venue)
}

/// What a picker row shows for an account: its label; else, when `unlabelled_at_venue` accounts of
/// its venue carry no label, the broker's own number (`venue_account_id`) so they can be told
/// apart; else "main".
pub(super) fn account_name(a: &WireDirectoryAccount, unlabelled_at_venue: usize) -> &str {
    match (&a.label, &a.venue_account_id) {
        (Some(label), _) => label,
        (None, Some(book)) if unlabelled_at_venue > 1 => book,
        _ => MAIN,
    }
}

/// Which running engine (an index into `blocks`) each database account runs, by account id; an
/// account the server does not run has no entry. An engine is found by the account's ROUTE KEY, the
/// routing answer (`VenueBlock::route_key`). When two accounts share that key (a venue's demo and
/// live books, both unlabelled — "twins": one venue, one label), the engine belongs to the one whose
/// tier its block names; if neither does (a node that names no tier), to the one with the lower id,
/// so the answer is stable.
///
/// Computed ONCE per frame for the whole directory (M-5 of the final review, slice B): asked per
/// account, the twin scan made the picker's rows quadratic in a list meant to reach fifty accounts
/// per venue — every Trade window, every frame, picker open or not.
pub(super) fn engines_run(dir: &WireDirectory, blocks: &[VenueBlock]) -> HashMap<i64, usize> {
    let mut twins: HashMap<(&str, Option<&str>), Vec<&WireDirectoryAccount>> = HashMap::new();
    for a in &dir.accounts {
        twins.entry((a.venue.as_str(), a.label.as_deref())).or_default().push(a);
    }
    let mut engines = HashMap::new();
    for ((venue, label), mut group) in twins {
        let Some(key) = TradeAddress::parse(venue, label, "").map(|a| a.route_key()) else {
            continue;
        };
        let Some(at) = blocks.iter().position(|b| b.route_key == key) else { continue };
        group.sort_by_key(|o| o.id);
        // The engine's tier in its own word (`AccountTier::as_str`); an engine that does not say
        // reads as `paper`.
        let word = blocks[at].tier.map_or(AccountTier::Paper.as_str(), AccountTier::as_str);
        let owner = group.iter().find(|o| o.tier == word).unwrap_or(&group[0]);
        engines.insert(owner.id, at);
    }
    engines
}

/// Why a listed account cannot be picked: `None` when the server runs it. A row the node names as
/// NOT RUNNING says the node's reason (`said`, decision 0156); any other unrun row says
/// [`NOT_RUNNING_WHY`].
pub(super) fn why_not(lists: bool, running: bool, said: Option<&str>) -> Option<&str> {
    if lists { (!running).then_some(said.unwrap_or(NOT_RUNNING_WHY)) } else { Some(NOT_LISTED_WHY) }
}

/// An account the node says it does NOT run, and why (decision 0156: an account runs at the tier
/// its row states, or not at all): one entry of `CoreSnapshot::not_running`, with the sentence the
/// window says for it ([`not_running_because`]). Built once per frame ([`not_running_rows`]),
/// because a picker row and the strip BORROW the sentence.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct NotRunningRow {
    /// The routing key the account's orders would carry (`TradeAddress::route_key`).
    pub(crate) route_key: String,
    /// The tier its account row states; `None` when the node names none (no row).
    pub(crate) tier: Option<AccountTier>,
    /// The sentence: [`NOT_RUNNING_WHY`]'s words, then the node's reason.
    pub(crate) why: String,
}

/// [`NotRunningRow`]s for every account the snapshot names as not running.
pub(crate) fn not_running_rows(snap: &CoreSnapshot) -> Vec<NotRunningRow> {
    snap.not_running
        .iter()
        .map(|n| NotRunningRow {
            route_key: n.route_key.clone(),
            tier: n.tier,
            why: not_running_because(&n.reason),
        })
        .collect()
}

/// The not-running entry for a directory account: its route key, and — where the node names the
/// row's tier — that tier, so of two unlabelled accounts sharing a route key (a venue's demo and
/// live books) each finds its own.
fn said_for<'a>(
    not_running: &'a [NotRunningRow],
    a: &WireDirectoryAccount,
) -> Option<&'a NotRunningRow> {
    let key = TradeAddress::parse(&a.venue, a.label.as_deref(), "")?.route_key();
    not_running.iter().find(|n| n.route_key == key && n.tier.is_none_or(|t| t.as_str() == a.tier))
}

/// The tier a database tier word reads as, for a row the server does not run. The wire carries the
/// word (decision 0091 §3); THIS is the edge where it becomes a tier, through the one parser
/// (`vike_model::AccountTier`, decision 0129). A word it refuses is `None` (drawn `MODE ?`), never
/// PAPER. (It was `mode_of_tier`, beside a `mode_of` that mapped an engine's own mode enum; with the
/// window's tier the one tier type, decision 0156, a running block's tier needs no mapping at all.)
fn tier_of(word: &str) -> Option<AccountTier> {
    word.parse().ok()
}

/// How the status strip reads an order's status word (`vike_model::OrderStatus::as_str`).
pub(crate) fn status_of(status: &str) -> StatusKind {
    match status {
        "FILLED" => StatusKind::Ok,
        "REJECTED" | "DENIED" | "EXPIRED" | "LIQUIDATED" => StatusKind::Error,
        _ => StatusKind::Info,
    }
}

/// A new window's instrument: the most recently used window's, else the backend's primary market
/// on the PRIMARY engine's own account (spec §3.11; C1). `None` while there is nothing honest to
/// open: before the node has named a primary market, while it publishes no primary BLOCK (a core
/// that faulted while publishing, the observer's placeholder), and for a primary block whose route
/// key names no account.
///
/// ⚠ The account is the primary block's, read from its route key. A node whose primary engine is a
/// labelled account runs NO default account on that venue, so a window on "default" there would
/// name a book the server does not run (`order_dispatch::tradable` refuses it). Without the block
/// the account is unknown, and a seed that guessed "default" would stay there after the real,
/// perhaps labelled, primary appeared.
pub(crate) fn seed(snap: &CoreSnapshot, last: Option<&TradePick>) -> Option<TradePick> {
    if let Some(last) = last {
        return Some(last.clone());
    }
    if snap.venue.is_empty() || snap.symbol.is_empty() {
        return None;
    }
    let primary = snap.portfolio.venues.first().filter(|b| b.venue == snap.venue)?;
    let label =
        vike_model::accounts::account_keys::label_of_route_key(&primary.venue, &primary.route_key)?;
    Some(TradePick {
        venue: snap.venue.clone(),
        account: label.text().map(str::to_string),
        symbol: snap.symbol.clone(),
    })
}

/// What the picker calls a lane twin's row, one per venue of `vike_secrets::LANE_TWINS`
/// (`every_lane_twin_has_a_picker_name` holds the pair). The twin is derived in memory over the default
/// account's row, so no directory row names it: the picker lists its engine as an unclaimed block, and
/// this is where the trader learns that `PERP` is the perpetual lane of the account above it.
pub(super) const LANE_TWIN_NAMES: &[(&str, &str)] = &[("binance", "PERP (perp lane)")];

/// [`LANE_TWIN_NAMES`]' name for `engine`, when it is its venue's lane twin: the table's label AND mount
/// symbol, the door's own test (an operator's account spelling the label elsewhere is not it).
fn lane_twin_name(engine: &VenueBlock, label: Option<&str>) -> Option<&'static str> {
    let twin = vike_secrets::lane_twin(&engine.venue)?;
    if label != Some(twin.label) || engine.symbol != twin.symbol {
        return None;
    }
    LANE_TWIN_NAMES.iter().find(|(venue, _)| *venue == engine.venue).map(|(_, name)| *name)
}

/// The venue · account picker's rows (spec §3.3): the settings database's accounts on every venue
/// that lists the instrument, running or not, plus any engine the server runs that no database row
/// claims; against a node with no `Directory` the snapshot's running accounts alone, by their keys.
/// The default account first within each venue, venues in the order they first appear. Linear in
/// the directory: every per-venue answer is counted once per frame ([`engines_run`], M-5).
pub(super) fn account_rows<'a>(src: &WindowSources<'a>) -> Vec<AccountRow<'a>> {
    let blocks = &src.snap.portfolio.venues;
    let mut rows: Vec<AccountRow<'a>> = Vec::new();
    let mut claimed = vec![false; blocks.len()];
    if let Some(dir) = src.directory {
        let engines = engines_run(dir, blocks);
        // Each venue's unlabelled accounts, counted once: per row it would be quadratic in a list
        // that is meant to reach fifty accounts per venue.
        let mut unlabelled_at: Vec<(&str, usize)> = Vec::new();
        for a in dir.accounts.iter().filter(|a| a.label.is_none()) {
            match unlabelled_at.iter().position(|(v, _)| *v == a.venue) {
                Some(k) => unlabelled_at[k].1 += 1,
                None => unlabelled_at.push((&a.venue, 1)),
            }
        }
        for a in &dir.accounts {
            let at = engines.get(&a.id).copied();
            if let Some(i) = at {
                claimed[i] = true;
            }
            let opens = src.opens(&a.venue);
            let unlabelled =
                unlabelled_at.iter().find(|(v, _)| *v == a.venue).map_or(0, |(_, n)| *n);
            rows.push(AccountRow {
                venue: &a.venue,
                venue_label: venue_label(Some(dir), &a.venue),
                product: if opens.is_some() { src.product(&a.venue) } else { "" },
                account: a.label.as_deref(),
                symbol: opens.unwrap_or(""),
                name: account_name(a, unlabelled),
                mode: at.map_or_else(|| tier_of(&a.tier), |i| blocks[i].tier),
                why_not: why_not(
                    opens.is_some(),
                    at.is_some(),
                    said_for(src.not_running, a).map(|n| n.why.as_str()),
                ),
            });
        }
    }
    for (b, _) in blocks.iter().zip(&claimed).filter(|(_, c)| !**c) {
        let opens = src.opens(&b.venue);
        // A row's account is a ROUTING answer (picking the row addresses orders to it), so it is
        // read from the route key, as `seed` reads it. A block whose label did not survive the wire
        // (the observe bridge drops a malformed one and keeps the route key) offers no row: shown
        // as "main" it would address the default account. The label text is the block's own, kept
        // only when it agrees with the route key.
        let Some(routed) =
            vike_model::accounts::account_keys::label_of_route_key(&b.venue, &b.route_key)
        else {
            continue;
        };
        let label = b.account.as_ref().and_then(AccountLabel::text);
        if routed.text() != label {
            continue;
        }
        rows.push(AccountRow {
            venue: &b.venue,
            venue_label: venue_label(src.directory, &b.venue),
            product: if opens.is_some() { src.product(&b.venue) } else { "" },
            account: label,
            symbol: opens.unwrap_or(""),
            name: lane_twin_name(b, label).or(label).unwrap_or(MAIN),
            mode: b.tier,
            why_not: opens.is_none().then_some(NOT_LISTED_WHY),
        });
    }
    let mut venues: Vec<&str> = Vec::new();
    for r in &rows {
        if !venues.contains(&r.venue) {
            venues.push(r.venue);
        }
    }
    rows.sort_by_key(|r| (venues.iter().position(|v| *v == r.venue), r.account.is_some()));
    rows
}
