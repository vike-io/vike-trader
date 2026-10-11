//! The bin's pure argument resolvers: `--asset/--tenor`, `--jobs`, `--kind`, `--tokens`, `--file`.

use super::*;

// ---- --asset/--tenor (family-layout selection) --------------------------------------------

#[test]
fn resolve_family_args_none_when_neither_flag_given() {
    let args: Vec<String> =
        ["prog", "--from", "2026-01-01"].iter().map(|s| s.to_string()).collect();
    assert_eq!(resolve_family_args(&args).unwrap(), None);
}

#[test]
fn resolve_family_args_pairs_both_flags() {
    let args: Vec<String> =
        ["prog", "--asset", "btc", "--tenor", "5m"].iter().map(|s| s.to_string()).collect();
    assert_eq!(resolve_family_args(&args).unwrap(), Some(("btc".to_string(), "5m".to_string())));
}

#[test]
fn resolve_family_args_rejects_a_lone_asset_or_tenor() {
    let asset_only: Vec<String> =
        ["prog", "--asset", "btc"].iter().map(|s| s.to_string()).collect();
    assert!(resolve_family_args(&asset_only).is_err());

    let tenor_only: Vec<String> = ["prog", "--tenor", "5m"].iter().map(|s| s.to_string()).collect();
    assert!(resolve_family_args(&tenor_only).is_err());
}

// ---- resolve_jobs: --jobs N default/clamp resolution -----------------------------------------

#[test]
fn resolve_jobs_uses_an_explicit_positive_value() {
    assert_eq!(resolve_jobs(Some("3"), 32), 3);
    assert_eq!(resolve_jobs(Some("16"), 4), 16, "explicit --jobs is never clamped by cores");
}

#[test]
fn resolve_jobs_defaults_to_min_4_cores_when_absent_zero_or_unparsable() {
    assert_eq!(resolve_jobs(None, 32), 4, "min(4, 32) — the measured sweet spot");
    assert_eq!(resolve_jobs(None, 2), 2, "min(4, 2) — never more workers than cores");
    assert_eq!(resolve_jobs(Some("0"), 32), 4, "0 falls back to the default, not 0 workers");
    assert_eq!(resolve_jobs(Some("garbage"), 32), 4);
    assert_eq!(resolve_jobs(None, 0), 1, "never fewer than one worker");
}

#[test]
fn kinds_from_arg_defaults_to_all_three() {
    assert_eq!(kinds_from_arg(None).unwrap(), Stream::all().to_vec());
    assert_eq!(kinds_from_arg(Some("all")).unwrap(), Stream::all().to_vec());
    assert_eq!(kinds_from_arg(Some("book")).unwrap(), vec![Stream::Book]);
    assert_eq!(kinds_from_arg(Some("trade")).unwrap(), vec![Stream::Trade]);
    assert_eq!(kinds_from_arg(Some("quote")).unwrap(), vec![Stream::Quote]);
    assert!(kinds_from_arg(Some("garbage")).is_err());
}

#[test]
fn resolve_tokens_unions_inline_and_file() {
    let dir =
        std::env::temp_dir().join(format!("vike_archive_backfill_test_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("tokens.txt");
    std::fs::write(&path, "# ids\nid_b\n\n  id_c\n").unwrap();

    let args: Vec<String> =
        ["prog", "--tokens", "id_a, id_b", "--tokens-file", path.to_str().unwrap()]
            .iter()
            .map(|s| s.to_string())
            .collect();
    let got = resolve_tokens(&args).unwrap().unwrap();
    assert_eq!(got, ["id_a", "id_b", "id_c"].iter().map(|s| s.to_string()).collect::<HashSet<_>>());

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn resolve_tokens_none_when_neither_flag_given() {
    let args: Vec<String> =
        ["prog", "--from", "2026-01-01"].iter().map(|s| s.to_string()).collect();
    assert_eq!(resolve_tokens(&args).unwrap(), None);
}

#[test]
fn resolve_tokens_reports_a_missing_tokens_file() {
    let args: Vec<String> = ["prog", "--tokens-file", "/no/such/path/vike-archive-test"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert!(resolve_tokens(&args).is_err());
}

// ---- validate_file_target: --file's single-(date, stream)-target requirement --------------

#[test]
fn validate_file_target_is_a_no_op_when_file_not_given() {
    // Even a multi-day range / all-kinds / dry-run combination is fine when --file is absent
    // — this validation exists ONLY to constrain --file.
    assert!(
        validate_file_target(
            false,
            true,
            &["2026-07-27".to_string(), "2026-07-28".to_string()],
            &Stream::all(),
        )
        .is_ok()
    );
}

#[test]
fn validate_file_target_accepts_one_day_and_one_explicit_kind() {
    assert!(
        validate_file_target(true, false, &["2026-07-28".to_string()], &[Stream::Book]).is_ok()
    );
}

#[test]
fn validate_file_target_rejects_dry_run() {
    assert!(
        validate_file_target(true, true, &["2026-07-28".to_string()], &[Stream::Book]).is_err()
    );
}

#[test]
fn validate_file_target_rejects_a_multi_day_range() {
    let dates = ["2026-07-27".to_string(), "2026-07-28".to_string()];
    assert!(validate_file_target(true, false, &dates, &[Stream::Book]).is_err());
}

#[test]
fn validate_file_target_rejects_zero_days() {
    assert!(validate_file_target(true, false, &[], &[Stream::Book]).is_err());
}

#[test]
fn validate_file_target_rejects_the_default_all_kind_expansion() {
    // kinds_from_arg(None) / kinds_from_arg(Some("all")) both expand to all three streams —
    // exactly the ambiguous case --file must reject.
    assert!(
        validate_file_target(true, false, &["2026-07-28".to_string()], &Stream::all()).is_err()
    );
}

// ---- validate_file_family_exclusion: --file × --asset/--tenor ------------------------------

#[test]
fn file_without_a_family_target_is_fine() {
    assert!(validate_file_family_exclusion(true, None).is_ok());
}

#[test]
fn a_family_target_without_file_is_fine() {
    let family = ("btc".to_string(), "5m".to_string());
    assert!(
        validate_file_family_exclusion(false, Some(&family)).is_ok(),
        "--asset/--tenor over HTTP is the whole point of the family layout"
    );
}

#[test]
fn file_plus_a_family_target_is_rejected_not_silently_ignored() {
    // The family pair only picks a URL, and --file makes no network call — so accepting the
    // combination would be a no-op that LOOKS like it narrowed the ingest.
    let family = ("btc".to_string(), "5m".to_string());
    let err = validate_file_family_exclusion(true, Some(&family)).unwrap_err();
    assert!(err.contains("--file"), "the message names both offending flags: {err}");
    assert!(err.contains("--asset"), "the message names both offending flags: {err}");
}

#[test]
fn neither_file_nor_family_is_fine() {
    assert!(
        validate_file_family_exclusion(false, None).is_ok(),
        "the plain flat-layout URL run must stay unaffected"
    );
}
