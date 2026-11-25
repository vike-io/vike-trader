//! The TRAINER pin: the upstream LightGBM release this crate drives and the walker is validated
//! against.
//!
//! Its twin, the model-text format version, is `vike-ml-prediction`'s `MODEL_VERSION`
//! (`crates/vike-ml-prediction/src/pins.rs`); a LightGBM bump moves both, and
//! `crates/vike-ml-training/CLAUDE.md` is the bump procedure. `scripts/lightgbm_cache.sh` reads
//! [`PINNED_LIGHTGBM_TAG`] out of THIS file by path, so it stays here, spelled on one line.
//!
//! ⚠ **It is not the bump proof** (`crates/vike-ml-training/CLAUDE.md`'s "The two pins, and what
//! neither proves"). [`PINNED_LIGHTGBM_TAG`], asserted against the binary by
//! [`crate::train::cli::LightGbmCli::new`], catches a different LightGBM on the box, not a format
//! change. The evidence is `tests/train_infer_equality.rs`: LightGBM's own `task=predict` against
//! the walker, row by row, over the same model file.

/// The upstream LightGBM release this crate is built and validated against.
///
/// Upstream moved from `microsoft/LightGBM` to `lightgbm-org/LightGBM` in 4.7.0 (old URLs
/// redirect); `v4.7.0` is tag sha `8f7036f0`, released 2026-07-18.
pub const PINNED_LIGHTGBM_TAG: &str = "v4.7.0";

#[cfg(test)]
mod tests {
    use super::PINNED_LIGHTGBM_TAG;

    /// The golden fixtures `tests/train_infer_equality.rs`'s capture tests write into
    /// `vike-ml-prediction`'s fixtures, one per objective.
    const GOLDEN: [&str; 3] =
        ["real_binary_categorical", "real_regression_categorical", "real_multiclass_categorical"];

    /// One `key=value` of a golden fixture's `#` header.
    fn provenance(stem: &str, key: &str) -> Option<String> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(format!("../vike-ml-prediction/tests/fixtures/{stem}_expected.tsv"));
        std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("committed fixture {}: {e}", path.display()))
            .lines()
            .find_map(|l| l.trim_start_matches('#').trim().strip_prefix(&format!("{key}=")))
            .map(str::to_string)
    }

    /// Beside the pin rather than beside the fixture, so a bump — an edit of THIS file — runs it in
    /// the default lane, on a box with no LightGBM binary.
    #[test]
    fn the_golden_fixture_records_which_lightgbm_produced_it_and_it_is_the_pinned_one() {
        // The cheapest guard against the most expensive mistake: a golden file captured from a
        // DIFFERENT LightGBM than the one this crate is pinned to.
        //
        // ⚠ It does NOT regenerate and byte-compare: LightGBM's docs caveat that versions, compilers
        // and systems may produce different models even under deterministic=true, so a byte gate
        // would be a cross-box flake generator. Pin the generator beside the artifact instead.
        for stem in GOLDEN {
            let provenance = |key: &str| provenance(stem, key);
            let tag = provenance("pinned_tag").expect("the fixture must record its pinned tag");
            assert_eq!(
                tag, PINNED_LIGHTGBM_TAG,
                "{stem}: this fixture was captured from LightGBM {tag}, but the crate is now pinned to {}. \
             Either the pin moved without the fixture being regenerated, or the fixture came from \
             the wrong box. Regenerate on the CI box with train_infer_equality.rs's capture test — and \
             run the equality gate there first, because THAT is what proves the bump did not \
             change the numbers. See crates/vike-ml-training/CLAUDE.md.",
                PINNED_LIGHTGBM_TAG
            );
            assert!(
                provenance("lightgbm_sha256").is_some(),
                "the binary's sha256 must be recorded"
            );
            assert!(
                provenance("lightgbm_cmake_flags").unwrap_or_default().contains("USE_OPENMP=OFF")
            );
        }
    }
}
