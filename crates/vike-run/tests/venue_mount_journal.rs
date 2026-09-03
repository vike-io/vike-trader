//! `journal_venue_mounts` — the writer for the `venue_mounted` change-journal kind.
//!
//! What these pin is the pairing that makes the record worth writing: `vike_mount::venue_arming`
//! is a PREDICTION and `Node::live_venues` is the OUTCOME, and only the two together can say
//! anything the arming screen does not already say. The case that carries the whole channel is
//! [`a_venue_that_probed_live_and_did_not_arm_records_mount_failed`] — a divergence NO pre-mount
//! reading can produce, and which nothing else in this tree records.

use std::collections::{HashMap, HashSet};

use vike_run::{journal_venue_mounts, MountPolicy, MOUNT_FAILED};

fn set(venues: &[&str]) -> HashSet<String> {
    venues.iter().map(|v| v.to_string()).collect()
}

/// Credentials in the shape the CEX live arms actually read, for two venues.
fn arming_vars() -> HashMap<String, String> {
    HashMap::from([
        ("BINANCE_DEMO_API_KEY".to_string(), "k".to_string()),
        ("BINANCE_DEMO_API_SECRET".to_string(), "s".to_string()),
        ("BYBIT_DEMO_API_KEY".to_string(), "k".to_string()),
        ("BYBIT_DEMO_API_SECRET".to_string(), "s".to_string()),
    ])
}

fn policy_with(venue: &str, mode: vike_mount::VenueMode) -> MountPolicy {
    MountPolicy { venues: MountPolicy::default().venues.declare(venue, mode), ..Default::default() }
}

/// The rows, computed the way the daemon computes them — through `vike_mount::venue_arming`,
/// from the map that is correct at that moment. `journal_venue_mounts` takes rows precisely so
/// a caller has to make this step deliberately, so the tests make it too.
fn rows(vars: &HashMap<String, String>, policy: &MountPolicy) -> Vec<vike_run::VenueArming> {
    vike_run::venue_arming(vars, &policy.venues)
}

fn lines(dir: &std::path::Path) -> Vec<serde_json::Value> {
    let changes = dir.join("changes");
    let Ok(rd) = std::fs::read_dir(&changes) else { return Vec::new() };
    let mut out = Vec::new();
    for e in rd.flatten() {
        let p = e.path();
        if p.extension().is_none_or(|x| x != "jsonl") {
            continue;
        }
        for l in std::fs::read_to_string(&p).unwrap().lines() {
            out.push(serde_json::from_str(l).unwrap());
        }
    }
    out
}

/// A state directory the CALLER owns: the returned guard removes it — and the `changes/*.jsonl`
/// `journal_venue_mounts` writes into it — when it drops, on the unwind path too, so a failing
/// assertion cleans up exactly like a passing one. **Bind it for the whole test** and read the path
/// off it (`let d = tmp("armed"); … Some(d.path())`); binding to a bare `_` would delete the
/// directory before the writer ran.
///
/// ⚠ The old spelling was `env::temp_dir().join(format!("vmj-{tag}-{pid}"))` + `remove_dir_all` +
/// `create_dir_all`, with a `remove_dir_all` at the end of each test. That `remove_dir_all` at the
/// TOP was a pre-clean against PID reuse, not cleanup, and it could not help across users: the CI box
/// runs tests as `the CI user` (CI) and `the operator` (verification lanes), and a reused PID landing on
/// the other user's directory makes `create_dir_all` succeed (it exists) and the write fail
/// `PermissionDenied`. The per-test cleanup at the BOTTOM was skipped on every panic, so this
/// binary was still leaking ~26 directories a day into the CI box's `/tmp`. `tempfile`'s random suffix
/// closes the first half and its `Drop` the second.
fn tmp(tag: &str) -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix(&format!("vmj-{tag}-"))
        .tempdir()
        .expect("create the journal state dir")
}

/// **The exemption.** A venue at the DEFAULT `paper` ceiling that duly mounted paper is not news:
/// it is the documented answer for a box with no `[venues]` table, and `boot_settings` already
/// anchors the ceilings. Writing 14 such lines per start would bury the two that matter.
#[test]
fn a_paper_venue_that_mounted_paper_writes_no_line() {
    let d = tmp("allpaper");
    // ANTI-VACUITY: the map ARMS under the widest ceiling, so silence below is the ceiling's doing.
    assert!(vike_mount::would_mount_live("binance", &arming_vars()));

    let out = journal_venue_mounts(
        Some(d.path()),
        &rows(&arming_vars(), &MountPolicy::default()),
        &HashSet::new(),
        "0.0.0",
        1_700_000_000_000,
    );
    assert!(out.is_empty(), "no venue is interesting, so nothing is appended");
    assert!(lines(d.path()).is_empty(), "and no file is created either: {:?}", lines(d.path()));
}

/// An ARMED venue records both tiers, and no block — nothing refused it.
#[test]
fn an_armed_venue_records_both_tiers_and_no_block() {
    let d = tmp("armed");
    let policy = policy_with("binance", vike_mount::VenueMode::Demo);
    let out = journal_venue_mounts(
        Some(d.path()),
        &rows(&arming_vars(), &policy),
        &set(&["binance"]),
        "0.0.0",
        1_700_000_000_000,
    );
    assert_eq!(out.len(), 1, "one interesting venue, one line");
    out[0].as_ref().expect("the append succeeds");

    let ls = lines(d.path());
    assert_eq!(ls.len(), 1, "{ls:?}");
    let v = &ls[0];
    assert_eq!(v["kind"], "venue_mounted");
    assert_eq!(v["target"]["venue"], "binance");
    assert_eq!(v["target"]["requested"], "demo");
    assert_eq!(v["target"]["effective"], "demo");
    assert!(v["target"]["block"].is_null(), "nothing refused it: {v}");
}

