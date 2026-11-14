//! The crate ships its own copy of the licence text, and the copy must equal the repository's.
//!
//! A `.crate` carries only what sits under the package root, so the FSL text has to be a file here
//! (`LICENSE.md`); `Cargo.toml` names the licence by SPDX id, and cargo warns when `license-file`
//! is given as well, so the copy is a plain file rather than a manifest key. A copy can drift from
//! its source, so this test re-derives it: byte for byte against the root `LICENSE.md`.
//!
//! The root file is not part of a published package, so when it is absent (the crate built from its
//! tarball, outside the repository) the comparison is skipped loudly rather than failed.

#[test]
fn the_shipped_licence_text_is_the_repository_licence() {
    let copy = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/LICENSE.md"))
        .expect("the crate's own LICENSE.md must exist: it is what the .crate ships");
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../../LICENSE.md");
    match std::fs::read(root) {
        Ok(source) => assert!(
            copy == source,
            "crates/vike-indicators/LICENSE.md differs from the repository's LICENSE.md; \
             re-copy it (`cp LICENSE.md crates/vike-indicators/LICENSE.md`)"
        ),
        Err(_) => {
            eprintln!("skipped: no repository LICENSE.md beside this crate (built from a tarball)")
        }
    }
}
