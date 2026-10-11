use super::*;

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