/// **THE CASE THIS CHANNEL EXISTS FOR.** The ceiling permitted it, the credentials were present,
/// the pre-mount probe said it would arm — and it is absent from `live_venues`, so its connect
/// failed and it traded paper. No pre-mount reading can produce this row, which is precisely why
/// the arming screen cannot answer it and the journal must.
#[test]
fn a_venue_that_probed_live_and_did_not_arm_records_mount_failed() {
    let d = tmp("failed");
    let vars = arming_vars();
    let policy = policy_with("binance", vike_mount::VenueMode::Demo);

    // ANTI-VACUITY, and it is load-bearing here: if the probe did NOT say "would arm", this test
    // would pass by recording an ordinary credential block and prove nothing about MOUNT_FAILED.
    assert!(
        vike_mount::would_mount_live_under("binance", &vars, vike_mount::VenueMode::Demo),
        "the fixture must PROBE live, or the mount-failed branch is never reached"
    );

    // ...and the outcome says it did not arm.
    let out = journal_venue_mounts(
        Some(d.path()),
        &rows(&vars, &policy),
        &HashSet::new(),
        "0.0.0",
        1_700_000_000_000,
    );
    assert_eq!(out.len(), 1);

    let ls = lines(d.path());
    assert_eq!(ls.len(), 1, "{ls:?}");
    let v = &ls[0];
    assert_eq!(v["target"]["requested"], "demo");
    assert_eq!(v["target"]["effective"], "paper", "it did not arm: {v}");
    assert_eq!(
        v["target"]["block"], MOUNT_FAILED,
        "a venue that PROBED live and did not appear in live_venues failed its connect — not a \
         credential or ceiling block, which is why no ArmingBlock variant can carry it: {v}"
    );
}

/// No project above the working directory ⇒ NOTHING is written, rather than a ledger invented in
/// whatever directory the process happens to sit in. The rule `vike_boot::journal_boot_settings`
/// follows and `vike-tradehub`'s `a_journal_less_surface_writes_nothing` pins.
#[test]
fn no_state_dir_writes_nothing() {
    let policy = policy_with("binance", vike_mount::VenueMode::Live);
    let out = journal_venue_mounts(
        None,
        &rows(&arming_vars(), &policy),
        &set(&["binance"]),
        "0.0.0",
        1_700_000_000_000,
    );
    assert!(out.is_empty(), "no state dir ⇒ no records attempted, and no location invented");
}

/// **A DEFAULT-ACCOUNT BOX WRITES THE RECORD IT ALWAYS WROTE, FIELD FOR FIELD** — the
/// unchanged-box property, measured on the ledger rather than argued.
///
/// `VenueMountTarget` gained an `account` field when the mount began fanning out per account, and
/// it is `#[serde(skip_serializing_if = "Option::is_none")]` for exactly this reason: a box with one
/// account per venue — every box with no `[accounts]` table — must produce bytes a reader of the old
/// ledger can still parse without learning a new key.
///
/// The assertion is on the ABSENCE of the key, not on its value: `"account": null` would also
/// round-trip, and would also be a new field in every line of every existing deployment's ledger.
#[test]
fn a_default_account_record_carries_no_account_field_at_all() {
    let dir = tmp("defaultacct");
    let vars = arming_vars();
    let policy = policy_with("binance", vike_mount::VenueMode::Demo);
    journal_venue_mounts(Some(dir.path()), &rows(&vars, &policy), &set(&["binance"]), "t", 1);

    let records = lines(dir.path());
    assert!(!records.is_empty(), "the fixture must actually write, or this proves nothing");
    for r in &records {
        assert_eq!(r["kind"], "venue_mounted");
        let target = r["target"].as_object().expect("a target object");
        assert!(
            !target.contains_key("account"),
            "a default-account mount must write no `account` key: {r}"
        );
        // …and the fields it always carried are all still there.
        for field in ["venue", "requested", "effective"] {
            assert!(target.contains_key(field), "{field} missing from {r}");
        }
    }
}

/// …and a LABELLED account's record NAMES it, so the ledger can tell two books of one exchange
/// apart. Without this the absence above would be satisfiable by a field that is never written.
#[test]
fn a_labelled_account_record_names_the_account() {
    let dir = tmp("labelledacct");
    let alt = vike_model::account_keys::AccountLabel::parse("ALT").expect("a legal label");
    let row = vike_mount::VenueArming {
        venue: "binance",
        label: alt,
        ceiling: vike_mount::VenueMode::Demo,
        effective: vike_mount::VenueMode::Demo,
        block: vike_mount::ArmingBlock::None,
    };
    journal_venue_mounts(Some(dir.path()), &[row], &set(&["binance#ALT"]), "t", 1);

    let records = lines(dir.path());
    assert_eq!(records.len(), 1, "{records:?}");
    let target = &records[0]["target"];
    assert_eq!(target["venue"], "binance", "the VENUE stays the roster id");
    assert_eq!(target["account"], "ALT", "…and the ACCOUNT is the label, not the route key");
    assert_eq!(target["effective"], "demo", "the route key is what said it armed");
}
