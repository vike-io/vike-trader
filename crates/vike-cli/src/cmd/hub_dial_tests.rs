//! A dial of a node is a property of the verbs that dial, so its scan lives where they do, and it
//! reads the shared crate those verbs stand on as well.

use std::path::{Path, PathBuf};

/// **Every production dial of a node in this crate passes the box's patience.** `connect` is the
/// default-window door for a caller with no setting to read; a verb that dialled through it would
/// silently ignore both rows. Scans this crate's non-test sources and `vike-client`'s, comments
/// excluded.
#[test]
fn no_production_dial_in_this_crate_bypasses_the_box_patience() {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).expect("read a source directory").flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if path.is_dir() {
                if !name.ends_with("_tests") && name != "tests" {
                    walk(&path, out);
                }
            } else if name.ends_with(".rs") && !name.ends_with("_tests.rs") {
                out.push(path);
            }
        }
    }
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    walk(&manifest.join("src"), &mut files);
    assert!(files.len() > 50, "the walk found {} files — wrong place", files.len());
    walk(&manifest.join("../vike-client/src"), &mut files);
    // Assembled: this file is skipped by name, but a literal would still read as data to a search.
    let needles = [
        concat!("TradehubCoreHandle", "::connect("),
        concat!("TradehubControlHandle", "::connect("),
    ];
    for file in files {
        let text = std::fs::read_to_string(&file).expect("read a source file");
        for (n, line) in text.lines().enumerate() {
            if line.trim_start().starts_with("//") {
                continue;
            }
            for needle in needles {
                assert!(
                    !line.contains(needle),
                    "{}:{} dials through `connect`, which ignores the box's link patience; use \
                     connect_with_read_timeout / connect_with_reply_timeout with \
                     `HubKeyring::patience`",
                    file.display(),
                    n + 1
                );
            }
        }
    }
}
