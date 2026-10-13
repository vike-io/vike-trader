//! The machine policies of the mount-wiring binary `risk_profile_wiring.rs`, which
//! `#[path]`-includes it (a bare `mod` would resolve beside the binary root, not here).

use vike_model::accounts::account_keys::AccountLabel;

/// A machine policy whose `account` table holds ONE active `demo` row for `venue`'s DEFAULT
/// account, every other field at its default. With an EMPTY credential map that account does NOT
/// RUN (decision 0156), so this is the policy of the scenarios that need LIVE INTENT — demo keys in
/// the map — and stop at the pre-connect budget refusal before anything dials. `venue` is each
/// caller's own `VENUE` constant.
pub fn armed(venue: &str) -> vike_mount::MountPolicy {
    vike_mount::MountPolicy::default().with_account(
        venue,
        &AccountLabel::Default,
        vike_model::AccountTier::Demo,
    )
}

/// A machine policy whose `account` table holds ONE active `paper` row for `venue`'s DEFAULT
/// account: the account runs the paper simulator (`vike_mount`'s `paper_engine`), offline, with
/// the operator budget, the universal defaults and the exposure ceilings folded exactly as the
/// shared tail folds them for a live outcome. Since decision 0156 this is the one network-free way
/// to mount a real venue id; the shared tail's own fold is pinned over planted live outcomes in
/// `crates/vike-mount/src/contract_tests/`.
pub fn paper_account(venue: &str) -> vike_mount::MountPolicy {
    vike_mount::MountPolicy::default().with_account(
        venue,
        &AccountLabel::Default,
        vike_model::AccountTier::Paper,
    )
}
