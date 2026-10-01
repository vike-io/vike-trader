use super::*;
use crate::scratch::ScratchDir;

/// A throwaway journal directory for one test.
///
/// Built with [`ScratchDir`], the same dogfooding [`crate::scratch`]'s own tests use: unique per
/// process, self-deleting on the panic path, and no `tempfile` dev-dependency in the crate every
/// binary in this workspace links. The system temp directory is legitimate here —
/// `crates/vike-ops/tests/system_temp_gate.rs` scopes itself to production code.
fn root() -> ScratchDir {
    ScratchDir::create_in(&std::env::temp_dir(), "vike-changejournal-selftest").expect("root")
}

fn journal(dir: &Path) -> ChangeJournal {
    ChangeJournal::new(dir.to_path_buf(), Proc::new("vike-test", 4711, "0.1.0"))
}

/// 2026-08-21T00:00:00Z, the anchor every timestamped test below uses.
const T: i64 = 1_787_356_800_000;

fn read_lines(path: &Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(path)
        .expect("journal file")
        .lines()
        .map(|l| serde_json::from_str(l).expect("each line parses as one JSON object"))
        .collect()
}

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
            // default would clamp every deployment with no `policy.toml` to 1x. So a value an
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

/// THE SIZE GATE. A maximal record of every kind, built from the most expensive character there
/// is, must still fit one page — because that arithmetic is the module doc's whole atomicity
/// mitigation, and prose arithmetic rots.
///
/// `"` is chosen deliberately: it is a single ASCII byte that `serde_json` escapes to two, which
/// is the worst expansion available once control characters are stripped (a non-ASCII char
/// passes through as its own UTF-8 bytes and does not expand at all).
#[test]
fn record_shapes_at_their_caps_fit_a_page() {
    let r = root();
    let worst = "\"".repeat(MAX_RECORD_BYTES); // longer than any cap, so every cap actually bites
    let j = ChangeJournal::new(r.path().to_path_buf(), Proc::new(&worst, u32::MAX, &worst));
    let actor = Actor::wire(Some(&worst), Some(&worst), Some(&worst));

    let setting =
        Change::set_setting(Outcome::Applied, actor.clone(), &worst, &worst, Some(&worst), &worst)
            .with_reason(Some(&worst));

    let key_names: Vec<String> =
        (0..MAX_CREDENTIAL_KEYS * 4).map(|i| format!("{}_{i}", "K".repeat(80))).collect();
    let refs: Vec<&str> = key_names.iter().map(String::as_str).collect();
    let credential =
        Change::credential_write(Outcome::Applied, actor.clone(), &worst, &worst, &worst, &refs)
            .with_reason(Some(&worst));

    let pairs: Vec<(String, Option<String>)> =
        (0..MAX_BOOT_ENTRIES * 3).map(|_| (worst.clone(), Some(worst.clone()))).collect();
    let borrowed: Vec<(&str, Option<&str>)> =
        pairs.iter().map(|(k, v)| (k.as_str(), v.as_deref())).collect();
    let boot =
        Change::boot_settings(Outcome::Applied, actor.clone(), &borrowed).with_reason(Some(&worst));

    // ⚠ `requested` and `effective` must DIFFER here, or `venue_mounted` drops `block` by
    // design and this stops being the maximal record. They also cannot differ by a SUFFIX: both
    // are capped to `MAX_IDENT_BYTES`, so a longer twin truncates to the same prefix and
    // compares equal — which would make this case quietly smaller than it claims to be. The
    // difference goes in the FIRST byte, costing exactly one byte of escape expansion.
    let effective = format!("x{worst}");
    let venue_mounted = Change::venue_mounted(
        Outcome::Applied,
        actor,
        &worst,
        // ⚠ `Some`, never `None`: the default account SKIPS this field, so a `None` here would
        // make the maximal record one field smaller than the largest one this kind can write —
        // which is the shape of under-measurement this whole gate exists to refuse.
        Some(&worst),
        &worst,
        &effective,
        Some(&worst),
    )
    .with_reason(Some(&worst));

    // ⚠ `old` AND `new` are both `Some`, for `venue_mounted`'s reason one case up: each of them
    // is skipped when absent — an account that named no book skips `old`, and a CLEAR skips
    // `new` — so a `None` in either position would make the maximal record one field smaller
    // than the largest one this kind can write. `account_id` is `i64::MIN`, the longest integer
    // rendering there is (twenty characters, the minus sign included) and the one value that
    // cannot be reached by capping a string.
    let account_book = Change::account_book(
        Outcome::Applied,
        Actor::wire(Some(&worst), Some(&worst), Some(&worst)),
        &worst,
        i64::MIN,
        &worst,
        &worst,
        Some(&worst),
        Some(&worst),
    )
    .with_reason(Some(&worst));

    // ⚠ Every optional cell is `Some` and the key list OVERFLOWS its cap, for the reasons the
    // two cases above give: a `None` label or a short key list would make this record smaller
    // than the largest one this kind can write, which is the under-measurement this whole gate
    // exists to refuse. `account_id` is `i64::MIN` for `account_book`'s reason.
    let account_lifecycle = Change::account_lifecycle(
        Outcome::Applied,
        Actor::wire(Some(&worst), Some(&worst), Some(&worst)),
        &worst,
        &worst,
        i64::MIN,
        &worst,
        &worst,
        Some(&worst),
        Some(&worst),
        false,
        &refs,
    )
    .with_reason(Some(&worst));

    let cases = [
        (KIND_SET_SETTING, setting),
        (KIND_CREDENTIAL_WRITE, credential),
        (KIND_BOOT_SETTINGS, boot),
        (KIND_VENUE_MOUNTED, venue_mounted),
        (KIND_ACCOUNT_BOOK, account_book),
        (KIND_ACCOUNT_LIFECYCLE, account_lifecycle),
    ];

    // THE COVERAGE CLAIM, and the reason this gate is worth more than it was. The case list
    // used to be three hand-written names; a fourth kind could join the journal without joining
    // the one-page guarantee, and nothing would have said so. Now the guarantee is asserted
    // over [`KINDS`], so a kind with no maximal record here is a RED test rather than a claim
    // quietly made over a shorter roster.
    let covered: Vec<&str> = cases.iter().map(|(_, c)| c.kind()).collect();
    for kind in KINDS {
        assert!(
            covered.contains(kind),
            "kind `{kind}` has no maximal record in this gate, so the one-page guarantee is \
                 being claimed over a roster the gate does not actually test"
        );
    }

    for (name, change) in cases {
        let line = j.render(T, &change).unwrap_or_else(|e| {
            panic!("a maximal {name} record must still render, got {e}");
        });
        assert!(
            line.len() <= MAX_RECORD_BYTES,
            "a maximal {name} record is {} bytes, over the {MAX_RECORD_BYTES}-byte page cap. \
                 Either a cap was raised without re-checking this arithmetic, or a field was added \
                 without one",
            line.len()
        );
        // …and it really is near the cap rather than trivially small, so this cannot pass
        // because the caps silently stopped applying.
        assert!(
            line.len() > 400,
            "a maximal {name} record collapsed to {} bytes — the caps are no longer being \
                 filled, so this gate is measuring nothing",
            line.len()
        );
    }
}

