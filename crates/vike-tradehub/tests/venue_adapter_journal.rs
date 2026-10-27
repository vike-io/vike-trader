//! `journal_venue_mounts` — the writer for the `venue_mounted` change-journal kind.
//!
//! What these pin is the pairing that makes the record worth writing: `vike_tradehub_core::venue_arming`
//! is a PREDICTION and `AssembledRuntime::live_venues` is the OUTCOME, and only the two together can say
//! anything `vike-backend venues` does not already say. The case that carries the whole channel is
//! [`a_venue_that_probed_live_and_did_not_arm_records_adapter_failed`] — a divergence NO pre-assembly
//! reading can produce, and which nothing else in this tree records.
//!
//! Moved from `crates/vike-run/tests/` when the venue-adapter contract finished
//! (docs/decisions/0096): `vike-run` held no registry, so it ran through the transitional one until
//! then. Default build only: `vike-tradehub-core`'s transitional registry carried ibkr, fxcm and polymarket
//! `FeatureAbsent` in every build and these assertions were written against that; each venue's
//! feature-on half is its own `crates/vike-gatewayhub/tests/trade_roster/ibkr_adapter.rs`,
//! `crates/vike-gatewayhub/tests/trade_roster/fxcm_adapter.rs` or `crates/vike-gatewayhub/tests/trade_roster/polymarket_adapter.rs`.
//! That crate-level `#![cfg]` also makes this its own test binary rather than a `daemon` member.
//! ⚠ It narrows where this runs: in `crates/vike-run/tests/` it also ran in the fxcm, ibkr and
//! polymarket-stack lanes, with vike-run's marker feature on. It runs in the default lane only
//! now; `crates/vike-tradehub/tests/daemon/live_gate_paper.rs` still drives `assemble_runtime` in those
//! lanes.
//! (W15: this crate has no venue features any more; the scripted table fixes ibkr, fxcm and polymarket
//! absent, as the default gatewayhub build answers, so these assertions run in every build.)

// The in-process backend the roster tests drive the fold through (wave 2, W9a).
#[path = "support/local_backend.rs"]
mod local;

use std::collections::{HashMap, HashSet};

use vike_tradehub_core::{AdapterPolicy, ArmingBlock, journal_venue_mounts};

fn set(venues: &[&str]) -> HashSet<String> {
    venues.iter().map(|v| v.to_string()).collect()
}

/// The deployment map: a credential belongs to its ACCOUNT (decision 0156 item 6), so the process
/// map carries none.
fn arming_vars() -> HashMap<String, String> {
    HashMap::new()
}

/// One active DEFAULT-account row for `venue` at `mode`, holding [`keys`].
fn policy_with(venue: &str, mode: vike_model::AccountTier) -> AdapterPolicy {
    AdapterPolicy::default().with_credentials(
        venue,
        &vike_model::accounts::account_keys::AccountLabel::Default,
        mode,
        &[("API_KEY", "k"), ("API_SECRET", "s")],
    )
}

/// The rows, computed the way the daemon computes them — through `vike_tradehub_core::venue_arming`,
/// from the map that is correct at that moment. `journal_venue_mounts` takes rows precisely so
/// a caller has to make this step deliberately, so the tests make it too.
fn rows(
    vars: &HashMap<String, String>,
    policy: &AdapterPolicy,
) -> Vec<vike_tradehub_core::VenueArming> {
    crate::local::all_rows(vars, policy)
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

/// **The exemption.** An account no active row states a tier for asked nothing of a venue, and is not
/// news: it does not run (decision 0156) for the documented reason, and the `account` table itself is
/// the record of what was asked. Writing 14 such lines per start would bury the two that matter.
#[test]
fn an_account_no_row_names_writes_no_line() {
    let d = tmp("allpaper");
    // ANTI-VACUITY: the map ARMS a `live` account, so silence below is the account table's doing.
    let live = policy_with("binance", vike_model::AccountTier::Live);
    assert!(
        crate::local::rows("binance", &arming_vars(), Some(&live))
            .iter()
            .all(|r| r.effective == Some(vike_model::AccountTier::Live)),
        "the keys arm a live account, so silence below is the account table's doing"
    );

    let out = journal_venue_mounts(
        Some(d.path()),
        &rows(&arming_vars(), &AdapterPolicy::default()),
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
    let policy = policy_with("binance", vike_model::AccountTier::Demo);
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

/// **THE CASE THIS CHANNEL EXISTS FOR.** The account's tier permitted it, the credentials were present,
/// the pre-assembly probe said it would arm — and it is absent from `live_venues`, so its connect
/// failed and it does NOT RUN (decision 0156). No pre-assembly reading can produce this row, which is why
/// the arming projection cannot answer it and the journal must.
#[test]
fn a_venue_that_probed_live_and_did_not_arm_records_adapter_failed() {
    let d = tmp("failed");
    let vars = arming_vars();
    let policy = policy_with("binance", vike_model::AccountTier::Demo);

    // ANTI-VACUITY, and it is load-bearing here: if the probe did NOT say "would arm", this test
    // would pass by recording an ordinary credential block and prove nothing about `AdapterFailed`.
    assert!(
        crate::local::rows("binance", &vars, Some(&policy))
            .iter()
            .any(|r| r.effective == Some(vike_model::AccountTier::Demo)),
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
    assert!(v["target"].get("effective").is_none(), "it did not arm, so it runs at no tier: {v}");
    assert_eq!(
        v["target"]["block"],
        ArmingBlock::AdapterFailed.as_str(),
        "a venue that PROBED live and did not appear in live_venues failed its connect — not a \
         credential or tier block, which only the outcome can tell: {v}"
    );
}

/// No project above the working directory ⇒ NOTHING is written, rather than a ledger invented in
/// whatever directory the process happens to sit in. The rule `vike_boot::journal_boot_settings`
/// follows and `vike-tradehub`'s `a_journal_less_surface_writes_nothing` pins.
#[test]
fn no_state_dir_writes_nothing() {
    let policy = policy_with("binance", vike_model::AccountTier::Live);
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
/// account per venue — every box with no labelled account — must produce bytes a reader of the old
/// ledger can still parse without learning a new key.
///
/// The assertion is on the ABSENCE of the key, not on its value: `"account": null` would also
/// round-trip, and would also be a new field in every line of every existing deployment's ledger.
#[test]
fn a_default_account_record_carries_no_account_field_at_all() {
    let dir = tmp("defaultacct");
    let vars = arming_vars();
    let policy = policy_with("binance", vike_model::AccountTier::Demo);
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
    let alt =
        vike_model::accounts::account_keys::AccountLabel::parse("ALT").expect("a legal label");
    let row = vike_tradehub_core::VenueArming {
        venue: "binance",
        label: alt,
        tier: Some(vike_model::AccountTier::Demo),
        effective: Some(vike_model::AccountTier::Demo),
        block: ArmingBlock::None,
        account_ids: vec![7],
    };
    journal_venue_mounts(Some(dir.path()), &[row], &set(&["binance#ALT"]), "t", 1);

    let records = lines(dir.path());
    assert_eq!(records.len(), 1, "{records:?}");
    let target = &records[0]["target"];
    assert_eq!(target["venue"], "binance", "the VENUE stays the roster id");
    assert_eq!(target["account"], "ALT", "…and the ACCOUNT is the label, not the route key");
    assert_eq!(target["effective"], "demo", "the route key is what said it armed");
}
