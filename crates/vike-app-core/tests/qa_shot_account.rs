//! **The `connections-account` capture arm and `scripts/qa_shots.sh` must agree**, and this file is
//! the only thing that can hold them equal.
//!
//! The arm (`vike_app_core::startup`, `shot_win == Some("connections-account")`) opens the
//! Connections panel with the account [`vike_app_core::startup::SHOT_ACCOUNT_LABEL`] selected. That
//! account exists **only because a key names it** — there is no registry a create could write a row
//! to, so `vike_connections::AccountGrids::from_vars` derives the account set from the key names in
//! the store. The capture is therefore judgeable only while the sheet's fixture store holds a
//! credential carrying that exact label. The two halves live in two languages, and neither compiler
//! can see the other.
//!
//! # The failure this exists to make loud
//!
//! Drift here is SILENT in the worst possible way: the arm still opens the right window, the app
//! still boots, the capture still writes a PNG and the index still counts it. What changes is that
//! `crates/vike-connections/src/view.rs`'s `account_strip` finds no store entry for the selected
//! label and renders the `(new)` chip over an all-absent grid — a frame that looks like a working
//! capture of an empty account rather than a broken capture of a real one. The sheet asserts
//! nothing about pixels by design, so nothing downstream would object either.
//!
//! # ⚠ It runs the REAL derivation over the script's own bytes
//!
//! The fixture store's lines are parsed out of the script and fed to the very function the tool body
//! calls, rather than compared against key names composed here. Two reasons, one of them a gate:
//!
//! * **Strength.** `AccountGrids::from_vars` does not merely parse a label — it DROPS a label whose
//!   grid lights no dot, because "the store holds this account" is defined as "this grid shows
//!   something for it". A key with a perfect-looking label on a name no venue reads would enumerate
//!   nothing, and a test that only checked the suffix would call that a pass.
//! * **⚠ No env-var-shaped literal and no key COMPOSITION may live here.** The labelled key space is
//!   unbounded by design and can have no `vike_ops::settings::SETTINGS` row, so spelling one as a
//!   literal demands a declaration that cannot exist (`crates/vike-connections/src/status.rs`'s
//!   module doc carries that argument — it is why that crate's account tests are integration tests
//!   too). Composing one is barred from the other side: `crates/vike-ops/tests/settings_registry.rs`'s
//!   `generated_key_sites_are_derived_and_pinned` pins the files that call the key BUILDERS, and a
//!   test calling `credential_key` would enrol this crate in the whole generated grid — measured,
//!   not guessed: an earlier draft of this file did exactly that and reddened that gate plus
//!   `every_generated_key_is_declared`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use vike_app_core::startup::{SHOT_ACCOUNT_LABEL, shot_account_label};
use vike_connections::AccountGrids;

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

fn qa_shots() -> String {
    let path = workspace_root().join("scripts").join("qa_shots.sh");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("the contact sheet script must be readable at {path:?}: {e}"))
}

/// The capture's name in the matrix — the `shot_app` row's first argument, and the PNG's basename.
const CAPTURE: &str = "05b-connections-account";

/// The shell that opens the fixture store's heredoc. Anchoring on the REDIRECT rather than on a
/// filename means the block is found by what it writes into, which is the fact that matters.
const FIXTURE_HEREDOC: &str = ">\"$ACCT_STORE\" <<EOF";

/// The `.env` map `scripts/qa_shots.sh` writes into its labelled-account settings root, parsed out
/// of the heredoc that writes it.
///
/// Comment and blank lines are skipped exactly as a `.env` reader skips them; nothing else is
/// interpreted, because nothing else needs to be — the key lines are literal in the script and the
/// only expanded line is the marker comment.
fn fixture_store(script: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let mut lines = script.lines().skip_while(|l| !l.contains(FIXTURE_HEREDOC));
    assert!(
        lines.next().is_some(),
        "scripts/qa_shots.sh no longer writes a fixture store: the heredoc opening with {FIXTURE_HEREDOC} is gone, so the labelled-account capture has no account to select"
    );
    for line in lines.take_while(|l| *l != "EOF") {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = t.split_once('=') {
            out.insert(k.to_string(), v.to_string());
        }
    }
    out
}

/// One `shot_app` row of the capture matrix, continuation lines and all, or `None` when the matrix
/// has no such row.
///
/// ⚠ **Anchored at `shot_app <name> ` on a line start, and that precision is the whole test.** The
/// obvious spelling — "does the script mention `VIKE_SHOT_WIN=connections-account`" — is VACUOUS
/// here and was measured so: deleting the matrix row outright left the test green, because the
/// judge-manifest `pose` string for this capture quotes the same knob back to a reader, and
/// `ACCT_SETTINGS` appears wherever the fixture root is built. A sheet that seeds a perfect fixture
/// store, documents the pose, and never opens the window is a capture of nothing — which is exactly
/// what a mention-anywhere search cannot see.
fn matrix_row(script: &str, name: &str) -> Option<String> {
    let head = format!("shot_app {name} ");
    let mut lines = script.lines().skip_while(|l| !l.starts_with(&head));
    let mut row = lines.next()?.to_string();
    while row.trim_end().ends_with('\\') {
        let next = lines.next()?;
        row.push('\n');
        row.push_str(next);
    }
    Some(row)
}