/// A record over the cap is REFUSED and NOTHING is written — not a truncated line, not an
/// empty file, not even the directory.
///
/// Driven through `append_record` with a hand-built [`ChangeRecord`], because every public
/// `Change` constructor caps its cells and an over-budget record is therefore unreachable
/// through [`ChangeJournal::append`]. That is the point of the caps and exactly why the belt
/// behind them needs its own seam: an untestable guard is an untested one.
#[test]
fn an_oversized_record_is_refused_and_writes_nothing() {
    let r = root();
    let dir = r.path().join("changes");
    let j = ChangeJournal::new(dir.clone(), Proc::new("vike-test", 1, "0.1.0"));

    let oversized = ChangeRecord {
        ts_ms: T,
        seq: 0,
        kind: KIND_SET_SETTING,
        outcome: Outcome::Applied,
        actor: Actor::Boot,
        target: Target::Setting(SettingTarget {
            file: "policy.toml".into(),
            key: "policy.x".into(),
            old: None,
            new: "x".repeat(MAX_RECORD_BYTES * 2),
        }),
        reason: None,
        process: Proc::new("vike-test", 1, "0.1.0"),
    };
    match j.append_record(&oversized) {
        Err(ChangeJournalError::TooLarge { bytes }) => {
            assert!(bytes > MAX_RECORD_BYTES, "the reported size is the real one: {bytes}");
        }
        other => panic!("an oversized record must be refused, got {other:?}"),
    }
    assert!(!dir.exists(), "a refused record leaves no directory, let alone a torn line");

    // …and a record just UNDER the cap goes through the same path, so the refusal above is
    // about the size rather than about `append_record` being broken.
    let ok = ChangeRecord {
        target: Target::Setting(SettingTarget {
            file: "policy.toml".into(),
            key: "policy.x".into(),
            old: None,
            new: "x".repeat(MAX_RECORD_BYTES / 2),
        }),
        ..oversized
    };
    let path = j.append_record(&ok).expect("an in-budget record is written");
    assert_eq!(read_lines(&path).len(), 1);
}

