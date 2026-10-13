//! (c) The disk-headroom leg and (d) the network leg, read through the NetProbe handle.

use super::*;

// ---- (c) disk headroom ----------------------------------------------------------------------

#[test]
fn ample_free_space_passes() {
    let cfg = PreflightConfig::default();
    let r = check_disk_headroom("journal", Path::new("/j"), &cfg, &healthy());
    assert_eq!(r.status, CheckStatus::Pass, "at the warn floor exactly, still ample");
    assert_eq!(r.venue, None, "disk is a GLOBAL check, not a per-venue one");
    assert_eq!(r.name, CHECK_DISK);
    assert!(r.message.contains("journal"), "{}", r.message);
}

/// The sizes in the message are `vike_util::bytes::iec_bytes`'s IEC text (`KiB`/`MiB`/`GiB`/`TiB`),
/// the units the OS tools an operator cross-checks print. The formatter's own goldens are
/// `crates/vike-util/src/bytes_tests.rs`'s; this pins that the check still speaks it.
#[test]
fn the_disk_message_prints_iec_units() {
    let cfg = PreflightConfig::default();
    let free: u64 = 2 * 1024 * 1024 * 1024;
    let probes = FnProbes::new().with_free_space_bytes(move |_: &Path| Ok(free));
    let r = check_disk_headroom("journal", Path::new("/j"), &cfg, &probes);
    assert!(r.message.ends_with("2.0 GiB free (warn 5.0 GiB, fail 1.0 GiB)"), "{}", r.message);
}

#[test]
fn free_space_below_the_warn_floor_warns() {
    let cfg = PreflightConfig::default();
    assert_eq!(disk_status(DEFAULT_DISK_WARN_BYTES - 1, &cfg), CheckStatus::Warn);
}

#[test]
fn free_space_below_the_fail_floor_fails() {
    let cfg = PreflightConfig::default();
    assert_eq!(disk_status(DEFAULT_DISK_FAIL_BYTES - 1, &cfg), CheckStatus::Fail);
    assert_eq!(disk_status(0, &cfg), CheckStatus::Fail);
}

#[test]
fn disk_floors_are_configurable() {
    let cfg = PreflightConfig {
        disk_warn_bytes: 2_000,
        disk_fail_bytes: 1_000,
        ..PreflightConfig::default()
    };
    assert_eq!(disk_status(5_000, &cfg), CheckStatus::Pass);
    assert_eq!(disk_status(1_500, &cfg), CheckStatus::Warn);
    assert_eq!(disk_status(500, &cfg), CheckStatus::Fail);
}

/// **The `config.preflight_disk_*_mib` defaults ARE the compiled floors.** `vike-config` cannot name
/// this crate's byte constants, so it spells the two defaults in MiB
/// (`vike_config::config::DEFAULT_PREFLIGHT_DISK_WARN_MIB` and its fail twin) for its cross-field
/// check; this holds the two spellings equal, and an absent row on either side is the compiled pair.
#[test]
fn the_disk_row_defaults_are_the_compiled_floors() {
    use vike_config::config::{DEFAULT_PREFLIGHT_DISK_FAIL_MIB, DEFAULT_PREFLIGHT_DISK_WARN_MIB};
    let compiled = DiskThresholds::default();
    assert_eq!(compiled.warn_bytes, DEFAULT_DISK_WARN_BYTES);
    assert_eq!(compiled.fail_bytes, DEFAULT_DISK_FAIL_BYTES);
    assert_eq!(
        DiskThresholds::from_rows(
            Some(DEFAULT_PREFLIGHT_DISK_WARN_MIB),
            Some(DEFAULT_PREFLIGHT_DISK_FAIL_MIB)
        ),
        compiled,
        "the config crate's MiB defaults are this crate's byte floors"
    );
    assert_eq!(DiskThresholds::from_rows(None, None), compiled, "no rows is today's preflight");
    let cfg = PreflightConfig::default();
    assert_eq!(
        (cfg.disk_warn_bytes, cfg.disk_fail_bytes),
        (DEFAULT_DISK_WARN_BYTES, DEFAULT_DISK_FAIL_BYTES)
    );
}

/// Each row sets ITS floor in MiB and leaves the other at the compiled default, and the largest
/// figure the loader accepts (and `u32::MAX`, which it never does) converts without overflow.
#[test]
fn the_disk_rows_set_each_floor_alone_and_never_overflow() {
    const MIB: u64 = 1024 * 1024;
    let compiled = DiskThresholds::default();
    let warn_only = DiskThresholds::from_rows(Some(8_192), None);
    assert_eq!((warn_only.warn_bytes, warn_only.fail_bytes), (8_192 * MIB, compiled.fail_bytes));
    let fail_only = DiskThresholds::from_rows(None, Some(512));
    assert_eq!((fail_only.warn_bytes, fail_only.fail_bytes), (compiled.warn_bytes, 512 * MIB));

    let max = vike_config::config::MAX_PREFLIGHT_DISK_MIB;
    assert_eq!(DiskThresholds::from_rows(Some(max), None).warn_bytes, u64::from(max) * MIB);
    assert_eq!(
        DiskThresholds::from_rows(Some(u32::MAX), None).warn_bytes,
        u64::from(u32::MAX) * MIB
    );
}

