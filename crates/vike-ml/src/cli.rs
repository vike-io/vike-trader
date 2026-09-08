//! The process driver: run the pinned LightGBM binary and read its output back.
//!
//! # Why a process and not a linked library
//!
//! A measured bakeoff disqualified every Rust binding: they drop `categorical_feature` at Dataset
//! construction, SILENTLY, and then record it in the saved model's parameter block anyway — an
//! artifact that lies to an auditor. Driving the upstream binary is the only route that gets the
//! feature right, and it happens to cost nothing: no crate, no FFI, no `unsafe`, no cmake or
//! bindgen in the build graph, and nothing in the `cargo deny --all-features` surface. The
//! precedent is `crates/vike-backfill/src/clickhouse_poly/ingest.rs`'s `run_export`, which shells
//! `clickhouse-client` rather than linking a client.
//!
//! # The binary is a PARAMETER
//!
//! [`LightGbmCli::new`] takes a path. This module reads no environment variable and knows no
//! default location — `crates/vike-ops/tests/settings_registry.rs` is why, and its `LIBRARY_PIN` is
//! a ratchet that may shrink and never grow. A binary resolves the path (from a config value or an
//! argument) and hands it down.
//!
//! # And it is CHECKED — by reading a file, because the binary cannot answer
//!
//! Training with a different LightGBM than the one this crate's walker was validated against
//! produces a model that parses, predicts, and is wrong in ways only the equality gate would see.
//! So [`LightGbmCli::new`] REFUSES a mismatch.
//!
//! ⚠ It has to read the `PROVENANCE` file `scripts/build_lightgbm.sh` writes beside the binary,
//! because **the binary cannot name itself**: upstream's `src/main.cpp` has no version handling,
//! the word "version" does not appear in `src/application/application.cpp`, and `--version` just
//! produces an unknown-parameter warning, "No training/prediction data, application quit", and a
//! non-zero exit.
//!
//! ⚠ **The honest weakness, stated rather than implied:** that file is SELF-REPORTED by our own
//! build recipe, and only its `tag=` line is ever READ. So the check catches exactly two things —
//! a binary with no `PROVENANCE` beside it (the ordinary state of one built by hand) and one whose
//! recipe recorded a different tag — and nothing else. It does not catch "somebody ran the recipe
//! against modified sources", and, the case worth naming because the recipe writes a `sha256=`
//! line that invites the opposite assumption, **it does not catch a different BUILD of the pinned
//! tag either: nothing in this crate hashes the binary.** That line is provenance for an auditor,
//! never a comparison — a genuine `v4.7.0` rebuilt `-DUSE_OPENMP=ON` has an identical `tag=`, and
//! passes, which is precisely why [`crate::grid::check_grid_params`] refuses `num_threads != 1` at
//! the point the fits happen rather than trusting this. The check is strictly weaker than a binary
//! naming itself, it is the only option available, and it is one more reason the EQUALITY GATE and
//! not this check is the real bump proof. See [`crate::pins`].

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::MlError;
use crate::pins::PINNED_LIGHTGBM_TAG;
use crate::train::{Task, TrainConfig, write_predict_csv};

/// A handle to a LightGBM binary that has been confirmed to be the pinned release.
#[derive(Clone, Debug)]
pub struct LightGbmCli {
    binary: PathBuf,
}

impl LightGbmCli {
    /// Point at a binary and verify its `PROVENANCE` records [`PINNED_LIGHTGBM_TAG`].
    pub fn new(binary: &Path) -> Result<Self, MlError> {
        let version = provenance_version(binary)?;
        if !version_matches_pin(&version) {
            return Err(refuse_version(version.trim()));
        }
        Ok(Self { binary: binary.to_path_buf() })
    }

    /// Skip the version probe. For tests that are ABOUT the failure paths, and for a caller who has
    /// already verified the binary once and is constructing per-worker handles in a hot loop.
    pub fn unchecked(binary: &Path) -> Self {
        Self { binary: binary.to_path_buf() }
    }

    pub fn binary(&self) -> &Path {
        &self.binary
    }

