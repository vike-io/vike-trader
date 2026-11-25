//! **The journalled write of ONE settings row** (`docs/decisions/0086`): `vike_config::write_setting_row`
//! plus its `vike_change_journal` record, BOTH outcomes, shared by `vike-cli` (`crates/vike-cli/src/cmd/settings_write.rs`'s
//! `set_setting_journalled`: `config set`, `backend mint-missing`, `backend connect`) and the desktop's Server group
//! (`crates/vike-app-core/src/backend/server_link.rs`'s `connect_now`), so a connect's dial rows are recorded by one
//! rule whichever door made them.
//!
//! What stays with each caller: WHO it is (the `actor`), where its ledger lives and how it words a ledger that cannot
//! take a record (the `record` closure), and what a refusal means to it (an exit rung, a line on a screen). The
//! decisions here are the CLI module's 1 to 3: `AppliedPendingRestart` always (a local write reaches no applier), a
//! refusal recorded too with the writer's own reason, and a journal failure never failing the call.

use std::path::Path;
use std::time::Duration;

use vike_change_journal::{Change, ChangeActor, ChangeOutcome};
use vike_config::RowPlanError;
use vike_config::write::RowReport;

/// Write `key = value` into `settings_dir`'s database within `budget`, and hand its `set_setting` record under
/// `actor` to `record`, which appends it: `AppliedPendingRestart` with the cells the writer returns, or `Refused`
/// carrying the writer's reason. The row's section is the key's first dotted part.
///
/// # Errors
/// The writer's refusal, unchanged (the database is byte-identical on every failure path).
pub fn write_setting_journalled(
    settings_dir: &Path,
    key: &str,
    value: &str,
    budget: Duration,
    actor: ChangeActor,
    record: impl FnOnce(ChangeOutcome, &Change),
) -> Result<RowReport, RowPlanError> {
    let section = key.split('.').next().unwrap_or(key);
    match vike_config::write_setting_row(settings_dir, key, value, budget) {
        Ok(report) => {
            record(
                ChangeOutcome::AppliedPendingRestart,
                &Change::set_setting(
                    ChangeOutcome::AppliedPendingRestart,
                    actor,
                    section,
                    &report.key,
                    report.old_value.as_deref(),
                    &report.new_value,
                ),
            );
            Ok(report)
        }
        Err(e) => {
            let error = e.to_string();
            // ⚠ `old: None` here is "no previous value was established", NOT "the key was unset in the store": the
            // writer refused, so it either never read one or never reached the point of reading one.
            // `vike_change_journal::ChangeTargetSetting`'s own doc carries that rule.
            record(
                ChangeOutcome::Refused,
                &Change::set_setting(ChangeOutcome::Refused, actor, section, key, None, value)
                    .with_reason(Some(&error)),
            );
            Err(e)
        }
    }
}

#[path = "settings_row_tests.rs"]
#[cfg(test)]
mod settings_row_tests;