/// Two records in the SAME millisecond are ordered by `seq`, which is what that field is for.
#[test]
fn two_records_in_one_millisecond_are_ordered_by_seq() {
    let r = root();
    let j = journal(r.path());
    let c = Change::set_setting(Outcome::Applied, Actor::Gui, "f.toml", "f.k", None, "1");
    let path = j.append(T, &c).expect("append");
    j.append(T, &c).expect("append");
    j.append(T, &c).expect("append");

    let lines = read_lines(&path);
    assert_eq!(lines.len(), 3, "three appends, three lines");
    let seqs: Vec<u64> = lines.iter().map(|v| v["seq"].as_u64().unwrap()).collect();
    assert!(seqs[0] < seqs[1] && seqs[1] < seqs[2], "strictly increasing: {seqs:?}");
    assert!(lines.iter().all(|v| v["ts_ms"] == T), "…within one millisecond");
}

/// Appends ACCUMULATE. A journal that truncated would look identical after one write, which is
/// exactly how an append-only store stops being one without anyone noticing.
#[test]
fn appends_accumulate_rather_than_replace() {
    let r = root();
    let j = journal(r.path());
    for i in 0..25 {
        let c = Change::set_setting(
            Outcome::Applied,
            Actor::Gui,
            "policy.toml",
            "policy.max_notional_per_order",
            None,
            &i.to_string(),
        );
        j.append(T, &c).expect("append");
    }
    let lines = read_lines(&j.file_for(T));
    assert_eq!(lines.len(), 25);
    assert_eq!(lines[0]["target"]["new"], "0", "the FIRST record still exists");
    assert_eq!(lines[24]["target"]["new"], "24");
}

/// Concurrent appenders both land, and every line stays whole — the multi-writer requirement
/// that ruled out an embedded database with an exclusive lock.
///
/// Threads rather than processes here (a test binary cannot portably re-exec itself); the
/// cross-PROCESS half is `crates/vike-model/tests/change_journal_concurrent.rs`, which spawns
/// real child processes. This one exists because it is the cheap version that runs everywhere.
#[test]
fn concurrent_appenders_do_not_interleave() {
    let r = root();
    let dir = r.path().to_path_buf();
    let writers = 8;
    let each = 40;
    let handles: Vec<_> = (0..writers)
        .map(|w| {
            let dir = dir.clone();
            std::thread::spawn(move || {
                let j = ChangeJournal::new(dir, Proc::new("vike-test", w as u32, "0.1.0"));
                for i in 0..each {
                    let c = Change::set_setting(
                        Outcome::Applied,
                        Actor::Gui,
                        "policy.toml",
                        "policy.max_notional_per_order",
                        None,
                        &format!("{w}-{i}"),
                    );
                    j.append(T, &c).expect("append");
                }
            })
        })
        .collect();
    for h in handles {
        h.join().expect("writer");
    }

    let lines = read_lines(&journal(r.path()).file_for(T));
    assert_eq!(lines.len(), writers * each, "every append landed");
    let mut seen: Vec<String> =
        lines.iter().map(|v| v["target"]["new"].as_str().unwrap().to_string()).collect();
    seen.sort();
    seen.dedup();
    assert_eq!(seen.len(), writers * each, "…and no two records were merged or lost");
}

