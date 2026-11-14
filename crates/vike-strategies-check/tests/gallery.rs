//! The gallery-born strategy files' contract (plan 2b, C3; the strategy gallery's own tests, moved to
//! the files): every one builds from empty params; its header runs it on paper with no key and a
//! `[run]` the lab reads; every file that opens with a licence header is one of them and every one
//! opens with an allowed one; no file names a collection or a competitor outside its notices; the
//! rules the gallery left out are absent; the licence files ship beside them; none calls a platform
//! transcendental.
//!
//! Reads `strategies/` at RUN time: a tree without it (the public mirror until the shipped set is
//! published there) skips loudly. The files themselves compile in this crate's library.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use vike_model::libm_walk::{banned_needles, cfg_test_ranges};
use vike_model::params::Params;
use vike_strategies_check::{gallery, gallery_file};
use vike_strategy::file::{StrategyFileHeader, StrategyFileRunsOn};

/// The SPDX lines a gallery-born file may open with (decision 0269 item 4: GPL never, nothing unlisted).
const LICENCES: [&str; 4] = [
    "// SPDX-License-Identifier: Apache-2.0",
    "// SPDX-License-Identifier: Apache-2.0 AND MPL-2.0",
    "// SPDX-License-Identifier: MPL-2.0",
    "// SPDX-License-Identifier: MIT",
];

const CHANGED: &str = "// Changed: rewritten by hand in Rust for vike.";

/// `strategies/` at the repository root, or `None` (with the reason printed) on a tree without it.
fn root() -> Option<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../strategies");
    if root.is_dir() {
        Some(root)
    } else {
        eprintln!("SKIPPED: {} is absent from this tree", root.display());
        None
    }
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// Every `.rs` under `root`, as its path relative to it with `/`, sorted.
fn rust_files(root: &Path) -> Vec<String> {
    let (mut out, mut stack) = (Vec::new(), vec![root.to_path_buf()]);
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display())) {
            let path = entry.expect("a readable directory entry").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let rel = path.strip_prefix(root).expect("under the root");
                out.push(rel.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    out.sort();
    out
}

/// The licence header: every line before the first empty one.
fn licence_header(text: &str) -> Vec<&str> {
    text.lines().take_while(|l| !l.is_empty()).collect()
}

#[test]
fn every_file_builds_from_empty_params_and_its_path_is_unique_and_in_order() {
    for pair in gallery::FILES.windows(2) {
        assert!(
            pair[0].path < pair[1].path,
            "{:?} must sort before {:?}",
            pair[0].path,
            pair[1].path
        );
    }
    let stems: BTreeSet<&str> = gallery::FILES
        .iter()
        .map(|e| Path::new(e.path).file_stem().and_then(|s| s.to_str()).expect("a stem"))
        .collect();
    assert_eq!(stems.len(), gallery::FILES.len(), "two files share a stem");
    assert_eq!(gallery::FILES.len(), 615, "the gallery's 615 rules, one file each");
    for entry in gallery::FILES {
        let _rule = (entry.rule)(&Params::default());
        assert!(entry.path.split('/').count() == 2, "{} is not <category>/<name>.rs", entry.path);
    }
}

#[test]
fn every_header_runs_on_paper_with_no_key_and_the_lab_reads_its_run() {
    let Some(root) = root() else { return };
    for &gallery::GalleryEntry { path, .. } in gallery::FILES {
        let header = StrategyFileHeader::parse(&read(&root.join(path)))
            .unwrap_or_else(|e| panic!("{path}: {e}"));
        assert_eq!(header.strategy.runs_on, StrategyFileRunsOn::Paper, "{path}");
        assert!(
            header.strategy.why.is_none(),
            "{path} argues live: the owner decides live per rule"
        );
        assert!(header.params.is_empty(), "{path} declares a key; no gallery rule reads one");
        let stem = Path::new(path).file_stem().and_then(|s| s.to_str()).expect("a stem");
        let file =
            gallery_file(&root, stem).unwrap_or_else(|| panic!("{stem} is not found by its stem"));
        assert_eq!(file.path, path);
        let run = header.run.unwrap_or_default();
        assert_eq!(
            (
                file.run.bound_exits,
                file.run.stacked_closes,
                file.run.fills_a_day,
                file.run.touch_at_market
            ),
            (run.bound_exits, run.stacked_closes, run.fills_a_day, run.touch_at_market),
            "{path}"
        );
    }
    assert!(gallery_file(&root, "no_such_rule").is_none(), "an absent stem is none");
}

/// A file opening with a licence header of its own is one of the gallery's, and every one of the
/// gallery's opens with an allowed SPDX line and carries the statement of change, before the first
/// empty line.
#[test]
fn every_licensed_file_is_the_gallerys_and_carries_an_allowed_licence_header() {
    let Some(root) = root() else { return };
    let licensed: Vec<String> = rust_files(&root)
        .into_iter()
        .filter(|rel| read(&root.join(rel)).starts_with("// SPDX-License-Identifier:"))
        .collect();
    let files: Vec<String> = gallery::FILES.iter().map(|e| e.path.to_string()).collect();
    assert_eq!(licensed, files, "the files with a licence header are not the gallery's");
    for path in &files {
        let text = read(&root.join(path));
        let header = licence_header(&text);
        assert!(
            header.first().is_some_and(|l| LICENCES.contains(l)),
            "{path} does not open with an allowed SPDX line ({LICENCES:?})"
        );
        assert!(header.iter().any(|l| l.starts_with(CHANGED)), "{path} lacks {CHANGED:?}");
    }
}

#[test]
fn the_licence_files_ship_non_empty_beside_them() {
    let Some(root) = root() else { return };
    for name in ["NOTICE", "LICENSE-APACHE-2.0.txt", "LICENSE-MPL-2.0.txt", "LICENSE-MIT"] {
        assert!(!read(&root.join(name)).trim().is_empty(), "{name} is empty");
    }
}

/// The collection or competitor name `text` carries, by the naming rule's list (no file name, code or
/// doc carries one; the licence notices a file opens with keep theirs verbatim). A link names its
/// site, so it is passed over.
fn banned_word(text: &str) -> Option<&'static str> {
    let lower = text.to_ascii_lowercase();
    let words: Vec<&str> = lower.split_whitespace().filter(|w| !w.contains("://")).collect();
    let words = words.join(" ");
    for stem in ["pine", "pyne"] {
        let starts_a_word = |(at, _): (usize, &str)| {
            !words[..at].chars().next_back().is_some_and(|c| c.is_ascii_lowercase())
        };
        if words.match_indices(stem).any(starts_a_word) {
            return Some(stem);
        }
    }
    const NAMES: [&str; 12] = [
        "tradingview",
        "quantconnect",
        "metatrader",
        "ninjatrader",
        "multicharts",
        "amibroker",
        "prorealtime",
        "backtrader",
        "freqtrade",
        "3commas",
        "cryptohopper",
        "trendspider",
    ];
    if let Some(name) = NAMES.into_iter().find(|n| words.contains(n)) {
        return Some(name);
    }
    words.split(|c: char| !c.is_ascii_alphanumeric()).any(|w| w == "wild").then_some("wild")
}