    /// Run one config file to completion and RETURN LightGBM's captured stdout.
    ///
    /// ⚠ The return value is not a convenience. `Log::Info`, `Log::Warning` and `Log::Debug` all
    /// `printf` to **stdout**; only `Log::Fatal` writes to stderr. So every warning LightGBM
    /// emits — including the unknown-parameter warnings that are the ONLY detector for a renamed
    /// or deprecated training parameter after a version bump — arrives here and nowhere else. A
    /// driver that discarded stdout would make Task 15's `verbosity=1` bump-proof story and
    /// `crates/vike-ml/CLAUDE.md`'s "read its log" instruction both false.
    ///
    /// A non-zero exit carries the tail of stderr, where `Log::Fatal` lands.
    pub fn run_config(&self, conf_path: &Path) -> Result<String, MlError> {
        let bin = self.binary.display();
        let out = Command::new(&self.binary)
            .arg(format!("config={}", conf_path.display()))
            .output()
            .map_err(|e| MlError::Trainer(format!("spawn {bin}: {e}")))?;
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            let last: String = err.lines().rev().take(10).collect::<Vec<_>>().join("\n");
            return Err(MlError::Backend(format!("{bin} exited {}: {last}", out.status)));
        }
        Ok(stdout)
    }

    /// Write `conf` into `scratch`, run it, and read the trained model text back.
    ///
    /// The config file is written per CALL rather than per fit-name, so a caller reusing one
    /// scratch directory per worker reuses one config path — see [`crate::grid`] for why 22,400
    /// distinct config files is a filesystem problem worth not having.
    pub fn train(&self, conf: &TrainConfig<'_>, scratch: &Path) -> Result<String, MlError> {
        let Some(model) = conf.output_model else {
            return Err(MlError::Shape("train needs `output_model`".into()));
        };
        let conf_path = scratch.join("train.conf");
        write_file(&conf_path, conf.render()?.as_bytes())?;
        self.run_config(&conf_path)?;
        std::fs::read_to_string(model)
            .map_err(|e| MlError::Io(format!("reading {}: {e}", model.display())))
    }

    /// LightGBM's own scorer over a model file: one probability per row, in row order.
    ///
    /// This is what the equality gate compares the walker against — LightGBM's answer, from
    /// LightGBM, on the same model file the walker reads.
    pub fn predict(
        &self,
        model: &Path,
        flat_x: &[f64],
        n_features: usize,
        scratch: &Path,
    ) -> Result<Vec<f64>, MlError> {
        self.predict_inner(model, flat_x, n_features, scratch, false)
    }

    /// The PRE-transform twin: the summed leaf values, before the objective's sigmoid.
    ///
    /// This exists so the "raw scores compare EXACTLY" rule has something enforcing it. Both sides
    /// sum the same 17-significant-digit leaf values in the same tree order with a plain `+`, and
    /// no transcendental is involved on either side — so a difference is not rounding, it is a row
    /// reaching a different leaf. [`crate::GbdtModel::raw_scores_batch`] is the other side.
    pub fn predict_raw(
        &self,
        model: &Path,
        flat_x: &[f64],
        n_features: usize,
        scratch: &Path,
    ) -> Result<Vec<f64>, MlError> {
        self.predict_inner(model, flat_x, n_features, scratch, true)
    }

    fn predict_inner(
        &self,
        model: &Path,
        flat_x: &[f64],
        n_features: usize,
        scratch: &Path,
        raw: bool,
    ) -> Result<Vec<f64>, MlError> {
        let data = scratch.join("predict.csv");
        let mut buf = Vec::new();
        // ⚠ Carries a DUMMY LABEL COLUMN — see `write_predict_csv`'s doc. A label-less file makes
        // an N-feature model see N-1 features and Log::Fatal.
        write_predict_csv(flat_x, n_features, &mut buf)?;
        write_file(&data, &buf)?;

        let result = scratch.join("predict.out");
        let params = crate::params::GbdtParams::default();
        let conf = TrainConfig {
            task: Task::Predict,
            params: &params,
            data: &data,
            output_model: None,
            input_model: Some(model),
            output_result: Some(&result),
            predict_raw_score: raw,
        };
        let conf_path = scratch.join("predict.conf");
        write_file(&conf_path, conf.render()?.as_bytes())?;
        self.run_config(&conf_path)?;

        let text = std::fs::read_to_string(&result)
            .map_err(|e| MlError::Io(format!("reading {}: {e}", result.display())))?;
        let out: Result<Vec<f64>, MlError> = text
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| {
                l.trim().parse::<f64>().map_err(|e| {
                    MlError::Backend(format!("prediction line {l:?} is not a number: {e}"))
                })
            })
            .collect();
        let out = out?;
        let want = flat_x.len() / n_features;
        if out.len() != want {
            return Err(MlError::Shape(format!("predicted {} rows, expected {want}", out.len())));
        }
        Ok(out)
    }
}

