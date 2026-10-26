//! `offline` -- vike-ig's plain offline suites as ONE test binary (`tests/offline/<name>.rs`).

// `#[path]` because this file is a test-target CRATE ROOT: a bare `mod recon_client_parse;` would
// resolve against `tests/` (the root's own directory), not `tests/offline/`.
#[path = "offline/any_symbol_facts.rs"]
mod any_symbol_facts;
#[path = "offline/any_symbol_quoted.rs"]
mod any_symbol_quoted;
#[path = "offline/close_does_not_collide_with_its_open.rs"]
mod close_does_not_collide_with_its_open;
#[path = "offline/close_limit_never_rests.rs"]
mod close_limit_never_rests;
#[path = "offline/confirm_never_terminalizes.rs"]
mod confirm_never_terminalizes;
// The scripted loopback IG gateway the facts and reconcile suites dial (`127.0.0.1`, never IG).
#[path = "offline/fake_ig.rs"]
mod fake_ig;
#[path = "offline/id_less_fills_are_refused.rs"]
mod id_less_fills_are_refused;
#[path = "offline/order_currency.rs"]
mod order_currency;
#[path = "offline/quote_feed.rs"]
mod quote_feed;
#[path = "offline/quote_restore.rs"]
mod quote_restore;
#[path = "offline/recon_client_admitted.rs"]
mod recon_client_admitted;
#[path = "offline/recon_client_parse.rs"]
mod recon_client_parse;
