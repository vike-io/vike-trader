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
