//! The bin's pure argument resolvers: `--asset/--tenor`, `--jobs`, `--kind`, `--tokens`, `--file`,
//! and the `Args` the command line is read into.

use super::*;

fn parsed(v: &[&str]) -> Result<Args, vike_util::cli::CliExit> {
    vike_backfill::cli::try_parse_from("vike_archive_backfill", v.iter().copied())
}

// ---- Args: every flag, both spellings -------------------------------------------------------

#[test]
fn every_flag_maps_in_both_spellings() {
    let spaced = parsed(&[
        "--from",
        "2026-07-28",
        "--to",
        "2026-07-29",
        "--kind",
        "book",
        "--tokens",
        "a,b",
        "--tokens-file",
        "/t",
        "--store",
        "/s",
        "--base",
        "http://h/archive",
        "--dry-run",
        "--asset",
        "btc",
        "--tenor",
        "5m",
        "--discovery-base",
        "http://h",
        "--bulk",
        "--bulk-max-batches",
        "3",
        "--bulk-max-rows",
        "4",
        "--bulk-max-bytes",
        "5",
        "--file",
        "/f",
        "--jobs",
        "2",
    ])
    .expect("the documented flag set parses");
    let inline = parsed(&[
        "--from=2026-07-28",
        "--to=2026-07-29",
        "--kind=book",
        "--tokens=a,b",
        "--tokens-file=/t",
        "--store=/s",
        "--base=http://h/archive",
        "--dry-run",
        "--asset=btc",
        "--tenor=5m",
        "--discovery-base=http://h",
        "--bulk",
        "--bulk-max-batches=3",
        "--bulk-max-rows=4",
        "--bulk-max-bytes=5",
        "--file=/f",
        "--jobs=2",
    ])
    .expect("the inline spelling parses");
    for a in [&spaced, &inline] {
        assert_eq!((a.from.as_deref(), a.to.as_deref()), (Some("2026-07-28"), Some("2026-07-29")));
        assert_eq!(a.kind.as_deref(), Some("book"));
        assert_eq!((a.tokens.as_deref(), a.tokens_file.as_deref()), (Some("a,b"), Some("/t")));
        assert_eq!((a.store.as_deref(), a.base.as_deref()), (Some("/s"), Some("http://h/archive")));
        assert!(a.dry_run && a.bulk && !a.gaps);
        assert_eq!((a.asset.as_deref(), a.tenor.as_deref()), (Some("btc"), Some("5m")));
        assert_eq!(a.discovery_base.as_deref(), Some("http://h"));
        assert_eq!(
            (a.bulk_max_batches, a.bulk_max_rows, a.bulk_max_bytes),
            (Some(3), Some(4), Some(5))
        );
        assert_eq!((a.file.as_deref(), a.jobs), (Some("/f"), Some(2)));
    }
}

/// `--gaps` alone is a complete command line (the days come from the store).
#[test]
fn gaps_alone_parses_and_a_typo_or_a_malformed_number_is_refused() {
    let a = parsed(&["--gaps", "--store", "/s"]).expect("--gaps alone");
    assert!(a.gaps && a.from.is_none() && a.to.is_none());
    let typo = parsed(&["--gaps", "--bulk-max-row", "3"]).expect_err("a typo");
    assert_eq!(typo.code, 2);
    assert!(typo.text.contains("--bulk-max-row"), "{}", typo.text);
    // These three used to fall back to the default in silence.
    for flag in ["--jobs", "--bulk-max-batches", "--bulk-max-rows", "--bulk-max-bytes"] {
        let e = parsed(&["--gaps", flag, "many"]).expect_err("not a number");
        assert!(e.text.contains(flag), "{flag}: {}", e.text);
    }
}

// ---- --asset/--tenor (family-layout selection) --------------------------------------------

#[test]
fn resolve_family_args_none_when_neither_flag_given() {
    assert_eq!(resolve_family_args(None, None).unwrap(), None);
}

#[test]
fn resolve_family_args_pairs_both_flags() {
    assert_eq!(
        resolve_family_args(Some("btc".to_string()), Some("5m".to_string())).unwrap(),
        Some(("btc".to_string(), "5m".to_string()))
    );
}

#[test]
fn resolve_family_args_rejects_a_lone_asset_or_tenor() {
    assert!(resolve_family_args(Some("btc".to_string()), None).is_err());
    assert!(resolve_family_args(None, Some("5m".to_string())).is_err());
}

// ---- resolve_jobs: --jobs N default/clamp resolution -----------------------------------------

#[test]
fn resolve_jobs_uses_an_explicit_positive_value() {
    assert_eq!(resolve_jobs(Some(3), 32), 3);
    assert_eq!(resolve_jobs(Some(16), 4), 16, "explicit --jobs is never clamped by cores");
}

#[test]
fn resolve_jobs_defaults_to_min_4_cores_when_absent_or_zero() {
    assert_eq!(resolve_jobs(None, 32), 4, "min(4, 32) — the measured sweet spot");
    assert_eq!(resolve_jobs(None, 2), 2, "min(4, 2) — never more workers than cores");
    assert_eq!(resolve_jobs(Some(0), 32), 4, "0 falls back to the default, not 0 workers");
    assert_eq!(resolve_jobs(None, 0), 1, "never fewer than one worker");
}

#[test]
fn bulk_config_overrides_only_the_given_fields() {
    let default = BulkConfig::default();
    let cfg = bulk_config(Some(3), None, Some(5));
    assert_eq!((cfg.max_batches, cfg.max_bytes), (3, 5));
    assert_eq!(cfg.max_rows, default.max_rows);
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

    let got = resolve_tokens(Some("id_a, id_b".to_string()), path.to_str()).unwrap().unwrap();
    assert_eq!(got, ["id_a", "id_b", "id_c"].iter().map(|s| s.to_string()).collect::<HashSet<_>>());

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn resolve_tokens_none_when_neither_flag_given() {
    assert_eq!(resolve_tokens(None, None).unwrap(), None);
}

#[test]
fn resolve_tokens_reports_a_missing_tokens_file() {
    assert!(resolve_tokens(None, Some("/no/such/path/vike-archive-test")).is_err());
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
