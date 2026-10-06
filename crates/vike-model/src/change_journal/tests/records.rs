use super::*;

/// The wire format, end to end: field ORDER, the absent-vs-empty distinction, and the file the
/// record lands in.
#[test]
fn a_settings_write_lands_as_one_json_line() {
    let r = root();
    let j = journal(r.path());
    let change = Change::set_setting(
        Outcome::AppliedPendingRestart,
        Actor::wire(Some("127.0.0.1:51234"), Some("control"), None),
        "policy.toml",
        "policy.max_notional_per_order",
        Some("100"),
        "250",
    )
    .with_reason(Some("raising ahead of the CPI print"));

    let path = j.append(T, &change).expect("append");
    assert_eq!(path.file_name().unwrap(), "changes-2026-08.jsonl");

    let raw = std::fs::read_to_string(&path).expect("read back");
    assert!(raw.ends_with('\n'), "every record is newline-terminated: {raw:?}");
    assert_eq!(raw.lines().count(), 1);

    // `kind` sits immediately after the timestamp and the sequence, so a reader can discard a
    // line on a short PREFIX rather than parsing it whole. This is the wire format, not a
    // formatting preference — asserted on the literal bytes, since a `serde_json::Value`
    // round-trip is order-blind and would pass whatever order the struct declares.
    assert!(raw.starts_with(r#"{"ts_ms":"#), "ts_ms leads the line: {raw}");
    let at = |k: &str| raw.find(k).unwrap_or_else(|| panic!("{k} in {raw}"));
    assert!(at(r#""seq""#) < at(r#""kind""#), "seq then kind: {raw}");
    assert!(at(r#""kind""#) < at(r#""outcome""#), "kind precedes outcome: {raw}");
    assert!(at(r#""kind""#) < at(r#""target""#), "…and the target it discriminates: {raw}");
    assert!(at(r#""kind""#) < 64, "kind is inside a short prefix: {raw}");

    let v = &read_lines(&path)[0];
    assert_eq!(v["kind"], KIND_SET_SETTING);
    assert_eq!(v["outcome"], "applied_pending_restart");
    assert_eq!(v["actor"]["origin"], "wire");
    assert_eq!(v["actor"]["peer"], "127.0.0.1:51234");
    assert_eq!(v["actor"]["scope"], "control");
    assert!(v["actor"].get("key_id").is_none(), "an absent key id records no field");
    assert_eq!(v["target"]["file"], "policy.toml");
    assert_eq!(v["target"]["key"], "policy.max_notional_per_order");
    assert_eq!(v["target"]["old"], "100");
    assert_eq!(v["target"]["new"], "250");
    assert_eq!(v["reason"], "raising ahead of the CPI print");
    assert_eq!(v["proc"]["bin"], "vike-test");
    assert_eq!(v["proc"]["pid"], 4711);
    assert_eq!(v["ts_ms"], T);
}

/// ⚠ ABSENT is not empty. "The ceiling was unset" and "the ceiling was blank" must not read
/// identically — the distinction `vike_config::write::SettingsWrite` draws with
/// `old_value: Option<String>` and the one an incident review turns on.
#[test]
fn an_absent_old_value_is_an_absent_field_not_an_empty_one() {
    let r = root();
    let j = journal(r.path());

    let absent =
        Change::set_setting(Outcome::Applied, Actor::Gui, "policy.toml", "policy.x", None, "1");
    let empty =
        Change::set_setting(Outcome::Applied, Actor::Gui, "policy.toml", "policy.x", Some(""), "1");
    let path = j.append(T, &absent).expect("append");
    j.append(T, &empty).expect("append");

    let lines = read_lines(&path);
    assert!(lines[0]["target"].get("old").is_none(), "not in the file at all: no field");
    assert_eq!(lines[1]["target"]["old"], "", "in the file, holding nothing: an empty string");
    // …and the two really are different lines, so this cannot pass by both being absent.
    assert_ne!(lines[0]["target"], lines[1]["target"]);
}

/// A rationale that sanitizes to nothing records NO field, rather than a blank one that reads
/// like a supplied-but-empty explanation.
#[test]
fn an_empty_reason_records_no_field() {
    let r = root();
    let j = journal(r.path());
    for raw in [None, Some(""), Some("   "), Some("\n\t")] {
        let c = Change::set_setting(Outcome::Applied, Actor::Gui, "f.toml", "f.k", None, "1")
            .with_reason(raw);
        let line = j.render(T, &c).expect("render");
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert!(v.get("reason").is_none(), "{raw:?} must record no reason: {line}");
    }
}

/// Every actor variant round-trips as `origin` plus what that channel actually knows. There is
/// no `user` field anywhere, because there are no human accounts in this system.
#[test]
fn every_actor_records_its_channel_and_never_a_user() {
    let r = root();
    let j = journal(r.path());
    let cases = [
        (Actor::wire(Some("<host>:9000"), Some("control"), Some("nk-3f21")), "wire"),
        (Actor::Gui, "gui"),
        (Actor::cli("vike-cli"), "cli"),
        (Actor::venue("ctrader"), "venue"),
        (Actor::Boot, "boot"),
    ];
    for (actor, origin) in cases {
        let c = Change::set_setting(Outcome::Applied, actor, "f.toml", "f.k", None, "1");
        let line = j.render(T, &c).expect("render");
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["actor"]["origin"], origin, "{line}");
        assert!(v["actor"].get("user").is_none(), "no invented human actor: {line}");
    }
    // The wire actor's key id is an ID, and the record must carry it rather than the key.
    let c = Change::set_setting(
        Outcome::Applied,
        Actor::wire(None, Some("control"), Some("nk-3f21")),
        "f.toml",
        "f.k",
        None,
        "1",
    );
    let line = j.render(T, &c).expect("render");
    assert!(line.contains("nk-3f21"), "{line}");
    let v: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert!(v["actor"].get("peer").is_none(), "an unknown peer records no field: {line}");
}

/// A credential record carries NAMES and a COUNT — and the count is the TRUE total, so a capped
/// record says how much it is not showing.
#[test]
fn a_credential_record_carries_names_and_a_true_count() {
    let r = root();
    let j = journal(r.path());
    let keys = ["OKX_LIVE_API_KEY", "OKX_LIVE_API_SECRET", "OKX_LIVE_API_PASSPHRASE"];
    let c =
        Change::credential_write(Outcome::Applied, Actor::Gui, "secrets.env", "okx", "LIVE", &keys);
    let path = j.append(T, &c).expect("append");
    let v = &read_lines(&path)[0];
    assert_eq!(v["kind"], KIND_CREDENTIAL_WRITE);
    assert_eq!(v["target"]["venue"], "okx");
    assert_eq!(v["target"]["tier"], "LIVE");
    assert_eq!(v["target"]["count"], 3);
    let recorded: Vec<&str> =
        v["target"]["keys"].as_array().unwrap().iter().map(|k| k.as_str().unwrap()).collect();
    assert_eq!(recorded, keys, "the names are what makes the record answerable");

    // Over the cap: the names are trimmed, the COUNT is not — the record admits the trim.
    let many: Vec<String> = (0..40).map(|i| format!("V{i}_LIVE_API_KEY")).collect();
    let refs: Vec<&str> = many.iter().map(String::as_str).collect();
    let c = Change::credential_write(
        Outcome::Applied,
        Actor::Gui,
        "secrets.env",
        "multi",
        "LIVE",
        &refs,
    );
    let Target::Credential(t) = c.target() else { panic!("credential target") };
    assert_eq!(t.keys().len(), MAX_CREDENTIAL_KEYS, "names are capped");
    assert_eq!(t.count(), 40, "…and the count still tells the truth");
}

/// A key NAME cell is reduced to `[A-Za-z0-9_]`, so a caller that passed the wrong thing cannot
/// get punctuation, whitespace, `=` or a line terminator into the record.
#[test]
fn a_credential_key_name_is_reduced_to_its_alphabet() {
    let r = root();
    let j = journal(r.path());
    let c = Change::credential_write(
        Outcome::Applied,
        Actor::Gui,
        "secrets.env",
        "okx",
        "LIVE",
        &["OKX_LIVE_API_KEY=sk-live-abc\nOKX_LIVE_API_SECRET"],
    );
    let line = j.render(T, &c).expect("render");
    let v: serde_json::Value = serde_json::from_str(&line).unwrap();
    let got = v["target"]["keys"][0].as_str().unwrap();
    assert!(!got.contains('='), "no assignment survives: {got}");
    assert!(!got.contains('\n') && !got.contains('-'), "no separator survives: {got}");
    assert_eq!(got, "OKX_LIVE_API_KEYskliveabcOKX_LIVE_API_SECRET", "{got}");
}

/// The boot anchor: one record per process start carrying the effective ceilings, with `None`
/// distinguishable from a value.
#[test]
fn a_boot_record_carries_the_effective_ceilings() {
    let r = root();
    let j = journal(r.path());
    let c = Change::boot_settings(
        Outcome::Applied,
        Actor::Boot,
        &[
            // ⚠ NOT `policy.max_leverage`, and the reason is semantic rather than cosmetic.
            // That field is `Consumed::No` in `crates/vike-config/tests/policy_is_consumed.rs`
            // BY DESIGN — `MountPolicy::from` deliberately does not carry it, because its `1.0`
            // default would clamp every deployment with no `policy` rows to 1x. So a value an
            // operator wrote there is set but NOT EFFECTIVE, and a record whose target is named
            // `settings` under a boot anchor claiming the effective ceilings would be asserting
            // the opposite. That gate also reads a textual mention as a read, so naming it here
            // turned CI red on the merged tree — but the fixture would have been wrong even if
            // the gate had stayed silent.
            ("config.store_root", Some("/srv/vike/market_data/hist")),
            ("policy.max_notional_per_order", Some("250")),
            ("policy.market_slippage", None),
            ("policy.halt_admit", Some("admit")),
        ],
    );
    let path = j.append(T, &c).expect("append");
    let v = &read_lines(&path)[0];
    assert_eq!(v["kind"], KIND_BOOT_SETTINGS);
    assert_eq!(v["actor"]["origin"], "boot");
    let s = v["target"]["settings"].as_array().unwrap();
    assert_eq!(s.len(), 4);
    assert_eq!(s[1][0], "policy.max_notional_per_order");
    assert_eq!(s[1][1], "250");
    assert!(s[2][1].is_null(), "an unset ceiling is null, not a missing pair: {v}");
}

/// A venue mount records BOTH tiers and the block between them — the record that answers the
/// question a per-venue switch invites: *"I set live, why did it trade paper?"*
#[test]
fn a_venue_mount_record_carries_both_tiers_and_the_block() {
    let r = root();
    let j = journal(r.path());
    let c = Change::venue_mounted(
        Outcome::Applied,
        Actor::Boot,
        "binance",
        None,
        "live",
        "paper",
        Some("no-credentials"),
    );
    let path = j.append(T, &c).expect("append");
    let v = &read_lines(&path)[0];
    assert_eq!(v["kind"], KIND_VENUE_MOUNTED);
    assert_eq!(v["target"]["venue"], "binance");
    assert_eq!(v["target"]["requested"], "live");
    assert_eq!(v["target"]["effective"], "paper");
    assert_eq!(v["target"]["block"], "no-credentials");
}

/// ...and a mount that REACHED what it was asked for records no block — while still recording.
///
/// Both halves are the point. Writing the record on agreement is what makes the channel usable
/// at all: an absence proves nothing, because *"no record for bybit"* reads identically whether
/// bybit mounted cleanly or never mounted. Dropping the block is the other half — nothing was
/// refused here, and a block on a mount that got what it asked for would document a refusal
/// that never happened.
#[test]
fn a_mount_that_reached_its_tier_records_no_block() {
    let r = root();
    let j = journal(r.path());
    let c = Change::venue_mounted(
        Outcome::Applied,
        Actor::Boot,
        "bybit",
        None,
        "demo",
        "demo",
        // A caller that hands one anyway is NOT obeyed — the constructor decides from the
        // tiers, so a caller cannot stamp a refusal onto a mount that succeeded.
        Some("no-credentials"),
    );
    let path = j.append(T, &c).expect("append");
    let v = &read_lines(&path)[0];
    assert_eq!(v["kind"], KIND_VENUE_MOUNTED);
    assert_eq!(v["target"]["effective"], "demo");
    assert!(
        v["target"]["block"].is_null(),
        "a mount that got what it asked for carries no block: {v}"
    );
    match c.target() {
        Target::VenueMounted(t) => {
            assert!(!t.diverged(), "demo asked and demo reached is not a divergence");
            assert_eq!(t.block(), None);
        }
        other => panic!("expected a venue-mount target, got {other:?}"),
    }
}
