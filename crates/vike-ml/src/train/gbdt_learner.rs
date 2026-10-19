//! ⚠ **THE ONE [`Learner`] THAT DRIVES A REAL TRAINER.** Everything else that implements the seam
//! in this workspace is a double.
//!
//! [`crate::seam::learner`] states the seam — fit a [`TrainData`] at a [`GridPoint`], read one
//! probability per row. This impl spawns the pinned LightGBM binary to do it, and owns what the
//! trainer insists on: a verified binary, per-fit scratch directories, a parameter bag, a
//! binned-dataset cache and the error mapping.
//!
//! # The contract
//!
//! **(a) The NON-searched parameters are the CALLER's**, handed over once as
//! [`GbdtLearner::new`]'s `base` and applied by [`params_for`] over
//! `crates/vike-ml/src/train/params.rs`'s `GbdtParams::default`. The five searched axes (`num_leaves`,
//! `max_depth`, `min_data_in_leaf`, `learning_rate`, `feature_fraction`) arrive on every
//! [`Learner::fit`] inside a [`GridPoint`], so a base bag must NOT restate them: a base copy of a
//! searched axis hands a DROPPED mapping a plausible value to fail silently with
//! (`no_searched_axis_is_also_pinned_in_the_base_where_it_could_mask_a_dropped_mapping`).
//!
//! **(b) A categorical column** is a FEATURE index in `TrainData::categorical` (the label column
//! is not counted, LightGBM's own convention), rendered as a COMMA-SEPARATED string by
//! `crates/vike-ml/src/train/params.rs`'s `categorical_feature_csv`. ⚠ Never a list: LightGBM's config
//! parser strips quotes but not brackets, so `[0,3]` names no column, the column is binned as a
//! NUMBER, nothing errors, and the saved model's parameter echo still claims it was categorical.
//! An empty `categorical` declares NOTHING (`None`). An index outside the row width — which
//! LightGBM ignores in silence — is refused before anything spawns
//! (`crates/vike-ml/src/train.rs`'s `TrainData::validate`, reached through [`check_shape`]).
//!
//! **(c) The learner and its model are `Sync`**, so the grid search stays parallel:
//! `the_learner_and_its_model_satisfy_the_bounds_the_parallel_search_needs` asserts it at compile
//! time on `GbdtLearner` written with no type argument, i.e. the [`LightGbmCli`] instantiation a
//! real caller builds. Each `fit` gets its OWN scratch directory ([`GbdtLearner::fit_dir`]): two
//! concurrent fits sharing one config path would each train the other's parameters.
//!
//! **(d) The error type is `String`**, for the reason [`crate::seam::learner`]'s module doc gives;
//! [`MlError`] is mapped through `Display` at every boundary.
//!
//! **(e) `num_threads = 1`.** The pinned binary is built `-DUSE_OPENMP=OFF` and
//! `crates/vike-ml/src/search/grid.rs`'s `check_grid_params` refuses anything else, because thread count
//! changes the float reduction order. Fits are parallel across PROCESSES, not inside one.
//!
//! # The handover, and what gates it
//!
//! The most consequential silent failure here is [`Learner::fit`] handing the trainer some other
//! [`GbdtParams`] than the one it just built (`&GbdtParams::default()` for `&params`): every grid
//! point would train identically, the search would still name a winner, and the winner would be
//! noise. [`Trainer`] is the smallest seam that makes the handover observable — one method per
//! real call `fit` makes into [`LightGbmCli`]: `train`, and `save_binary`, because binning BAKES
//! `min_data_in_leaf` and the categorical declaration into the dataset. `#[cfg(test)]`'s
//! `RecordingTrainer` keeps the [`GbdtParams`] it is handed (for the binning too), and
//! `the_trainer_is_handed_the_parameters_the_fit_just_built` asserts every searched axis, the seed
//! and the categorical CSV at that boundary — each against its value AND against the library
//! default it would wear had some other bag been handed over.
//!
//! ⚠ **That test's whole-bag equality and its per-field assertions kill DIFFERENT mutations; do
//! not simplify either away.** An axis dropped from `point_bag` vanishes from BOTH sides of the
//! equality at once, so only the literals see it; `&GbdtParams::default()` for `&params` moves
//! every field at once, so the equality alone sees that.
//!
//! Not gated by a test that runs in CI, and both LOUD rather than plausible:
//!
//! * **That LightGBM HONOURS what it is handed** — the `#[ignore]`d smokes and
//!   `crates/vike-ml/tests/train_infer_equality.rs`. The real [`Trainer`] impl being a REAL
//!   delegation IS gated: `fit_checks_the_shape_before_it_spawns_anything` reaches the spawn
//!   through `Trainer::train` on a [`LightGbmCli`] and asserts the error names the missing binary.
//! * **The two PATHS in the config** — the binary `binned_for` produced and the model file the fit
//!   names. A wrong one fails the child's open on the first fit anybody runs.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use crate::seam::fit_cache;
use crate::search::grid::{bin_dataset, check_grid_params};
use crate::train::cli::LightGbmCli;
use crate::train::{BinPreFilter, Task, TrainConfig};
use crate::{
    Capture, CapturedFit, FitImportance, GbdtModel, GbdtParams, GridPoint, Learner, MlError,
    TrainData, importance_from_model_text, parse_model_text,
};

