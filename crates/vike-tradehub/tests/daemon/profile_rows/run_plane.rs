//! THE RUN PLANE: the run-profile rows that BIND, and the refusal they must not be able to remove.

use std::assert_matches;
use std::collections::HashMap;

use vike_secrets::profile_store::ProfileKind;
use vike_tradehub::profile_rows::{
    ArmingOutcome, LiveRefusal, REFUSING_RISK_KEYS, live_risk_budget_missing, resolve_arming,
    rows_to_run_profile,
};

use super::*;

// ---------------------------------------------------------------------------------------------
// THE RUN PLANE — the rows that BIND, and the refusal they must not be able to remove
// ---------------------------------------------------------------------------------------------

/// A `run`-kind `StoredProfile` from `(path, TOML-rendered value)` pairs — the shape
/// `vike-cli config bootstrap-run` writes, built by hand here so no assertion below depends on
/// a filesystem or on the CLI.
fn run_rows(name: &str, settings: &[(&str, &str)]) -> vike_secrets::profile_store::StoredProfile {
    vike_secrets::profile_store::StoredProfile {
        row: vike_secrets::profile_store::ProfileRow {
            name: name.to_string(),
            kind: ProfileKind::Run,
            active: false,
            note: None,
        },
        mounts: Vec::new(),
        params: std::collections::BTreeMap::new(),
        settings: settings.iter().map(|(p, v)| ((*p).to_string(), (*v).to_string())).collect(),
        recorder: None,
    }
}

/// the CI box's `settings/run-live.toml`, as rows: `mode` plus the two ceilings.
fn prod2_run_rows() -> vike_secrets::profile_store::StoredProfile {
    run_rows(
        "run-live",
        &[
            ("mode", "\"live\""),
            ("risk.max_notional_per_order", "100.0"),
            ("risk.max_total_exposure", "500.0"),
        ],
    )
}

/// **EVERY KEY OF THE LIVE RUN PROFILE SURVIVES the rows and comes back through the REAL parser.**
/// Named individually rather than counted: a count passes over a swap.
#[test]
fn a_run_profile_round_trips_through_the_rows_and_back_through_the_real_parser() {
    let from_rows = rows_to_run_profile(&prod2_run_rows()).expect("the stored body must reload");
    let from_file = run(PROD2_SHAPED_RUN_PROFILE);
    assert_eq!(from_rows.mode, vike_core::Mode::Live, "`mode` survived");
    assert_eq!(from_rows.risk.max_notional_per_order, Some(100.0));
    assert_eq!(from_rows.risk.max_total_exposure, Some(500.0));
    assert_eq!(
        from_rows.mode, from_file.mode,
        "the row body and the CI box's own file agree about the one key that gates a live mount"
    );
    // ...and every table the file does not set stays unset rather than acquiring a default the
    // operator never typed.
    assert_eq!(from_rows.sinks, vike_core::run_profile::Sinks::default());
    assert_eq!(from_rows.guards, vike_core::run_profile::Guards::default());
    assert!(from_rows.sinks.journal.is_none(), "no [sinks.journal] table on the CI box's body");
}

/// **THE ONE THAT MATTERS FOR THE RUN PLANE: a box that is LIVE today is live after.**
///
/// Two comparisons, because neither is the whole claim:
///
/// * `ArmingOutcome` — the value the daemon logs at startup — resolved once from the FILE-loaded
///   run profile and once from the ROW-loaded one, against the same daemon profile and the same
///   live flag. That answers WHETHER it arms and on WHICH mounts.
/// * the `[risk]` table itself, field for field. ⚠ **This half did not exist and the assert
///   message claimed it did.** `ArmingOutcome::Live { primary, mounts }` carries no risk numbers
///   at all, so *"reading the run profile from ROWS changed what this box trades"* passed on any
///   non-`None` pair — a WIDENED ceiling included. It was not hypothetical either: the two
///   fixtures disagreed about `max_total_exposure` (`1000.0` here against the rows' `500.0`) for
///   as long as the test existed, and it was green. See [`PROD2_SHAPED_RUN_PROFILE`].
///
/// `ProfileRisk` derives `PartialEq`, so the second comparison is the WHOLE table rather than the
/// two ceilings a sample would name — a `[risk]` key added to that type and dropped by the
/// migration fails here without anybody remembering to list it.
#[test]
fn a_prod2_shaped_live_box_arms_identically_from_the_run_rows() {
    let profile = daemon(PROD2_SHAPED_DAEMON_PROFILE);
    let from_file = run(PROD2_SHAPED_RUN_PROFILE);
    let before = resolve_arming(true, Some(&from_file), &profile);
    assert_matches!(
        &before, ArmingOutcome::Live { primary, .. } if primary == "bybit/BTCUSDT",
        "the pre-migration state must resolve LIVE, or this test measures the wrong box: {before:?}"
    );
    let from_rows = rows_to_run_profile(&prod2_run_rows()).expect("the stored body must reload");
    let after = resolve_arming(true, Some(&from_rows), &profile);
    assert_eq!(
        before, after,
        "reading the run profile from ROWS changed WHETHER this box arms, or WHICH mounts it arms"
    );
    assert_eq!(
        from_rows.risk, from_file.risk,
        "reading the run profile from ROWS changed the pre-trade CEILINGS this box judges every \
         order against, which is the one thing this migration may not do — and which the \
         ArmingOutcome comparison above cannot see, because that value carries no risk numbers"
    );
}

