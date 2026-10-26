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

/// The backtesthub's key goes through the SAME file-layer check as its siblings — it is a
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

/// The gatewayhub's key goes through the SAME file-layer check as its siblings, and says nothing
/// about loopback: that rule is the daemon's bind guard (`vike_hub_proto::bind::bind_exposure` is a
/// crate above this one), so a public `host:port` is a well-formed ROW here.
#[test]
fn the_gatewayhub_address_is_checked_like_every_other_address() {
    let mut c = Config::default();
    assert_eq!(c.gatewayhub_addr, None, "unset stays unset — the default is the constant");
    c.apply(
        ConfigPatch { gatewayhub_addr: Some("127.0.0.1:7883".to_string()), ..Default::default() },
        file(),
    )
    .unwrap();
    assert_eq!(c.gatewayhub_addr, Some("127.0.0.1:7883".to_string()));

    let mut public = Config::default();
    public
        .apply(
            ConfigPatch { gatewayhub_addr: Some("0.0.0.0:7883".to_string()), ..Default::default() },
            file(),
        )
        .expect("the loopback rule is the daemon's, not the row's");

    let err = Config::default()
        .apply(
            ConfigPatch { gatewayhub_addr: Some("localhost".to_string()), ..Default::default() },
            file(),
        )
        .unwrap_err();
    assert!(err.to_string().contains("gatewayhub_addr"), "the key is named: {err}");
}