/// `base` with the point applied, the seed pinned and the categorical columns declared.
///
/// `base` is a CONSTRUCTOR PARAMETER so no caller's regularisation is compiled into this crate. A
/// caller assembling its own: `bagging_freq` defaults to `0`, which DISABLES bagging however small
/// `bagging_fraction` is — one setting wearing two names; dropping either alone silently trains an
/// unbagged model.
///
/// ⚠ The last step is [`check_grid_params`], HERE because this is where a fit's parameters are
/// finalised: a later edit to a caller's base bag, or to the library default it inherits, cannot
/// then turn a 30-minute grid into a multi-week one (the 68x threading cliff) in silence. Today it
/// refuses nothing — `GbdtParams::default` pins `num_threads = 1` and `apply` refuses the NAME as
/// a searched axis.
fn params_for(
    base: &GbdtParams,
    p: &GridPoint,
    seed: u64,
    categorical: &[usize],
) -> Result<GbdtParams, String> {
    let mut params = base.with_point(&p.as_param_point()).map_err(|e| e.to_string())?;
    params.seed = seed;
    params.categorical_feature =
        (!categorical.is_empty()).then(|| GbdtParams::categorical_feature_csv(categorical));
    check_grid_params(&params).map_err(|e| e.to_string())?;
    Ok(params)
}

/// The one train invocation, built in one place so a test can render exactly what a fit runs.
fn train_config<'a>(
    params: &'a GbdtParams,
    data: &'a Path,
    output_model: &'a Path,
) -> TrainConfig<'a> {
    TrainConfig {
        task: Task::Train,
        params,
        data,
        output_model: Some(output_model),
        input_model: None,
        output_result: None,
        predict_raw_score: false,
    }
}

/// The shape refusal, in the error type this seam speaks. The check is
/// `crates/vike-ml/src/train.rs`'s `TrainData::validate`; this file's part is running it BEFORE
/// anything is written or spawned.
fn check_shape(d: &TrainData<'_>) -> Result<(), String> {
    d.validate().map_err(|e| e.to_string())
}

