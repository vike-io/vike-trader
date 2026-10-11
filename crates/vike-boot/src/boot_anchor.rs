//! The boot anchor: one change-journal record of the effective ceilings per process start.

use std::path::{Path, PathBuf};

use vike_config::Policy;
use vike_model::change_journal::{Actor, Change, ChangeJournal, ChangeJournalError, Outcome, Proc};

/// The dotted key every [`boot_ceilings`] entry is spelled with — the same `<section>.<key>` shape
/// `vike-cli config show` prints and `vike_model::change_journal::SettingTarget`'s `key` carries,
/// so a `boot_settings` line and a `set_setting` line naming the same ceiling are `grep`-able with
/// one pattern.
const CEILING_KEY_PREFIX: &str = "policy.";

/// **The EFFECTIVE ceilings, as `(dotted key, value)` pairs** — what one boot record carries.
///
/// `None` is a ceiling that is NOT SET, and it is distinct from a missing pair: the key is always
/// present, only its value is absent, which is why `vike_model::change_journal::BootSettingsTarget`
/// renders it as `null` rather than omitting the entry. "Uncapped" and "this build did not report
/// that key" must not read identically to somebody comparing two records a month apart; every row
/// below that can be absent takes this shape.
///
/// # What is claimed, and the one field that is not
///
/// The record claims only the ceilings that are EFFECTIVE (a `Consumed::At` row in
/// `crates/vike-config/tests/policy_is_consumed.rs`). Which ACCOUNTS were armed is not a `Policy`
/// field at all — it is the `account` table's `tier` and `active` — and the `venue_mounted` journal
/// records name every account the mount armed, so this anchor does not restate them.
///
/// `max_leverage` is excluded, and the reason is semantic: `crates/vike-mount/src/policy.rs`'s
/// `MountPolicy::from` deliberately does not carry it (its `1.0` default would clamp every
/// deployment with no `policy` rows to 1x), so a value written there is SET and NOT EFFECTIVE, as
/// `policy_is_consumed.rs`'s row for it says. A record claiming it would assert the opposite.
///
/// The dead-man pair is a THIRD shape: the timeout is `null` when the key is absent (the switch is
/// OFF) and its number when written, and the action is claimed only while a written timeout arms
/// the switch — the test `crates/vike-tradehub/src/venue_arming/deadman.rs`'s
/// `deadman_config_from_policy` applies, restated because this crate sits below it.
///
/// ⚠ The destructure below is EXHAUSTIVE on purpose — do NOT add `..`. A new [`Policy`] field must
/// break this line, so its author has to decide whether the boot anchor claims it, exactly as
/// `policy_is_consumed.rs`'s own destructure forces them to say where it is consumed.
pub fn boot_ceilings(policy: &Policy) -> Vec<(String, Option<String>)> {
    let Policy {
        max_leverage: _,
        max_notional_per_order,
        max_account_exposure,
        max_sizing_equity,
        market_slippage,
        halt_admit,
        deadman_timeout_ms,
        deadman_action,
        // Read back through its resolver below: absent means ARMED, so the field is not the value.
        link_deadman_grace_ms: _,
    } = policy;
    vec![
        // Quote-currency notional cap on any single order. `None` = uncapped.
        (
            format!("{CEILING_KEY_PREFIX}max_notional_per_order"),
            max_notional_per_order.map(|v| v.to_string()),
        ),
        // The ACCOUNT-aggregate open-notional ceiling. `None` = uncapped.
        (
            format!("{CEILING_KEY_PREFIX}max_account_exposure"),
            max_account_exposure.map(|v| v.to_string()),
        ),
        // The cap on the equity FIGURE sizing sees. `None` = the VENUE's wallet figure, uncapped.
        (
            format!("{CEILING_KEY_PREFIX}max_sizing_equity"),
            max_sizing_equity.map(|v| v.to_string()),
        ),
        // The emulated-market aggression band. `None` = each venue keeps its own literal.
        (format!("{CEILING_KEY_PREFIX}market_slippage"), market_slippage.map(|v| v.to_string())),
        // How much evidence the HALT sentinel demands: an enum with a default, so always set.
        (format!("{CEILING_KEY_PREFIX}halt_admit"), Some(halt_admit.as_str().to_string())),
        // The dead-man timeout. `None` = absent, the switch OFF; `0` (explicit off) stays a number.
        (
            format!("{CEILING_KEY_PREFIX}deadman_timeout_ms"),
            deadman_timeout_ms.map(|v| v.to_string()),
        ),
        // The action, claimed only while the timeout arms the switch (`Some(n)`, `n > 0`).
        (
            format!("{CEILING_KEY_PREFIX}deadman_action"),
            deadman_timeout_ms
                .filter(|&ms| ms != vike_config::DEADMAN_DISABLED_MS)
                .map(|_| deadman_action.as_str().to_string()),
        ),
        // The LINK dead-man's grace: always set, absent being ARMED at the default.
        (
            format!("{CEILING_KEY_PREFIX}link_deadman_grace_ms"),
            policy.link_deadman_grace_ms_effective().map(|ms| ms.to_string()),
        ),
    ]
}

