//! What the moved tests had from `vike-mount`'s own `#[cfg(test)]` helpers, which no other crate
//! can reach, plus the one-line map builder several of them repeated.
#![allow(dead_code)]

use std::collections::HashMap;

use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;
use vike_mount::MountPolicy;

/// A policy whose `account` table holds ONE active row for `venue`'s DEFAULT account at `mode` —
/// the tier most of these tests must state, or the mount returns at its paper early return and
/// the gate under test is never reached. It holds no credentials: a test that arms the account
/// plants them on the row (`crate::account_book::keyed_policy`).
pub fn armed_policy(venue: &str, mode: AccountTier) -> MountPolicy {
    MountPolicy::default().with_account(venue, &AccountLabel::Default, mode)
}

/// A policy holding one active DEFAULT-account row at `mode` for EVERY roster venue — for the
/// tests whose subject is the OTHER gate (absent credentials), which a paper tier would otherwise
/// short-circuit.
pub fn all_armed_policy(mode: AccountTier) -> MountPolicy {
    vike_model::VENUES
        .iter()
        .fold(MountPolicy::default(), |p, v| p.with_account(v, &AccountLabel::Default, mode))
}

/// A DEPLOYMENT-name map from `(name, value)` pairs — the folded flags (`POLY_EXEC`) a mount reads
/// beside the account's credentials. No account credential rides here (decision 0156 item 6).
pub fn vars(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect()
}