/// The TWO calls a fit makes into the process driver — the train, and the once-per-dataset
/// binning the train reads from.
///
/// It exists for a test: without it, handing over [`train_config`]'s output (and the binning's
/// parameter bag) is unobservable from inside this process — see the module doc's "The
/// handover". Each method is one real call the fit makes; keep it that thin.
///
/// `Sync` because [`Learner`] is (the grid search calls `fit` from `rayon::par_iter`), so a
/// trainer that is not `Sync` is refused where it is SUBSTITUTED, not in a `rayon` type error
/// elsewhere.
///
/// ⚠ Crate-private, so the learner's non-trait methods below are NOT in an `impl<T: Trainer>`
/// block: a crate-private trait in the bounds of an inherent impl on a `pub` type is
/// `private_bounds`, which is `-D warnings`.
pub(crate) trait Trainer: Sync {
    fn train(&self, conf: &TrainConfig<'_>, scratch: &Path) -> Result<String, MlError>;
    /// Bin `data` into LightGBM's binary dataset format at `out`, with
    /// [`BinPreFilter::MatchFit`] semantics: `params.min_data_in_leaf` and
    /// `params.categorical_feature` are consumed at construction, so a later `train` reading
    /// `out` reproduces a plain per-fit CSV train at those values byte-for-byte.
    fn save_binary(
        &self,
        data: &TrainData<'_>,
        params: &GbdtParams,
        out: &Path,
        scratch: &Path,
    ) -> Result<(), MlError>;
}

/// The real trainer: straight delegations.
///
/// `train` is spelled with the fully-qualified call rather than `self.train(conf, scratch)` so the
/// delegation names the inherent method it forwards to: an inherent method shadows a trait one of
/// the same name, and a reader should not have to know that to see which of the two runs.
///
/// ⚠ `save_binary` bins with [`BinPreFilter::MatchFit`], NEVER `MinDataAgnostic`, whose trees
/// measurably diverge from a CSV fit's whenever the default pre-filter would have dropped a feature
/// (see [`BinPreFilter`]'s doc). Gated against the real binary by
/// `the_binned_fit_matches_the_per_fit_csv_train_across_every_searched_axis_value`.
impl Trainer for LightGbmCli {
    fn train(&self, conf: &TrainConfig<'_>, scratch: &Path) -> Result<String, MlError> {
        LightGbmCli::train(self, conf, scratch)
    }