/// ⚠ **THE APPEND PATH TAKES THE LOCK — and a contended append WAITS rather than losing its
/// record.** This is the machine-checked half of the module doc's serialisation argument.
///
/// What it can and cannot prove is worth stating plainly, because the two are easy to conflate.
/// It PROVES that `append` blocks on `<dir>/`[`CHANGES_LOCK_FILE`] and completes once that lock
/// is free — remove the lock and the writer finishes immediately, so this test goes red. It does
/// NOT prove the Docker Desktop case: reproducing lost appends needs a filesystem that loses
/// them, no CI box has one **because no CI box is Windows or macOS**, and that half rests
/// on the hand measurement recorded in the module doc and in
/// `docs/ops/tradehub-container.md`.
/// ⚠ The reason used to be given as "there is no container runtime on any of them", which is
/// false — both self-hosted boxes run Docker (measured 2026-09-25; the CI box built and published
/// every release image). A Linux runner's runtime cannot produce a Docker Desktop HOST
/// PASSTHROUGH mount, so the conclusion is unchanged and only its reason moves.
///
/// The lock is held from an INDEPENDENT descriptor in this same process, which works because
/// `flock`/`LockFileEx` key on the open file description rather than on the process — see
/// [`AppendLock`].
#[test]
fn an_append_waits_for_a_held_lock_instead_of_writing_beside_it() {
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    use std::time::Duration;

    let r = root();
    let dir = r.path().to_path_buf();
    std::fs::create_dir_all(&dir).expect("journal dir");

    let held = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join(CHANGES_LOCK_FILE))
        .expect("open the sentinel");
    held.try_lock().expect("this test takes the append lock first");

    let started = Arc::new(AtomicBool::new(false));
    let done = Arc::new(AtomicBool::new(false));
    let (s, d, wdir) = (started.clone(), done.clone(), dir.clone());
    let writer = std::thread::spawn(move || {
        let j = ChangeJournal::new(wdir, Proc::new("vike-test", 1, "0.1.0"));
        let c = Change::set_setting(
            Outcome::Applied,
            Actor::Gui,
            "policy.toml",
            "policy.max_notional_per_order",
            None,
            "250",
        );
        s.store(true, Ordering::SeqCst);
        j.append(T, &c).expect("the append completes once the lock is released");
        d.store(true, Ordering::SeqCst);
    });

    // ⚠ ANTI-VACUITY, half one: wait until the writer has genuinely REACHED the append. A
    // "still running" assertion against a thread that has not started yet measures nothing —
    // it is the shape of contention test that never contends.
    while !started.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(1));
    }
    // …then a window in which it must NOT get through. Without the lock this append is a
    // create + one write + an fsync, so half a second is orders of magnitude of slack.
    for _ in 0..50 {
        std::thread::sleep(Duration::from_millis(10));
        assert!(
            !done.load(Ordering::SeqCst),
            "the append completed while another descriptor held {CHANGES_LOCK_FILE} — the \
                 append path is NOT taking the lock"
        );
    }
    assert!(
        !dir.join(month_file_name(T)).exists(),
        "a blocked append must not have created the month file either"
    );

    // ⚠ ANTI-VACUITY, half two: release, and the SAME writer finishes. Without this the test
    // would pass just as well against a writer that had crashed, hung or never spawned.
    held.unlock().expect("release");
    drop(held);
    writer.join().expect("the writer finishes once the lock is free");
    assert!(done.load(Ordering::SeqCst));
    assert_eq!(read_lines(&journal(r.path()).file_for(T)).len(), 1, "…and its record landed");
}

/// A lock that cannot be TAKEN AT ALL refuses the record loudly, and writes nothing — the
/// module doc's second half. A filesystem this journal cannot serialise on is one whose
/// completeness nobody can claim, so the caller gets an error it can log rather than a silence
/// it cannot.
///
/// Driven by putting a DIRECTORY where the sentinel belongs: `OpenOptions::open` cannot hand
/// back a file handle for one on either platform this ships on, which is a portable way to
/// reach a branch whose real-world cause (a filesystem whose locking errors) no box here has.
#[test]
fn a_lock_that_cannot_be_taken_refuses_the_record_and_writes_nothing() {
    let r = root();
    let dir = r.path().to_path_buf();
    std::fs::create_dir_all(dir.join(CHANGES_LOCK_FILE)).expect("a directory in the way");
    let j = journal(&dir);
    let c = Change::set_setting(Outcome::Applied, Actor::Gui, "policy.toml", "policy.k", None, "1");
    match j.append(T, &c) {
        Err(ChangeJournalError::Lock(e)) => {
            let msg = ChangeJournalError::Lock(e).to_string();
            assert!(msg.contains(CHANGES_LOCK_FILE), "the error names the sentinel: {msg}");
            assert!(msg.contains("REFUSED"), "…and says the record was not written: {msg}");
        }
        other => panic!("an unusable lock must refuse the record, got {other:?}"),
    }
    assert!(!j.file_for(T).exists(), "nothing was written unserialised");
}

