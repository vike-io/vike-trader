//! Per-venue arming decisions: what `assemble_runtime` actually started (`CexArming`), which network a
//! venue's accounts start on, and the `data_only` credential-withholding rule — and, at the
//! bottom, the two dead-man switches the LIVE start arms (the silence switch and the per-venue
//! link switch), which are arming decisions of the same kind: pure folds from `policy`, the venue
//! table and the started plan onto what the core is told to arm. Those moved here from
//! `crate::tradehub_cli` with the `run` phase split, bodies unchanged.
//!
//! Layout: `venue_arming/tier.rs` holds the network rule (`exec_tier`, `selected_tier`,
//! `cex_mainnet_enabled`, `feed_tier_from_rows`, `feed_tier`); `venue_arming/arming.rs` the exec
//! badge and the per-venue `*_arming` disclosures; `venue_arming/deadman.rs` the dead-man folds
//! (`deadman_config_from_policy`, `adapter_link_disclosure`, `link_deadman_config_from_policy`,
//! `link_deadman_arming_report`, `deadman_absent_warning`). `warn_deadman_absent` stays in this
//! file: its tracing target is this module's path, `vike_tradehub::venue_arming`, which
//! `crates/vike-tradehub/tests/deadman_absent_warning.rs` filters on exactly. The `data_only`
//! rule, `withhold_data_only_credentials`, is in this file too.

// In-crate callers name the canonical child path (`crate::venue_arming::arming::cex_arming`); the
// root re-exports ONLY what `tests/` (or another crate) names as `vike_tradehub::venue_arming::X`.
pub(crate) mod arming;
pub(crate) mod deadman;
pub(crate) mod tier;

pub use deadman::deadman_absent_warning;
pub use tier::{FeedChoice, cex_mainnet_enabled, feed_tier_from_rows, selected_tier};

/// The `Once` latch over [`deadman_absent_warning`]: `tracing::warn!` the message, at most once
/// per process, and only when there is one (an absent key). Same idiom, and the same reason, as
/// `crates/vike-tradehub-core/src/arming.rs`'s `report_unaddressable_accounts` — the fact is
/// process-wide, and a paste-ready block repeated is a paste-ready block buried. Called from
/// `live_runtime_with` beside the `deadman:` construction, and from nowhere else: the composition
/// root is where the policy is known to be a LIVE start's, and the gate-off `start_paper` arm
/// neither arms the switch nor warns about it.
///
/// ⚠ The latch sits INSIDE the `None` test, not around it, so a call that had nothing to say does
/// not consume the one chance to say it — the test that drives this counts events across a
/// `Some(n)`, a `Some(0)` and two `None`s in that order and expects exactly one.
pub fn warn_deadman_absent(policy: &vike_config::Policy) {
    static ONCE: std::sync::Once = std::sync::Once::new();
    if let Some(message) = deadman_absent_warning(policy) {
        ONCE.call_once(|| tracing::warn!("{message}"));
    }
}

/// **The `data_only` declaration's withhold**: `accounts` with every credential of `venue` taken
/// out — each of its accounts' fields and the venue's own application fields (cTrader's OAuth
/// client) — and how many fields were withheld. Field NAMES are counted; no value is read for any
/// other purpose, and none is printed.
///
/// A credential belongs to its ACCOUNT (decision 0156 item 6): the exec start reads an account's
/// fields out of `vike_tradehub_core::AdapterPolicy::accounts` (`AccountDirectory::credentials_for`), never
/// out of the deployment map, so a declared venue's credentials are withheld THERE. `live_runtime_with`
/// (`crates/vike-tradehub/src/tradehub_cli/live_runtime.rs`) calls it AFTER every feed plan resolved
/// — each credentialed-data plan CARRIES its config, so the FEED keeps the credentials exec loses —
/// and BEFORE the armed-live probe, the arming rows and `assemble_runtime` read the policy. The workspace
/// live gate (absent credentials ⇒ paper) then keeps the venue's exec on the paper book, by
/// declaration, through the ordinary path rather than a parallel switch. The ROWS are untouched:
/// the account keeps its tier and holds nothing.
///
/// A directory whose rows or credentials were never read, or would not open, holds nothing a start
/// could resolve, and is returned as it is with a count of zero.
pub fn withhold_data_only_credentials(
    accounts: &vike_secrets::account_directory::AccountDirectory,
    venue: &str,
) -> (vike_secrets::account_directory::AccountDirectory, usize) {
    accounts.without_venue_credentials(venue)
}

/// **How many credential key NAMES the `account` rows of `venue` own** — the count the `data_only`
/// disclosure prints as `keys_withheld`, taken from NAMES and never from a value.
///
/// This daemon holds no venue credential value (decision 0146's second reopener: its credential
/// handle is `NoVenueKeys`, and the book it hands the account directory is empty), so
/// [`withhold_data_only_credentials`] counts nothing here in production — the book it filters is
/// empty — and the disclosure's number would read `0` for a venue that does hold keys. The key
/// NAMES (`AccountDirectory::keys`, which `NoVenueKeys` leaves readable: a name is not a value)
/// carry the same count: one live `credential` row per `(account, field)`, so one name is one field.
///
/// ⚠ **One term of the old count is not here**: a venue's APPLICATION fields (cTrader's OAuth client,
/// filed against the venue and no account) have no `account_keys` entry, so a `data_only` cTrader
/// reads one or two fewer than the book-based count did. The line is a disclosure of the
/// declaration, not an arming input.
///
/// A directory whose rows or key names were never read, or would not open, owns none: `0`.
pub fn held_key_names(
    accounts: &vike_secrets::account_directory::AccountDirectory,
    venue: &str,
) -> usize {
    let Some(Ok(Some(keys))) = accounts.keys() else { return 0 };
    let Some(rows) = accounts.rows().and_then(Result::ok).and_then(vike_secrets::Accounts::known)
    else {
        return 0;
    };
    rows.iter()
        .filter(|r| r.venue == venue)
        .filter_map(|r| keys.get(&r.id))
        .map(|k| k.names.len())
        .sum()
}
