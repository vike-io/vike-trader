use super::*;

fn file() -> &'static Path {
    Path::new("config.toml")
}

#[test]
fn a_row_sets_the_store_root() {
    let mut c = Config::default();
    c.apply(ConfigPatch { store_root: Some("/from/row".into()), ..Default::default() }, file())
        .unwrap();
    assert_eq!(c.store_root, Some(PathBuf::from("/from/row")));
}

/// The FILE half of the `state_dir` deletion. The ENV half is `crate::removed`'s
/// `the_unread_state_dir_is_refused_and_offers_no_replacement_line`; both must bite, because a
/// deployment can carry either spelling and neither configured anything.
#[test]
fn the_removed_state_dir_key_is_refused_and_does_not_point_at_the_state_root() {
    let err = Config::default()
        .apply(
            ConfigPatch { state_dir: Some("/srv/vike/state".into()), ..Default::default() },
            file(),
        )
        .unwrap_err()
        .to_string();
    assert!(err.starts_with("config.toml: state_dir = "), "{err}");
    assert!(err.contains("NOTHING read it"), "{err}");
    assert!(err.contains("<settings>/state"), "{err} must warn off the look-alike directory");
}

#[test]
fn cli_outranks_the_row() {
    let mut c = Config::default();
    c.apply(ConfigPatch { store_root: Some("/from/row".into()), ..Default::default() }, file())
        .unwrap();
    c.apply_cli(&CliOverrides { store_root: Some("/from/cli".into()), ..Default::default() })
        .unwrap();
    assert_eq!(c.store_root, Some(PathBuf::from("/from/cli")));
}

#[test]
fn an_unwritten_key_stays_unset() {
    let mut c = Config::default();
    c.apply(ConfigPatch { log_dir: Some("/logs".into()), ..Default::default() }, file()).unwrap();
    assert_eq!(c.log_dir, Some(PathBuf::from("/logs")));
    assert_eq!(c.datahub_addr, None, "unset stays unset — presence is the Studio's branch");
}

/// `check_addr` is the row layer's address rule, so it needs its own direct coverage through
/// `Config::apply`.
/// A value with no `:` must be rejected, naming the key.
#[test]
fn a_malformed_file_address_is_rejected() {
    let err = Config::default()
        .apply(
            ConfigPatch { datahub_addr: Some("localhost".to_string()), ..Default::default() },
            file(),
        )
        .unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("datahub_addr"), "{msg}");
    assert!(msg.contains("localhost"), "{msg}");
}

/// The REQ-2 advertisement key rides `tradehub_addr`'s exact wiring: the row layer with the
/// shared `check_addr` rule, and a malformed value rejected naming the key.
#[test]
fn datahub_advertise_addr_is_a_checked_row() {
    let mut c = Config::default();
    c.apply(
        ConfigPatch {
            datahub_advertise_addr: Some("127.0.0.1:7878".to_string()),
            ..Default::default()
        },
        file(),
    )
    .unwrap();
    assert_eq!(c.datahub_advertise_addr, Some("127.0.0.1:7878".to_string()));

    let err = Config::default()
        .apply(
            ConfigPatch {
                datahub_advertise_addr: Some("noport".to_string()),
                ..Default::default()
            },
            file(),
        )
        .unwrap_err();
    assert!(err.to_string().contains("datahub_advertise_addr"), "{err}");
    assert_eq!(
        Config::default().datahub_advertise_addr,
        None,
        "unset advertises nothing — the pre-REQ-2 Welcome"
    );
}

/// The daemon's SELF-REPORT override rides the same wiring one key over.
///
/// ⚠ **`None` here does NOT mean "report nothing"**, which is the one way this key differs
/// from every other address in this struct and the reason the last assertion is spelled out:
/// unset is the ORDINARY state, in which the daemon discovers its own address from the routing
/// table (`crates/vike-tradehub/src/self_address.rs`). An operator has to configure nothing to
/// see a real address; this key is the override for the cases a route lookup cannot answer —
/// NAT above all.
#[test]
fn tradehub_advertise_addr_is_a_checked_row() {
    // RFC 5737 documentation addresses: a real box's address must never reach a tracked file.
    let mut c = Config::default();
    c.apply(
        ConfigPatch {
            tradehub_advertise_addr: Some("203.0.113.7:7879".to_string()),
            ..Default::default()
        },
        file(),
    )
    .unwrap();
    assert_eq!(c.tradehub_advertise_addr, Some("203.0.113.7:7879".to_string()));

    let err = Config::default()
        .apply(
            ConfigPatch {
                tradehub_advertise_addr: Some("noport".to_string()),
                ..Default::default()
            },
            file(),
        )
        .unwrap_err();
    assert!(err.to_string().contains("tradehub_advertise_addr"), "{err}");
    assert_eq!(
        Config::default().tradehub_advertise_addr,
        None,
        "unset is the ORDINARY state: the daemon discovers its own address and reports that"
    );
}