fn write_file(path: &Path, bytes: &[u8]) -> Result<(), MlError> {
    std::fs::write(path, bytes).map_err(|e| MlError::Io(format!("writing {}: {e}", path.display())))
}

/// The provenance file's path: `PROVENANCE` beside the binary.
///
/// One public spelling of the convention, because a second copy is a law spelled twice:
/// [`provenance_version`] reads its `tag=` line, and a learner building the fit identity
/// [`crate::fit_cache`] keys on digests the FILE BYTES this path names into every cache key
/// (`crates/vike-ml/src/gbdt_learner.rs`'s `GbdtLearner` is the one that does;
/// the provenance is the recorded witness of
/// WHICH trainer produced a score, and the closest thing to the binary's identity that exists —
/// this module's own header explains why the binary cannot name itself).
pub fn provenance_path(binary: &Path) -> PathBuf {
    binary.with_file_name("PROVENANCE")
}

/// The upstream tag a binary was built from, per the `PROVENANCE` file beside it.
///
/// ⚠ Not a process probe, because there is nothing to probe: LightGBM's `src/main.cpp` has no
/// version handling, `src/application/application.cpp` never mentions a version, and `--version`
/// yields an unknown-parameter warning plus "No training/prediction data, application quit" on a
/// non-zero exit. A probe that concatenated stdout and stderr while ignoring the exit status would
/// feed that Fatal text into the comparison and refuse every binary in existence.
///
/// A MISSING file is an ordinary state — somebody built the binary by hand — so it is an error
/// naming both paths and the recipe, never a panic.
pub fn provenance_version(binary: &Path) -> Result<String, MlError> {
    let path = provenance_path(binary);
    let text = std::fs::read_to_string(&path).map_err(|e| {
        MlError::Trainer(format!(
            "no PROVENANCE beside the binary {} (looked at {}): {e}. This crate cannot ask the \
             binary what it is — upstream LightGBM has no version flag — so the build recipe \
             records it instead. Rebuild with `just lightgbm-build`",
            binary.display(),
            path.display()
        ))
    })?;
    text.lines()
        .find_map(|l| l.trim().strip_prefix("tag="))
        .map(str::to_string)
        .ok_or_else(|| MlError::Trainer(format!("{} has no `tag=` line", path.display())))
}

/// Does a recorded tag equal the pinned release?
///
/// Exact, not a substring: this compares one recorded field against one constant, both written in
/// the same `vN.N.N` form, and a substring test would accept `v4.7.01`.
fn version_matches_pin(recorded: &str) -> bool {
    recorded.trim() == PINNED_LIGHTGBM_TAG
}

