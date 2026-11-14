//! `offline` -- vike-ig's plain offline suites as ONE test binary (`tests/offline/<name>.rs`).

// `#[path]` because this file is a test-target CRATE ROOT: a bare `mod reconcile_client_parse;` would
// resolve against `tests/` (the root's own directory), not `tests/offline/`.
#[path = "offline/account_settles.rs"]
mod account_settles;
#[path = "offline/activity_close_matches_its_fill.rs"]
mod activity_close_matches_its_fill;
#[path = "offline/amend_settles.rs"]
mod amend_settles;
#[path = "offline/any_symbol_facts.rs"]
mod any_symbol_facts;
#[path = "offline/any_symbol_quoted.rs"]
mod any_symbol_quoted;
#[path = "offline/captured.rs"]
mod captured;
#[path = "offline/catalog_search.rs"]
mod catalog_search;
#[path = "offline/close_does_not_collide_with_its_open.rs"]
mod close_does_not_collide_with_its_open;
#[path = "offline/close_limit_never_rests.rs"]
mod close_limit_never_rests;
#[path = "offline/confirm_is_asked_again.rs"]
mod confirm_is_asked_again;
#[path = "offline/confirm_never_terminalizes.rs"]
mod confirm_never_terminalizes;
#[path = "offline/dated_epics.rs"]
mod dated_epics;
// The scripted loopback IG gateway the facts and reconcile suites dial (`127.0.0.1`, never IG).
#[path = "offline/fake_ig.rs"]
mod fake_ig;
#[path = "offline/fills_are_honest.rs"]
mod fills_are_honest;
#[path = "offline/id_less_fills_are_refused.rs"]
mod id_less_fills_are_refused;
#[path = "offline/mount_value.rs"]
mod mount_value;
#[path = "offline/operator_lookup.rs"]
mod operator_lookup;
#[path = "offline/order_currency.rs"]
mod order_currency;
#[path = "offline/quote_feed.rs"]
mod quote_feed;
#[path = "offline/quote_restore.rs"]
mod quote_restore;
#[path = "offline/reconcile_client_admitted.rs"]
mod reconcile_client_admitted;
#[path = "offline/reconcile_client_parse.rs"]
mod reconcile_client_parse;