    fn save_binary(
        &self,
        data: &TrainData<'_>,
        params: &GbdtParams,
        out: &Path,
        scratch: &Path,
    ) -> Result<(), MlError> {
        bin_dataset(self, data, params, BinPreFilter::MatchFit, out, scratch).map(|_| ())
    }
}

// ---- the per-dataset binary cache -------------------------------------------------------------

/// Identity of a binned dataset: the training data's CONTENT plus the one searched axis that is
/// consumed at dataset construction.
///
/// The search hands the SAME [`TrainData`] to every trial of a fold, so a fold's fits share one
/// binary per `min_data_in_leaf` value. ⚠ `min_data_in_leaf` is in the key because it is
/// DATASET-BAKED: under LightGBM's default `feature_pre_filter` a bin built at one value trains
/// DIFFERENT trees at another ([`BinPreFilter`]'s doc carries the measurement). The other four
/// searched axes are tree-growth parameters and deliberately absent.
///
/// `x_ptr`/lengths alone would be an ABA hazard — a freed fold's buffer address can be reused by a
/// later fold of identical size — so the key also carries a 64-bit content fingerprint of the
/// features and labels: cheap insurance against a collision that also needs the same address and
/// lengths, not a security boundary.
#[derive(Clone, Debug, PartialEq, Eq)]
struct BinKey {
    x_ptr: usize,
    x_len: usize,
    n_rows: usize,
    n_cols: usize,
    x_hash: u64,
    y_hash: u64,
    categorical: Vec<usize>,
    min_data_in_leaf: u32,
}

impl BinKey {
    fn of(d: &TrainData<'_>, min_data_in_leaf: u32) -> Self {
        Self {
            x_ptr: d.x.as_ptr() as usize,
            x_len: d.x.len(),
            n_rows: d.n_rows,
            n_cols: d.n_cols,
            x_hash: fingerprint_f64(d.x),
            y_hash: fingerprint_labels(d.y),
            categorical: d.categorical.to_vec(),
            min_data_in_leaf,
        }
    }
}

/// A word-at-a-time rotate–xor–multiply fingerprint (FxHash's mixing constant). Not cryptographic,
/// and does not need to be (see [`BinKey`]); inline because eight lines do not buy a dependency.
fn fingerprint_words<I: Iterator<Item = u64>>(words: I) -> u64 {
    const K: u64 = 0x517c_c1b7_2722_0a95;
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for w in words {
        h = (h.rotate_left(5) ^ w).wrapping_mul(K);
    }
    h
}

fn fingerprint_f64(xs: &[f64]) -> u64 {
    fingerprint_words(xs.iter().map(|v| v.to_bits()))
}

/// The label twin. ⚠ Hashes the BIT PATTERN of each label, one word each, for the same reason
/// [`fingerprint_f64`] does: `-0.0` and `0.0` are `==` as floats and are different bytes in the
/// CSV the binning writes, so a value-wise fold would give two different datasets one key.
fn fingerprint_labels(ys: &[f32]) -> u64 {
    fingerprint_words(ys.iter().map(|v| u64::from(v.to_bits())))
}

/// One binned dataset on disk. Dropping the LAST handle deletes its directory, which is what makes
/// eviction safe under concurrency: an in-flight fit holds an [`Arc`] to the file it is training
/// from, so an evicted-while-in-use binary outlives the fit and dies with it.
#[derive(Debug)]
struct BinFile {
    dir: PathBuf,
    bin: PathBuf,
}

impl Drop for BinFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// One key's slot: a [`OnceLock`] so concurrent first fits of a fold bin ONCE — the losers block
/// on the winner's binning (a TPE startup batch arrives ten-at-once on a cold key).
///
/// ⚠ The `Err` arm is cached too: a binning failure (a spawn or disk failure) is answered to every
/// later fit of the same key without retrying — the blast radius a per-fit CSV had, spelled as one
/// failure instead of twenty-one.
#[derive(Default)]
struct BinSlot {
    once: OnceLock<Result<Arc<BinFile>, String>>,
}

/// How many binned datasets a [`GbdtLearner`] keeps resident by default.
///
/// ⚠ **A CONSTANT rather than a function of the thread pool**, so this crate needs no `rayon`
/// dependency. A caller that knows its pool width passes its own number to
/// [`GbdtLearner::with_bin_capacity`]; an under-sized cache is a RE-BIN, never a wrong answer.
///
/// 64 because a live fold holds at most four keys (one bin per `min_data_in_leaf` value its
/// trials reach, plus the final whole-fold refit), so 64 keeps sixteen concurrent folds resident
/// with slack.
pub const DEFAULT_BIN_CAPACITY: usize = 64;

/// The fold-keyed binning cache.
///
/// LRU over a `Vec` (move-to-back on hit, evict from the front past `capacity`), linear because
/// `capacity` is small. Entries die in two steps: eviction drops the cache's handle, and the
/// directory is deleted when the last in-flight fit drops its own ([`BinFile`]'s `Drop`).
/// Dropping the cache (with its learner) drops every entry.
struct BinCache {
    capacity: usize,
    entries: Mutex<Vec<(BinKey, Arc<BinSlot>)>>,
}

impl BinCache {
    fn new(capacity: usize) -> Self {
        Self { capacity, entries: Mutex::new(Vec::new()) }
    }

