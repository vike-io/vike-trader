//! **No signer of this crate takes its nonce off the bare local clock**: each takes it from its client's
//! `vike_bridge_core::signer::VenueClock`, as binance's do (`crates/bridges/binance/tests/offline/signer_clock.rs`
//! says why). Offline: this venue's key pair is a mainnet one.

use std::path::Path;
use vike_data::test_support::text::rs_files;

#[test]
fn no_signer_reads_the_bare_local_clock() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rs_files(&root, &mut files);
    assert!(!files.is_empty(), "no source file under {}", root.display());
    let mut bare = Vec::new();
    for file in files {
        for line in std::fs::read_to_string(&file).unwrap().lines() {
            if line.contains("SignerAster::new(") && line.contains("now_us)") {
                bare.push(format!("{}: {}", file.display(), line.trim()));
            }
        }
    }
    assert!(bare.is_empty(), "{} sites sign on the local clock:\n{}", bare.len(), bare.join("\n"));
}
