//! **The BOOT ANCHOR's behaviour** — `vike_boot::journal_boot_settings`, driven against a real
//! directory tree.
//!
//! # What this file is asserting, and what it deliberately is not
//!
//! The anchor is a **BRACKET, not a detector.** Nothing in this workspace observes a hand edit of
//! `<project>/settings/policy.toml` — there is no file watcher in the tree (`notify` is not a
//! workspace dependency) and the running process does not notice the edit at all. So there is no
//! test here called `a_hand_edit_is_detected`, and there must never be one. What IS gated is the
//! weaker property the anchor actually buys:
//! [`two_starts_with_a_changed_ceiling_bracket_the_change`] — two consecutive records that DISAGREE
//! prove something changed between them, and two that AGREE prove it did not. Both halves matter:
//! a record whose target varied for reasons unrelated to the settings (a pid, a timestamp) would
//! make "these two disagree" trivially true and the bracket unfalsifiable, which is why
//! [`target_of`] compares the target object rather than the line.
//!
//! # Raw lines rather than a JSON parser, on purpose
//!
//! Two of the four properties here are about the SERIALIZED SHAPE — `null` versus an omitted pair,
//! and a key that must not appear at all — and a `serde_json::Value` round-trip is blind to exactly
//! that distinction in one direction and would need the parser to be trusted in the other.
//! `vike_model::change_journal`'s own `a_settings_write_lands_as_one_json_line` makes the same call
//! for the same reason. It also keeps `serde_json` out of this crate's dev-dependencies, which
//! `tests/dependency_floor.rs` would tolerate and which is still one less edge.

use std::path::{Path, PathBuf};

use tempfile::TempDir;
use vike_boot::{boot_ceilings, journal_boot_settings};
use vike_config::Policy;

/// 2026-08-21T00:00:00Z — the same anchor `vike_model::change_journal`'s own suite stamps, so a
/// file name quoted by a failure here is comparable with one quoted there.
const T: i64 = 1_787_356_800_000;

/// One day later, still inside the same month — so a two-start bracket lands in ONE file and the
/// test reads it with one `read_to_string`.
const T_NEXT_DAY: i64 = T + 86_400_000;

/// The monthly file both timestamps above belong to.
const MONTH: &str = "changes-2026-08.jsonl";

/// The version string every call here stamps, distinct enough to be recognised in a raw line.
const VER: &str = "0.1.0-anchor-test";

/// `<root>/state` — the ALREADY-RESOLVED state directory a composition root hands in. Deliberately
/// a sub-directory that does not exist yet: `settings/state/changes` is not a marker and a fresh
/// install has none, so the write has to create it.
fn state_dir(root: &Path) -> PathBuf {
    root.join("state")
}

/// Every line of the month's journal file, in write order.
fn journal_lines(state: &Path) -> Vec<String> {
    let path = state.join("changes").join(MONTH);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("journal file {} could not be read: {e}", path.display()))
        .lines()
        .map(str::to_string)
        .collect()
}

