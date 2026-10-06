//! `--write-profile` / `--show-effective` and the compute-daemon address ladder.

use super::*;

// ---- --write-profile / --show-effective ------------------------------------------------------

#[test]
fn render_effective_is_valid_toml_with_the_overrides_as_comments() {
    let ov = vec![
        Override {
            key: "engine.fee_rate".to_string(),
            value: parse_scalar("0.001"),
            origin: Origin::Sugar("--fee"),
        },
        Override {
            key: "engine.cash".to_string(),
            value: parse_scalar("1000"),
            origin: Origin::Set,
        },
        Override {
            key: "data.kind".to_string(),
            value: toml::Value::String("bar".to_string()),
            origin: Origin::Implied("no --kind"),
        },
    ];
    let body = build_profile_toml(None, &ov).unwrap();
    let out = render_effective(&body, &ov);
    // The whole thing still parses: a redirect produces a usable profile.
    let v: toml::Value = toml::from_str(&out).expect("--show-effective output must be TOML");
    assert_eq!(v["engine"]["fee_rate"].as_float(), Some(0.001));
    assert!(out.contains("--fee"), "names the sugar flag that set it: {out}");
    assert!(out.contains("--set"), "…and the --set origin: {out}");
    assert!(out.contains("implied"), "…and the implied one: {out}");
    assert!(
        out.lines().take_while(|l| l.starts_with('#') || l.trim().is_empty()).count() >= 4,
        "the origin block is a comment header: {out}"
    );
}

