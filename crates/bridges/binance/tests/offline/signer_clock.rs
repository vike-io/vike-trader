//! **No signer of this crate stamps a request off the bare local clock.** One account used to sign spot
//! orders on the venue's clock and its reconcile reads, perp orders and spot subscribe on the local one, so
//! with a clock the venue refuses, part of an account worked and the rest did not. Every signer now takes its
//! stamp from its client's `vike_bridge_core::signer::VenueClock`, and this holds the source to it (a
//! signer built in a test names a fixed clock, so it never reads as one of these).

use std::path::Path;
use vike_data::test_support::text::rs_files;

#[test]
fn no_signer_or_auth_frame_reads_the_bare_local_clock() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rs_files(&root, &mut files);
    assert!(!files.is_empty(), "no source file under {}", root.display());
    let mut bare = Vec::new();
    for file in files {
        for line in std::fs::read_to_string(&file).unwrap().lines() {
            let signer = line.contains("BinanceHmacSigner::new(") && line.contains("now_ms)");
            let frame = line.contains("let now_ms = vike_model::now_ms");
            if signer || frame {
                bare.push(format!("{}: {}", file.display(), line.trim()));
            }
        }
    }
    assert!(bare.is_empty(), "{} sites sign on the local clock:\n{}", bare.len(), bare.join("\n"));
}
