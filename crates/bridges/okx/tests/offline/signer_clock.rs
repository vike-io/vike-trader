//! **No signer or private-socket login of this crate stamps a request off the bare local clock** (owner ruling B-4).
//! Every signer takes its stamp from its client's `vike_bridge_core::signer::VenueClock` (`clock.ms()`), the one a
//! start holds or a one-shot client's own, and both private sockets' logins stamp the same; this holds the source to
//! it (a signer built in a test names a fixed clock, `|| 0`, so it never reads as one of these). The smokes keep the
//! local clock on purpose and live outside `src/`.

use std::path::Path;
use vike_data::test_support::text::rs_files;

#[test]
fn no_signer_or_login_reads_the_bare_local_clock() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rs_files(&root, &mut files);
    assert!(!files.is_empty(), "no source file under {}", root.display());
    let mut bare = Vec::new();
    for file in files {
        let login_file = file.ends_with("user_data.rs");
        for line in std::fs::read_to_string(&file).unwrap().lines() {
            let signer = line.contains("OkxV5Signer::new(") && line.contains("now_ms");
            let login = login_file && line.trim() == "vike_model::now_ms,";
            if signer || login {
                bare.push(format!("{}: {}", file.display(), line.trim()));
            }
        }
    }
    assert!(bare.is_empty(), "{} sites sign on the local clock:\n{}", bare.len(), bare.join("\n"));
}