#[test]
fn no_file_names_a_collection_or_a_competitor_outside_its_notices() {
    assert_eq!(banned_word("a rule from x_pine"), Some("pine"), "the check finds one");
    assert_eq!(banned_word("a spine, see https://www.tradingview.com/x"), None, "a link passes");
    let Some(root) = root() else { return };
    for &gallery::GalleryEntry { path, .. } in gallery::FILES {
        assert_eq!(banned_word(path), None, "the file name {path}");
        let text = read(&root.join(path));
        for line in text.lines().skip_while(|l| !l.is_empty()) {
            if let Some(word) = banned_word(line) {
                panic!("{path} names {word:?} outside its notices: {line}");
            }
        }
    }
}

/// The strategy lab's rules the gallery did not take (each with its reason in the review's stays-out
/// list): no file of the shipped set is one.
#[test]
fn the_rules_left_out_are_absent() {
    const LEFT_OUT: [&str; 4] = [
        "ajay_r5_41",
        "enhanced_ob_retest_strategy_v7_0",
        "htf_high_low_repaint_strategy",
        "optimized_heikin_ashi_strategy_with_buy_sell_opt",
    ];
    let Some(root) = root() else { return };
    let files = rust_files(&root);
    for name in LEFT_OUT {
        assert!(gallery_file(&root, name).is_none(), "{name} is a gallery file");
        let file = format!("/{name}.rs");
        assert!(!files.iter().any(|f| format!("/{f}").ends_with(&file)), "{name}.rs is shipped");
    }
}

/// Every shipped `.rs` calls no platform transcendental: `libm::log` for `.ln()`, `libm::pow` for
/// `.powf()`, a multiplication for a square (decision 0032). A file's numbers decide orders, so they
/// must be the same on every platform. The needles' blind spots are
/// `vike_model::libm_walk::banned_needles`'; a file's `#[cfg(test)]` items (the taker files carry
/// their twins' unit tests) are skipped by `vike_model::libm_walk::cfg_test_ranges`, each asserted closed.
#[test]
fn no_file_calls_a_platform_transcendental() {
    let Some(root) = root() else { return };
    let banned = banned_needles();
    let files = rust_files(&root);
    assert!(files.len() >= gallery::FILES.len(), "the scan read {} files", files.len());
    let mut found = Vec::new();
    for rel in &files {
        let text = read(&root.join(rel));
        let lines: Vec<&str> = text.lines().collect();
        let test_items = cfg_test_ranges(&lines);
        for &(start, _, closed) in &test_items {
            assert!(closed, "{rel}: the test item opening at line {} never closes", start + 1);
        }
        let in_test =
            |i: usize| test_items.iter().any(|&(start, end, _)| (start..end).contains(&i));
        for (i, line) in lines.iter().enumerate() {
            let code = line.trim_start();
            if !in_test(i)
                && !code.starts_with("//")
                && banned.iter().any(|pat| line.contains(pat.as_str()))
            {
                found.push(format!("  {rel}:{} {code}", i + 1));
            }
        }
    }
    assert!(
        found.is_empty(),
        "a strategy file takes its transcendentals from the `libm` crate (`libm::log`, `libm::pow`, ...) and a square \
         as a multiplication (decision 0032):\n{}",
        found.join("\n")
    );
}
