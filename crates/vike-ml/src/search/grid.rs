//! Running a parameter grid over a fixed set of datasets — the shape this backend was chosen for.
//!
//! # Bin once, fit many
//!
//! The grid varies PARAMS over a FIXED set of fold×horizon datasets, so each dataset is binned ONCE
//! with `task=save_binary` and every later fit reads a ~2.2 MB binary instead of re-parsing an
//! 11.6 MB CSV (measured ~212 ms of write plus ~300 ms of parse per fit; ~260 GB of text I/O over
//! 22,400 fits).
//!
//! ⚠ **No in-process binding can do this**: `Booster::train` CONSUMES its Dataset, so a binding
//! re-bins on every fit — a measured 95 ms/fit floor that cannot be amortized.
//!
//! ⚠ The binning config must carry `categorical_feature`: binning is where LightGBM decides
//! categorical-vs-numeric, so a binary built without it is all-numeric FOREVER.
//! [`crate::train::TrainConfig::render`] writes it for `Task::SaveBinary`.
//!
//! # Two operational hazards
//!
//! * **Log volume.** A `tracing` event per fit is 22,400 records in the trace-level JSON file
//!   layer, which `RUST_LOG` does not turn down. THIS MODULE LOGS NOTHING; a caller reports per
//!   FOLD, never per fit, and runs a grid under a `warn` `preferences.log_file_level` row.
//! * **The threading cliff by the back door.** [`check_grid_params`] refuses a `num_threads` other
//!   than 1: a search axis is how a 30-minute grid becomes a multi-week one on an OpenMP build. ⚠ A
//!   CALLER runs it where its fit parameters are finalised — today
//!   `crates/vike-ml/src/train/gbdt_learner.rs`'s `params_for`, the whole live caller.
//!
//! Filesystem churn (one config, model and prediction file per fit) is the caller's to manage: a
//! per-worker `Workspace` and a `fit_point` driver that faced it here were deleted for having no
//! caller — the one consumer runs 32 concurrent fits in ONE process, each with its own scratch.

use std::path::{Path, PathBuf};

use crate::error::MlError;
use crate::train::cli::LightGbmCli;
use crate::train::params::GbdtParams;
use crate::train::{BinPreFilter, Task, TrainConfig, TrainData};

/// Refuse the parameters that turn a 30-minute grid into a multi-week one.
pub fn check_grid_params(params: &GbdtParams) -> Result<(), MlError> {
    if params.num_threads != 1 {
        return Err(MlError::Shape(format!(
            "num_threads={} in a grid fit. Every fit here is single-threaded BY DESIGN — the \
             parallelism is ACROSS fits, never inside one — and LightGBM's own threading measured \
             143293 ms against 2105 ms at one thread on this data shape, a 68x cliff in the wrong \
             direction. The pinned binary is built -DUSE_OPENMP=OFF so it cannot honour this \
             anyway; refusing it here means a binary rebuilt WITH OpenMP does not turn 30 minutes \
             into weeks silently.",
            params.num_threads
        )));
    }
    Ok(())
}

/// Bin one dataset once, into LightGBM's binary dataset format.
///
/// Returns the path of the binary. Every later fit on this fold passes it as `data`. Which fits
/// the binary may legitimately serve is `pre_filter`'s declaration — [`BinPreFilter`]'s doc
/// carries the measured divergence behind the two modes; this crate's grid shape passes
/// [`BinPreFilter::MinDataAgnostic`], while `crates/vike-ml/src/train/gbdt_learner.rs`'s per-fold reuse
/// passes [`BinPreFilter::MatchFit`] and keys its cache on `params.min_data_in_leaf`.
pub fn bin_dataset(
    cli: &LightGbmCli,
    data: &TrainData<'_>,
    params: &GbdtParams,
    pre_filter: BinPreFilter,
    out: &Path,
    scratch: &Path,
) -> Result<PathBuf, MlError> {
    // ⚠ Unique per call: a caller may reuse `scratch` across concurrent binnings, which would
    // otherwise race on one input file and the `.bin` beside it.
    let tag = format!("{}-{:p}", std::process::id(), data.x.as_ptr());
    let csv = scratch.join(format!("bin-input-{tag}.csv"));
    let mut buf = Vec::new();
    data.write_csv(&mut buf)?;
    std::fs::write(&csv, &buf)
        .map_err(|e| MlError::Io(format!("writing {}: {e}", csv.display())))?;

    // `save_binary` writes `<data>.bin` beside its input, so the input path decides the output.
    let conf = TrainConfig {
        task: Task::SaveBinary(pre_filter),
        params,
        data: &csv,
        output_model: None,
        input_model: None,
        output_result: None,
        predict_raw_score: false,
    };
    let conf_path = scratch.join(format!("save-binary-{tag}.conf"));
    std::fs::write(&conf_path, conf.render()?)
        .map_err(|e| MlError::Io(format!("writing {}: {e}", conf_path.display())))?;
    cli.run_config(&conf_path)?;

    // Read from upstream source: `Dataset::SaveBinaryFile(nullptr)` appends ".bin" to
    // `data_filename_`, and `DatasetLoader::CheckCanLoadFromBin` loads `foo.csv.bin` correctly.
    let produced = csv.with_extension("csv.bin");
    // ⚠ COPY + remove, not `rename`: `out` and `scratch` may sit on different filesystems, where
    // `rename` fails with EXDEV after the expensive work is done.
    std::fs::copy(&produced, out).map_err(|e| {
        MlError::Io(format!("copying {} to {}: {e}", produced.display(), out.display()))
    })?;
    let _ = std::fs::remove_file(&produced);
    let _ = std::fs::remove_file(&csv);
    let _ = std::fs::remove_file(&conf_path);
    Ok(out.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_grid_point_with_a_thread_count_other_than_one_is_refused() {
        // The 68x cliff by the back door: a config that ASKS for 32 threads will eventually meet an
        // OpenMP build. Refused where the fits happen, with the measurement in the message.
        let p = GbdtParams { num_threads: 32, ..GbdtParams::default() };
        let err = check_grid_params(&p).unwrap_err().to_string();
        assert!(err.contains("num_threads"), "{err}");
        assert!(err.contains("143293") || err.contains("143,293"), "cite the measurement: {err}");
        assert!(check_grid_params(&GbdtParams::default()).is_ok());
    }
}