/// Records land in the file for their OWN month, so a backdated record does not pollute the
/// current one and the retention bound stays a calendar bound.
#[test]
fn each_record_lands_in_its_own_month() {
    let r = root();
    let j = journal(r.path());
    let jan = 1_767_225_600_000; // 2026-01-01T00:00:00Z
    let c = Change::set_setting(Outcome::Applied, Actor::Gui, "f.toml", "f.k", None, "1");
    assert_eq!(j.append(jan, &c).unwrap().file_name().unwrap(), "changes-2026-01.jsonl");
    assert_eq!(j.append(T, &c).unwrap().file_name().unwrap(), "changes-2026-08.jsonl");
    assert_eq!(month_file_name(-1), "changes-1969-12.jsonl", "pre-1970 floors toward -inf");
}

/// Retention: the newest `max_files` months survive, oldest first. Names sort in calendar
/// order, so this needs no clock and no `stat`.
#[test]
fn prune_keeps_the_newest_months_and_removes_the_oldest() {
    let r = root();
    let j = journal(r.path());
    std::fs::create_dir_all(r.path()).unwrap();
    for (y, m) in [(2024, 11), (2024, 12), (2025, 1), (2025, 2), (2026, 8)] {
        std::fs::write(r.path().join(format!("changes-{y:04}-{m:02}.jsonl")), b"{}\n").unwrap();
    }
    let pruned = j.prune(Some(2));
    assert_eq!((pruned.found, pruned.removed, pruned.failed), (5, 3, 0), "{pruned:?}");
    assert!(!r.path().join("changes-2024-11.jsonl").exists());
    assert!(!r.path().join("changes-2025-01.jsonl").exists());
    assert!(r.path().join("changes-2025-02.jsonl").exists(), "second-newest kept");
    assert!(r.path().join("changes-2026-08.jsonl").exists(), "newest kept");
}

/// Under the limit the prune removes NOTHING — the anti-vacuity twin. A prune that deleted on
/// every call would take out the month currently being written.
#[test]
fn prune_below_the_limit_removes_nothing() {
    let r = root();
    let j = journal(r.path());
    std::fs::create_dir_all(r.path()).unwrap();
    for m in 1..=3 {
        std::fs::write(r.path().join(format!("changes-2026-{m:02}.jsonl")), b"{}\n").unwrap();
    }
    assert_eq!(j.prune(Some(DEFAULT_MAX_CHANGE_FILES)), Pruned { found: 3, removed: 0, failed: 0 });
    assert!(r.path().join("changes-2026-01.jsonl").exists());
}

/// `None` is retention OFF, asserted against a population that WOULD be pruned under the
/// default — so this cannot pass by a limit nobody reached.
#[test]
fn prune_with_no_limit_keeps_everything() {
    let r = root();
    let j = journal(r.path());
    std::fs::create_dir_all(r.path()).unwrap();
    let n = DEFAULT_MAX_CHANGE_FILES + 4;
    for i in 0..n {
        let (y, m) = (2000 + i / 12, i % 12 + 1);
        std::fs::write(r.path().join(format!("changes-{y:04}-{m:02}.jsonl")), b"{}\n").unwrap();
    }
    assert_eq!(j.prune(None), Pruned::default(), "no limit means no work at all");
    assert_eq!(std::fs::read_dir(r.path()).unwrap().count(), n);
    assert_eq!(j.prune(Some(DEFAULT_MAX_CHANGE_FILES)).removed, 4, "…and the default bites");
}

/// An absent directory is the ordinary state of a project that has changed nothing — a startup
/// must not fail over housekeeping.
#[test]
fn prune_of_an_absent_directory_is_silent() {
    let r = root();
    let j = ChangeJournal::new(r.path().join("never"), Proc::new("t", 1, "0"));
    assert_eq!(j.prune(Some(1)), Pruned::default());
}

/// ⚠ The prune DELETES what [`is_month_file_name`] accepts, so anything it is unsure about must
/// fall outside — and must not be counted either, or the bound bites early on files it will
/// never remove.
#[test]
fn prune_leaves_anything_that_is_not_a_monthly_file_alone() {
    let r = root();
    let j = journal(r.path());
    std::fs::create_dir_all(r.path()).unwrap();
    let strangers = [
        "changes-old.jsonl",
        "changes-2026-08.jsonl.bak",
        "changes-2026-8.jsonl",
        "changes-20260-8.jsonl",
        "export.csv",
        "README",
        // ⚠ The append lock's own sentinel. Deleting the file every writer coordinates on
        // would be housekeeping breaking the serialisation — see [`CHANGES_LOCK_FILE`].
        CHANGES_LOCK_FILE,
    ];
    for name in strangers {
        std::fs::write(r.path().join(name), b"not mine\n").unwrap();
    }
    for m in 1..=4 {
        std::fs::write(r.path().join(format!("changes-2026-{m:02}.jsonl")), b"{}\n").unwrap();
    }
    let pruned = j.prune(Some(1));
    assert_eq!(pruned.found, 4, "only real monthly files are counted: {pruned:?}");
    assert_eq!(pruned.removed, 3);
    for name in strangers {
        assert!(r.path().join(name).exists(), "{name} must survive housekeeping");
    }
}

