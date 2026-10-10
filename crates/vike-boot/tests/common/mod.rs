//! The source walk shared by this crate's two ROOT gates, `one_owner.rs` and
//! `boot_journal_wiring.rs`, and the ONE definition of "a booting crate" both of them key on
//! ([`booting_crates`]). Two gates deriving their roster from two spellings would be checking two
//! different populations; spelled once, they cannot.
//!
//! Text-only over the real `crates/**/src/**.rs` tree, comments stripped: `one_owner.rs`'s module
//! doc states the mechanism and its two declared limitations.

use std::path::{Path, PathBuf};

/// The call that MAKES a crate a booting one — the anchor the roster is derived from, so no roster
/// is written down here (this repo's rosters-in-prose all rotted; the root `CLAUDE.md` says to
/// derive them).
pub const BOOT_CALL: &str = "vike_boot::boot(";

// Same spelling as `crates/vike-ops/tests/common/repo.rs`'s `workspace_root` (keeps the `..`); the
// `parent()` twins, e.g. `crates/vike-catalog/tests/baseline_artifact.rs`'s `repo_root`, do not.
/// Workspace root, resolved from `CARGO_MANIFEST_DIR` (never CWD) — the idiom every text gate in
/// this repo uses.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

/// Every `.rs` file under `crates/**/src/`, as `(repo-relative path, text)`.
///
/// The two vendored trees the root manifest EXCLUDES from the workspace are skipped: neither is
/// ours and neither could call a vike function.
///
/// A `#[cfg(test)] mod NAME;` module that lives in its own file is left out whole — the same code
/// [`production_half`] cuts away when the module sits inline.
pub fn sources() -> Vec<(String, String)> {
    let root = repo_root();
    let mut out = Vec::new();
    walk(&root.join("crates"), &root, &mut out);
    out.sort();
    let test_files = vike_model::libm_walk::cfg_test_module_rel_files(&out);
    out.retain(|(rel, _)| !test_files.contains(rel));
    out
}

fn walk(dir: &Path, root: &Path, out: &mut Vec<(String, String)>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
            // `target/` in a crate directory, and the two vendored trees.
            if matches!(name, "target" | "vendor" | "protogen") {
                continue;
            }
            walk(&path, root, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            let rel = path.strip_prefix(root).unwrap_or(&path).to_string_lossy().replace('\\', "/");
            if !rel.contains("/src/") {
                continue;
            }
            if let Ok(text) = std::fs::read_to_string(&path) {
                out.push((rel, text));
            }
        }
    }
}

// Unlike `crates/vike-ops/tests/common/strip.rs`'s `strip_line_comments_keeping_urls`, this also
// cuts at the `//` of a `https://`.
/// Line comments removed — so a `//!` module doc or a `//` note NAMING one of these functions is
/// prose, not a call. The same stripper as
/// `crates/vike-buildinfo/tests/identity_adoption/scan.rs`'s `strip_comments`.
///
/// Load-bearing in BOTH directions: `crates/vike-cli/src/lib.rs` explains its anchor exemption in a
/// comment that names `journal_boot_settings`, and `crates/vike-boot/src/lib.rs`'s own doc names
/// `boot`.
pub fn strip_comments(text: &str) -> String {
    text.lines()
        .map(|l| match l.find("//") {
            Some(i) => &l[..i],
            None => l,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The file with its trailing `#[cfg(test)] mod …` block cut off — see the first declared
/// limitation in `one_owner.rs`'s module doc.
pub fn production_half(text: &str) -> String {
    let clean = strip_comments(text);
    let lines: Vec<&str> = clean.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        if line.trim() != "#[cfg(test)]" {
            continue;
        }
        let next = lines[i + 1..].iter().find(|l| !l.trim().is_empty());
        if next.is_some_and(|l| l.trim_start().starts_with("mod ")) {
            return lines[..i].join("\n");
        }
    }
    clean
}

/// The `crates/<name>` directory prefix of a repo-relative source path.
pub fn crate_dir(rel: &str) -> String {
    // `crates/vike-desktop/src/…` and `crates/bridges/aster/src/…` — the bridges tree is one
    // level deeper, and either way the CRATE is everything above `/src/`.
    match rel.split_once("/src/") {
        Some((dir, _)) => dir.to_string(),
        None => rel.to_string(),
    }
}

/// THE derived roster: the crates whose PRODUCTION code calls [`BOOT_CALL`], `vike-boot` itself
/// excluded, sorted and deduplicated — so it counts CRATES, not files. A boot call inside a test
/// module (inline, cut by [`production_half`], or a file of its own, dropped by [`sources`]) does
/// not make a crate a root.
pub fn booting_crates(all: &[(String, String)]) -> Vec<String> {
    let mut out: Vec<String> = all
        .iter()
        .filter(|(rel, text)| {
            !rel.starts_with("crates/vike-boot/") && production_half(text).contains(BOOT_CALL)
        })
        .map(|(rel, _)| crate_dir(rel))
        .collect();
    out.sort();
    out.dedup();
    out
}