/// The refusal — long on purpose, because the alternative is training with the wrong library.
fn refuse_version(found: &str) -> MlError {
    MlError::Trainer(format!(
        "this binary's PROVENANCE records `{found}`, but vike-ml is pinned to upstream LightGBM \
         `{PINNED_LIGHTGBM_TAG}`. Training with a different release produces a model this crate's \
         walker was never validated against — it will parse and predict, and only \
         `tests/train_infer_equality.rs` would notice. Rebuild with `just lightgbm-build`, or, if \
         the bump is INTENDED, follow `crates/vike-ml/CLAUDE.md`: move `crate::pins`, re-run the \
         equality gate on the box that holds the binary, and regenerate the golden fixtures."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A path nothing will ever be at — the `vike-backfill` idiom
    /// (`crates/vike-backfill/src/backtest_bridge.rs`'s `sweep_runs_every_point_over_this_store`
    /// points a store at a deliberately nonexistent client and asserts the error names it).
    const MISSING_BIN: &str = "vike-no-such-lightgbm-binary";

    /// A scratch directory NO OTHER RUN can already own, removed when the returned guard drops.
    ///
    /// ⚠ This exists because a FIXED `/tmp/<name>` is a CI-RELIABILITY hazard, not a tidiness one.
    /// CI runs as one user and agents as another **on the same box**, and whichever creates the
    /// directory first OWNS it — every later `create_dir_all` under the other user returns
    /// `PermissionDenied` and keeps doing so forever, because nothing ever cleans `/tmp`.
    /// [`a_binary_with_no_provenance_beside_it_names_both_paths_rather_than_panicking`] and
    /// [`the_provenance_tag_line_is_what_is_read`] were both reproducibly red that way.
    /// `crates/vike-ops/tests/temp_path_gate.rs` is the gate that keeps the shape out.
    ///
    /// ⚠ This used to answer with a bare `PathBuf` under a `<temp>/vike_ml_<tag>_<pid>_<nanos>`
    /// name, and a unique name fixes only half of it: **nothing removed the directory**. Measured
    /// on the CI box on 2026-08-25, `/tmp` held 44,840 leaked test directories accumulating since at
    /// least 2026-08-18, this crate's families among them. A PID is also REUSED, so the collision
    /// the paragraph above describes stays reachable for as long as the stale directory survives —
    /// which was forever. [`vike_model::scratch::ScratchDir`] closes both halves: its `Drop`
    /// removes the directory (so a stale one cannot outlive the test that made it), and its
    /// `create_in` clears a pre-existing path, which is only reachable through pid reuse after an
    /// abort.
    ///
    /// ⚠ It is `ScratchDir` rather than `tempfile::TempDir` — the workspace's usual answer — for
    /// the reason this comment used to give for taking no guard at all: this crate ships zero
    /// dependencies beyond `vike-model` (`crates/vike-ml/CLAUDE.md`, quoting its plan's Global
    /// Constraints), and a dev-dependency is still a dependency of that claim. `ScratchDir` is
    /// std-only and lives IN `vike-model`, so the constraint costs nothing here.
    fn scratch(tag: &str) -> vike_model::scratch::ScratchDir {
        vike_model::scratch::ScratchDir::create_in(&std::env::temp_dir(), &format!("vike_ml_{tag}"))
            .expect("scratch dir")
    }

    #[test]
    fn a_binary_that_is_not_there_is_a_trainer_error_naming_the_path_and_the_recipe() {
        let err = LightGbmCli::new(Path::new(MISSING_BIN)).unwrap_err();
        let msg = err.to_string();
        assert!(matches!(err, MlError::Trainer(_)), "{msg}");
        assert!(msg.contains(MISSING_BIN), "the operator must see WHICH path: {msg}");
        assert!(msg.contains("just lightgbm-build"), "...and how to get one: {msg}");
    }

    #[test]
    fn running_a_config_through_a_missing_binary_reaches_the_spawn_and_says_so() {
        // `unchecked` skips the version probe, so this proves the RUN path's own error, not the
        // constructor's — they are different failures and an operator needs to tell them apart.
        let cli = LightGbmCli::unchecked(Path::new(MISSING_BIN));
        let err = cli.run_config(Path::new("whatever.conf")).unwrap_err();
        assert!(err.to_string().contains(MISSING_BIN), "{err}");
    }

    #[test]
    fn a_version_that_is_not_the_pin_is_refused_rather_than_trained_with() {
        // The compare half is pure, so it is tested without a process and without a file. A
        // binary that is not the pinned release is an OPERATOR fault — the box has the wrong
        // LightGBM on it — and training with it produces a model this crate never validated.
        assert!(version_matches_pin("v4.7.0"));
        assert!(!version_matches_pin("v4.6.0"));
        assert!(!version_matches_pin(""));

        let err = refuse_version("v4.6.0").to_string();
        assert!(err.contains("v4.6.0"), "what it found: {err}");
        assert!(err.contains(crate::pins::PINNED_LIGHTGBM_TAG), "what it wants: {err}");
        assert!(err.contains("train_infer_equality"), "what proves a bump: {err}");
    }

    #[test]
    fn a_binary_with_no_provenance_beside_it_names_both_paths_rather_than_panicking() {
        // The ORDINARY state of a binary somebody built by hand, so it is an error with a fix in
        // it, not a panic. ⚠ The binary itself cannot answer this question at all: LightGBM's
        // src/main.cpp has no version handling and `--version` yields an unknown-parameter
        // warning plus "No training/prediction data, application quit" on a non-zero exit.
        let dir = scratch("no_provenance");
        let fake = dir.join("lightgbm");
        std::fs::write(&fake, b"not really a binary").unwrap();
        let err = provenance_version(&fake).unwrap_err();
        let msg = err.to_string();
        assert!(matches!(err, MlError::Trainer(_)), "{msg}");
        assert!(msg.contains("PROVENANCE"), "the file it wanted: {msg}");
        assert!(msg.contains("lightgbm"), "the binary it was asked about: {msg}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_provenance_tag_line_is_what_is_read() {
        let dir = scratch("provenance_ok");
        let fake = dir.join("lightgbm");
        std::fs::write(&fake, b"x").unwrap();
        std::fs::write(
            dir.join("PROVENANCE"),
            b"tag=v4.7.0\nsha=8f7036f0\ncmake_flags=-DUSE_OPENMP=OFF\n",
        )
        .unwrap();
        assert_eq!(provenance_version(&fake).unwrap(), "v4.7.0");
        assert!(LightGbmCli::new(&fake).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_recorded_sha256_is_provenance_and_is_never_compared_against_the_bytes_on_disk() {
        // ⚠ Pins what this check does NOT do, because the module doc used to claim the opposite and
        // an operator who believes it is trusting a guarantee that does not exist.
        // `scripts/build_lightgbm.sh` records a `sha256=` line; `provenance_version` reads `tag=`
        // and nothing else, and there is no hash implementation anywhere in this crate to compare
        // it with. Two DIFFERENT binaries under one unchanged PROVENANCE — whose sha256 matches
        // neither — are both accepted.
        //
        // The case that makes this matter rather than pedantic: a genuine v4.7.0 rebuilt with
        // -DUSE_OPENMP=ON. Same tag, different bytes, accepted here — and that configuration
        // measured 143,293 ms against 2,105 ms in the backend bakeoff. `grid::check_grid_params`
        // is what actually stands between the grid and that cliff; this constructor never could.
        // pid alone was the old spelling here; `scratch` adds the nanos, because a pid REPEATS
        // and a `/tmp` entry left behind by a panicked run under the other user does not expire.
        let dir = scratch("provenance_sha");
        let fake = dir.join("lightgbm");
        std::fs::write(
            dir.join("PROVENANCE"),
            format!(
                "tag={PINNED_LIGHTGBM_TAG}\ncmake_flags=-DUSE_OPENMP=OFF\nsha256={}\n",
                "0".repeat(64)
            ),
        )
        .unwrap();

        std::fs::write(&fake, b"one build of the pinned tag").unwrap();
        assert!(LightGbmCli::new(&fake).is_ok(), "a sha256 that matches nothing is still accepted");
        std::fs::write(&fake, b"a DIFFERENT build of the same tag, e.g. -DUSE_OPENMP=ON").unwrap();
        assert!(
            LightGbmCli::new(&fake).is_ok(),
            "the bytes changed and the PROVENANCE did not — if this ever fails, somebody made the \
             check verify the hash, which is a real improvement: update the module doc and \
             crates/vike-ml/CLAUDE.md's failure table to say so, and delete this test"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
