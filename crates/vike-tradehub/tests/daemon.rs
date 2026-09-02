//! `daemon` — vike-tradehub's plain integration suites (mounts, observe roundtrips, lifecycle,
//! policy ceiling, wiring pins): ONE test binary over what used to be eleven.
//!
//! Grouped per the rule in `crates/vike-backtest/CLAUDE.md`'s "Test-binary consolidation" section:
//! each former `tests/<name>.rs` is now `tests/daemon/<name>.rs`, included below as a plain
//! module. Test names and bodies are unchanged — only the binary they link into
//! (`--test policy_ceiling_e2e` is now `--test daemon`; `-- <name>` filtering is untouched).
//!
//! NOT here, on purpose — every exclusion below is an eligibility verdict, not an oversight:
//! `telegram_control.rs` is crate-level `#![cfg]`-gated on the `telegram` feature, and
//! `sigterm_stop.rs` is crate-level `#![cfg(unix)]` — a member may only carry a crate-level gate
//! every member shares, so each stays its own binary. `control_roundtrip.rs` is a THIRD kind of
//! exclusion the survey's `env::set_var`/`set_current_dir` grep cannot see: its own
//! `audit_capture` module installs the process-wide `tracing` default subscriber
//! (`tracing::subscriber::set_global_default`) and its doc comment says outright that this
//! requires winning "the one-global-subscriber race to another test's initializer" — a bet that
//! is only safe when every OTHER test in the same process stays out of that race. Half the files
//! grouped here call `vike_log::test_init()`, which installs its own (different) global default
//! subscriber the same way; under a plain `cargo test -p vike-tradehub --test daemon` (all
//! members as THREADS in one process — the exact mode this file exists to support) whichever
//! subscriber installs first wins process-wide, so `control_roundtrip.rs`'s two audit-content
//! assertions (`a_command_rationale_reaches_the_audit_record_sanitized`,
//! `remote_control_handle_carries_a_reason_and_the_bare_call_carries_none`) failed 3/3 reruns once
//! grouped alongside them — reproducible, not a load flake (`nextest`, which runs every test in
//! its own process, never saw it: the race can only manifest when two files' tests share a
//! process). `control_roundtrip.rs` therefore stays its OWN standalone binary, uncontested —
//! and `settings_write_audit.rs` (REQ-7's old→new audit assertions) is that same verdict a
//! second time: it installs its own capture subscriber, so it stays standalone too. Every
//! member actually below is plain — no crate-level `#![cfg]`, no `#[ignore]`, no proptest
//! sidecar, and no process-global mutation of ANY kind (env, cwd, or the tracing default
//! subscriber) — because nextest runs each test in its own process, but the feature lanes and a
//! plain `cargo test -p vike-tradehub` run one binary's tests as THREADS in one process.

// `#[path]` because this file is a test-target CRATE ROOT: a bare `mod alerts_mount;` would
// resolve against `tests/` (the root's own directory), not `tests/daemon/`.
#[path = "daemon/account_badge_wiring_pin.rs"]
mod account_badge_wiring_pin;
#[path = "daemon/alerts_mount.rs"]
mod alerts_mount;
#[path = "daemon/any_strategy_mount.rs"]
mod any_strategy_mount;
#[path = "daemon/cex_feed_wiring_pin.rs"]
mod cex_feed_wiring_pin;
#[path = "daemon/docs_profiles_parse.rs"]
mod docs_profiles_parse;
#[path = "daemon/headless_lifecycle.rs"]
mod headless_lifecycle;
#[path = "daemon/help_and_log_dir.rs"]
mod help_and_log_dir;
#[path = "daemon/live_gate_paper.rs"]
mod live_gate_paper;
#[path = "daemon/live_wired_venues_pin.rs"]
mod live_wired_venues_pin;
#[path = "daemon/materializer_sink.rs"]
mod materializer_sink;
#[path = "daemon/mount_resurrect.rs"]
mod mount_resurrect;
#[path = "daemon/multi_mount_profile.rs"]
mod multi_mount_profile;
#[path = "daemon/observe_roundtrip.rs"]
mod observe_roundtrip;
#[path = "daemon/policy_ceiling_e2e.rs"]
mod policy_ceiling_e2e;
#[path = "daemon/rhai_mount.rs"]
mod rhai_mount;
#[path = "daemon/run_profile_risk_wiring.rs"]
mod run_profile_risk_wiring;
#[path = "daemon/settings_hot_reload.rs"]
mod settings_hot_reload;
#[path = "daemon/settings_show.rs"]
mod settings_show;
#[path = "daemon/settings_write.rs"]
mod settings_write;
