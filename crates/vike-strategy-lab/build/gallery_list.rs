//! The strategy gallery's list, derived from the repository's `strategies/` folder: every `.rs` file below
//! it whose text opens with an SPDX licence line (a hand-written rewrite of a published script,
//! `strategies/NOTICE`), in path order, each a `#[path]` module named by its stem with its `rule` in `FILES`.
//!
//! `build.rs` writes the lab's list from it (`src/gallery.rs` includes what it writes), so a strategy file
//! added to or removed from the folder needs no edit in this crate. The test-surface gate walks the same
//! modules by the same rule, spelled there (`crates/vike-ops/tests/architecture/test_surface_gate/source_walk.rs`'s
//! `with_gallery`), and `tests/gallery.rs` holds the list equal to the folder's licensed files.

use std::fs;
use std::path::Path;

/// The line a gallery-born file's text opens with.
const LICENCE_LINE: &str = "// SPDX-License-Identifier:";

/// Every gallery-born `.rs` file under `strategies`, as its path relative to it with `/`, sorted. A
/// missing folder is an empty list.
///
/// # Panics
/// When a folder or a `.rs` file under `strategies` cannot be read: a file the list silently left out
/// would leave the gallery's checks without it.
pub fn gallery_files(strategies: &Path) -> Vec<String> {
    let mut out = Vec::new();
    if !strategies.is_dir() {
        return out;
    }
    let mut stack = vec![strategies.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display())) {
            let path = entry.unwrap_or_else(|e| panic!("{}: {e}", dir.display())).path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let text =
                    fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
                if text.starts_with(LICENCE_LINE) {
                    let rel =
                        path.strip_prefix(strategies).expect("a path under the folder walked");
                    out.push(rel.to_string_lossy().replace('\\', "/"));
                }
            }
        }
    }
    out.sort();
    out
}

/// The Rust source of the gallery's list for `files` (from [`gallery_files`]): one `#[path]` module per
/// file, its path spelled `{folder}{file}` (`folder` names the strategies folder as the including file's
/// `#[path]` must, ending in `/`), then `FILES`, every one's path and `rule`. The including scope names
/// `GalleryFileEntry`.
pub fn gallery_source(folder: &str, files: &[String]) -> String {
    let stem = |file: &str| {
        let name = file.rsplit('/').next().unwrap_or(file);
        name.strip_suffix(".rs").unwrap_or(name).to_string()
    };
    let mut out = String::new();
    for file in files {
        out += &format!("#[path = {:?}]\npub mod {};\n", format!("{folder}{file}"), stem(file));
    }
    out += "\n/// Every gallery-born strategy file, in path order.\n";
    out += "pub const FILES: &[GalleryFileEntry] = &[\n";
    for file in files {
        out += &format!("    GalleryFileEntry {{ path: {file:?}, rule: {}::rule }},\n", stem(file));
    }
    out += "];\n";
    out
}
