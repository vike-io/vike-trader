//! **What the venue half reads of the operator's settings: the `account` table, the `venue_setting`
//! rows and the one stop the startup preflight can lay on a venue** (wave 2, W9b).
//!
//! In `vike-tradehub-core` the venue half read these three fields off `AdapterPolicy`, a projection
//! of `vike_config::Policy` that also carries the ceilings and the paper knobs the fold applies. The
//! gatewayhub applies none of those, owns no `Policy`, and may not name that crate (`GATEWAYHUB_FENCE`),
//! so the three fields travel as their own value. The field names are the old ones, so a body moved
//! from the policy-reading form reads the same.
//!
//! The gatewayhub READS these itself, from the settings database it owns the credentials of
//! (`crate::gatewayhub_cli`); they never cross the wire (decision 0164 verdict 6: no credential and no
//! setting value travels toward a client).

use std::collections::{BTreeMap, BTreeSet};

use vike_secrets::account_directory::AccountDirectory;
use vike_secrets::venue_setting::VenueSettings;

/// See the module doc. Not `Copy`: both maps own allocations. Call sites pass
/// `Option<&TradeSnapshot>`, `None` being "no account table at all" (no account runs).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TradeSnapshot {
    /// **The settings database's `account` table, with each account's credentials by field.**
    /// [`AccountDirectory::unread`] (the default) runs NO account: no row states a tier (decision
    /// 0156).
    pub accounts: AccountDirectory,
    /// **Every venue's `venue_setting` rows.** Each bridge receives its venue's view as
    /// `AdapterInputs::settings`, empty for a venue with no rows.
    pub venue_settings: BTreeMap<String, VenueSettings>,
    /// **Why the `venue_setting` rows could not be read**, when they could not (`None` = they were read,
    /// or there was nothing to read). [`Self::venue_settings`] is EMPTY then, and an empty map is not
    /// "no rows": a start would run on each bridge's built-in defaults (ibkr's gateway host and port, the
    /// account-wide market list, the order-path opt-ins), none of which stands in for the operator's
    /// rows. So a start on a snapshot that carries this is refused with the reason (`BridgeTrade::start`),
    /// the way an unreadable `account` table is ([`AccountDirectory`] carries its own error).
    pub venue_settings_unread: Option<String>,
    /// **The venues the startup preflight hard-FAILED this session**: a STOP for every `demo`/`live`
    /// account of each (`ArmingBlock::PreflightFailed`). The lane's table is ALWAYS computed with this
    /// empty (the stop is the tradehub's overlay over the table); only a start the tradehub asked to
    /// withhold (`Withhold::PreflightFailed`) names the venue here.
    pub preflight_failed: BTreeSet<String>,
}

/// **The `account` table a caller's snapshot carries**, or the UNREAD one with no snapshot — the ONE
/// spelling of that fallback. A `match`, not `map_or_else(AccountDirectory::unread_ref, …)`, which
/// unifies the arms at `'static` and fails to borrow-check.
pub(crate) fn directory_of(snapshot: Option<&TradeSnapshot>) -> &AccountDirectory {
    match snapshot {
        Some(s) => &s.accounts,
        None => AccountDirectory::unread_ref(),
    }
}