/// **⚠ THE REFUSAL, KILL-PROVED ON THE ROW PATH.** `run-live.toml`'s own comment says *"A live
/// mount REFUSES TO START without it"*, and the whole hazard of this migration is a row plane that
/// resolves to "no ceiling" where the file plane refused.
///
/// Three unresolvable shapes, one verdict each, and none of them is `Live`:
///
/// 1. a `run` body with NO `[risk]` table at all — the shape a half-filled migration leaves;
/// 2. a body carrying only ONE of the two refusing ceilings — the shape a typo leaves;
/// 3. no run profile at all, which is the state every unmigrated box is in and the one the row
///    path must reproduce exactly.
///
/// The missing-key list is DERIVED from `vike_config::ceilings::PRE_TRADE_CEILINGS`
/// (`live_risk_budget_missing`), so this cannot pass by agreeing with a second copy of the roster.
#[test]
fn a_run_body_that_cannot_supply_the_budget_refuses_the_live_mount_rather_than_defaulting() {
    let profile = daemon(PROD2_SHAPED_DAEMON_PROFILE);

    // 1 — a body with `mode` and nothing else.
    let bare = rows_to_run_profile(&run_rows("bare", &[("mode", "\"live\"")]))
        .expect("a profile that sets no ceiling is a VALID profile — it just cannot arm one");
    assert_eq!(bare.risk.max_notional_per_order, None);
    assert_eq!(bare.risk.max_total_exposure, None);
    assert_eq!(
        resolve_arming(true, Some(&bare), &profile),
        ArmingOutcome::LiveRefused(LiveRefusal::MissingRiskBudget(REFUSING_RISK_KEYS.to_vec())),
        "a row body with no ceilings must REFUSE the mount, never fall back to a permissive default"
    );

    // 2 — half a budget. The refusal must name the key that is missing and not the one that is set.
    let half = rows_to_run_profile(&run_rows(
        "half",
        &[("mode", "\"live\""), ("risk.max_notional_per_order", "100.0")],
    ))
    .expect("loads");
    assert_eq!(
        live_risk_budget_missing(Some(&half.risk)),
        vec!["max_total_exposure"],
        "exactly the unset refusing ceiling, derived from PRE_TRADE_CEILINGS"
    );
    assert_matches!(
        resolve_arming(true, Some(&half), &profile),
        ArmingOutcome::LiveRefused(LiveRefusal::MissingRiskBudget(_)),
        "half a budget is still a refusal"
    );

    // 3 — no profile at all: the state of every box that has not crossed, unchanged.
    assert_eq!(
        resolve_arming(true, None, &profile),
        ArmingOutcome::LiveRefused(LiveRefusal::MissingRiskBudget(REFUSING_RISK_KEYS.to_vec())),
        "absence still resolves to absence"
    );
}

/// A row body whose `mode` is not `live` is REFUSED for a live mount rather than merged — the same
/// refusal `RunProfile::risk_for_live_venue_mount` makes for a file, reached through the row path.
/// That is why `mode` is a stored row at all: the retired `profile_risk` mirror carried none, so it
/// could never have answered "may this arm a live mount".
#[test]
fn a_non_live_run_body_refuses_the_live_mount_through_the_row_path_too() {
    let profile = daemon(PROD2_SHAPED_DAEMON_PROFILE);
    let paper = rows_to_run_profile(&run_rows(
        "paper",
        &[
            ("mode", "\"paper\""),
            ("risk.max_notional_per_order", "100.0"),
            ("risk.max_total_exposure", "500.0"),
        ],
    ))
    .expect("a paper profile is a valid profile");
    assert!(paper.risk_for_live_venue_mount().is_err(), "the live arm refuses it outright");
    assert_matches!(
        resolve_arming(true, Some(&paper), &profile),
        ArmingOutcome::LiveRefused(LiveRefusal::NotLiveMode(_)),
        "and the folded outcome says WHY, rather than reporting a missing budget"
    );
}

/// **A row body that cannot be MATERIALISED is an `Err`, never a silent `None`.** The daemon turns
/// this into `ExitCode::FAILURE`; what is proven here is the half a pure test can prove — that the
/// function refuses rather than returning a profile with the bad key dropped.
///
/// Two shapes, and the first is the one that matters: a `profile_setting` path naming nothing real
/// is caught by `RunProfile`'s own `deny_unknown_fields`, which is why the renderer needs no key
/// vocabulary of its own.
#[test]
fn an_unreadable_run_body_refuses_rather_than_dropping_the_key_it_cannot_read() {
    let e = rows_to_run_profile(&run_rows(
        "typo",
        &[("mode", "\"live\""), ("risk.max_notionl_per_order", "100.0")],
    ))
    .expect_err("an unknown `[risk]` key must refuse, or a ceiling silently vanishes");
    assert!(e.contains("max_notionl_per_order"), "the refusal names the key: {e}");

    // ...and a body with no `mode` at all: the field has no serde default, so it cannot be dropped.
    let e = rows_to_run_profile(&run_rows("nomode", &[("risk.max_total_exposure", "500.0")]))
        .expect_err("`mode` is required");
    assert!(e.contains("mode"), "{e}");

    // ⚠ And the tombstones are refused through the row path too, in `validate`'s own words — a row
    // for a table the daemon DELETES would read as more authoritative than the file line it
    // replaced.
    let e =
        rows_to_run_profile(&run_rows("dead", &[("mode", "\"live\""), ("broker.kind", "\"x\"")]))
            .expect_err("`[broker]` is a tombstone");
    assert!(e.contains("broker"), "{e}");
}