/// **The floors drive the leg**: a pair built from the rows, handed to the check through
/// `PreflightConfig`, moves the PASS/WARN/FAIL lines exactly where the rows say.
#[test]
fn row_floors_move_the_disk_verdict() {
    const MIB: u64 = 1024 * 1024;
    let floors = DiskThresholds::from_rows(Some(200), Some(100));
    let cfg = PreflightConfig {
        disk_warn_bytes: floors.warn_bytes,
        disk_fail_bytes: floors.fail_bytes,
        ..PreflightConfig::default()
    };
    assert_eq!(disk_status(200 * MIB, &cfg), CheckStatus::Pass);
    assert_eq!(disk_status(200 * MIB - 1, &cfg), CheckStatus::Warn);
    assert_eq!(disk_status(100 * MIB, &cfg), CheckStatus::Warn);
    assert_eq!(disk_status(100 * MIB - 1, &cfg), CheckStatus::Fail);
    // …where the compiled floors would have called the same figure ample or doomed.
    let compiled = PreflightConfig::default();
    assert_eq!(disk_status(DEFAULT_DISK_WARN_BYTES, &compiled), CheckStatus::Pass);
    assert_eq!(disk_status(150 * MIB, &compiled), CheckStatus::Fail);
}

/// An unqueryable directory warns — a preflight must not ground the app on its own inability
/// to measure.
#[test]
fn unqueryable_free_space_warns() {
    let cfg = PreflightConfig::default();
    let probes = FnProbes::new().with_free_space_bytes(disk_unqueryable);
    let r = check_disk_headroom("journal", Path::new("/nope"), &cfg, &probes);
    assert_eq!(r.status, CheckStatus::Warn);
    assert!(r.message.contains("no such directory"));
}

/// Every configured dir is checked, and the configured path reaches the probe.
#[test]
fn every_configured_dir_is_checked() {
    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    let probes = healthy().with_free_space_bytes(move |p: &Path| {
        sink.lock().unwrap().push(p.display().to_string());
        Ok(DEFAULT_DISK_WARN_BYTES)
    });
    let dirs = vec![
        ("journal".to_string(), PathBuf::from("/data/journal")),
        ("hist".to_string(), PathBuf::from("/market_data/hist")),
    ];
    let cfg = PreflightConfig { dirs, ..PreflightConfig::default() };
    let report = run_preflight(&cfg, &probes, None);
    assert_eq!(seen.lock().unwrap().len(), 2);
    let disk_checks = report.checks.iter().filter(|c| c.name == CHECK_DISK).count();
    assert_eq!(disk_checks, 2, "one disk check per configured dir");
}

// ---- (d) network: the EXISTING NetProbe, read through its handle ----------------------------

#[test]
fn no_net_probe_wired_warns() {
    let r = check_network(None, true);
    assert_eq!(r.status, CheckStatus::Warn);
    assert_eq!(r.venue, None);
    assert_eq!(r.name, CHECK_NETWORK);
}

/// A run with NO venue to check is a paper run: no order will be placed, so connectivity is NOT
/// APPLICABLE. It used to WARN on every credential-free start (developer-addressed text, the
/// fires-forever-on-a-healthy-box shape the clock table was built to remove).
#[test]
fn a_run_with_no_venue_to_check_does_not_warn_about_a_missing_net_probe() {
    let report = run_preflight(&PreflightConfig::default(), &healthy(), None);
    let net: Vec<&CheckReport> = report.checks.iter().filter(|c| c.name == CHECK_NETWORK).collect();
    assert_eq!(net.len(), 1, "the row is still PRESENT, never vanished");
    assert_eq!(net[0].status, CheckStatus::NotApplicable, "{}", net[0].message);
    assert!(report.go());
}

/// …but a checked venue means a live mount is in play, so unknown liveness still WARNs.
#[test]
fn a_missing_net_probe_still_warns_when_a_venue_is_being_checked() {
    let cfg = PreflightConfig { clock_venues: vec!["binance".to_string()], ..Default::default() };
    let report = run_preflight(&cfg, &healthy(), None);
    let net = report.checks.iter().find(|c| c.name == CHECK_NETWORK).expect("a network row");
    assert_eq!(net.status, CheckStatus::Warn, "{}", net.message);
}

/// An unprobed handle's optimistic `internet_up() == true` is NOT a measurement: warn, not pass.
#[test]
fn an_unprobed_net_handle_warns_rather_than_passes() {
    let p = NetProbe::with_defaults();
    let h = p.handle();
    assert!(h.internet_up(), "precondition: optimistic");
    assert!(!h.has_probed(), "precondition: unmeasured");
    assert_eq!(check_network(Some(&h), true).status, CheckStatus::Warn);
}

#[test]
fn a_measured_up_net_probe_passes() {
    let p = net_probe(true);
    let r = check_network(Some(&p.handle()), true);
    assert_eq!(r.status, CheckStatus::Pass);
    assert!(r.remediation.is_empty());
}

#[test]
fn a_measured_down_net_probe_fails_globally() {
    let p = net_probe(false);
    let r = check_network(Some(&p.handle()), true);
    assert_eq!(r.status, CheckStatus::Fail);
    assert_eq!(r.venue, None, "network is global, so it flips the go bit");
}

/// The handle is Arc-shared state, so preflight never depends on the probe's lifetime.
#[test]
fn a_net_handle_outlives_its_probe() {
    let p = net_probe(false);
    let h = p.handle();
    drop(p);
    assert_eq!(check_network(Some(&h), true).status, CheckStatus::Fail);
}
