//! The reconcile manager's integration suites and the `CoreLaneSink` valuation-law suite, in one test
//! binary — they came from vike-core's `tests/recon.rs` and `tests/wiring.rs` when the manager, the sink and
//! `journal_view_from_store` moved here (wave 5, P6a).
//!
//! Each member is `tests/recon/<name>.rs`, `#[path]`-included below; test NAMES are unchanged, so
//! `-- <name>` filtering still works. The shared builders are the copies in `tests/common/core_kit.rs`,
//! reached as `crate::kit::<module>::<item>` exactly as in vike-core's suites. The suites whose SUBJECT is the
//! fold (`reconcile_reports`, quarantine, restore) stayed in vike-core and drive it by sending
//! `Command::ReconcileReports` to the handle.

#[path = "common/core_kit.rs"]
mod kit;

#[path = "recon/mark_slot_semantics.rs"]
mod mark_slot_semantics;
#[path = "recon/reconcile_admitted_scope.rs"]
mod reconcile_admitted_scope;
#[path = "recon/reconcile_audit_gating.rs"]
mod reconcile_audit_gating;
#[path = "recon/reconcile_balance.rs"]
mod reconcile_balance;
#[path = "recon/reconcile_continuous_audit.rs"]
mod reconcile_continuous_audit;
#[path = "recon/reconcile_journal_view.rs"]
mod reconcile_journal_view;
#[path = "recon/reconcile_manager.rs"]
mod reconcile_manager;
#[path = "recon/reconcile_per_account.rs"]
mod reconcile_per_account;
