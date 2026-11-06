//! Editing a file's bytes in place — what a corruption test does to a segment it wrote itself.

use std::path::Path;

/// Overwrite `path` at byte offset `at` with `bytes` (a whole-file rewrite: the length stays, the
/// mtime may move). Panics if the file cannot be read or written, or is shorter than the patch.
pub fn plant(path: &Path, at: usize, bytes: &[u8]) {
    let mut all = std::fs::read(path).unwrap();
    all[at..at + bytes.len()].copy_from_slice(bytes);
    std::fs::write(path, all).unwrap();
}
