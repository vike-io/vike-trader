//! **Which tier an account runs at, from the `account` table — the ONE reader both daemons share.**
//!
//! The tradehub's engine assembly (`vike-tradehub-core`) and the gatewayhub's arming table read the
//! same `account` rows, so the answer lives BELOW both: here, in the layer that already owns
//! [`ArmingBlock`] and sees `vike-secrets` (the rows) and `vike-venues` (`venue_tier_scope`). It used
//! to be `vike-tradehub-core`'s `arming.rs`, taking the whole `AdapterPolicy`; it takes the narrower
//! [`AccountDirectory`] that `AdapterPolicy::accounts` already is, so the move drags no policy type
//! down with it. `None` is "no table was read" and answers the same as an UNREAD directory.
//!
//! What is here: [`account_tier`] (the answer), [`venue_tier`] (the same, for a venue's default
//! account), [`known_accounts`] (which accounts a venue has rows for), [`account_ids`] (the rows
//! behind an answer, for a remedy that names `--id`) and [`PAPER_RUNS`] (what a `paper` tier answers
//! in a projection). What is NOT here: the bridge's own `resolve` (whether a credential set can
//! arm), the interlock, the exclusive claim — all of them judge a tier this module only SELECTS.
//!
//! No setting, no environment variable and no I/O: a pure function of rows already read.

use vike_model::accounts::account_keys::AccountLabel;
use vike_secrets::account_directory::AccountDirectory;

use crate::ArmingBlock;

/// What a `paper` tier answers in a projection: the paper simulator runs it.
pub const PAPER_RUNS: (Option<vike_model::AccountTier>, ArmingBlock) =
    (Some(vike_model::AccountTier::Paper), ArmingBlock::PaperTier);

/// The tier of one venue's DEFAULT account — [`account_tier`] at [`AccountLabel::Default`], for the
/// callers that ask about a venue rather than an account (the startup preflight's legs). `None`
/// when no active row states one: that account does not run, so no probe asks anything of it.
#[must_use]
pub fn venue_tier(
    accounts: Option<&AccountDirectory>,
    venue: &str,
) -> Option<vike_model::AccountTier> {
    account_tier(accounts, venue, &AccountLabel::Default).ok()
}