/// **No FILE rung is left: the variable that named one is REFUSED at startup** (decision 0111).
/// The run profile is the active `run` row and nothing else, so a unit or `.env` still setting the
/// variable must stop the daemon and name the command that writes the row — a variable that
/// looked set and decided nothing would be the pre-trade ceilings an operator believes in, gone
/// silent.
#[test]
fn the_retired_run_profile_variable_refuses_startup_naming_the_writer() {
    // Composed rather than spelled: the settings registry's literal sweep reads a whole env-shaped
    // literal as a READ of that variable, and this daemon reads it nowhere.
    let name = ["VIKE", "RUN", "PROFILE"].join("_");
    let env: HashMap<String, String> =
        [(name.clone(), "/p/settings/run-live.toml".to_string())].into_iter().collect();
    let refusal =
        vike_config::refuse_removed_env(&env).expect_err("a set run-profile variable must refuse");
    assert!(refusal.contains(&name), "the refusal names the variable: {refusal}");
    assert!(refusal.contains("bootstrap-run"), "…and the command that writes the row: {refusal}");
}

/// **The journal is the `config.journal_dir` row's, whatever the run profile says** (decision 0111,
/// amended 2026-10-11). A run profile can neither turn the journal on, nor point it elsewhere, nor
/// turn it off: it only TUNES the journal the row names. That is what makes this daemon and
/// `vike-datahub`'s materializer, which follows the same row, agree by construction.
#[test]
fn the_journal_is_the_directory_rows_and_the_profile_only_tunes_it() {
    use vike_tradehub::profile_rows::{JournalRung, journal_config_for};
    let rung = JournalRung { dir: Some("/no/such/journal-dir".into()), snapshot_every: Some(64) };
    let tuned = rows_to_run_profile(&run_rows(
        "j",
        &[
            ("mode", "\"paper\""),
            ("sinks.journal.snapshot_every", "128"),
            ("sinks.journal.flush_every", "8"),
        ],
    ))
    .expect("a tuning-only [sinks.journal] loads");

    // ⚠ NO ROW = OFF, even though the profile carries a `[sinks.journal]`: a profile cannot turn
    // the journal on.
    assert!(
        journal_config_for(Some(&tuned), &JournalRung::default()).is_none(),
        "no config.journal_dir row means no WAL, whatever the run profile carries"
    );

    // A profile with NO `[sinks.journal]` cannot turn the row's journal off (the old rule did).
    let bare = rows_to_run_profile(&prod2_run_rows()).expect("loads");
    let from_row = journal_config_for(Some(&bare), &rung).expect("the row turns the journal on");
    assert_eq!(from_row.dir, std::path::PathBuf::from("/no/such/journal-dir"), "the row's dir");
    assert_eq!(from_row.snapshot_every, 64, "…at the cadence row's value");

    // The row set AND a tuning table: the directory is still the row's, and the keys the profile
    // names win over the cadence row; a key it leaves off keeps the default.
    let both = journal_config_for(Some(&tuned), &rung).expect("the row turns the journal on");
    assert_eq!(both.dir, std::path::PathBuf::from("/no/such/journal-dir"), "never the profile's");
    assert_eq!(both.snapshot_every, 128, "the profile's tuning applies");
    assert_eq!(both.file.flush_every, 8);
    assert_eq!(
        both.file.segment_bytes,
        vike_core::JournalConfig::at("/no/such/journal-dir").file.segment_bytes,
        "a key the profile leaves off keeps the default"
    );
}

/// **A stored run body that still carries the RETIRED `sinks.journal.dir` refuses to load**, and
/// the refusal names the fix — so the daemon's boot (which treats a run row it cannot load as a
/// HARD failure) tells the operator what to do instead of journalling somewhere the row does not
/// name.
#[test]
fn a_stored_run_body_with_the_retired_journal_dir_refuses_with_the_fix() {
    let refused = rows_to_run_profile(&run_rows(
        "j",
        &[("mode", "\"paper\""), ("sinks.journal.dir", "\"/no/such/vike-wal\"")],
    ))
    .expect_err("the retired key must not load");
    for needle in
        ["sinks.journal.dir", "vike-cli config set config.journal_dir <dir>", "bootstrap-run"]
    {
        assert!(refused.contains(needle), "the refusal must name {needle:?}: {refused}");
    }
}
