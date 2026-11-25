//! Writes the strategy gallery's list into `$OUT_DIR/gallery_files.rs`, which `src/gallery.rs` includes:
//! every gallery-born file under the repository's `strategies/` folder
//! (`crates/vike-strategy-lab/build/gallery_list.rs`'s `gallery_files`), so a strategy file added to or
//! removed from that folder needs no edit in this crate.
//!
//! Only a build with the `gallery` feature writes it and watches the folder: the crates that dev-depend on
//! this one for `stand_in` and the makers build it without the feature, and a strategy edit reruns nothing
//! for them.

#[path = "build/gallery_list.rs"]
mod gallery_list;

use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    // One watched path switches off cargo's default rerun on any file of the package.
    println!("cargo:rerun-if-changed=build/gallery_list.rs");
    if env::var_os("CARGO_FEATURE_GALLERY").is_none() {
        return;
    }
    let manifest =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR"));
    let strategies = manifest.join("../../strategies");
    // A folder: cargo reruns this script when a file below it is added, removed or changed.
    println!("cargo:rerun-if-changed={}", strategies.display());
    let folder = format!("{}/", strategies.to_string_lossy().replace('\\', "/"));
    let source = gallery_list::gallery_source(&folder, &gallery_list::gallery_files(&strategies));
    let out =
        PathBuf::from(env::var_os("OUT_DIR").expect("cargo sets OUT_DIR")).join("gallery_files.rs");
    // Written only when it differs, so a change below `strategies/` that adds or removes no file
    // recompiles no more than that file's own edit does.
    if fs::read_to_string(&out).ok().as_deref() != Some(source.as_str()) {
        fs::write(&out, source).unwrap_or_else(|e| panic!("{}: {e}", out.display()));
    }
}