/// **The tier one ACCOUNT of one venue runs at, from the `account` table** — **the one place
/// `Option<&AccountDirectory>` becomes a [`vike_model::AccountTier`]**, so no caller invents its own
/// answer for a policy-less start. `Ok(tier)` when the active rows state exactly one tier (`paper`
/// included: that account runs the paper simulator), else `Err(why)` — the account does NOT RUN
/// (decision 0156), and nothing below reads a credential for it.
///
/// Over the rows of `venue` whose `label` equals the account's label text (`NULL` for the DEFAULT
/// account), `vike_secrets::Accounts::active_tier` answers:
///
/// * no directory, or a table nobody read (an UNREAD directory, or a box with no store):
///   `NoAccountRow`. NOT RUNNING is the fail-safe end: the widening mistake has to be typed.
/// * a store that exists and would not open: `AccountNotInStore` (loud at the root).
/// * no row: `NoAccountRow`; rows, none active: `AccountInactive`; every active row `paper`:
///   `Ok(Paper)`.
/// * exactly ONE non-paper tier among the active rows (other active rows may be `paper`, and two
///   rows of the SAME tier are fine — dukascopy's two demo books): that tier.
/// * TWO non-paper tiers active (`demo` AND `live` for one account): `TierConflict`. No pick: the
///   engine route key and the live lock carry no tier, so one process starts one engine per
///   account, and choosing between a demo and a mainnet key set is the operator's to make
///   (`vike-cli secrets account deactivate --id N` on one of them).
/// * ⚠ **A VENUE-scoped tier** (`vike_venues::venue_tier_scope` answers `TierScope::Venue`: ibkr,
///   decision 0140) adds one rule: an account that would arm by the rows of its own label does not
///   run, `TierConflict`, when the venue's active rows of ANY label state two non-paper tiers
///   (`vike_secrets::Accounts::active_tier_of_venue`). The box rides ONE session of that venue, so
///   an unlabelled `demo` row beside a labelled `live` one arms neither. An account whose own rows
///   already refuse it keeps that cause, and a `paper` row still runs the simulator (it opens no
///   session). Every other venue is 0119's per-label rule alone.
///
/// ⚠ **It only SELECTS; the bridge still decides whether it can arm.** The tier reaches the bridge
/// AS IS, as `AdapterInputs::tier` (decision 0129) — the start (`vike-tradehub-core`'s
/// `make_engine_for_account`), the projection (its `contract_arming`), the clock read and the
/// startup credential probe all hand over this answer, and none recomputes a `tier == live` bit of
/// its own (an open-coded copy of that bit once sent a `demo` binance a SIGNED MAINNET balance read
/// at startup while the start bound demo). For binance, bybit, okx and hyperliquid it is the whole
/// of what keeps a `demo` account off MAINNET. A `live` account whose bridge can only bind DEMO
/// does not run (`resolution_to_arming`, the no-downgrade rule).
pub fn account_tier(
    accounts: Option<&AccountDirectory>,
    venue: &str,
    label: &AccountLabel,
) -> Result<vike_model::AccountTier, ArmingBlock> {
    use vike_model::AccountTier as Mode;
    use vike_secrets::ActiveTier;

    let Some(directory) = accounts else { return Err(ArmingBlock::NoAccountRow) };
    let rows = match directory.rows() {
        None => return Err(ArmingBlock::NoAccountRow),
        Some(Err(_)) => return Err(ArmingBlock::AccountNotInStore),
        Some(Ok(rows)) => rows,
    };
    let tier = match rows.active_tier(venue, label.text()) {
        // `Unanswerable`: a box with no `account` table has no row to state a tier.
        None | Some(ActiveTier::NoRow) => return Err(ArmingBlock::NoAccountRow),
        Some(ActiveTier::Inactive) => return Err(ArmingBlock::AccountInactive),
        Some(ActiveTier::Conflict) => return Err(ArmingBlock::TierConflict),
        Some(ActiveTier::Tier(tier)) => tier,
    };
    // A VENUE-scoped tier (decision 0140): an account that would arm does not run when the venue's
    // rows of ANY label state two tiers. A `paper` row runs the simulator whatever the others say.
    if tier != Mode::Paper && venue_conflicts(rows, venue) {
        return Err(ArmingBlock::TierConflict);
    }
    Ok(tier)
}

/// Whether `venue`'s tier is judged over EVERY label (`vike_venues::TierScope::Venue`) and its
/// active rows, of any label, state two non-paper tiers
/// (`vike_secrets::Accounts::active_tier_of_venue`).
/// Always `false` for an account-scoped venue: there 0119's per-label rule is the whole rule.
fn venue_conflicts(rows: &vike_secrets::Accounts, venue: &str) -> bool {
    vike_venues::venue_tier_scope(venue) == vike_venues::TierScope::Venue
        && rows.active_tier_of_venue(venue) == Some(vike_secrets::ActiveTier::Conflict)
}