    fn slot(&self, key: BinKey) -> Arc<BinSlot> {
        let mut entries = self.entries.lock().expect("bin cache poisoned");
        if let Some(pos) = entries.iter().position(|(k, _)| *k == key) {
            let hit = entries.remove(pos);
            let slot = Arc::clone(&hit.1);
            entries.push(hit);
            return slot;
        }
        let slot = Arc::new(BinSlot::default());
        entries.push((key, Arc::clone(&slot)));
        while entries.len() > self.capacity {
            // The front is the least recently used. Removing it only drops the CACHE's handle;
            // an in-flight fit still holding the file keeps it alive until it finishes.
            entries.remove(0);
        }
        slot
    }
}

/// The real [`Learner`]: gradient-boosted trees, fitted by the pinned LightGBM binary.
///
/// ⚠ The trainer binary is a PARAMETER — no environment variable finds one
/// (`crates/vike-ops/tests/settings_secrets/settings_registry.rs`'s `LIBRARY_PIN`) — and so is the
/// scratch root a fit writes under.
///
/// `T` defaults to [`LightGbmCli`], so every `GbdtLearner` written with no type argument means the
/// real learner. Only this file's test trainers name another `T`.
pub struct GbdtLearner<T = LightGbmCli> {
    cli: T,
    scratch: PathBuf,
    /// Makes each fit's scratch directory unique. `fit` is called from `rayon::par_iter`, and one
    /// shared config path across concurrent fits is two fits training each other's parameters.
    next: AtomicU64,
    /// The per-dataset binaries the fits of a fold share — see [`BinCache`].
    bins: BinCache,
    /// The parameters every fit starts from, before its [`GridPoint`] is applied — see
    /// [`params_for`].
    base: GbdtParams,
    /// SHA-256 of the trainer binary's `PROVENANCE` file bytes, read ONCE at construction — the
    /// trainer's contribution to [`Learner::fit_identity`]. `None` means the cross-run fit cache
    /// never engages: the safe direction (a refit, never a wrong answer).
    provenance: Option<[u8; 32]>,
}

impl GbdtLearner<LightGbmCli> {
    /// `binary` is the pinned LightGBM CLI (`just lightgbm-build` writes one at
    /// `bin/lightgbm/lightgbm`); `scratch` is where per-fit subdirectories are created; `base` is
    /// the parameter bag every fit starts from ([`GbdtParams::default`], LightGBM's own defaults,
    /// for a caller with no opinion).
    ///
    /// The binary's provenance is checked here, ONCE: a wrong release parses, predicts and is
    /// wrong in ways only `crates/vike-ml/tests/train_infer_equality.rs` would see. The bin cache
    /// is sized at [`DEFAULT_BIN_CAPACITY`]; see [`Self::with_bin_capacity`].
    pub fn new(binary: &Path, scratch: &Path, base: GbdtParams) -> Result<Self, String> {
        let cli = LightGbmCli::new(binary).map_err(|e| e.to_string())?;
        std::fs::create_dir_all(scratch)
            .map_err(|e| format!("creating the scratch root {}: {e}", scratch.display()))?;
        // A race that removes the file `LightGbmCli::new` just verified degrades to `None` — an
        // uncached learner — rather than an error.
        let provenance = std::fs::read(crate::train::cli::provenance_path(binary))
            .ok()
            .map(|bytes| fit_cache::digest_of(&bytes));
        Ok(Self {
            cli,
            scratch: scratch.to_path_buf(),
            next: AtomicU64::new(0),
            bins: BinCache::new(DEFAULT_BIN_CAPACITY),
            base,
            provenance,
        })
    }
}

impl<T> GbdtLearner<T> {
    /// Resize the binned-dataset cache (see [`DEFAULT_BIN_CAPACITY`]).
    ///
    /// ⚠ A PERFORMANCE knob and nothing else: an evicted entry is re-binned, so no value can change
    /// a fitted model. `0` is legal and means "bin every fit".
    #[must_use]
    pub fn with_bin_capacity(mut self, capacity: usize) -> Self {
        self.bins = BinCache::new(capacity);
        self
    }

    /// A directory no other fit in this process will use.
    fn fit_dir(&self) -> PathBuf {
        self.scratch.join(format!(
            "fit-{}-{}",
            std::process::id(),
            self.next.fetch_add(1, Ordering::Relaxed)
        ))
    }

