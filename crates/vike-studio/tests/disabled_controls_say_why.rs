//! Every control the Studio disables says why on hover (design system spec §4.2: "a control with
//! nothing behind it is disabled and says why on hover — never a live, dead button").
//!
//! ⚠ **The reason must be `on_disabled_hover_text`; `on_hover_text` is not one.** egui shows
//! `on_hover_text` only while its widget is ENABLED (egui 0.36's `Tooltip::for_enabled`), so a
//! reason attached that way to a disabled control is never seen. Five Studio controls did exactly
//! that until the design-system migration: the toolbar's Run, the empty panel's Run backtest, Run
//! Sweep, Run study and a saved row's Load. A kit button
//! (`vike_ui_theme::components::button::ActionButton`) can only be disabled with a reason, so this
//! reads the rest: every `add_enabled(` in the crate's production code carries an
//! `on_disabled_hover_text` in the same statement.
//!
//! Text-level, like the tree's other source gates: production lines only (every `#[cfg(test)]`
//! item and every test-module file skipped, through `vike_model::libm_walk`), comment lines
//! dropped, and a statement read from `add_enabled(` to its first `.clicked()` or `;`.

use std::path::{Path, PathBuf};

use vike_model::libm_walk::{cfg_test_module_rel_files, cfg_test_ranges};

// The `join("..")` twins, e.g. `crates/vike-ops/tests/common/repo.rs`'s `workspace_root`, keep
// the `..`; `crates/vike-catalog/tests/baseline_artifact.rs`'s `repo_root` uses `parent()` too.
/// `parent()` twice rather than `join("..")` twice: the walk starts from `CARGO_MANIFEST_DIR`, and
/// `strip_prefix` is LEXICAL, so a root spelled with `..` never prefixes the paths it finds and
/// every "repo-relative" path came back absolute.
fn workspace_root() -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest.parent().and_then(Path::parent).expect("crates/<name> is two levels deep").into()
}

/// (repo-relative path, production text) of every `.rs` under this crate's `src/`.
fn production_sources() -> Vec<(String, String)> {
    let root = workspace_root();
    let mut files = Vec::new();
    let mut stack = vec![Path::new(env!("CARGO_MANIFEST_DIR")).join("src")];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("read src").flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let rel =
                    path.strip_prefix(&root).unwrap_or(&path).to_string_lossy().replace('\\', "/");
                files.push((rel, std::fs::read_to_string(&path).expect("read source")));
            }
        }
    }
    files.sort();
    let test_files = cfg_test_module_rel_files(&files);
    files.retain(|(rel, _)| !test_files.contains(rel));
    files
        .into_iter()
        .map(|(rel, text)| {
            let lines: Vec<&str> = text.lines().collect();
            let tests = cfg_test_ranges(&lines);
            assert!(tests.iter().all(|(_, _, closed)| *closed), "{rel}: a test item never closes");
            let prod: Vec<&str> = lines
                .iter()
                .enumerate()
                .filter(|(i, _)| !tests.iter().any(|(s, e, _)| (*s..*e).contains(i)))
                .map(|(_, l)| *l)
                .filter(|l| !l.trim_start().starts_with("//"))
                .collect();
            (rel, prod.join("\n"))
        })
        .collect()
}

/// Every `add_enabled(` statement that gives no reason while disabled.
fn unexplained(sources: &[(String, String)]) -> Vec<String> {
    let mut out = Vec::new();
    for (rel, text) in sources {
        for (i, _) in text.match_indices("add_enabled(") {
            let rest = &text[i..];
            let end = [rest.find(".clicked()"), rest.find(';')]
                .into_iter()
                .flatten()
                .min()
                .unwrap_or(rest.len());
            if !rest[..end].contains("on_disabled_hover_text") {
                out.push(format!("{rel}: {}", rest.lines().next().unwrap_or_default().trim()));
            }
        }
    }
    out
}

#[test]
fn every_disabled_control_says_why() {
    let offenders = unexplained(&production_sources());
    assert!(
        offenders.is_empty(),
        "disabled with no reason a pointer can see — chain `.on_disabled_hover_text(…)`, or use \
         a kit button's `disabled_because` (spec §4.2): {offenders:#?}"
    );
}

/// Anti-vacuity: the walk reaches the controls it judges, and it tells a plant from a reason.
#[test]
fn the_scan_sees_the_studio_controls_and_judges_a_plant() {
    let sources = production_sources();
    let n: usize = sources.iter().map(|(_, t)| t.matches("add_enabled(").count()).sum();
    assert!(n >= 5, "only {n} add_enabled sites found — the walk stopped matching");
    let plant = |body: &str| vec![("plant.rs".to_string(), body.to_string())];
    let silent = "if ui.add_enabled(ok, b).on_hover_text(\"x\").clicked() {}";
    let said = "if ui.add_enabled(ok, b).on_disabled_hover_text(\"x\").clicked() {}";
    assert_eq!(unexplained(&plant(silent)).len(), 1);
    assert!(unexplained(&plant(said)).is_empty());
}
