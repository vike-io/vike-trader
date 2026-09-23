//! Running a parameter grid over a fixed set of datasets — the shape this backend was chosen for.
//!
//! # Bin once, fit many
//!
//! The grid varies PARAMS over a FIXED set of fold×horizon datasets. So each dataset is binned ONCE
//! with `task=save_binary` (measured: ~20 s for 92 of them) and every later fit reads a ~2.2 MB
//! binary instead of re-parsing an 11.6 MB CSV — measured at ~212 ms of write plus ~300 ms of parse
//! per fit, which over 22,400 fits is roughly 260 GB of text I/O that simply does not happen.
//!
//! ⚠ **No in-process binding can do this**, which is a large part of why this crate drives a
//! process: `Booster::train` CONSUMES its Dataset, so a binding re-bins on every fit — a measured
//! 95 ms/fit floor that cannot be amortized.
//!
//! ⚠ The binning config must carry `categorical_feature`. Binning is where LightGBM decides
//! categorical-vs-numeric, so a binary built without it is all-numeric FOREVER, however loudly a
//! later fit declares the column — the same silent failure that disqualified every Rust binding.
//! [`crate::train::TrainConfig::render`] writes it for `Task::SaveBinary`; [`bin_dataset`] is why.
//!
//! # Two operational hazards, each closed here rather than remembered
//!
//! * **Log volume.** A `tracing` event per fit writes 22,400 records into the trace-level JSON file
//!   layer, which `RUST_LOG` does not turn down. `pmxt_backfill` once wrote 341 GB that way and
//!   nearly filled the disk hosting a live trading node. THIS MODULE LOGS NOTHING; a caller reports
//!   per FOLD (92 lines), never per fit, and sets `VIKE_LOG_FILE_LEVEL=warn` for a grid run.
//! * **The threading cliff by the back door.** [`check_grid_params`] refuses a `num_threads` other
//!   than 1, because a search axis is exactly how a 30-minute grid becomes a multi-week one on a
//!   box whose binary was rebuilt with OpenMP. ⚠ It is a check a CALLER runs at the point its fit
//!   parameters are finalised — this module does not drive a fit any more (see below), so it has
//!   nowhere of its own to run it. `crates/vike-ml/src/gbdt_learner.rs`'s
//!   `params_for` is that point today, and it is the whole live caller — a private function in
//!   THIS crate now rather than in a study's, since the learner that owns it was promoted out of
//!   the research crate before that crate dissolved.
//!
//! # A third hazard used to be closed here, by code nothing called
//!
//! Filesystem churn: one config, one model and one prediction file per fit is 67,200 files, so a
//! `Workspace` gave each WORKER one of each to overwrite per fit and a `fit_point` drove one fit
//! against a pre-binned dataset through it. Neither ever had a caller outside this crate's own
//! tests. The one consumer drives its own fit — it needs the model TEXT and its own per-fit
//! scratch directory, because its parallelism is 32 concurrent fits inside ONE process rather than
//! one worker per process — so both are deleted rather than left as a recommendation nobody took.
//! The hazard is real and its answer moved with the code that faced it; what is gone is the
//! unused implementation, not the rule.

use std::path::{Path, PathBuf};

use crate::cli::LightGbmCli;
use crate::error::MlError;
use crate::params::GbdtParams;
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
/// [`BinPreFilter::MinDataAgnostic`], while `crates/vike-ml/src/gbdt_learner.rs`'s per-fold reuse
/// passes [`BinPreFilter::MatchFit`] and keys its cache on `params.min_data_in_leaf`.
pub fn bin_dataset(
    cli: &LightGbmCli,
    data: &TrainData<'_>,
    params: &GbdtParams,
    pre_filter: BinPreFilter,
    out: &Path,
    scratch: &Path,
) -> Result<PathBuf, MlError> {
    // ⚠ Unique per call. `scratch` is a directory the CALLER chose and may well reuse across
    // concurrent binnings — 92 datasets is exactly the shape somebody parallelizes — and two
    // binnings sharing `bin-input.csv` would race on the same file and the same `.bin` beside it.
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

    // RESOLVED against upstream source rather than assumed: `Config::Set` auto-sets
    // `save_binary=true` when `task=save_binary`, `Dataset::SaveBinaryFile(nullptr)` appends
    // ".bin" to `data_filename_`, `InitTrain` then logs "Save data as binary finished, exit" and
    // exits 0, and `DatasetLoader::CheckCanLoadFromBin` loads `data=/path/foo.csv.bin` correctly.
    let produced = csv.with_extension("csv.bin");
    // ⚠ COPY + remove, not `rename`. The module doc tells callers to put `scratch` on the NVMe,
    // which is precisely how `out` and `scratch` end up on different filesystems — and `rename`
    // then fails with EXDEV, after the expensive part of the work is already done.
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
        // The 68x cliff, arriving by the back door: a search axis named `num_threads`. The binary
        // is built without OpenMP so it could not honour it anyway, but a config that ASKS for 32
        // threads is a config somebody will eventually run against an OpenMP build. Refuse it
        // where the fits happen, with the measurement in the message.
        let p = GbdtParams { num_threads: 32, ..GbdtParams::default() };
        let err = check_grid_params(&p).unwrap_err().to_string();
        assert!(err.contains("num_threads"), "{err}");
        assert!(err.contains("143293") || err.contains("143,293"), "cite the measurement: {err}");
        assert!(check_grid_params(&GbdtParams::default()).is_ok());
    }
}