/// `instance_origin` is a checked row, and a malformed value must be REFUSED naming the key.
///
/// The refusal is the half that matters: an instance silently running UNTAGGED is
/// indistinguishable from one that was never configured — which is the whole failure the tag
/// exists to end. So a typo must stop the process, never degrade to "no origin".
#[test]
fn instance_origin_is_a_checked_row_and_refuses_a_bad_tag() {
    let mut c = Config::default();
    assert_eq!(c.instance_origin, None, "unset is the default — today's untagged ids");
    c.apply(
        ConfigPatch { instance_origin: Some("West".to_string()), ..Default::default() },
        file(),
    )
    .unwrap();
    // Normalised by `InstanceOrigin::parse`, which is the ONE authority for the rule.
    assert_eq!(c.instance_origin.as_ref().map(|o| o.as_str()), Some("west"));

    let err = Config::default()
        .apply(
            ConfigPatch { instance_origin: Some("a-b".to_string()), ..Default::default() },
            file(),
        )
        .unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("instance_origin"), "{msg}");
    assert!(msg.contains("letters and digits"), "the reason must reach the operator: {msg}");
}

/// A blank value is rejected too — "must at least be non-blank" is a separate half of the
/// rule from "must contain a `:`", and both halves need to actually fire.
#[test]
fn a_blank_file_address_is_rejected() {
    let err = Config::default()
        .apply(ConfigPatch { tradehub_addr: Some("   ".to_string()), ..Default::default() }, file())
        .unwrap_err();
    assert!(err.to_string().contains("tradehub_addr"), "{err}");
}

/// A well-formed `host:port` value passes through unchanged — `check_addr` must not reject
/// what it is supposed to accept.
#[test]
fn a_well_formed_file_address_is_accepted() {
    let mut c = Config::default();
    c.apply(
        ConfigPatch {
            datahub_addr: Some("127.0.0.1:9000".to_string()),
            tradehub_addr: Some("0.0.0.0:9100".to_string()),
            ..Default::default()
        },
        file(),
    )
    .unwrap();
    assert_eq!(c.datahub_addr, Some("127.0.0.1:9000".to_string()));
    assert_eq!(c.tradehub_addr, Some("0.0.0.0:9100".to_string()));
}

/// The predicate every layer's check wraps — pinned directly so a mutant flipping `!`,
/// dropping the blank half, or dropping the colon half is caught here even before it reaches
/// any one layer's error shape.
#[test]
fn is_host_port_accepts_only_non_blank_colon_bearing_values() {
    assert!(is_host_port("127.0.0.1:9000"));
    assert!(is_host_port("localhost:80"));
    // A colon is non-whitespace, so padding around it still counts as non-blank content.
    assert!(is_host_port("   :   "));
    assert!(!is_host_port("localhost"));
    assert!(!is_host_port(""));
    assert!(!is_host_port("   "));
}

/// `apply_cli`'s `datahub_addr` check shares `is_host_port` with the file/env layers, and this
/// test drives a bad CLI address through `apply_cli` itself — without it, the whole check replaced
/// with `Ok(())`, or its `!` deleted, survives: a malformed value is rejected, naming the flag.
#[test]
fn a_malformed_cli_address_is_rejected() {
    let err = Config::default()
        .apply_cli(&CliOverrides {
            datahub_addr: Some("localhost".to_string()),
            ..Default::default()
        })
        .unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("addr"), "{msg}");
    assert!(msg.contains("localhost"), "{msg}");
}

/// The blank half of the rule, through the CLI layer.
#[test]
fn a_blank_cli_address_is_rejected() {
    let err = Config::default()
        .apply_cli(&CliOverrides { datahub_addr: Some("   ".to_string()), ..Default::default() })
        .unwrap_err();
    assert!(err.to_string().contains("addr"), "{err}");
}

/// The COMPUTE daemon's key goes through the SAME file-layer check as its siblings — it is a
/// `host:port` like the rest, and a key that skipped `check_addr` would accept a value the
/// dialler then fails on with a worse message.
#[test]
fn the_backtest_address_is_checked_like_every_other_address() {
    let mut c = Config::default();
    assert_eq!(c.backtest_addr, None, "unset stays unset — the default is the constant");
    c.apply(
        ConfigPatch { backtest_addr: Some("<host>:7880".to_string()), ..Default::default() },
        file(),
    )
    .unwrap();
    assert_eq!(c.backtest_addr, Some("<host>:7880".to_string()));

    let err = Config::default()
        .apply(
            ConfigPatch { backtest_addr: Some("localhost".to_string()), ..Default::default() },
            file(),
        )
        .unwrap_err();
    assert!(err.to_string().contains("backtest_addr"), "the key is named: {err}");
}

/// ⚠ The DEFAULT, pinned as a SEPARATION rather than as a number on its own. `7879` is the
/// live order-signing daemon's port and `7878` is the data server's; a compute default equal
/// to either would aim every client of that plane at a process that does not serve it, and one
/// of those two signs orders. The first writing of ruling 7 picked `7879`, which is exactly the
/// mistake this test exists to make unrepeatable.
#[test]
fn the_compute_default_is_neither_of_its_neighbours() {
    assert_eq!(DEFAULT_BACKTEST_ADDR, "127.0.0.1:7880");
    assert_ne!(DEFAULT_BACKTEST_ADDR, "127.0.0.1:7879", "that is the LIVE trading daemon");
    assert_ne!(DEFAULT_BACKTEST_ADDR, DEFAULT_DATAHUB_ADDR, "that is the DATA server");
    // …and it is a value the file layer would accept, so the three rungs cannot disagree about
    // what an address is.
    assert!(is_host_port(DEFAULT_BACKTEST_ADDR));
}

