use super::*;

fn record(venue: &str, prefix: &str, id: &str, book: Option<&str>) -> ConfirmationRecord {
    ConfirmationRecord {
        venue: venue.to_string(),
        key_prefix: prefix.to_string(),
        label: None,
        tier: None,
        handshake_account_id: id.to_string(),
        observed_row: Some(7),
        observed_book: book.map(str::to_string),
        at_ms: 1_787_356_800_000,
    }
}

/// The LABELLED twin of [`record`] — an account with no credential-key prefix, addressed by the
/// store's `UNIQUE (venue, tier, label)` instead.
fn labelled(venue: &str, tier: &str, label: &str, id: &str) -> ConfirmationRecord {
    ConfirmationRecord {
        key_prefix: String::new(),
        label: Some(label.to_string()),
        tier: Some(tier.to_string()),
        ..record(venue, "", id, None)
    }
}

/// **TWO LABELLED ACCOUNTS OF ONE VENUE ARE TWO ENTRIES**, which is the whole reason
/// [`ConfirmationRecord::address`] carries more than `(venue, key_prefix)`.
///
/// Both carry an EMPTY prefix by construction, so the old two-field key collapsed them onto
/// `(venue, "")` and the second confirmation silently evicted the first — one account's venue
/// answer filed against nothing, with no error and no way to notice.
#[test]
fn two_labelled_accounts_do_not_evict_each_other() {
    let a = labelled("hyperliquid", "live", "ALT", "0xaaa");
    let b = labelled("hyperliquid", "live", "SPREAD", "0xbbb");
    let merged = merge(merge(Vec::new(), a.clone()), b.clone());
    assert_eq!(merged.len(), 2, "two accounts, two records: {merged:?}");
    assert!(merged.contains(&a) && merged.contains(&b));
}

/// …and the SAME labelled account twice is still ONE entry, newest wins — the property the
/// merge exists for, held across the wider key.
#[test]
fn one_labelled_account_answering_twice_is_one_entry() {
    let first = labelled("hyperliquid", "live", "ALT", "0xaaa");
    let again = ConfirmationRecord {
        handshake_account_id: "0xbbb".to_string(),
        ..labelled("hyperliquid", "live", "ALT", "0xaaa")
    };
    let merged = merge(merge(Vec::new(), first), again.clone());
    assert_eq!(merged, vec![again], "newest wins");
}

/// **THE SAME LABEL AT TWO TIERS IS TWO ACCOUNTS**, which is why the tier rides along with the
/// label and `(venue, label)` alone would not do. `UNIQUE (venue, tier, label)` is the store's
/// index and this key mirrors it.
#[test]
fn one_label_at_two_tiers_is_two_entries() {
    let demo = labelled("hyperliquid", "demo", "ALT", "0xdemo");
    let live = labelled("hyperliquid", "live", "ALT", "0xlive");
    assert_eq!(merge(merge(Vec::new(), demo), live).len(), 2);
}

/// **A RECORD PARKED BEFORE LABELS WERE ADDRESSABLE STILL READS**, as the unlabelled account it
/// was — the property `#[serde(default)]` buys, and the one that decides whether a box with an
/// existing `account-confirmations.json` keeps its dukascopy evidence across an upgrade.
///
/// ⚠ And the new fields are ABSENT from the JSON rather than `null`, so a record written by this
/// build and one written by the previous build for the SAME unlabelled account are byte-identical
/// — which is what keeps them one merge entry instead of two.
#[test]
fn a_record_written_before_labels_existed_still_reads_and_round_trips() {
    let old = r#"{"venue":"dukascopy","key_prefix":"DUKASCOPY_DEMO1_",
            "handshake_account_id":"DEMO2cGyrc","observed_row":7,
            "observed_book":"3709890","at_ms":1787356800000}"#;
    let parsed: ConfirmationRecord = serde_json::from_str(old).expect("an older record reads");
    assert_eq!(parsed.key_prefix, "DUKASCOPY_DEMO1_");
    assert_eq!(parsed.label, None, "no label ⇒ the prefix addresses it, exactly as before");
    assert_eq!(parsed.tier, None);

    let written = serde_json::to_string(&parsed).expect("serializes");
    assert!(!written.contains("label"), "an unlabelled record writes NO label key: {written}");
    assert!(!written.contains("tier"), "…and no tier key either: {written}");
}

struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "vike-confirm-{tag}-{}-{}",
            std::process::id(),
            crate::time::clock::now_ms()
        ));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        Scratch(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The three verdicts, and the one that matters: a DIFFERENT book is neither a learn nor a
/// confirm, whatever the two strings look like.
#[test]
fn a_different_book_is_a_disagreement() {
    assert_eq!(verdict("3709890", None), Verdict::Learns);
    assert_eq!(verdict("3709890", Some("3709890")), Verdict::Confirms);
    assert_eq!(
        verdict("DEMO2cGyrc", Some("3716974")),
        Verdict::Disagrees { stored: "3716974".to_string() }
    );
}

/// ⚠ The FORM residual, pinned as a test rather than only as prose: a login-shaped handshake id
/// against a numeric stored book is reported as a disagreement, because this function compares
/// two strings and the spec (§9) has not settled which form dukascopy's handshake carries.
/// Classifying the shapes would be a guess over a value nobody has measured.
#[test]
fn a_form_mismatch_is_reported_as_a_disagreement_rather_than_classified() {
    let v = verdict("DEMO1abcd", Some("3709890"));
    assert!(matches!(v, Verdict::Disagrees { .. }), "no shape heuristic lives here: {v:?}");
}

/// One entry per account: a second handshake for the same key prefix REPLACES the first rather
/// than growing the file, and a different account's entry is untouched.
#[test]
fn a_second_handshake_replaces_its_own_entry_and_no_other() {
    let s = Scratch::new("merge");
    let dir = Some(s.0.as_path());
    park(dir, record("dukascopy", "DUKASCOPY_DEMO1_", "A", None)).expect("park 1");
    park(dir, record("dukascopy", "DUKASCOPY_DEMO2_", "B", None)).expect("park 2");
    park(dir, record("dukascopy", "DUKASCOPY_DEMO1_", "A2", Some("A"))).expect("park 3");
    let got = read(dir).expect("read");
    assert_eq!(got.len(), 2, "one entry per (venue, key_prefix): {got:?}");
    let demo1 = got.iter().find(|r| r.key_prefix == "DUKASCOPY_DEMO1_").expect("demo1");
    assert_eq!(demo1.handshake_account_id, "A2", "newest wins");
    let demo2 = got.iter().find(|r| r.key_prefix == "DUKASCOPY_DEMO2_").expect("demo2");
    assert_eq!(demo2.handshake_account_id, "B", "the other account is untouched");
}

/// An ABSENT file is an answer (nothing parked), never an error — the ordinary state of every
/// box that has not mounted a confirming venue.
#[test]
fn an_absent_file_reads_as_nothing_parked() {
    let s = Scratch::new("absent");
    assert!(read(Some(s.0.as_path())).expect("absent is an answer").is_empty());
    // …and so is having no state directory at all, which is a process that declared no project.
    assert!(read(None).expect("no state dir is an answer").is_empty());
}

/// A MALFORMED file is an ERROR rather than "nothing parked": a parse failure that read as an
/// empty set would make a fold that is silently doing nothing look like a fold with nothing to
/// do.
#[test]
fn a_malformed_file_is_an_error_rather_than_an_empty_set() {
    let s = Scratch::new("malformed");
    std::fs::write(s.0.join(CONFIRMATIONS_FILE), b"{ not json").expect("plant");
    let err = read(Some(s.0.as_path())).expect_err("malformed is loud");
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
}

/// A fold consumes entries through [`replace_all`], and what it leaves is what a later read
/// sees.
#[test]
fn replace_all_is_what_a_fold_leaves_behind() {
    let s = Scratch::new("replace");
    let dir = Some(s.0.as_path());
    park(dir, record("dukascopy", "DUKASCOPY_DEMO1_", "A", None)).expect("park 1");
    park(dir, record("dukascopy", "DUKASCOPY_DEMO2_", "B", None)).expect("park 2");
    let keep: Vec<ConfirmationRecord> =
        read(dir).expect("read").into_iter().filter(|r| r.key_prefix.ends_with("DEMO2_")).collect();
    replace_all(dir, keep).expect("replace");
    let got = read(dir).expect("read back");
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].key_prefix, "DUKASCOPY_DEMO2_");
}

/// Nothing in a record can hold a credential: the `Debug` is derived precisely because there is
/// nothing to redact, and this test is what makes that claim checkable if a field is ever added.
#[test]
fn a_record_carries_no_field_a_credential_could_land_in() {
    let r = record("dukascopy", "DUKASCOPY_DEMO1_", "DEMO1abcd", Some("3709890"));
    let json = serde_json::to_value(&r).expect("serializes");
    // ⚠ **SORTED, and that is not tidiness — an order-sensitive assertion here FAILS IN ONE
    // BUILD AND PASSES IN ANOTHER.** `serde_json::Map` is a `BTreeMap` (sorted keys) by default
    // and an `IndexMap` (declaration order) under its `preserve_order` feature, and features are
    // UNIFIED across a build: `cargo test -p vike-model` got sorted keys while the workspace
    // nextest lane got declaration order, because something else in that graph turns the feature
    // on. Measured, both directions, on the same commit. What this test is about is the SET of
    // fields, so it sorts and the serializer's own business stays the serializer's.
    let mut fields: Vec<&str> =
        json.as_object().expect("object").keys().map(String::as_str).collect();
    fields.sort_unstable();
    assert_eq!(
        fields,
        vec![
            "at_ms",
            "handshake_account_id",
            "key_prefix",
            "observed_book",
            "observed_row",
            "venue"
        ],
        "a NEW field here needs an argument for why a credential cannot reach it"
    );
}
