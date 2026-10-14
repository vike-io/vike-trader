//! `offline` -- vike-ig's five plain offline suites as ONE test binary (`tests/offline/<name>.rs`).

// `#[path]` because this file is a test-target CRATE ROOT: a bare `mod recon_client_parse;` would
// resolve against `tests/` (the root's own directory), not `tests/offline/`.
#[path = "offline/close_does_not_collide_with_its_open.rs"]
mod close_does_not_collide_with_its_open;
#[path = "offline/close_limit_never_rests.rs"]
mod close_limit_never_rests;
#[path = "offline/confirm_never_terminalizes.rs"]
mod confirm_never_terminalizes;
#[path = "offline/id_less_fills_are_refused.rs"]
mod id_less_fills_are_refused;
#[path = "offline/recon_client_parse.rs"]
mod recon_client_parse;