/// ⚠ The DEFAULT, pinned as a SEPARATION from every neighbour rather than as a number alone. Each
/// of 7878 (data), 7879 (the order-signing daemon), 7880 (backtesthub) and 7881 (strategy-builder)
/// is TAKEN on the box this daemon runs beside, and 7882 is the address the docs use as an example:
/// a default equal to any of them would aim the data daemon at a process that does not speak this
/// protocol, and one of those signs orders.
#[test]
fn the_gatewayhub_default_is_none_of_its_neighbours() {
    assert_eq!(DEFAULT_GATEWAYHUB_ADDR, "127.0.0.1:7883");
    for taken in [DEFAULT_DATAHUB_ADDR, DEFAULT_BACKTEST_ADDR] {
        assert_ne!(DEFAULT_GATEWAYHUB_ADDR, taken);
    }
    for port in ["7879", "7881", "7882"] {
        assert!(
            !DEFAULT_GATEWAYHUB_ADDR.ends_with(&format!(":{port}")),
            "{DEFAULT_GATEWAYHUB_ADDR} collides with a neighbour on :{port}"
        );
    }
    // …and it is a value the file layer would accept, so the rungs cannot disagree about what an
    // address is.
    assert!(is_host_port(DEFAULT_GATEWAYHUB_ADDR));
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

/// `config.reconcile_startup_delay_ms` has a CEILING, and it is not the reader's: the reader would
/// honour any `u32`. It is half the window in which a restored order mutes its strategy's tagged
/// resubmit, because the first pass is what ends that refusal — a pass that starts after the window
/// leaves the order resting unregistered beside the strategy's new quote. So one millisecond over
/// the bound is REFUSED, naming the key and the bound; the refusal never clamps; and `0`, the
/// default's `2000` and the bound itself are taken.
#[test]
fn the_startup_delay_is_refused_above_half_the_restore_window() {
    let bound = MAX_RECONCILE_STARTUP_DELAY_MS;
    for over in [bound + 1, bound * 2, u32::MAX] {
        let mut c = Config::default();
        let err = c
            .apply(
                ConfigPatch { reconcile_startup_delay_ms: Some(over), ..Default::default() },
                file(),
            )
            .unwrap_err()
            .to_string();
        assert!(
            err.starts_with("config.toml: reconcile_startup_delay_ms = "),
            "the refusal names the file and the KEY: {err}"
        );
        assert!(err.contains(&format!("maximum {bound}")), "{over}: it names the bound: {err}");
        assert_eq!(
            c.reconcile_startup_delay_ms, None,
            "{over}: refused, never clamped to the bound"
        );
    }

    for ok in [0, 2_000, bound] {
        let c =
            apply_p3(ConfigPatch { reconcile_startup_delay_ms: Some(ok), ..Default::default() })
                .unwrap_or_else(|e| panic!("{ok}: {e}"));
        assert_eq!(c.reconcile_startup_delay_ms, Some(ok));
    }
}

/// `config.node_ack_wait_ms` defaults to the control door's price hold plus a margin (a client that
/// hangs up inside the hold never reads its refusal), and its range is closed on both sides: the
/// bounds themselves are taken, one millisecond either side is REFUSED naming the key, the value and
/// both bounds, and a refusal leaves the field where it was — nothing clamps.
#[test]
fn the_node_ack_wait_outlasts_the_doors_price_hold_and_is_a_closed_range() {
    use crate::config::{ACK_WAIT_MARGIN_MS, DOOR_PRICE_WAIT_MS};
    assert_eq!(Config::default().node_ack_wait_ms, DEFAULT_NODE_ACK_WAIT_MS);
    assert_eq!(DEFAULT_NODE_ACK_WAIT_MS, DOOR_PRICE_WAIT_MS + ACK_WAIT_MARGIN_MS);
    assert_eq!((DOOR_PRICE_WAIT_MS, DEFAULT_NODE_ACK_WAIT_MS), (4_000, 6_000));
    const { assert!(DEFAULT_NODE_ACK_WAIT_MS > DOOR_PRICE_WAIT_MS, "the client outlasts the hold") };
    assert_eq!((MIN_NODE_ACK_WAIT_MS, MAX_NODE_ACK_WAIT_MS), (500, 30_000));

    for ok in [MIN_NODE_ACK_WAIT_MS, DEFAULT_NODE_ACK_WAIT_MS, 12_345, MAX_NODE_ACK_WAIT_MS] {
        let c = apply_p3(ConfigPatch { node_ack_wait_ms: Some(ok), ..Default::default() })
            .unwrap_or_else(|e| panic!("{ok}: {e}"));
        assert_eq!(c.node_ack_wait_ms, ok);
    }
    for bad in [0, MIN_NODE_ACK_WAIT_MS - 1, MAX_NODE_ACK_WAIT_MS + 1, u32::MAX] {
        let mut c = Config::default();
        let err = c
            .apply(ConfigPatch { node_ack_wait_ms: Some(bad), ..Default::default() }, file())
            .unwrap_err()
            .to_string();
        assert!(
            err.starts_with(&format!("config.toml: node_ack_wait_ms = {bad} is outside")),
            "the refusal names the file, the KEY and the value: {err}"
        );
        assert!(err.contains("500..=30000"), "{bad}: it names both bounds: {err}");
        assert_eq!(c.node_ack_wait_ms, DEFAULT_NODE_ACK_WAIT_MS, "{bad}: refused, never clamped");
    }
}

// -- the trading-mount batch ------------------------------------------------------------------

/// `config.paper_slippage` takes a finite fraction from `0` to the ceiling, both ends included, and
/// REFUSES everything else naming the key and the bound: a negative band pays the trader to be
/// filled, a non-finite one poisons every fill price, and a figure like `2` is `0.0002` mistyped as
/// a percent. Nothing is clamped — the stored row would otherwise differ from the one filed.
#[test]
fn the_paper_slippage_row_takes_zero_to_the_ceiling_and_refuses_everything_else() {
    for ok in [0.0, 0.0002, 0.001, MAX_PAPER_SLIPPAGE] {
        let c = apply_p3(ConfigPatch { paper_slippage: Some(ok), ..Default::default() })
            .unwrap_or_else(|e| panic!("{ok}: {e}"));
        assert_eq!(c.paper_slippage, Some(ok));
    }
    let past_the_ceiling = MAX_PAPER_SLIPPAGE + 1e-9;
    for bad in [-0.0001, past_the_ceiling, 2.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let mut c = Config::default();
        let err = c
            .apply(ConfigPatch { paper_slippage: Some(bad), ..Default::default() }, file())
            .unwrap_err()
            .to_string();
        assert!(err.starts_with("config.toml: paper_slippage = "), "{bad}: the KEY leads: {err}");
        assert!(err.contains("0.05"), "{bad}: it names the ceiling: {err}");
        assert_eq!(c.paper_slippage, None, "{bad}: refused, never clamped");
    }
}

/// The two preflight disk floors are each in `1..=MAX_PREFLIGHT_DISK_MIB` — `0` is a floor that can
/// never be crossed (a check switched off by accident) and a larger figure is bytes written where
/// MiB are read — and the refusal names the key and the range. Nothing is clamped.
#[test]
fn each_preflight_disk_floor_is_refused_outside_its_range() {
    let max = MAX_PREFLIGHT_DISK_MIB;
    for bad in [0, max + 1, u32::MAX] {
        for key in ["preflight_disk_warn_mib", "preflight_disk_fail_mib"] {
            // The range is judged before the pair, so only the RANGE can be what refuses here.
            let patch = if key == "preflight_disk_warn_mib" {
                ConfigPatch { preflight_disk_warn_mib: Some(bad), ..Default::default() }
            } else {
                ConfigPatch { preflight_disk_fail_mib: Some(bad), ..Default::default() }
            };
            let mut c = Config::default();
            let err = c.apply(patch, file()).unwrap_err().to_string();
            assert!(err.starts_with(&format!("config.toml: {key} = ")), "{key}={bad}: {err}");
            assert!(err.contains(&format!("1..={max}")), "{key}={bad}: it names the range: {err}");
            assert_eq!(c, Config::default(), "{key}={bad}: refused, nothing written");
        }
    }
    for (warn, fail) in [(2, 1), (max, 1), (max, max - 1)] {
        let c = apply_p3(ConfigPatch {
            preflight_disk_warn_mib: Some(warn),
            preflight_disk_fail_mib: Some(fail),
            ..Default::default()
        })
        .unwrap_or_else(|e| panic!("{warn}/{fail}: {e}"));
        assert_eq!(
            (c.preflight_disk_warn_mib, c.preflight_disk_fail_mib),
            (Some(warn), Some(fail))
        );
    }
}

/// **The pair must order — warn ABOVE fail — and a floor not written counts at its DEFAULT.** A
/// `warn` at or below the `fail` floor is a warning that can never be reached, so it is refused
/// naming BOTH keys (the key written leads, the other is in the message), at the write and at the
/// load. The defaults are the compiled floors, so one key alone can still be refused for its twin.
#[test]
fn the_preflight_disk_pair_must_order_with_unwritten_floors_at_their_defaults() {
    let warn_default = DEFAULT_PREFLIGHT_DISK_WARN_MIB;
    let fail_default = DEFAULT_PREFLIGHT_DISK_FAIL_MIB;
    assert!(warn_default > fail_default, "premise: the defaults order");

    let refused = |warn: Option<u32>, fail: Option<u32>| {
        let mut c = Config::default();
        let patch = ConfigPatch {
            preflight_disk_warn_mib: warn,
            preflight_disk_fail_mib: fail,
            ..Default::default()
        };
        let err = c.apply(patch, file()).unwrap_err().to_string();
        assert_eq!(c, Config::default(), "{warn:?}/{fail:?}: refused, nothing written");
        err
    };
    let both_keys_named = |err: &str| {
        assert!(err.contains("preflight_disk_warn_mib"), "{err}");
        assert!(err.contains("preflight_disk_fail_mib"), "{err}");
    };

    // Both written, the wrong way round and equal: the WARN key leads.
    for (warn, fail) in [(100, 500), (500, 500)] {
        let err = refused(Some(warn), Some(fail));
        assert!(err.starts_with("config.toml: preflight_disk_warn_mib = "), "{err}");
        both_keys_named(&err);
    }
    // `warn` alone, at or below the DEFAULT fail floor.
    for warn in [1, fail_default - 1, fail_default] {
        let err = refused(Some(warn), None);
        assert!(err.starts_with("config.toml: preflight_disk_warn_mib = "), "{err}");
        both_keys_named(&err);
        assert!(err.contains(&format!("({fail_default})")), "it states the default it met: {err}");
    }
    // `fail` alone, at or above the DEFAULT warn floor: the FAIL key leads.
    for fail in [warn_default, warn_default + 1, MAX_PREFLIGHT_DISK_MIB] {
        let err = refused(None, Some(fail));
        assert!(err.starts_with("config.toml: preflight_disk_fail_mib = "), "{err}");
        both_keys_named(&err);
        assert!(err.contains(&format!("({warn_default})")), "it states the default it met: {err}");
    }
    // …and a floor already held by an earlier layer counts as written, not as the default.
    let mut c = Config { preflight_disk_warn_mib: Some(8_000), ..Config::default() };
    let err = c
        .apply(ConfigPatch { preflight_disk_fail_mib: Some(9_000), ..Default::default() }, file())
        .unwrap_err()
        .to_string();
    both_keys_named(&err);
    assert_eq!(c.preflight_disk_fail_mib, None);

    // What orders is taken: the pair, and either key alone against the other's default.
    for (warn, fail) in
        [(Some(500), Some(100)), (Some(fail_default + 1), None), (None, Some(warn_default - 1))]
    {
        let c = apply_p3(ConfigPatch {
            preflight_disk_warn_mib: warn,
            preflight_disk_fail_mib: fail,
            ..Default::default()
        })
        .unwrap_or_else(|e| panic!("{warn:?}/{fail:?}: {e}"));
        assert_eq!((c.preflight_disk_warn_mib, c.preflight_disk_fail_mib), (warn, fail));
    }
}

/// `config.inflight_confirm_ms` is `0` (off) or a cadence in `MIN..=MAX`, both ends included, and
/// REFUSES everything between and beyond naming the key and the range: a sub-second cadence is a
/// request storm against a venue that is already slow, and past a minute the rung is behind the
/// stuck-order ladder it exists to beat. Nothing is clamped.
#[test]
fn the_inflight_confirm_row_is_off_or_a_cadence_in_range() {
    for ok in [0, MIN_INFLIGHT_CONFIRM_MS, 2_000, MAX_INFLIGHT_CONFIRM_MS] {
        let c = apply_p3(ConfigPatch { inflight_confirm_ms: Some(ok), ..Default::default() })
            .unwrap_or_else(|e| panic!("{ok}: {e}"));
        assert_eq!(c.inflight_confirm_ms, Some(ok), "{ok}: stored as written, `0` included");
    }
    for bad in [1, 500, MIN_INFLIGHT_CONFIRM_MS - 1, MAX_INFLIGHT_CONFIRM_MS + 1, u32::MAX] {
        let mut c = Config::default();
        let err = c
            .apply(ConfigPatch { inflight_confirm_ms: Some(bad), ..Default::default() }, file())
            .unwrap_err()
            .to_string();
        assert!(
            err.starts_with("config.toml: inflight_confirm_ms = "),
            "{bad}: the KEY leads: {err}"
        );
        assert!(
            err.contains(&format!("{MIN_INFLIGHT_CONFIRM_MS}..={MAX_INFLIGHT_CONFIRM_MS}")),
            "{bad}: it names the range: {err}"
        );
        assert!(err.contains("or 0 for off"), "{bad}: it names the off word: {err}");
        assert_eq!(c.inflight_confirm_ms, None, "{bad}: refused, never clamped");
    }
}

/// `config.strategy_state_save_ms` is `0` (off) or a cadence in `MIN..=MAX`, both ends included, and
/// REFUSES everything between and beyond naming the key and the range: every save is an fsynced
/// file write on the core thread, and past an hour a crash loses more than the clean-shutdown save
/// would have kept. Nothing is clamped.
#[test]
fn the_state_save_row_is_off_or_a_cadence_in_range() {
    for ok in [0, MIN_STRATEGY_STATE_SAVE_MS, 60_000, MAX_STRATEGY_STATE_SAVE_MS] {
        let c = apply_p3(ConfigPatch { strategy_state_save_ms: Some(ok), ..Default::default() })
            .unwrap_or_else(|e| panic!("{ok}: {e}"));
        assert_eq!(c.strategy_state_save_ms, Some(ok), "{ok}: stored as written, `0` included");
    }
    for bad in [1, 999, MIN_STRATEGY_STATE_SAVE_MS - 1, MAX_STRATEGY_STATE_SAVE_MS + 1, u32::MAX] {
        let mut c = Config::default();
        let patch = ConfigPatch { strategy_state_save_ms: Some(bad), ..Default::default() };
        let err = c.apply(patch, file()).unwrap_err().to_string();
        assert!(
            err.starts_with("config.toml: strategy_state_save_ms = "),
            "{bad}: the KEY leads: {err}"
        );
        assert!(
            err.contains(&format!("{MIN_STRATEGY_STATE_SAVE_MS}..={MAX_STRATEGY_STATE_SAVE_MS}")),
            "{bad}: it names the range: {err}"
        );
        assert!(err.contains("or 0 for off"), "{bad}: it names the off word: {err}");
        assert_eq!(c.strategy_state_save_ms, None, "{bad}: refused, never clamped");
    }
}

/// A patch that names none of the batch's keys changes nothing: the default `Config` is what the
/// mount read before the rows existed.
#[test]
fn a_patch_without_the_batch_keys_leaves_every_one_of_them_unset() {
    let c = apply_p3(ConfigPatch::default()).unwrap();
    assert_eq!(c, Config::default());
    assert_eq!(c.paper_slippage, None);
    assert_eq!((c.preflight_disk_warn_mib, c.preflight_disk_fail_mib), (None, None));
    assert_eq!(c.inflight_confirm_ms, None);
    assert_eq!(c.strategy_state_save_ms, None);
}

// -- the data plane's per-box tuning rows ---------------------------------------------------------

/// One data-plane row: its key, its range, a patch carrying a value for it, and the field it fills.
type DataPlaneRow =
    (&'static str, RangeInclusive<u32>, fn(u32) -> ConfigPatch, fn(&Config) -> Option<u32>);

/// A row of the table below. A function, because its arguments are the coercion site that turns
/// each non-capturing closure into the `fn` pointer the tuple holds (a tuple inside `vec![]` is not).
fn data_plane_row(
    key: &'static str,
    range: RangeInclusive<u32>,
    patch: fn(u32) -> ConfigPatch,
    field: fn(&Config) -> Option<u32>,
) -> DataPlaneRow {
    (key, range, patch, field)
}

/// The seven rows, spelled once for the tests below. A row added to `Config` for the data plane
/// belongs here: the default-table test at the end fails until it is.
fn data_plane_rows() -> Vec<DataPlaneRow> {
    vec![
        data_plane_row(
            "datahub_catalog_ttl_secs",
            DATAHUB_CATALOG_TTL_SECS,
            |v| ConfigPatch { datahub_catalog_ttl_secs: Some(v), ..Default::default() },
            |c| c.datahub_catalog_ttl_secs,
        ),
        data_plane_row(
            "recorder_silent_secs",
            RECORDER_SILENT_SECS,
            |v| ConfigPatch { recorder_silent_secs: Some(v), ..Default::default() },
            |c| c.recorder_silent_secs,
        ),
        data_plane_row(
            "recorder_tick_secs",
            RECORDER_TICK_SECS,
            |v| ConfigPatch { recorder_tick_secs: Some(v), ..Default::default() },
            |c| c.recorder_tick_secs,
        ),
        data_plane_row(
            "recorder_flush_max_rows",
            RECORDER_FLUSH_MAX_ROWS,
            |v| ConfigPatch { recorder_flush_max_rows: Some(v), ..Default::default() },
            |c| c.recorder_flush_max_rows,
        ),
        data_plane_row(
            "recorder_flush_max_age_secs",
            RECORDER_FLUSH_MAX_AGE_SECS,
            |v| ConfigPatch { recorder_flush_max_age_secs: Some(v), ..Default::default() },
            |c| c.recorder_flush_max_age_secs,
        ),
        data_plane_row(
            "datahub_chain_cadence_secs",
            DATAHUB_CHAIN_CADENCE_SECS,
            |v| ConfigPatch { datahub_chain_cadence_secs: Some(v), ..Default::default() },
            |c| c.datahub_chain_cadence_secs,
        ),
        data_plane_row(
            "datahub_options_poll_secs",
            DATAHUB_OPTIONS_POLL_SECS,
            |v| ConfigPatch { datahub_options_poll_secs: Some(v), ..Default::default() },
            |c| c.datahub_options_poll_secs,
        ),
    ]
}

/// A box without the rows is the box that existed before them: every field is unset, and applying a
/// patch that names none of them leaves it so.
#[test]
fn a_box_without_the_data_plane_rows_has_every_one_unset() {
    let untouched = apply_p3(ConfigPatch::default()).unwrap();
    for (key, _, _, field) in data_plane_rows() {
        assert_eq!(field(&Config::default()), None, "{key}: the default is unset");
        assert_eq!(field(&untouched), None, "{key}: an empty patch leaves it unset");
    }
}

/// Both ends of every range are taken, one step outside either end is REFUSED naming the key and
/// the range, and a refusal stores nothing — the value is never clamped to the bound.
#[test]
fn every_data_plane_row_takes_both_ends_of_its_range_and_refuses_one_step_outside() {
    for (key, range, patch, field) in data_plane_rows() {
        let (lo, hi) = (*range.start(), *range.end());
        for ok in [lo, hi, lo + (hi - lo) / 2] {
            let c = apply_p3(patch(ok)).unwrap_or_else(|e| panic!("{key} = {ok}: {e}"));
            assert_eq!(field(&c), Some(ok), "{key}: the value is stored as written");
        }
        for bad in [lo - 1, hi + 1, u32::MAX] {
            let mut c = Config::default();
            let err = c.apply(patch(bad), file()).unwrap_err().to_string();
            assert!(
                err.starts_with(&format!("config.toml: {key} = {bad} ")),
                "{key} = {bad}: the refusal names the file, the key and the value: {err}"
            );
            assert!(
                err.contains(&format!("allowed range {lo} to {hi}")),
                "{key} = {bad}: and the range: {err}"
            );
            assert_eq!(field(&c), None, "{key} = {bad}: refused, never clamped");
        }
    }
}

/// `config.recorder_silent_secs` takes `0` — the flag's own "watchdog off" — from OUTSIDE its range,
/// and nothing else outside it: the gap between `0` and the floor is refused like any other value.
#[test]
fn the_silence_threshold_takes_zero_as_off_and_refuses_the_gap_under_its_floor() {
    let silent = |v: u32| {
        apply_p3(ConfigPatch { recorder_silent_secs: Some(v), ..Default::default() })
            .map(|c| c.recorder_silent_secs)
    };
    assert_eq!(silent(0), Ok(Some(0)), "0 switches the watchdog off");
    for gap in [1, 30, RECORDER_SILENT_SECS.start() - 1] {
        let err = silent(gap).unwrap_err();
        assert!(err.contains("recorder_silent_secs") && err.contains("write 0"), "{gap}: {err}");
    }
    assert_eq!(silent(*RECORDER_SILENT_SECS.start()), Ok(Some(60)));
}

/// The ranges are ordered, and the shipped default of every row is a value its own range takes, so
/// writing the default back is legal. (The defaults live in the crates that read them; the pin
/// tests there hold them equal to these numbers.)
#[test]
fn the_data_plane_ranges_are_ordered_and_hold_their_shipped_defaults() {
    let defaults: [(&str, u32); 7] = [
        ("datahub_catalog_ttl_secs", 6 * 60 * 60),
        ("recorder_silent_secs", 300),
        ("recorder_tick_secs", 30),
        ("recorder_flush_max_rows", 5_000),
        ("recorder_flush_max_age_secs", 30),
        ("datahub_chain_cadence_secs", 60),
        ("datahub_options_poll_secs", 12),
    ];
    let rows = data_plane_rows();
    assert_eq!(rows.len(), defaults.len(), "one default per row");
    for ((key, range, _, _), (named, default)) in rows.iter().zip(defaults) {
        assert_eq!(*key, named, "the tables list the rows in the same order");
        assert!(range.start() <= range.end(), "{key}: an empty range refuses everything");
        assert!(range.contains(&default), "{key}: the default {default} is outside {range:?}");
    }
}

// -- the connection-timing rows ----------------------------------------------------------------

/// The restated ranges are the numbers the daemons' pin tests hold against the runtime constants, so
/// a deliberate edit here is a visible edit to this file as well.
#[test]
fn the_connection_timing_ranges_are_where_they_were_argued() {
    assert_eq!(
        (NODE_IDLE_READ_TIMEOUT_MIN_MS, NODE_IDLE_READ_TIMEOUT_MAX_MS),
        (120_000, 3_600_000)
    );
    assert_eq!((MD_HEARTBEAT_MIN_MS, MD_HEARTBEAT_MAX_MS), (1_000, 60_000));
}

/// A box without either row resolves neither: the daemons then run the constants they always ran.
#[test]
fn the_connection_timing_rows_default_to_absent() {
    let c = Config::default();
    assert_eq!(c.node_idle_read_timeout_ms, None);
    assert_eq!(c.md_heartbeat_ms, None);
}

/// `config.node_idle_read_timeout_ms` takes both ends of its range and the default's own value, and
/// REFUSES one millisecond outside either end, naming the key and the bound it missed. The refusal
/// never clamps: the field is still unset afterwards.
#[test]
fn the_idle_read_timeout_row_is_refused_outside_its_range_and_never_clamped() {
    let (min, max) = (NODE_IDLE_READ_TIMEOUT_MIN_MS, NODE_IDLE_READ_TIMEOUT_MAX_MS);
    for ok in [min, 300_000, max] {
        let c = apply_p3(ConfigPatch { node_idle_read_timeout_ms: Some(ok), ..Default::default() })
            .unwrap_or_else(|e| panic!("{ok}: {e}"));
        assert_eq!(c.node_idle_read_timeout_ms, Some(ok));
    }
    for (bad, needle) in [
        (0, format!("minimum {min}")),
        (min - 1, format!("minimum {min}")),
        (max + 1, format!("maximum {max}")),
        (u32::MAX, format!("maximum {max}")),
    ] {
        let mut c = Config::default();
        let err = c
            .apply(
                ConfigPatch { node_idle_read_timeout_ms: Some(bad), ..Default::default() },
                file(),
            )
            .unwrap_err()
            .to_string();
        assert!(
            err.starts_with("config.toml: node_idle_read_timeout_ms = "),
            "the refusal names the file and the KEY: {err}"
        );
        assert!(err.contains(&needle), "{bad}: it names the bound it missed: {err}");
        assert_eq!(c.node_idle_read_timeout_ms, None, "{bad}: refused, never clamped");
    }
}

/// `config.md_heartbeat_ms` is the same shape over its own range; `0` (a heartbeat written
/// continuously) is below the floor and refused.
#[test]
fn the_heartbeat_row_is_refused_outside_its_range_and_never_clamped() {
    let (min, max) = (MD_HEARTBEAT_MIN_MS, MD_HEARTBEAT_MAX_MS);
    for ok in [min, 15_000, max] {
        let c = apply_p3(ConfigPatch { md_heartbeat_ms: Some(ok), ..Default::default() })
            .unwrap_or_else(|e| panic!("{ok}: {e}"));
        assert_eq!(c.md_heartbeat_ms, Some(ok));
    }
    for (bad, needle) in [
        (0, format!("minimum {min}")),
        (min - 1, format!("minimum {min}")),
        (max + 1, format!("maximum {max}")),
        (u32::MAX, format!("maximum {max}")),
    ] {
        let mut c = Config::default();
        let err = c
            .apply(ConfigPatch { md_heartbeat_ms: Some(bad), ..Default::default() }, file())
            .unwrap_err()
            .to_string();
        assert!(
            err.starts_with("config.toml: md_heartbeat_ms = "),
            "the refusal names the file and the KEY: {err}"
        );
        assert!(err.contains(&needle), "{bad}: it names the bound it missed: {err}");
        assert_eq!(c.md_heartbeat_ms, None, "{bad}: refused, never clamped");
    }
}

// -- config.recorder_retention_days_by_kind and the tool-feed cadences ----------------------------

/// The retention row is stored as TEXT, as written: the reader parses it again with the same
/// function, so a refusal at the write is the only one there will ever be.
#[test]
fn the_retention_row_is_kept_as_written_and_the_empty_row_is_taken() {
    for ok in ["book:14,depth:14,trade:90", "bar:1", ""] {
        let c = apply_p3(ConfigPatch {
            recorder_retention_days_by_kind: Some(ok.into()),
            ..Default::default()
        })
        .unwrap_or_else(|e| panic!("{ok:?}: {e}"));
        assert_eq!(c.recorder_retention_days_by_kind.as_deref(), Some(ok));
    }
    assert_eq!(Config::default().recorder_retention_days_by_kind, None, "absent is the default");
}

/// Every grammar refusal reaches the operator through `Config::apply` naming the FILE and the KEY,
/// and leaves the field unset.
#[test]
fn the_retention_row_refuses_a_bad_grammar_naming_the_key() {
    for bad in
        ["exec_fill:30", "book:0", "bok:14", "book:14,book:30", "book:14, trade:90", "book", ","]
    {
        let mut c = Config::default();
        let err = c
            .apply(
                ConfigPatch {
                    recorder_retention_days_by_kind: Some(bad.into()),
                    ..Default::default()
                },
                file(),
            )
            .unwrap_err()
            .to_string();
        assert!(
            err.starts_with("config.toml: recorder_retention_days_by_kind = "),
            "{bad:?}: the refusal names the file and the KEY: {err}"
        );
        assert_eq!(c.recorder_retention_days_by_kind, None, "{bad:?}: refused, never kept");
    }
}

/// The three cadence rows take both ends of 60..=86400, and refuse one second outside either end —
/// at the write, never clamped — naming the bound that was broken.
#[test]
fn the_feed_cadence_rows_take_both_ends_and_refuse_one_outside_each() {
    /// The patch that sets the cadence row named `key` to `v`.
    fn patch(key: &str, v: u32) -> ConfigPatch {
        match key {
            "datahub_news_poll_secs" => {
                ConfigPatch { datahub_news_poll_secs: Some(v), ..Default::default() }
            }
            "datahub_calendar_poll_secs" => {
                ConfigPatch { datahub_calendar_poll_secs: Some(v), ..Default::default() }
            }
            "datahub_equity_poll_secs" => {
                ConfigPatch { datahub_equity_poll_secs: Some(v), ..Default::default() }
            }
            other => panic!("no cadence row {other}"),
        }
    }
    for key in ["datahub_news_poll_secs", "datahub_calendar_poll_secs", "datahub_equity_poll_secs"]
    {
        for ok in [MIN_FEED_POLL_SECS, 300, 1_800, MAX_FEED_POLL_SECS] {
            apply_p3(patch(key, ok)).unwrap_or_else(|e| panic!("{key}={ok}: {e}"));
        }
        for (bad, word) in [
            (0, "minimum"),
            (MIN_FEED_POLL_SECS - 1, "minimum"),
            (MAX_FEED_POLL_SECS + 1, "maximum"),
            (u32::MAX, "maximum"),
        ] {
            let mut c = Config::default();
            let err = c.apply(patch(key, bad), file()).unwrap_err().to_string();
            assert!(err.starts_with(&format!("config.toml: {key} = {bad} ")), "{key}: {err}");
            assert!(err.contains(word), "{key}={bad}: it names the {word}: {err}");
            assert_eq!(c, Config::default(), "{key}={bad}: refused, never clamped");
        }
    }
    let c = apply_p3(ConfigPatch {
        datahub_news_poll_secs: Some(60),
        datahub_calendar_poll_secs: Some(3_600),
        datahub_equity_poll_secs: Some(86_400),
        ..Default::default()
    })
    .unwrap();
    assert_eq!(
        (c.datahub_news_poll_secs, c.datahub_calendar_poll_secs, c.datahub_equity_poll_secs),
        (Some(60), Some(3_600), Some(86_400)),
        "each row lands in its own field"
    );
}

// -- config.gatewayhub_connect_wait_ms ------------------------------------------------------------

/// The connect wait's range and default are where they were argued, and a box without the row
/// resolves nothing: the tradehub then runs [`DEFAULT_GATEWAYHUB_CONNECT_WAIT_MS`]. A deliberate edit
/// to either end is a visible edit here as well, and the default sits inside its own range.
#[test]
fn the_connect_wait_range_and_default_are_where_they_were_argued() {
    assert_eq!(
        (GATEWAYHUB_CONNECT_WAIT_MIN_MS, DEFAULT_GATEWAYHUB_CONNECT_WAIT_MS),
        (1_000, 60_000)
    );
    assert_eq!(GATEWAYHUB_CONNECT_WAIT_MAX_MS, 600_000);
    assert!(
        (GATEWAYHUB_CONNECT_WAIT_MIN_MS..=GATEWAYHUB_CONNECT_WAIT_MAX_MS)
            .contains(&DEFAULT_GATEWAYHUB_CONNECT_WAIT_MS)
    );
    assert_eq!(Config::default().gatewayhub_connect_wait_ms, None);
}

/// `config.gatewayhub_connect_wait_ms` takes both ends of its range and the default's own value,
/// and REFUSES one millisecond outside either end (and zero, which would mean both "no wait" and
/// "unset"), naming the key and the bound it missed. The refusal never clamps.
#[test]
fn the_connect_wait_row_is_refused_outside_its_range_and_never_clamped() {
    let (min, max) = (GATEWAYHUB_CONNECT_WAIT_MIN_MS, GATEWAYHUB_CONNECT_WAIT_MAX_MS);
    for ok in [min, DEFAULT_GATEWAYHUB_CONNECT_WAIT_MS, max] {
        let c =
            apply_p3(ConfigPatch { gatewayhub_connect_wait_ms: Some(ok), ..Default::default() })
                .unwrap_or_else(|e| panic!("{ok}: {e}"));
        assert_eq!(c.gatewayhub_connect_wait_ms, Some(ok));
    }
    for (bad, needle) in [
        (0, format!("minimum {min}")),
        (min - 1, format!("minimum {min}")),
        (max + 1, format!("maximum {max}")),
        (u32::MAX, format!("maximum {max}")),
    ] {
        let mut c = Config::default();
        let err = c
            .apply(
                ConfigPatch { gatewayhub_connect_wait_ms: Some(bad), ..Default::default() },
                file(),
            )
            .unwrap_err()
            .to_string();
        assert!(
            err.starts_with("config.toml: gatewayhub_connect_wait_ms = "),
            "the refusal names the file and the KEY: {err}"
        );
        assert!(err.contains(&needle), "{bad}: it names the bound it missed: {err}");
        assert_eq!(c.gatewayhub_connect_wait_ms, None, "{bad}: refused, never clamped");
    }
}
