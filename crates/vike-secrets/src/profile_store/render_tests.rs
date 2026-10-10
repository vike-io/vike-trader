use std::collections::BTreeMap;

use super::*;

fn stored(kind: ProfileKind, settings: &[(&str, &str)], mounts: Vec<MountRow>) -> StoredProfile {
    StoredProfile {
        row: ProfileRow { name: "p".into(), kind, active: false, note: None },
        mounts,
        params: BTreeMap::new(),
        settings: settings.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect(),
        recorder: None,
    }
}

/// **The three shape rules [`render_run_toml`] exists to get right**, each of which produces an
/// INVALID document when it is missed — so this is a parse check, not a formatting preference.
///
/// 1. `mode` sorts AFTER `guards.*` in a `BTreeMap`, so emitting rows in path order puts a bare
///    key after a header. TOML refuses that.
/// 2. `guards.freshness_ms` < `guards.margin_call.buffer` < `guards.max_drawdown` in path
///    order, so a naive walk opens `[guards]`, opens `[guards.margin_call]`, then RE-OPENS
///    `[guards]`. TOML refuses a duplicate table.
/// 3. A parent table must precede its child, which lexicographic order over the dotted table
///    paths gives for free — asserted rather than assumed.
#[test]
fn a_run_document_renders_valid_toml_whatever_order_the_paths_sort_in() {
    let doc = render_run_toml(&stored(
        ProfileKind::Run,
        &[
            ("guards.freshness_ms", "500"),
            ("guards.margin_call.buffer", "0.1"),
            ("guards.max_drawdown", "0.25"),
            ("mode", "\"live\""),
            ("name", "\"rt\""),
            ("risk.max_notional_per_order", "100.0"),
            ("sinks.gui", "true"),
            ("sinks.journal.dir", "\"/var/wal\""),
        ],
        Vec::new(),
    ));
    // Rule 1: every bare key precedes the first header.
    let first_header = doc.find('[').expect("the document has tables");
    assert!(
        doc[..first_header].contains("mode = ") && doc[..first_header].contains("name = "),
        "a top-level key landed after a header, which TOML refuses:\n{doc}"
    );
    // Rule 2: one header per table.
    assert_eq!(doc.matches("[guards]").count(), 1, "duplicate `[guards]` header:\n{doc}");
    // Rule 3: the parent precedes the child.
    assert!(
        doc.find("[guards]") < doc.find("[guards.margin_call]"),
        "a child table preceded its parent:\n{doc}"
    );
    assert!(doc.find("[sinks]") < doc.find("[sinks.journal]"), "{doc}");

    // ...and the whole thing is a document a parser accepts, with every value where it belongs.
    // (Asserted here as a STRUCTURAL check — this crate carries no `toml` dependency, so the
    // parse-equality half is `crates/vike-cli/src/cmd/config/mirror_profile.rs`'s fence and
    // `crates/vike-tradehub/tests/daemon/profile_rows.rs`'s reload through the REAL parser.)
    for want in [
        "mode = \"live\"",
        "freshness_ms = 500",
        "buffer = 0.1",
        "max_drawdown = 0.25",
        "max_notional_per_order = 100.0",
        "dir = \"/var/wal\"",
    ] {
        assert!(doc.contains(want), "`{want}` missing from:\n{doc}");
    }
}

/// **THE SINGLE-MOUNT SPELLING, and the three conditions that decide it.** A deployment whose
/// selection moves to a row must keep its state-sidecar key, and the key is derived from the
/// spelling — see [`render_daemon_toml`]'s doc.
#[test]
fn one_unprimaried_mount_at_ordinal_zero_renders_the_single_mount_spelling() {
    let mut m = MountRow::new(0, "bybit", "CryptoPerp");
    m.token_id = Some("BTCUSDT".into());
    m.interval = Some("1m".into());
    m.qty = Some(0.001);
    let doc = render_daemon_toml(&stored(
        ProfileKind::Daemon,
        &[("daemon.summary_ms", "60000")],
        vec![m.clone()],
    ))
    .expect("renders");
    assert!(!doc.contains("[[mounts]]"), "the spelling moved:\n{doc}");
    // Rule 1 again: the bare mount keys precede `[daemon]`.
    assert!(doc.find("venue = ").unwrap() < doc.find("[daemon]").unwrap(), "{doc}");
    assert!(doc.contains("asset_class = \"CryptoPerp\""), "always emitted:\n{doc}");
    assert!(doc.contains("qty = 0.001"), "{doc}");
    assert!(!doc.contains("primary"), "a single-mount profile carries no `primary` key:\n{doc}");

    // ...and each of the three conditions ALONE flips it back to the array spelling.
    let mut declared = m.clone();
    declared.is_primary = true;
    for (why, mounts) in [
        ("a declared primary", vec![declared]),
        ("a non-zero ordinal", vec![MountRow::new(1, "bybit", "CryptoPerp")]),
        (
            "two mounts",
            vec![MountRow::new(0, "bybit", "CryptoPerp"), MountRow::new(1, "okx", "CryptoSpot")],
        ),
    ] {
        let mut mounts = mounts;
        for row in &mut mounts {
            if row.symbol.is_none() && row.token_id.is_none() {
                row.symbol = Some("X".into());
            }
        }
        let doc = render_daemon_toml(&stored(ProfileKind::Daemon, &[], mounts)).expect("renders");
        assert!(doc.contains("[[mounts]]"), "{why} must render the array spelling:\n{doc}");
    }
}

/// A `profile_setting` path a daemon profile has no home for is an ERROR, never a silent drop.
#[test]
fn a_daemon_setting_path_outside_the_daemon_table_is_refused() {
    let e = render_daemon_toml(&stored(
        ProfileKind::Daemon,
        &[("risk.max_total_exposure", "500.0")],
        vec![MountRow::new(0, "bybit", "CryptoPerp")],
    ))
    .expect_err("a run-profile key has no home in a daemon document");
    assert!(e.contains("risk.max_total_exposure"), "names the path: {e}");
    assert!(e.contains("silently dropped"), "{e}");
}

/// `f64` values keep their TYPE across the store: `1000.0` must not come back as the integer
/// `1000`, or a round-trip fence reports a migration defect that is really a renderer bug.
#[test]
fn a_whole_number_float_still_renders_as_a_float() {
    let mut m = MountRow::new(0, "bybit", "CryptoPerp");
    m.symbol = Some("BTCUSDT".into());
    m.seed_cash = Some(1000.0);
    m.tick_size = Some(0.1);
    let doc = render_daemon_toml(&stored(ProfileKind::Daemon, &[], vec![m])).expect("renders");
    assert!(doc.contains("seed_cash = 1000.0"), "{doc}");
    assert!(doc.contains("tick_size = 0.1"), "{doc}");
}