    /// A directory no other binning in this process will use — same counter, distinct prefix.
    fn bin_dir(&self) -> PathBuf {
        self.scratch.join(format!(
            "bin-{}-{}",
            std::process::id(),
            self.next.fetch_add(1, Ordering::Relaxed)
        ))
    }
}

/// Bin one dataset for one `min_data_in_leaf`, into its own directory.
///
/// A free function rather than a method for the `private_bounds` reason [`Trainer`]'s doc gives.
fn bin_once<T: Trainer>(
    l: &GbdtLearner<T>,
    d: &TrainData<'_>,
    params: &GbdtParams,
) -> Result<Arc<BinFile>, String> {
    let dir = l.bin_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    let bin = dir.join("train.bin");
    match l.cli.save_binary(d, params, &bin, &dir) {
        Ok(()) => Ok(Arc::new(BinFile { dir, bin })),
        Err(e) => {
            let _ = std::fs::remove_dir_all(&dir);
            Err(e.to_string())
        }
    }
}

/// The binary for `(d, params.min_data_in_leaf)`, binning it if this is the first fit to ask.
fn binned_for<T: Trainer>(
    l: &GbdtLearner<T>,
    d: &TrainData<'_>,
    params: &GbdtParams,
) -> Result<Arc<BinFile>, String> {
    let slot = l.bins.slot(BinKey::of(d, params.min_data_in_leaf));
    slot.once.get_or_init(|| bin_once(l, d, params)).clone()
}

/// The train half: run one fit off an already-binned dataset and parse the model it wrote.
fn fit_in<T: Trainer>(
    cli: &T,
    dir: &Path,
    data: &Path,
    params: &GbdtParams,
) -> Result<GbdtModel, String> {
    let model_path = dir.join("model.txt");
    let text =
        cli.train(&train_config(params, data, &model_path), dir).map_err(|e| e.to_string())?;
    parse_model_text(&text).map_err(|e| e.to_string())
}

impl<T: Trainer> Learner for GbdtLearner<T> {
    type Model = GbdtModel;

