//! **No desktop line names a retired shared hub key as what a person signs with** (2026-11-16).
//!
//! Since the ACCESS cutover (decision 0288 item 8) this PC's device key signs every hub dial, and no
//! hub loads a shared person key. The owner's desktop still logged, at every start, an ERROR that the
//! shared tradehub observe key was absent and every attempt would be refused, right before the observe
//! link connected signed by the device key. A connect now says ONE thing about its signer, and only
//! when this PC has no device key: how to make one; and no code of the desktop reads, writes or names
//! a retired key at all.

use std::path::{Path, PathBuf};

use vike_app_core::backend::registry::{no_device_key, signer_notice};
use vike_hub_protocol::conn::no_device_key_cure;
use vike_venues::credential_keys::RETIRED_HUB_KEYS;

/// The desktop's no-key line is the hub protocol's ONE cure for where the record points (this box's
/// hubs, or another server's), never a wording of its own, and names no retired key.
#[test]
fn a_device_key_means_no_signer_line_and_none_means_the_one_fix() {
    for (address, this_box) in [("127.0.0.1:9301", true), ("the CI box.example:9301", false)] {
        assert_eq!(signer_notice(true, address), None, "the device key signs: nothing to say");
        let line = no_device_key(address);
        assert_eq!(signer_notice(false, address), Some(line.clone()));
        assert!(line.contains(no_device_key_cure(this_box)), "{line}");
        assert!(!line.contains(no_device_key_cure(!this_box)), "{line}");
        assert!(!RETIRED_HUB_KEYS.iter().any(|k| line.contains(k)), "{line}");
    }
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

/// Every production `.rs` file under `dir` (test files and test folders left out).
fn production_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        if path.is_dir() {
            if !name.starts_with("tests") && !name.ends_with("_tests") {
                production_sources(&path, out);
            }
        } else if name.ends_with(".rs")
            && !name.ends_with("_tests.rs")
            && name != "tests.rs"
            && name != "test_support.rs"
        {
            out.push(path);
        }
    }
}

/// The names may be talked about in comments; no code line of the desktop or `vike-app-core` may carry
/// one, which is what a read, a message, a check's text or a form's notice would be.
#[test]
fn no_desktop_line_names_a_retired_shared_key() {
    let root = workspace_root();
    let mut files = Vec::new();
    for krate in ["crates/vike-app-core/src", "crates/vike-desktop/src"] {
        production_sources(&root.join(krate), &mut files);
    }
    assert!(files.len() > 50, "the walk found the sources ({} files)", files.len());
    let mut hits = Vec::new();
    for file in &files {
        let text = std::fs::read_to_string(file).expect("a source file");
        for (n, line) in text.lines().enumerate() {
            let code = line.trim_start();
            if !code.starts_with("//") && RETIRED_HUB_KEYS.iter().any(|k| code.contains(k)) {
                hits.push(format!("{}:{}: {}", file.display(), n + 1, code));
            }
        }
    }
    assert!(hits.is_empty(), "a retired shared key is named in code:\n{}", hits.join("\n"));
}