/// **The sheet's fixture store holds the account the arm selects — through the app's own
/// derivation, over the script's own bytes.**
///
/// Reddens on a rename of [`SHOT_ACCOUNT_LABEL`], on the script's fixture keys losing their label,
/// on the fixture naming a key no venue reads (which enumerates NO account, however good the label
/// looks), and on the account grammar changing shape under any of them.
#[test]
fn the_contact_sheet_seeds_the_account_the_capture_arm_selects() {
    let vars = fixture_store(&qa_shots());
    assert!(
        !vars.is_empty(),
        "the fixture heredoc parsed to nothing — every assertion below would be vacuous"
    );

    let want = shot_account_label();
    assert_eq!(
        want.text(),
        Some(SHOT_ACCOUNT_LABEL),
        "the arm's own label must parse — see startup.rs's \
         the_labelled_account_arm_names_a_valid_label"
    );

    let grids = AccountGrids::from_vars(&vars);
    let labels: Vec<_> = grids.labels().cloned().collect();
    assert_eq!(
        labels,
        vec![want.clone()],
        "the account set the panel will derive from the sheet's fixture store must be exactly the \
         account the arm selects. Anything else and 05b-connections-account renders the `(new)` \
         chip over an all-absent grid: a capture that ran, saved a PNG, and shows an empty account \
         while claiming to show a filled one."
    );

    let grid = grids.grid_for(&want).expect("the label enumerated, so it has a grid");
    assert!(
        grid.iter().any(|s| s.sim || s.demo || s.live),
        "the seeded account must light at least one credential dot — that dot is the only thing in \
         the frame that distinguishes this capture from the plain 05-connections one"
    );
}

/// **The sheet actually RUNS the arm, and runs it against the fixture root.**
///
/// The test above would pass on a script that seeded a perfect fixture store and never opened the
/// window — and equally on one that ran the arm against the credential-free root, where the account
/// does not exist. Both are captures of nothing; neither is visible from the fixture store alone.
///
/// Every assertion is made against the ROW ([`matrix_row`]), never against the script as a whole —
/// see that function for the measured reason.
#[test]
fn the_contact_sheet_runs_the_arm_against_the_fixture_root() {
    let script = qa_shots();
    let row = matrix_row(&script, CAPTURE).unwrap_or_else(|| {
        panic!(
            "scripts/qa_shots.sh has no `shot_app {CAPTURE} …` row — a seeded fixture store that \
             no capture opens is a store nobody ever sees, and the labelled-account screen goes \
             back to being reachable by no capture at all"
        )
    });
    assert!(
        row.contains("VIKE_SHOT_WIN=connections-account"),
        "the {CAPTURE} row must invoke the arm; it reads: {row}"
    );
    assert!(
        row.contains("ACCT_SETTINGS"),
        "the {CAPTURE} row must point VIKE_SETTINGS_DIR at the generated fixture root — against \
         the sheet's ordinary credential-free root the labelled account exists in no store at \
         all, and the capture shows one chip. It reads: {row}"
    );
}

/// ⚠ **The anti-vacuity half of the test above.**
///
/// [`matrix_row`]'s anchoring is what makes that test able to fail, and anchoring is invisible from
/// a passing run — the deleted-row mutation that motivated it passed a mention-anywhere assertion
/// with the row gone. So this pins the discrimination directly: a name the matrix does not run
/// resolves to `None` even when the script talks about it at length.
#[test]
fn a_capture_the_matrix_does_not_run_resolves_to_no_row() {
    let script = qa_shots();
    assert!(
        matrix_row(&script, CAPTURE).is_some(),
        "the real row must resolve, or nothing below is a discrimination"
    );
    assert!(
        matrix_row(&script, "99-not-a-capture").is_none(),
        "a name with no `shot_app` row must not resolve — otherwise the test above cannot fail"
    );
    // ⚠ THE probe, and the one the assertion above cannot stand in for. `99-not-a-capture` appears
    // NOWHERE in the script, so a `matrix_row` degraded back to the mention-anywhere form this file
    // exists to forbid — `script.contains(name).then(|| script.to_string())` — still answers `None`
    // for it and every test here stays green. MEASURED: that exact degradation passed the whole
    // file before this line existed. `connections-account` is the discrimination, because the
    // script says it five times (the header note, the pose arm, the row's own knob, the judge
    // manifest) and runs NO `shot_app connections-account` row — only `05b-connections-account`.
    // So a mention-anywhere matcher answers `Some` here and anchoring answers `None`.
    assert!(
        matrix_row(&script, "connections-account").is_none(),
        "a name the script MENTIONS but does not run must not resolve — this is the assertion that \
         fails when `matrix_row` stops anchoring at a line-leading `shot_app <name> `"
    );
    assert!(
        script.contains("connections-account"),
        "…and the probe above is only a discrimination while the script really does mention it"
    );
    // The precise trap: this string IS in the script (the judge manifest's pose quotes it), and a
    // mention-anywhere check would call that a running capture.
    assert!(
        script.matches("VIKE_SHOT_WIN=connections-account").count() > 1,
        "the knob is mentioned outside its row (the pose text), which is why row-anchoring matters"
    );
}