    /// ⚠ Trains from a PRE-BINNED dataset, shared through [`BinCache`] by every fit of the same
    /// data at the same `min_data_in_leaf`, keyed on the data's CONTENT because the seam hands
    /// `fit` no fold identity. A fold's 20 search trials write and parse the ~8 MB training CSV
    /// at most three times (once per `min_data_in_leaf` value reached) instead of twenty.
    ///
    /// The models are BIT-IDENTICAL to per-fit CSV construction, which is why `min_data_in_leaf`
    /// and the categorical declaration are both in [`BinKey`]:
    /// `the_binned_fit_matches_the_per_fit_csv_train_across_every_searched_axis_value` proves it
    /// for all three values of every searched axis.
    fn fit(&self, d: &TrainData<'_>, p: &GridPoint, seed: u64) -> Result<Self::Model, String> {
        check_shape(d)?;
        let params = params_for(&self.base, p, seed, d.categorical)?;
        // Held for the whole fit: an LRU eviction drops only the cache's handle, so the binary
        // cannot be deleted out from under the child process reading it.
        let bin = binned_for(self, d, &params)?;
        let dir = self.fit_dir();
        std::fs::create_dir_all(&dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
        let out = fit_in(&self.cli, &dir, &bin.bin, &params);
        // Removed on BOTH outcomes: a rejected grid point is an ordinary event, and its error
        // string already carries LightGBM's stderr tail.
        let _ = std::fs::remove_dir_all(&dir);
        out
    }

    /// The cross-run fit cache's train half: a digest over THE ACTUAL BYTES [`Self::fit`] would
    /// consume for `(d, p, seed)` — content-addressed, never a description (the constraint is
    /// [`crate::seam::fit_cache`]'s module doc).
    ///
    /// ⚠ The transcript tag `vike-research fit-identity v1` is a FROZEN cache key, not a name:
    /// `crates/vike-ml/CLAUDE.md`'s "The learner seam, the double and the fit cache" says why.
    /// Change it only when the identity's CONTENT changes.
    ///
    /// # The enumeration — every input of a fit, and which part covers it
    ///
    /// A fit bins `d` ([`bin_once`] → `write_csv` → the child's `save_binary`), trains from the
    /// binary ([`fit_in`] → [`train_config`] → the child), then parses the model text. Its inputs:
    ///
    /// * **the training data** — the CSV part: the EXACT bytes `TrainData::write_csv` streams to
    ///   the binning, hashed through [`fit_cache::DigestWriter`] without materialising. The RENDER,
    ///   not the raw f64 bits, on purpose: a formatter change changes what the child reads, and
    ///   must re-key.
    /// * **the grid point, the seed, the categorical declaration and every non-searched
    ///   parameter** — the two RENDER parts: the `Task::Train` and `Task::SaveBinary(MatchFit)`
    ///   configs exactly as a fit renders them, with the two PATH lines normalised to placeholders
    ///   (paths differ per run and do not reach the trees — the matrix test is that proof). This
    ///   also covers the [`BinPreFilter`] CHOICE, which is dataset-baked.
    /// * **the trainer binary** — the PROVENANCE part, read at construction. Residual inherited
    ///   from the pin check (`crates/vike-ml/src/train/cli.rs`'s module doc): a hand-swapped binary under
    ///   a stale PROVENANCE defeats both the pin and this key.
    /// * **the model TEXT FORMAT the parse expects** — `crate::MODEL_VERSION`.
    ///
    /// Deliberately excluded: the scratch paths (normalised) and the in-process inference/scorer
    /// code (the score key's declared residual, in [`crate::seam::fit_cache`]'s module doc).
    fn fit_identity(&self, d: &TrainData<'_>, p: &GridPoint, seed: u64) -> Option<[u8; 32]> {
        let provenance = self.provenance?;
        // The same refusals `fit` applies, in the same order — a shape or parameter `fit` would
        // reject gets no identity, so a rejection is never served from (or stored into) the cache.
        if check_shape(d).is_err() {
            return None;
        }
        let params = params_for(&self.base, p, seed, d.categorical).ok()?;
        let train_render =
            train_config(&params, Path::new("DATA"), Path::new("MODEL")).render().ok()?;
        let bin_render = TrainConfig {
            task: Task::SaveBinary(BinPreFilter::MatchFit),
            params: &params,
            data: Path::new("DATA"),
            output_model: None,
            input_model: None,
            output_result: None,
            predict_raw_score: false,
        }
        .render()
        .ok()?;
        let mut csv = fit_cache::DigestWriter::new();
        d.write_csv(&mut csv).ok()?;

        let mut t = fit_cache::Transcript::new("vike-research fit-identity v1");
        t.digest_part(provenance);
        t.part(crate::MODEL_VERSION.as_bytes());
        t.part(train_render.as_bytes());
        t.part(bin_render.as_bytes());
        t.digest_part(csv.finish());
        Some(t.finish())
    }

    /// The winner-refit capture: the same fit, keeping the model TEXT long enough to answer
    /// `want` — its `split_feature=`/`split_gain=` arrays folded into a [`FitImportance`]
    /// (`crates/vike-ml/src/model/importance.rs`'s `importance_from_model_text`), and the text ITSELF.
    ///
    /// ⚠ The text is the trainer's own bytes, never a re-serialisation of the parsed
    /// [`GbdtModel`] (there is no model WRITER). The scratch `model.txt` is deleted with the fit
    /// directory, so the `String` is the artifact that survives and has to be handed UP.
    ///
    /// Spelled BESIDE [`Learner::fit`] rather than folded into it, so the per-trial hot path stays
    /// untouched; the two share every real step through the same helpers and differ only in the
    /// scratch-directory choreography (same lifecycle, removed on both outcomes).
    ///
    /// Each half of `want` is answered on its own: a text-only request cannot be FAILED by an
    /// importance parse, and an importance-only one does not carry a megabyte of text up.
    fn fit_captured(
        &self,
        d: &TrainData<'_>,
        p: &GridPoint,
        seed: u64,
        want: Capture,
    ) -> Result<CapturedFit<Self::Model>, String> {
        check_shape(d)?;
        let params = params_for(&self.base, p, seed, d.categorical)?;
        let bin = binned_for(self, d, &params)?;
        let dir = self.fit_dir();
        std::fs::create_dir_all(&dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
        let model_path = dir.join("model.txt");
        let text = self.cli.train(&train_config(&params, &bin.bin, &model_path), &dir);
        let _ = std::fs::remove_dir_all(&dir);
        let text = text.map_err(|e| e.to_string())?;
        let model = parse_model_text(&text).map_err(|e| e.to_string())?;
        // The type is named rather than inferred, so `FitImportance` is a real use of the import
        // and the doc link above cannot rot into an unused one.
        let importance: Option<FitImportance> =
            want.importance.then(|| importance_from_model_text(&text)).transpose()?;
        Ok((model, importance, want.text.then_some(text)))
    }
}

#[path = "gbdt_learner_tests.rs"]
#[cfg(test)]
mod gbdt_learner_tests;