/// ⚠ A TABLE-valued override must not put a raw NEWLINE inside the `#` comment line.
/// `--set engine.impact={model="sqrt"}` is spellable today — nothing refuses it, because
/// [`set_profile_key`]'s table check fires only when the LEAF is already a table — and a
/// `toml::Value::Table` serializes as a `[v]` SECTION with its keys on following lines. Printed
/// verbatim that breaks the one property this whole flag rests on: that stdout is valid TOML,
/// so `--show-effective > run.toml` writes a usable profile.
#[test]
fn a_table_valued_override_does_not_break_the_comment_header() {
    let ov = vec![Override {
        key: "engine.impact".to_string(),
        value: parse_scalar(r#"{ model = "sqrt", coef = 0.5 }"#),
        origin: Origin::Set,
    }];
    assert!(ov[0].value.is_table(), "the fixture must really be a table value");
    // Built WITHOUT the applier (which refuses a key naming a whole table): the renderer is
    // what is under test, and it must survive any value that can reach it.
    let body = "[engine.impact]\nmodel = \"sqrt\"\ncoef = 0.5\n";
    let out = render_effective(body, &ov);
    let header: Vec<&str> = out.lines().take_while(|l| l.starts_with('#')).collect();
    assert!(header.iter().any(|l| l.contains("engine.impact")), "the row is still rendered: {out}");
    // THE PROPERTY: every header line is a comment, and the whole document still parses.
    let v: toml::Value = toml::from_str(&out).unwrap_or_else(|e| panic!("{e}\n{out}"));
    assert_eq!(v["engine"]["impact"]["model"].as_str(), Some("sqrt"));
}

/// ⚠ An `Origin::Implied` row that `build_profile_toml` SKIPPED must not be reported as though
/// it applied. `--profile tick.toml --show-effective` would otherwise print
/// `data.kind "bar" implied` three lines above a document reading `kind = "tick"` — a
/// provenance answer that is positively wrong, on every `--show-effective` run that also
/// carries a `--profile` naming one of the two implied keys.
#[test]
fn an_implied_row_the_profile_overrode_is_not_reported_as_applied() {
    let ov =
        parse_run_args(["--venue", "binance"].map(String::from).into_iter()).unwrap().overrides;
    let body = build_profile_toml(Some("[data]\nkind = \"tick\"\n"), &ov).unwrap();
    let out = render_effective(&body, &ov);

    let kind_row = out
        .lines()
        .find(|l| l.starts_with('#') && l.contains("data.kind"))
        .unwrap_or_else(|| panic!("the implied row is still rendered: {out}"));
    assert!(
        kind_row.contains("NOT APPLIED"),
        "the row must say it did not apply: {kind_row}\nfull output:\n{out}"
    );
    // …and the document itself is the file's value, which is what makes the old row a lie.
    let v: toml::Value = toml::from_str(&out).unwrap();
    assert_eq!(v["data"]["kind"].as_str(), Some("tick"));
}

/// …and the other direction: a row that genuinely DID apply is not annotated away.
#[test]
fn an_implied_row_that_applied_is_reported_plainly() {
    let ov =
        parse_run_args(["--venue", "binance"].map(String::from).into_iter()).unwrap().overrides;
    let body = build_profile_toml(None, &ov).unwrap();
    let out = render_effective(&body, &ov);
    let kind_row = out
        .lines()
        .find(|l| l.starts_with('#') && l.contains("data.kind"))
        .unwrap_or_else(|| panic!("the implied row is rendered: {out}"));
    assert!(kind_row.contains("implied"), "{kind_row}");
    assert!(!kind_row.contains("NOT APPLIED"), "it DID apply: {kind_row}");
}

/// An explicit flag naming the same key beats the implied row, and the row says so.
///
/// ⚠ **Read what this case can and cannot see.** `--kind tick` makes the built document `"tick"`
/// while the implied row carries `"bar"`, so [`implied_row_applied`]'s VALUE comparison already
/// answers — the sibling-override early return above it is never the reason. An earlier
/// write-up advertised this test as gating that clause; it does not, and deleting the clause
/// leaves this test green. `an_implied_row_a_sibling_carrying_the_same_value_is_not_reported_as_applied`
/// is the case that gates it. (This is the same class of claim the round-1 FIX 5 correction was
/// raised for — a test asserted to prove something it cannot — so it is written at the test
/// rather than in a report somebody has to find.)
#[test]
fn an_implied_row_an_explicit_flag_overrode_is_not_reported_as_applied() {
    let ov = parse_run_args(["--kind", "tick"].map(String::from).into_iter()).unwrap().overrides;
    let body = build_profile_toml(None, &ov).unwrap();
    let out = render_effective(&body, &ov);
    let rows: Vec<&str> =
        out.lines().filter(|l| l.starts_with('#') && l.contains("data.kind")).collect();
    assert_eq!(rows.len(), 2, "the sugar row AND the implied row are both listed: {out}");
    assert!(
        rows.iter().any(|l| l.contains("--kind") && !l.contains("NOT APPLIED")),
        "the sugar row applied: {rows:?}"
    );
    assert!(rows.iter().any(|l| l.contains("NOT APPLIED")), "the implied row did not: {rows:?}");
}

/// ⚠ **THE ONLY CASE THAT GATES [`implied_row_applied`]'s SIBLING-OVERRIDE CLAUSE**, and it
/// exists because nothing else did.
///
/// That clause returns early when another override on the same command line names the key. Its
/// two neighbours cannot see it: both make the built document hold a value the implied row does
/// NOT carry (`tick` vs `bar`, or the file's own), so the VALUE comparison on the next line
/// answers first and the clause is dead weight as far as they are concerned. Delete the clause
/// and they stay green.
///
/// It is observable only when a sibling sets the key to the SAME value the implied row would
/// have: the sugar (or `--set`) pass writes `bar`, the implied pass then SKIPS because the key
/// is already set, and a renderer without the clause compares `bar == bar` and reports a row
/// that never applied as applied. Both spellings are covered because they take different passes
/// in `build_profile_toml` and only the clause makes them agree.
#[test]
fn an_implied_row_a_sibling_carrying_the_same_value_is_not_reported_as_applied() {
    for argv in [
        vec!["--venue", "binance", "--kind", "bar"],
        vec!["--venue", "binance", "--set", "data.kind=bar"],
    ] {
        let ov = parse_run_args(argv.iter().map(|s| (*s).to_string())).unwrap().overrides;
        let body = build_profile_toml(None, &ov).unwrap();
        let out = render_effective(&body, &ov);
        // The IMPLIED row specifically — the sibling's own row carries its flag as the origin,
        // so filtering on `implied` isolates the one under test.
        let implied_row = out
            .lines()
            .find(|l| l.starts_with('#') && l.contains("data.kind") && l.contains("implied"))
            .unwrap_or_else(|| panic!("{argv:?}: the implied row is rendered: {out}"));
        assert!(
            implied_row.contains("NOT APPLIED"),
            "{argv:?}: a sibling set data.kind, so the implied row did NOT apply — the value \
                 being identical is exactly what makes this invisible without the sibling \
                 clause: {implied_row}\nfull output:\n{out}"
        );
        // …and the document is still right, so nobody is misled about the RUN either way.
        let v: toml::Value = toml::from_str(&out).unwrap();
        assert_eq!(v["data"]["kind"].as_str(), Some("bar"));
    }
}

#[test]
fn write_profile_and_show_effective_parse_and_join_the_refusal_roster() {
    let a = parse_run_args(
        ["--venue", "binance", "--write-profile", "out.toml", "--show-effective"]
            .map(String::from)
            .into_iter(),
    )
    .unwrap();
    assert_eq!(a.write_profile.as_deref(), Some("out.toml"));
    assert!(a.show_effective);
    assert!(PARAMS_REFUSED.contains(&"--write-profile"));
    assert!(PARAMS_REFUSED.contains(&"--show-effective"));
}

// ---- the address ladder ----------------------------------------------------------------------

/// The address ladder, moved here from `study` — ⚠ **not** because that verb disappears into
/// this one. Owner ruling R1 makes it `vike-cli research study`, a sub-verb of a FOURTH plane,
/// so it survives beside `backtest` rather than inside it. The move is forced by something
/// else: both verbs dial the SAME compute daemon on the SAME `config.backtest_addr` setting,
/// and two copies could answer differently about a blank rung — aiming a verb at `7878` (the
/// data server) or `7879` (the daemon that signs orders) instead of `7880`.
///
/// ⚠ `backtest` did NOT read `config.backtest_addr` before this: it applied a compiled-in const
/// inside `parse_run_args`, and the dispatcher handed the setting to `study` alone. Spec §5.6
/// describes the ladder as though it already existed on this verb.
#[test]
fn the_address_ladder_prefers_the_flag_then_the_setting_then_the_compiled_default() {
    assert_eq!(resolve_addr(Some("<host>:1"), Some("<host>:2")), "<host>:1");
    assert_eq!(resolve_addr(None, Some("<host>:2")), "<host>:2");
    assert_eq!(resolve_addr(None, None), vike_config::DEFAULT_BACKTEST_ADDR);
}

/// A BLANK rung is an ABSENT rung — an `Environment=` line or a settings key set to `""` must
/// not dial the empty string.
#[test]
fn a_blank_address_rung_falls_through_and_a_padded_one_is_trimmed() {
    assert_eq!(resolve_addr(Some("   "), Some("<host>:2")), "<host>:2");
    assert_eq!(resolve_addr(Some(""), None), vike_config::DEFAULT_BACKTEST_ADDR);
    assert_eq!(resolve_addr(None, Some(" ")), vike_config::DEFAULT_BACKTEST_ADDR);
    assert_eq!(resolve_addr(Some(" <host>:1 "), None), "<host>:1");
}

/// `--addr` is no longer collapsed in the parser: the setting is not visible there.
#[test]
fn the_parser_leaves_the_address_unresolved() {
    let a = parse_run_args(["--profile", "p.toml"].map(String::from).into_iter()).unwrap();
    assert!(a.addr.is_none(), "the parser cannot see config.backtest_addr");
    let b = parse_run_args(
        ["--profile", "p.toml", "--addr", "1.2.3.4:9"].map(String::from).into_iter(),
    )
    .unwrap();
    assert_eq!(b.addr.as_deref(), Some("1.2.3.4:9"));
}
