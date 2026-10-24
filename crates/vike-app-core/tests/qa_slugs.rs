//! **Every `VIKE_SHOT_WIN=<slug>` the contact sheet's capture matrix passes names a window the
//! app opens**, and this file is the only thing that holds the script's spellings to the app's.
//!
//! # The failure this exists to make loud
//!
//! A `VIKE_SHOT_WIN` value that matches no arm and no Data Manager destination is not an error:
//! `vike_app_core::ui::startup::plan` opens the DEFAULT CHART and logs a warning nobody reads while
//! a headless capture runs. The capture then still writes a PNG, the index still counts it and the
//! contact sheet carries a BTCUSDT chart under the destination's filename. A destination's capture
//! spelling is its rail label folded (`DataDest::label` through `shot_key`), so a rename of a
//! destination renames its slug: this is the guard for exactly that, the one place a stale
//! `scripts/qa_shots.sh` row is caught before a human has to notice a chart where a screen should be.
//!
//! The check reads the script, not a copy of it: the slugs are parsed out of the matrix's own
//! `shot_app <name> VIKE_SHOT_WIN=<slug>` rows (anchored at a line start, as
//! `tests/qa_shot_account.rs`'s `matrix_row` is, because the pose and `look=` prose quote the same
//! knob back to a reader), and each is run through the REAL planner.
//!
//! ⚠ **The script is read at RUN time and the test skips LOUDLY without it, never `include_str!`:**
//! the public mirror ships no `scripts/` (`scripts/publish_mirror.sh`'s `DENY`), and an
//! `include_str!` of it would stop the mirror's tests compiling at all.

use std::path::{Path, PathBuf};

use vike_app_core::ui::startup::{StartupEnv, plan};
use vike_app_core::ui::tool_views::DataDest;
use vike_app_core::ui::workspace::WinKind;

// Same spelling as `tests/qa_shot_account.rs`'s `workspace_root` (keeps the `..`).
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

/// The script's text, or `None` when this checkout carries no `scripts/` (the public mirror).
fn qa_shots() -> Option<String> {
    std::fs::read_to_string(workspace_root().join("scripts").join("qa_shots.sh")).ok()
}

/// Every `(capture name, VIKE_SHOT_WIN slug)` of the matrix: a line that STARTS with
/// `shot_app <name>` and carries a `VIKE_SHOT_WIN=<slug>` word. A comment, a `pose=` or a `look=`
/// string that quotes the knob is not a row and is not read.
fn matrix_slugs(script: &str) -> Vec<(String, String)> {
    script
        .lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            (words.next()? == "shot_app").then_some(())?;
            let name = words.next()?.to_string();
            let slug = words.find_map(|w| w.strip_prefix("VIKE_SHOT_WIN="))?;
            Some((name, slug.to_string()))
        })
        .collect()
}

/// Does the real planner open a window for this slug, rather than fall through to the default
/// chart? (The fall-through is the only arm whose first window is a `WinKind::Chart`: every
/// hand-written arm and the Data Manager arm open a tool window.)
fn opens_a_window_of_its_own(slug: &str) -> bool {
    let area = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1920.0, 1080.0));
    let env = StartupEnv { shot_win: Some(slug.to_string()), ..StartupEnv::default() };
    plan(area, &env, None).wins[0].kind != WinKind::Chart
}

/// **Each slug the matrix passes resolves to a window of its own.**
///
/// Reddens on a renamed destination whose capture row still spells the old rail label, on a typo in
/// a row, and on a hand-written arm renamed without its row.
#[test]
fn every_matrix_slug_opens_a_window_of_its_own() {
    let Some(script) = qa_shots() else {
        eprintln!(
            "SKIPPED every_matrix_slug_opens_a_window_of_its_own: scripts/qa_shots.sh is not in \
             this checkout (the public mirror withholds scripts/); the full tree runs it"
        );
        return;
    };
    let rows = matrix_slugs(&script);
    // The anti-vacuity floor: a parse that found nothing (the matrix reshaped, the anchor lost)
    // would pass the loop below having checked nothing.
    assert!(
        rows.len() >= DataDest::ALL.len(),
        "the matrix parsed to {} `shot_app … VIKE_SHOT_WIN=` rows; the Data Manager's rail alone has \
         {} destinations, so the parse has lost the matrix: {rows:?}",
        rows.len(),
        DataDest::ALL.len()
    );
    let dead: Vec<_> = rows.iter().filter(|(_, slug)| !opens_a_window_of_its_own(slug)).collect();
    assert!(
        dead.is_empty(),
        "these capture rows pass a `VIKE_SHOT_WIN` that opens NO window of its own — the app would \
         capture the default BTCUSDT chart under their names: {dead:?}"
    );
}

/// ⚠ **The discrimination half of the test above.** The loop is only able to fail while the
/// predicate can answer `false` and the parser reads a row and not a quotation of one.
#[test]
fn the_check_can_tell_a_dead_slug_and_a_quotation_from_a_row() {
    assert!(
        !opens_a_window_of_its_own("not-an-arm"),
        "a slug no arm owns falls through to the chart"
    );
    assert!(opens_a_window_of_its_own("studio"), "a hand-written arm opens its own window");
    assert!(
        opens_a_window_of_its_own("all-series"),
        "a derived Data Manager spelling opens its own"
    );

    let script = "\
# shot_app 00-commented VIKE_SHOT_WIN=commented-out
        pose=\"shot_app 01-quoted VIKE_SHOT_WIN=quoted\"
shot_app 02-row VIKE_SHOT_WIN=row-slug \\
    \"VIKE_SETTINGS_DIR=x\"
shot_app 03-no-knob VIKE_SCALE=1
";
    assert_eq!(
        matrix_slugs(script),
        vec![("02-row".to_string(), "row-slug".to_string())],
        "only a line that STARTS with `shot_app <name>` and carries the knob is a matrix row"
    );
}
