//! The helpers more than one `key_shapes` member uses -- one copy each, merged byte-identical.

use std::collections::HashMap;

use vike_connections::VenueCredStatus;
use vike_model::accounts::account_keys::AccountLabel;

/// The four venues whose bridges do not read the generic `{VENUE}_{TIER}_API_*` grid, and whose arms
/// were therefore repaired. Every OTHER roster venue must answer exactly what each member's frozen
/// `baseline` answers: that is the whole content of `read_shapes`' `untouched_venues_are_unchanged`
/// and of `write_shapes`' `untouched_venues_compose_the_same_keys`.
pub(super) const REPAIRED: &[&str] = &["alpaca", "ctrader", "ibkr", "polymarket"];

pub(super) fn vars_of(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect()
}

/// One venue's three columns, in `Sim`/`Demo`/`Live` order — the shape every expectation in the
/// `key_shapes` members is written in, so a wrong column cannot be read as the right one.
pub(super) fn tiers(s: &VenueCredStatus) -> (bool, bool, bool) {
    (s.sim, s.demo, s.live)
}

/// A label the grammar accepts, built through the ONE validator so this fixture cannot pin a
/// spelling `vike_model::accounts::account_keys` would refuse.
pub(super) fn alt() -> AccountLabel {
    AccountLabel::parse("ALT").expect("a legal label")
}