/// **Write ONE boot anchor** — the effective ceilings at this process start — into
/// `<state_dir>/changes/changes-YYYY-MM.jsonl`, with actor origin `boot`.
///
/// It is the durable twin of the disclosure a root prints from
/// [`Booted::boot_lines`](crate::Booted::boot_lines), which goes to a rolling log file that
/// `vike_log::DEFAULT_MAX_LOG_FILES` prunes, so it is no record a week later
/// (`vike_model::change_journal`'s module doc carries the measurement). This is.
///
/// # ⚠ It is a BRACKET, not a detector. Do not describe it as one.
///
/// **Nothing in this workspace observes a HAND EDIT of a settings row's value in
/// `<project>/settings/db/vike.db`**: there is no file watcher, the seal check
/// (`vike_config::adoption_integrity`) compares ROW COUNTS, which a value changed in place keeps,
/// and `vike_model::change_journal`'s `set_setting` channel sees only writes through a journalled
/// surface (the daemon's control socket, `vike-cli config set`). What this buys is weaker: **two
/// consecutive boot records that disagree prove something changed between them** — not WHO, not
/// WHEN, and at the RESTART CADENCE's resolution, so a ceiling edited and reverted between two
/// starts is invisible to it by construction.
///
/// # Rate, and the tail that bounds it
///
/// One record per start, a few hundred bytes each (`vike_model::change_journal::MAX_RECORD_BYTES`
/// caps a line at 4 KB). The tail is a CRASH LOOP: `deploy/vike-tradehub.service` sets
/// `RestartSec=5`, so the worst case is roughly 17k records a day. That is why the anchor is
/// per-START rather than periodic — a re-read on a timer would need a second settings load, which
/// `crates/vike-boot/tests/one_owner.rs` exists to refuse.
///
/// # Returns
///
/// * `None` — **no state directory resolved, so NOTHING is written**: a root with no project must
///   not invent a ledger location (`crates/vike-tradehub/tests/settings_write_journal.rs`'s
///   `a_journal_less_surface_writes_nothing`).
/// * `Some(Err(_))` — the append failed. Returned rather than logged, because this crate carries
///   no `tracing`; the ROOT emits it, which it can, because unlike [`boot`](crate::boot) this runs
///   after `vike_log::init`.
///
/// `ts_ms` is a PARAMETER: `vike_model::change_journal` reads no clock, and the root stamps it.
pub fn journal_boot_settings(
    state_dir: Option<&Path>,
    policy: &Policy,
    version: &str,
    ts_ms: i64,
) -> Option<Result<PathBuf, ChangeJournalError>> {
    let state_dir = state_dir?;
    let ceilings = boot_ceilings(policy);
    let borrowed: Vec<(&str, Option<&str>)> =
        ceilings.iter().map(|(k, v)| (k.as_str(), v.as_deref())).collect();
    // `Outcome::Applied`: the values ARE in effect, which is the whole claim. `Actor::Boot`: the
    // channel is the process starting, and there is no human to name — see that enum's doc.
    let change = Change::boot_settings(Outcome::Applied, Actor::Boot, &borrowed);
    // `Proc::current` reads `current_exe`, which is the very thing being recorded — one file is
    // written by several binaries, and "which one wrote this line" is the first question asked of a
    // record nobody expected.
    let journal = ChangeJournal::in_state_dir(state_dir, Proc::current(version));
    Some(journal.append(ts_ms, &change))
}
