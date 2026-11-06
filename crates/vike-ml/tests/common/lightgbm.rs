//! The pinned LightGBM binary `lightgbm_cli_smoke` and `train_infer_equality` drive, and the scratch
//! directory each run writes into (`#[path]`-included by each).

use std::path::PathBuf;

use vike_ml::train::cli::LightGbmCli;
use vike_paths::scratch::ScratchDir;

/// `<repo>/bin/lightgbm/lightgbm` — the path `just lightgbm-build` writes by default.
pub fn binary_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../bin/lightgbm/lightgbm")
}

/// `Some(cli)` when the pinned binary is present and IS the pinned version; `None` (with a printed
/// reason) otherwise.
pub fn cli() -> Option<LightGbmCli> {
    let path = binary_path();
    if !path.exists() {
        eprintln!("SKIP: no LightGBM binary at {} — run `just lightgbm-build`", path.display());
        return None;
    }
    Some(LightGbmCli::new(&path).expect("a present binary must be the pinned version"))
}

/// A scratch directory `<family>_<tag>` the calling test OWNS — removed when the returned guard drops.
///
/// ⚠ Never a bare pid-keyed `PathBuf`: measured on the CI box on 2026-08-25, that shape (the
/// `vike_ml_smoke_*` and `vike_ml_equality_*` families) had leaked 2,102 `/tmp` directories, and a
/// REUSED pid on a box running these tests as TWO users meets the other user's directory —
/// `create_dir_all` succeeds and the write fails `PermissionDenied`.
/// `vike_paths::scratch::ScratchDir`'s `Drop` closes both halves.
pub fn scratch(family: &str, tag: &str) -> ScratchDir {
    ScratchDir::create_in(&std::env::temp_dir(), &format!("{family}_{tag}")).expect("scratch dir")
}