/// The file-name matcher, at the boundaries the prune turns on.
#[test]
fn the_month_file_matcher_is_strict() {
    assert!(is_month_file_name("changes-2026-08.jsonl"));
    assert!(is_month_file_name("changes-0001-01.jsonl"));
    assert!(!is_month_file_name("changes-2026-8.jsonl"), "the month must be two digits");
    assert!(!is_month_file_name("changes-2026_08.jsonl"), "the separator is a hyphen");
    assert!(!is_month_file_name("changes-2026-08.jsonl.bak"));
    assert!(!is_month_file_name("changes-2026-08.json"));
    assert!(!is_month_file_name("2026-08.jsonl"));
    assert!(!is_month_file_name("changes-.jsonl"));
    // Every name this module MINTS must be accepted — the two halves cannot drift apart.
    for ts in [-1i64, 0, T, 4_102_444_800_000] {
        assert!(is_month_file_name(&month_file_name(ts)), "{ts}");
    }
}

/// Control characters never reach the file, whatever the caller passed. Belt and braces: the
/// JSON escape would already keep the framing intact, and `grep`, a terminal and a naive
/// splitter all see the raw bytes.
#[test]
fn no_control_character_survives_into_the_line() {
    let r = root();
    let j = journal(r.path());
    let nasty = "flat\n{\"kind\":\"forged\",\"seq\":0}\r\nmore\u{0}\u{7f}\u{85}";
    let c = Change::set_setting(Outcome::Applied, Actor::Gui, "f.toml", "f.k", None, nasty)
        .with_reason(Some(nasty));
    let path = j.append(T, &c).expect("append");

    let raw = std::fs::read_to_string(&path).expect("read");
    assert_eq!(raw.lines().count(), 1, "one record is still one line: {raw:?}");
    assert!(!raw.trim_end().contains('\n'), "no embedded newline");
    assert!(!raw.contains('\u{0}') && !raw.contains('\r'), "no NUL, no CR");
    let v: serde_json::Value = serde_json::from_str(raw.trim_end()).expect("still parses");
    assert!(v["target"]["new"].as_str().unwrap().starts_with("flat{"));
}

/// Every cell is BYTE-capped on a char boundary, so a multi-byte sequence is never split into
/// invalid UTF-8 — the trap a byte-wise truncation walks into.
#[test]
fn cells_are_byte_capped_without_splitting_a_multibyte_char() {
    let snow = "☃";
    assert_eq!(snow.len(), 3, "the fixture must actually be multi-byte");
    let long = snow.repeat(MAX_FIELD_BYTES);
    let out = clean_field(&long);
    assert!(out.len() <= MAX_FIELD_BYTES);
    assert!(out.chars().all(|c| c == '☃'), "no partial sequence: {out:?}");
    assert_eq!(out.len() % 3, 0, "whole chars only");
    // Under the cap, nothing is touched.
    assert_eq!(clean_field("policy.max_notional_per_order"), "policy.max_notional_per_order");
}

/// The directory is created on first append, because `settings/state/changes` is not a marker
/// and a fresh install has none.
#[test]
fn the_journal_directory_is_created_on_first_append() {
    let r = root();
    let dir = r.path().join("state").join(CHANGES_SUBDIR);
    assert!(!dir.exists(), "precondition");
    let j = ChangeJournal::new(dir.clone(), Proc::new("t", 1, "0"));
    let c = Change::set_setting(Outcome::Applied, Actor::Boot, "f.toml", "f.k", None, "1");
    j.append(T, &c).expect("append creates the directory");
    assert!(dir.is_dir());
}

/// `in_state_dir` puts the journal exactly where the module doc says, under an ALREADY-RESOLVED
/// state directory rather than a fresh walk.
#[test]
fn in_state_dir_joins_the_changes_subdirectory() {
    let state = Path::new("/srv/vike-<unit>/settings/state");
    let j = ChangeJournal::in_state_dir(state, Proc::new("t", 1, "0"));
    assert_eq!(j.dir(), state.join(CHANGES_SUBDIR));
    assert_eq!(CHANGES_SUBDIR, "changes");
}