/// The record's `target` object, sliced out of the raw line.
///
/// ⚠ This is what makes the bracket falsifiable. `ts_ms`, `seq` and `proc.pid` differ between any
/// two records by construction, so comparing whole LINES would report "these two disagree" for two
/// starts that read an identical `policy.toml` — which is the reading a bracket must never give.
/// `reason` is the only field that could sit between the two anchors and a boot record never
/// carries one (`Change::boot_settings` sets it to `None` and nothing here attaches one).
fn target_of(line: &str) -> &str {
    let start = line.find(r#""target":"#).unwrap_or_else(|| panic!("no target in {line}"));
    let end = line.find(r#","proc":"#).unwrap_or_else(|| panic!("no proc in {line}"));
    assert!(start < end, "the wire order is target then proc: {line}");
    &line[start..end]
}

/// ONE start, ONE record, `origin: "boot"` — and the count tracks the CALLS rather than being a
/// property of a file that could only ever hold one line.
#[test]
fn a_start_writes_exactly_one_boot_record_with_origin_boot() {
    let dir = TempDir::new().expect("temp dir");
    let state = state_dir(dir.path());
    assert!(!state.exists(), "precondition: the state tree does not exist yet");

    let path = journal_boot_settings(Some(&state), &Policy::default(), VER, T)
        .expect("a resolved state directory writes")
        .expect("the append succeeds");
    assert_eq!(path.file_name().unwrap(), MONTH, "the record lands in its own month");
    assert_eq!(path.parent().unwrap(), state.join("changes"), "…under <state dir>/changes");

    let lines = journal_lines(&state);
    assert_eq!(lines.len(), 1, "one start is one record: {lines:?}");
    let line = &lines[0];
    assert!(line.contains(r#""kind":"boot_settings""#), "{line}");
    assert!(line.contains(r#""origin":"boot""#), "the actor is the process starting: {line}");
    assert!(line.contains(r#""outcome":"applied""#), "the values ARE in effect: {line}");
    assert!(line.contains(&format!(r#""ver":"{VER}""#)), "the writing build names itself: {line}");
    assert!(!line.contains(r#""user""#), "no invented human actor: {line}");

    // …and a SECOND start appends a second line. Without this, "exactly one" would also be
    // satisfied by a journal that truncates — which is how an append-only store stops being one.
    journal_boot_settings(Some(&state), &Policy::default(), VER, T_NEXT_DAY)
        .expect("a second start writes")
        .expect("the append succeeds");
    assert_eq!(journal_lines(&state).len(), 2, "one record PER start, and the first survives");
}

/// ⚠ An UNSET ceiling is a `null` VALUE, never an omitted pair.
///
/// "Uncapped" and "this build did not report that key" must not read identically to somebody
/// comparing two records a month apart — the same distinction
/// `vike_model::change_journal::SettingTarget`'s `old` draws in the opposite direction, and the one
/// an incident review turns on.
#[test]
fn an_unset_ceiling_is_null_and_not_an_omitted_pair() {
    let dir = TempDir::new().expect("temp dir");
    let state = state_dir(dir.path());
    let policy = Policy { max_notional_per_order: Some(250.0), ..Policy::default() };
    assert!(policy.market_slippage.is_none(), "precondition: the fixture really is unset");

    journal_boot_settings(Some(&state), &policy, VER, T).expect("writes").expect("appends");
    let line = &journal_lines(&state)[0];

    assert!(
        line.contains(r#"["policy.market_slippage",null]"#),
        "an unset ceiling is a null VALUE — the KEY is always present and only the value is \
         missing: {line}"
    );
    assert!(
        !line.contains(r#"["policy.market_slippage"]"#),
        "…and it is NOT a one-element pair, which would read as 'this build did not report it': \
         {line}"
    );
    // …and the SET one alongside it, so the null above is about that ceiling being unset rather
    // than about every value collapsing to null.
    assert!(
        line.contains(r#"["policy.max_notional_per_order","250"]"#),
        "a set ceiling carries its value: {line}"
    );
}

/// **THE BRACKET.** Two starts, a ceiling changed between them, and the two records DISAGREE at
/// that key — without either of them claiming to know who changed it or when.
///
/// Both directions are asserted, because only the pair is a bracket: records that also disagreed
/// when nothing changed would be noise, and records that agreed when something did would be a lie.
#[test]
fn two_starts_with_a_changed_ceiling_bracket_the_change() {
    let dir = TempDir::new().expect("temp dir");
    let state = state_dir(dir.path());

    let before = Policy { max_notional_per_order: Some(100.0), ..Policy::default() };
    journal_boot_settings(Some(&state), &before, VER, T).expect("writes").expect("appends");

    // …an operator opens `policy.toml` in an editor here. NOTHING in this workspace observes that:
    // no file watcher exists, and the process that read the file has already exited. The next start
    // is the only thing that ever notices, and all it can prove is that the two disagree.
    let after = Policy { max_notional_per_order: Some(250.0), ..Policy::default() };
    journal_boot_settings(Some(&state), &after, VER, T_NEXT_DAY).expect("writes").expect("appends");

    let lines = journal_lines(&state);
    assert_eq!(lines.len(), 2, "two starts, two records: {lines:?}");
    assert!(lines[0].contains(r#"["policy.max_notional_per_order","100"]"#), "{}", lines[0]);
    assert!(lines[1].contains(r#"["policy.max_notional_per_order","250"]"#), "{}", lines[1]);
    assert_ne!(
        target_of(&lines[0]),
        target_of(&lines[1]),
        "the bracket: two consecutive anchors that disagree are the whole instrument"
    );

    // The OTHER direction, and the one that makes the assertion above mean something: a third start
    // that read the SAME file must produce a target IDENTICAL to its predecessor's, or "they
    // disagree" is true of every pair and proves nothing about the settings.
    journal_boot_settings(Some(&state), &after, VER, T_NEXT_DAY + 1)
        .expect("writes")
        .expect("appends");
    let lines = journal_lines(&state);
    assert_eq!(
        target_of(&lines[1]),
        target_of(&lines[2]),
        "an unchanged ceiling must produce an identical target — a bracket that always disagrees \
         is not a bracket"
    );

    // …and a ceiling that goes from SET to UNSET is still a visible disagreement rather than a
    // record that quietly stops mentioning it. This is the null-vs-omitted property inside the
    // instrument that depends on it.
    journal_boot_settings(Some(&state), &Policy::default(), VER, T_NEXT_DAY + 2)
        .expect("writes")
        .expect("appends");
    let lines = journal_lines(&state);
    assert!(
        lines[3].contains(r#"["policy.max_notional_per_order",null]"#),
        "a ceiling that was removed reads as null, not as a missing key: {}",
        lines[3]
    );
    assert_ne!(
        target_of(&lines[2]),
        target_of(&lines[3]),
        "…and it brackets like any other change"
    );
}

/// **No resolvable project ⇒ NOTHING is written.** Not a file, not a directory, not an invented
/// location — the honest degradation `vike_tradehub::server::SettingsShowSource`'s `change_journal`
/// already makes, and the rule
/// `crates/vike-tradehub/tests/settings_write_journal.rs`'s `a_journal_less_surface_writes_nothing`
/// states for the other channel.
#[test]
fn a_root_with_no_resolvable_project_writes_nothing() {
    let dir = TempDir::new().expect("temp dir");
    let state = state_dir(dir.path());
    let policy = Policy { max_notional_per_order: Some(250.0), ..Policy::default() };

    assert!(
        journal_boot_settings(None, &policy, VER, T).is_none(),
        "no state directory means no record, and no location to invent one at"
    );
    assert!(!state.exists(), "not even the state directory is created");
    assert_eq!(
        std::fs::read_dir(dir.path()).expect("read temp root").count(),
        0,
        "a journal-less start leaves the tree exactly as it found it"
    );

    // …and the SAME fixture WITH a state directory does write, so the absence above is the `None`
    // rather than a fixture that could never have written anything.
    journal_boot_settings(Some(&state), &policy, VER, T).expect("writes").expect("appends");
    assert_eq!(journal_lines(&state).len(), 1, "the fixture is capable of writing");
}

/// ⚠ **A ceiling that is SET but NOT EFFECTIVE must not appear in a record claiming the effective
/// ceilings.**
///
/// `crates/vike-mount/src/policy.rs`'s `MountPolicy::from` deliberately does not carry the leverage
/// ceiling — its `1.0` default would clamp every deployment with no `policy.toml` to 1x — so a
/// value written there is set and enforces nothing, and
/// `crates/vike-config/tests/policy_is_consumed.rs`'s row for it says exactly that. A boot anchor
/// that listed it would assert the opposite of the gate.
#[test]
fn the_anchor_never_claims_a_ceiling_that_is_not_effective() {
    let dir = TempDir::new().expect("temp dir");
    let state = state_dir(dir.path());
    // Spelled as a struct-update literal rather than a field assignment on a binding named
    // `policy`, because `policy_is_consumed.rs` reads that spelling as a READ of the field and
    // turns CI red — see this test's own doc for why the field is excluded in the first place.
    let policy = Policy { max_leverage: 25.0, ..Policy::default() };

    assert!(
        boot_ceilings(&policy).iter().all(|(k, _)| !k.contains("leverage")),
        "the ceiling set names a value that is set and enforces nothing: {:?}",
        boot_ceilings(&policy)
    );

    journal_boot_settings(Some(&state), &policy, VER, T).expect("writes").expect("appends");
    let line = &journal_lines(&state)[0];
    assert!(!line.contains("leverage"), "…and it does not reach the wire either: {line}");

    // The anti-vacuity half: every ceiling that IS effective must be there, or this test would pass
    // just as well against a record that carried nothing at all.
    for key in [
        "policy.max_notional_per_order",
        "policy.market_slippage",
        "policy.halt_admit",
        // ⚠ The per-venue ARMING ceiling — added when stage 3 folded it in at
        // `crates/vike-mount/src/lib.rs`'s `make_engine_with_legs` and
        // `crates/vike-config/tests/policy_is_consumed.rs`'s row for it became a `Consumed::At`.
        // It is the one ceiling whose DEFAULT changes an outcome, so a record that claims the
        // effective ceilings and omits it would be silent about the most consequential line.
        "policy.venues",
    ] {
        assert!(line.contains(key), "{key} is effective and must be recorded: {line}");
    }
    assert!(
        line.contains(r#"["policy.halt_admit","admit""#),
        "an enum ceiling with a compiled-in default is EFFECTIVE, so it is never null: {line}"
    );
}

/// **The arming ceiling is recorded AGGREGATED, and the aggregation names the risky tiers.**
///
/// `live=[…] demo=[…] paper=<n>` rather than fourteen entries — the shape
/// `vike_boot::boot_ceilings`'s doc argues for and the size test below depends on. Both halves are
/// asserted: an armed venue must be NAMED (an incident review asks "which venues could this process
/// have traded on?", and a count cannot answer it), and the paper majority must be a COUNT (naming
/// it would be most of the record and would say nothing actionable).
#[test]
fn the_arming_ceiling_is_recorded_as_named_tiers_and_a_paper_count() {
    let roster = vike_model::VENUES.len();

    // A machine with no `[venues]` table: everything paper, and the record says so out loud rather
    // than by omission — this is the state in which every venue is capped, so silence would be the
    // worst possible reading.
    let (_, value) = boot_ceilings(&Policy::default())
        .into_iter()
        .find(|(k, _)| k == "policy.venues")
        .expect("the arming ceiling is an effective ceiling");
    assert_eq!(value.as_deref(), Some(format!("live=[] demo=[] paper={roster}").as_str()));

    // …and a machine that armed two venues names both, in roster order, with the rest counted.
    let policy = Policy {
        venues: vike_config::VenuePolicy::default()
            .declare("bybit", vike_config::VenueMode::Live)
            .declare("binance", vike_config::VenueMode::Demo),
        ..Policy::default()
    };
    let (_, value) = boot_ceilings(&policy)
        .into_iter()
        .find(|(k, _)| k == "policy.venues")
        .expect("the arming ceiling is an effective ceiling");
    assert_eq!(
        value.as_deref(),
        Some(format!("live=[bybit] demo=[binance] paper={}", roster - 2).as_str()),
        "the armed tiers are NAMED; only the paper majority is a count"
    );
}

/// A record's size, stated where it can be checked rather than estimated in prose — the number the
/// module doc's rate arithmetic rests on.
///
/// It is a CEILING assertion with a floor beneath it, not a pin: the point is that a boot anchor is
/// a few hundred bytes, so a decade of restarts is kilobytes rather than the 341 GB log file this
/// workspace once wrote.
#[test]
fn one_boot_record_is_a_few_hundred_bytes() {
    let dir = TempDir::new().expect("temp dir");
    let state = state_dir(dir.path());
    let policy = Policy {
        max_notional_per_order: Some(250.0),
        market_slippage: Some(0.002),
        ..Policy::default()
    };
    journal_boot_settings(Some(&state), &policy, VER, T).expect("writes").expect("appends");

    let bytes = journal_lines(&state)[0].len() + 1; // + the newline the file carries
    assert!(
        (150..=600).contains(&bytes),
        "a boot record measured {bytes} bytes. Under 150 means the ceilings stopped being \
         recorded; over 600 means the rate arithmetic in `journal_boot_settings`'s doc needs \
         redoing (the hard cap is vike_model::change_journal::MAX_RECORD_BYTES at 4096)"
    );
}