/// A well-formed value passes through `apply_cli` unchanged.
#[test]
fn a_well_formed_cli_address_is_accepted() {
    let mut c = Config::default();
    c.apply_cli(&CliOverrides {
        datahub_addr: Some("127.0.0.1:9000".to_string()),
        ..Default::default()
    })
    .unwrap();
    assert_eq!(c.datahub_addr, Some("127.0.0.1:9000".to_string()));
}

// -- the decision-0111 P3 rows ----------------------------------------------------------------

/// One `config.*` row through the file-layer apply, as `crate::mirror` drives it from the store.
fn apply_p3(patch: ConfigPatch) -> Result<Config, String> {
    let mut c = Config::default();
    c.apply(patch, file()).map(|()| c).map_err(|e| e.to_string())
}

/// `config.reconcile_policy` takes exactly the four words, one spelling each, and REFUSES the rest:
/// the variable's reader falls back to `hybrid` — which auto-applies `PositionDrift` — for a word
/// it does not know, so a typo'd row must never reach it.
#[test]
fn the_reconcile_policy_row_takes_the_four_words_and_refuses_everything_else() {
    for word in RECONCILE_POLICIES {
        let c = apply_p3(ConfigPatch { reconcile_policy: Some(word.into()), ..Default::default() })
            .unwrap_or_else(|e| panic!("{word}: {e}"));
        assert_eq!(c.reconcile_policy.as_deref(), Some(word));
    }
    for bad in ["quarintine", "Quarantine", "external_quarantine", "external", "", "SECRET"] {
        let msg =
            apply_p3(ConfigPatch { reconcile_policy: Some(bad.into()), ..Default::default() })
                .unwrap_err();
        assert!(msg.contains("reconcile_policy"), "{bad:?}: {msg}");
        assert!(msg.contains("external-quarantine"), "the refusal lists the words: {msg}");
        assert!(!msg.contains("SECRET"), "the refusal printed the value: {msg}");
    }
}

/// Every other P3 bound refuses a value the reader would silently replace with its default (or
/// misread), and takes an ordinary one.
#[test]
fn the_p3_bounds_refuse_what_the_reader_would_silently_replace() {
    let refused = [
        (
            "reconcile_lookback_ms",
            ConfigPatch { reconcile_lookback_ms: Some(0), ..Default::default() },
        ),
        (
            "journal_snapshot_every",
            ConfigPatch { journal_snapshot_every: Some(0), ..Default::default() },
        ),
        (
            "reconcile_balance_tol_abs",
            ConfigPatch { reconcile_balance_tol_abs: Some(-1.0), ..Default::default() },
        ),
        (
            "reconcile_balance_tol_rel",
            ConfigPatch { reconcile_balance_tol_rel: Some(f64::NAN), ..Default::default() },
        ),
        (
            "tradehub_control_rate",
            ConfigPatch { tradehub_control_rate: Some(0.0), ..Default::default() },
        ),
        (
            "tradehub_control_rate",
            ConfigPatch { tradehub_control_rate: Some(f64::INFINITY), ..Default::default() },
        ),
        ("pin_cores", ConfigPatch { pin_cores: Some("  ".into()), ..Default::default() }),
        (
            "datahub_live_resident",
            ConfigPatch { datahub_live_resident: Some(String::new()), ..Default::default() },
        ),
        (
            "datahub_bind_addr",
            ConfigPatch { datahub_bind_addr: Some("localhost".into()), ..Default::default() },
        ),
    ];
    for (key, patch) in refused {
        let msg = apply_p3(patch).unwrap_err();
        assert!(msg.contains(key), "{key}: {msg}");
    }

    let c = apply_p3(ConfigPatch {
        reconcile_interval_ms: Some(0),
        reconcile_audit_ms: Some(0),
        reconcile_lookback_ms: Some(1),
        reconcile_startup_delay_ms: Some(0),
        reconcile_balance_tol_abs: Some(0.0),
        reconcile_balance_tol_rel: Some(1e-4),
        tradehub_control_rate: Some(0.5),
        pin_cores: Some("md:28,core:29".into()),
        datahub_bind_addr: Some("127.0.0.1:7878".into()),
        datahub_live_resident: Some("binance:BTCUSDT:depth".into()),
        journal_snapshot_every: Some(1),
        ..Default::default()
    })
    .unwrap();
    assert_eq!(c.reconcile_interval_ms, Some(0), "0 is the reader's own 'no repeat' spelling");
    assert_eq!(c.reconcile_audit_ms, Some(0), "0 is the reader's own 'no audit' spelling");
    assert_eq!(c.tradehub_control_rate, Some(0.5));
    assert_eq!(c.journal_snapshot_every, Some(1));
}