/// **The ACTIVE `account.id`s of `(venue, label)` behind an arming answer** — what
/// [`crate::VenueArming::account_ids`] carries, so a remedy can name the `--id` to change.
///
/// * a stated tier (`demo`, `live`, or every active row `paper`): the active rows of that tier;
/// * `TierConflict`: the active NON-paper rows — the ones to choose between; for a VENUE-scoped
///   tier (`vike_venues::TierScope::Venue`, decision 0140) those of EVERY label of the venue;
/// * anything else (no row, none active, no table): empty.
///
/// Keyed on the TABLE's answer ([`account_tier`]), not the bridge's: a `NoCredentials` or
/// `NoAccountSupport` row still names the row whose keys or venue fell short.
#[must_use]
pub fn account_ids(
    accounts: Option<&AccountDirectory>,
    venue: &str,
    label: &AccountLabel,
) -> Vec<i64> {
    use vike_model::AccountTier as Mode;
    let answer = account_tier(accounts, venue, label);
    let Some(Ok(rows)) = directory_or_unread(accounts).rows() else { return Vec::new() };
    let Some(rows) = rows.known() else { return Vec::new() };
    let wanted = |row_tier: Mode| match answer {
        Ok(tier) => row_tier == tier,
        Err(ArmingBlock::TierConflict) => row_tier != Mode::Paper,
        Err(_) => false,
    };
    // A venue-scoped conflict is a fact about the rows of EVERY label: its remedy names them all.
    let any_label = answer == Err(ArmingBlock::TierConflict)
        && vike_venues::venue_tier_scope(venue) == vike_venues::TierScope::Venue;
    rows.iter()
        .filter(|a| a.active && a.venue == venue)
        .filter(|a| any_label || a.label.as_deref() == label.text())
        .filter(|a| wanted(a.tier))
        .map(|a| a.id)
        .collect()
}

/// **Every account of `venue` this box knows about**: the default account when it has a row (an
/// unlabelled `account` row of the venue, active or not) or when nothing else would be listed (an
/// empty store, an unread table, a venue no row names — so a box with an empty store still SAYS why
/// its default account does not run), FIRST; then the labels in order: the `label`s of this venue's
/// `account` ROWS, active or not — an inactive labelled row must be listed (as
/// `AccountInactive`), and a label with no keys too, or a typo would vanish instead of showing as
/// `NoCredentials`.
///
/// ⚠ **A venue whose accounts are all labelled lists no default account** (decision 0172): a row
/// for an account nobody created read as "1 not running" beside the accounts that exist. That
/// account is still NOT RUNNING (`account_tier` answers `NoAccountRow`), it is just not a row.
///
/// ⚠ **The account ROWS are the one source**: a credential belongs to its account row
/// (`credential.account_id`, decision 0156 item 6), so no account is read out of a credential's
/// NAME.
#[must_use]
pub fn known_accounts(venue: &str, accounts: Option<&AccountDirectory>) -> Vec<AccountLabel> {
    let mut labels: Vec<AccountLabel> = Vec::new();
    let mut default_has_a_row = false;
    if let Some(Ok(rows)) = directory_or_unread(accounts).rows()
        && let Some(rows) = rows.known()
    {
        for row in rows.iter().filter(|a| a.venue == venue) {
            match row.label.as_deref() {
                None => default_has_a_row = true,
                // A label the label grammar refuses names no addressable account: skipped.
                Some(text) => labels.extend(AccountLabel::parse(text).ok()),
            }
        }
    }
    labels.retain(|l| !l.is_default());
    labels.sort();
    labels.dedup();
    // Default FIRST (`make_engine_accounts`'s order contract: callers bind `[0]`), and never an
    // EMPTY answer: callers fan out over it, and an empty one reads as a venue the roster lacks.
    let mut out = Vec::with_capacity(labels.len() + 1);
    if default_has_a_row || labels.is_empty() {
        out.push(AccountLabel::Default);
    }
    out.extend(labels);
    out
}

/// **The `account` table a caller carries**, or the UNREAD one with none — the ONE spelling of that
/// fallback. A `match`, not `map_or_else(AccountDirectory::unread_ref, …)`, which unifies the arms
/// at `'static` and fails to borrow-check.
fn directory_or_unread(accounts: Option<&AccountDirectory>) -> &AccountDirectory {
    match accounts {
        Some(directory) => directory,
        None => AccountDirectory::unread_ref(),
    }
}

#[path = "account_arming_tests.rs"]
#[cfg(test)]
mod account_arming_tests;
