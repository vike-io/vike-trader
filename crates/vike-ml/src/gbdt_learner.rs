//! ⚠ **THE ONE [`Learner`] THAT DRIVES A REAL TRAINER.** Everything else that implements the seam
//! in this workspace is a double.
//!
//! [`crate::learner`] states the seam — fit a [`TrainData`] at a [`GridPoint`], read one
//! probability per row. This file is the impl that spawns a child process to do it, and it is
//! where everything the trainer insists on stops: a verified binary, per-fit scratch
//! directories, a parameter bag, a binned-dataset cache and an error enum.
//!
//! # Where it came from, and what the move changed
//!
//! It was the research crate's own `studies/cohort/ml_adapter.rs` — a STUDY's file, and the only
//! working `Learner` impl in the tree. `docs/superpowers/specs/2026-08-24-research-engine-user-split-design.md`'s
//! Phase 1 named it a promotion into this crate, with two surgical conditions, and its Phase 6
//! stated the deadline: *"Promotion is cheap while both trees are in one workspace and a single
//! `cargo check` proves the move; it is expensive the day after the split… Do it before the
//! departure, or do not do it at all."* The study has now departed to `user_data/research/`, and
//! this file is what stayed.
//!
//! **The two conditions, both honoured, both visible in the constructor:**
//!
//! * the base parameter bag is a CONSTRUCTOR PARAMETER ([`GbdtLearner::new`]'s `base`), so no
//!   caller's regularisation is compiled into this crate — see [`params_for`];
//! * the bin cache's capacity is a fixed [`DEFAULT_BIN_CAPACITY`] with a
//!   [`GbdtLearner::with_bin_capacity`] override, rather than a function of `rayon`'s pool — so
//!   promoting it added NO dependency to a crate whose value is that it is feature-free.
//!
//! Nothing else moved. The choreography, the cache, the identity digest and every test below are
//! the study's, unchanged — which is why [`Learner::fit_identity`]'s transcript tag still reads
//! `vike-research fit-identity v1`: it is a CACHE KEY, not a citation, and renaming it would
//! invalidate every fit cached under it for no correctness reason at all. ⚠ The crate that minted
//! that spelling no longer exists — `vike-research` was dissolved and deleted outright — and the
//! token is FROZEN anyway. A name that no longer names anything is precisely what a content-address
//! schema tag is allowed to be; the day it changes must be a day the fit identity's CONTENT
//! changed, never a tidy-up.
//!
//! ⚠ **One consequence to keep in view: the two `TrainData`s merged.** The study's had
//! `categorical`, an explicit row count and `split_at_row`; the library's had a validating
//! constructor and the CSV writer. They met HERE, under an alias, and every fit rebuilt the
//! labels into a fresh `Vec<f32>` to cross between them. There is one type now, the labels are
//! `f32` (the trainer's own width, packed once by the caller), and the shape check this
//! file used to own is `crates/vike-ml/src/train.rs`'s `TrainData::validate`.
//!
//! # The four questions this file exists to answer
//!
//! **(a) Where the NON-searched parameters live.** In the CALLER, handed over once at construction
//! and applied by [`params_for`]. Five parameters
//! (`num_leaves`, `max_depth`, `min_data_in_leaf`, `learning_rate`, `feature_fraction`) are the
//! searched axes and arrive on every [`Learner::fit`] call inside a [`GridPoint`], so a base bag
//! must NOT restate them — a base copy of a searched axis is dead state whose only effect would be
//! to hand a DROPPED mapping a plausible value to fail silently with, and
//! `no_searched_axis_is_also_pinned_in_the_base_where_it_could_mask_a_dropped_mapping` is that
//! rule as a test. Everything else — `objective`, `verbosity`, `num_iterations`, the two
//! `lambda_*`, the two `bagging_*` — is the caller's to state, over
//! `crates/vike-ml/src/params.rs`'s `GbdtParams::default`.
//!
//! **(b) How a categorical column is declared.** `TrainData::categorical` carries FEATURE column
//! indices (the label column is not counted, which is LightGBM's own convention); they become a
//! COMMA-SEPARATED string through `crates/vike-ml/src/params.rs`'s `categorical_feature_csv`. It
//! must be a string and never a list: LightGBM's config parser strips quotes but not brackets, so
//! `[0,3]` names no column at all, the column is binned as a NUMBER, nothing errors, and the saved
//! model's parameter echo still claims the column was categorical. An empty `categorical` declares
//! NOTHING (`None`), not an empty list. An index outside the row width is refused before anything
//! spawns — `crates/vike-ml/src/train.rs`'s `TrainData::validate` does it, reached through
//! [`check_shape`] — because LightGBM ignores an out-of-range index in silence, which is the same
//! silent outcome.
//!
//! **(c) `fit` and the model are `Sync`.** `GbdtLearner` is a verified binary path, a scratch root
//! and a counter; [`GbdtModel`] is `{usize, Objective, Vec<String>, Vec<Tree>}`, plain data
//! throughout. Neither bound the seam declares had to be relaxed and the grid search stays parallel —
//! `the_learner_and_its_model_satisfy_the_bounds_the_parallel_search_needs` asserts it at compile
//! time rather than leaving it to a `rayon` type error in another module, on `GbdtLearner` written
//! with no type argument, i.e. on the [`LightGbmCli`] instantiation a real caller builds.
//! ([`Trainer`] is `Sync` for the same reason and says so, so a trainer that is not cannot be
//! substituted into a learner the search then tries to share.) Each `fit` gets its OWN scratch
//! directory ([`GbdtLearner::fit_dir`]) for exactly this reason: the parallelism is real, and two
//! concurrent fits sharing one config path would each train the other's parameters.
//!
//! **(d) The error type is `String`.** [`MlError`] is mapped through `Display` at every
//! boundary. The seam asked for `Result<_, String>` because a point the learner rejects must not
//! abort the other 242, and a rejection's whole value is the sentence — `MlError::Backend` already
//! carries LightGBM's own stderr tail inside it.
//!
//! # Two divergences from the oracle, stated rather than discovered later
//!
//! * **`num_threads = 1`.** The oracle's `lgb.LGBMClassifier` uses every core. The pinned binary is
//!   built `-DUSE_OPENMP=OFF` and `crates/vike-ml/src/grid.rs`'s `check_grid_params` refuses
//!   anything else, because thread count changes the float reduction order — a reproducible model
//!   is worth one core here. Fits are parallel across PROCESSES, not inside one.
//! * **A full 243-point grid, not 20 TPE trials.** That divergence belongs to
//!   the caller's own search module, not here; this file fits ONE point.
//!
//! # The handover, and what gates it
//!
//! Every property stated above is killed by a test in this file that runs in an ordinary
//! `cargo test`, and that now includes the one that used to be excluded: that [`Learner::fit`]
//! hands the trainer THE PARAMETERS IT JUST BUILT rather than some other [`GbdtParams`]. Replacing
//! `&params` with `&GbdtParams::default()` in `fit` used to leave every unit test below GREEN
//! (measured, not supposed) and redden only two `#[ignore]`d smokes against a binary CI does not
//! have — `two_fits_with_different_seeds_differ` and
//! `a_grid_point_changes_the_model_the_seam_hands_back` in
//! `crates/vike-ml/tests/gbdt_learner_smoke.rs`. It is the most consequential silent failure
//! available here: every point of a caller's grid
//! would train identically, the search would still finish and still name a winner, and the winner
//! would be noise.
//!
//! [`Trainer`] is what closed it, and it is deliberately the smallest thing that could: the calls
//! `fit` makes into `crates/vike-ml/src/cli.rs`'s [`LightGbmCli`] — `train`, joined by
//! `save_binary` when the per-fold binary reuse landed, because binning is now the step that BAKES
//! `min_data_in_leaf` and the categorical declaration into the dataset — each implemented for the
//! real driver as a straight delegation. `#[cfg(test)]`'s `RecordingTrainer` is then handed the
//! same [`TrainConfig`] a fit passes and keeps the [`GbdtParams`] inside it (for the binning too),
//! and `the_trainer_is_handed_the_parameters_the_fit_just_built` asserts every searched axis, the
//! seed and the categorical CSV at that boundary — each against its value AND against the library
//! default it would wear had some other bag been handed over.
//!
//! ⚠ **The whole-bag equality at the end of that test and the per-field assertions above it kill
//! DIFFERENT mutations, and neither half is decoration.** An axis dropped from `point_bag`
//! vanishes from BOTH sides of the equality at once, so only the literals see it; `&params`
//! replaced by `&GbdtParams::default()` moves every field at once, so the equality alone sees that.
//! Measured on the branch that added them, by mutating the test as well as the code: with the
//! per-field assertions deleted, a dropped `feature_fraction` leaves this test GREEN and the
//! default-bag substitution still reddens it; with the equality deleted instead, the dropped axis
//! reddens it. Do not simplify either half away.
//!
//! Two things about a fit are still not gated by a test that runs in CI. Both are LOUD, which is
//! the whole difference — this file's residuals are about failures that produce a plausible model:
//!
//! * **That LightGBM HONOURS what it is handed.** A recording double proves the handover and can
//!   prove nothing whatever about the child process. That is what the `#[ignore]`d smokes above and
//!   `crates/vike-ml/tests/train_infer_equality.rs` are for. ⚠ The real [`Trainer`] impl being a
//!   REAL delegation rather than a canned answer is NOT in this bucket and is gated:
//!   `fit_checks_the_shape_before_it_spawns_anything` reaches the spawn through `Trainer::train` on
//!   a [`LightGbmCli`] and asserts the error names the missing binary.
//! * **The two PATHS in the config** — the binary `binned_for` just produced, and the model file
//!   the fit names. A wrong one is an immediate open failure from the child, not a plausible
//!   model, so it fails the first time anybody runs a fit at all.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use crate::cli::LightGbmCli;
use crate::fit_cache;
use crate::grid::{bin_dataset, check_grid_params};
use crate::train::{BinPreFilter, Task, TrainConfig};
use crate::{
    Capture, CapturedFit, FitImportance, GbdtModel, GbdtParams, GridPoint, Learner, MlError,
    TrainData, importance_from_model_text, parse_model_text,
};

/// The parameters a fit starts from, before its [`GridPoint`] is applied.
///
/// ⚠ **A CONSTRUCTOR PARAMETER, not a constant, and that is the whole of what this promotion
/// changed.** The five fixed keys that used to live here — `num_iterations`, `lambda_l1`,
/// `lambda_l2`, `bagging_fraction`, `bagging_freq` — were one study's `config.py`, and a library
/// that baked them in would be a library with one caller's regularisation compiled into it. Every
/// caller now states its own bag; [`GbdtLearner::new`] uses [`GbdtParams::default`], which is
/// LightGBM's own defaults and therefore the answer that surprises nobody.
///
/// The two settings that bite when a caller assembles its own: `bagging_freq` defaults to `0`,
/// which DISABLES bagging however small `bagging_fraction` is — so those two are one setting
/// wearing two names and dropping either alone silently trains an unbagged model.
///
/// This function is `base` with the point applied, the seed pinned and the categorical columns
/// declared.
///
/// ⚠ The last step is [`check_grid_params`], and it belongs HERE because this is where a fit's
/// parameters are finalised — the 68x threading cliff has to be refused at the point the fits
/// happen, not at the point somebody writes a default. It has nothing to refuse today
/// (`GbdtParams::default` pins `num_threads = 1`, and no searched axis can reach the field at all
/// — `apply` refuses the NAME), which is the state a guard should be in; what it buys is that a
/// later edit to a caller's base bag, or to the library default it inherits, cannot turn a
/// 30-minute grid into a multi-week one in silence.
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

/// The shape refusal, in the error type this seam speaks.
///
/// ⚠ The CHECK itself is `crates/vike-ml/src/train.rs`'s `TrainData::validate` and no longer lives
/// here: it moved with the type it is about, when the study's `TrainData` and the library's merged
/// into one. What stays is the two things that are this file's own — running it BEFORE anything is
/// written or spawned, and rendering `MlError` through `Display`, because a point the learner
/// rejects must not abort the other 242 and the whole value of a rejection is its sentence.
fn check_shape(d: &TrainData<'_>) -> Result<(), String> {
    d.validate().map_err(|e| e.to_string())
}

/// The TWO calls a fit makes into the model crate's process driver — the train itself, and the
/// once-per-dataset binning the train reads from.
///
/// It exists for exactly one reason, and the reason is a test: without it, the lines that hand
/// [`train_config`]'s output (and the binning's parameter bag) over are unobservable from inside
/// this process, and replacing either with `GbdtParams::default()` is a change no `cargo test`
/// can see — see this module's "The handover" section. `save_binary` joined `train` when the
/// per-fold binary reuse landed, and for the same reason `train` existed: the binning is now the
/// step that BAKES `min_data_in_leaf` and the categorical declaration into the dataset, so an
/// unobserved handover there is exactly the silent-categorical failure class this crate drives a
/// process to avoid. Nothing else about `vike-ml` is abstracted here and nothing else should be:
/// the value of this seam is its thinness, and each method is one real call the fit makes.
///
/// `Sync` because [`Learner`] is — the grid search calls `fit` from `rayon::par_iter`, so whatever
/// sits in this slot is shared across threads. Stating it on the trait means a trainer that is not
/// `Sync` is refused where it is SUBSTITUTED rather than in a `rayon` type error somewhere else.
///
/// Crate-private on purpose: it is this file's own test seam, not API. That is also why the
/// learner's non-trait methods below are NOT in an `impl<T: Trainer>` block — a crate-private trait
/// in the bounds of an inherent impl on a `pub` type is `private_bounds`, which is `-D warnings`.
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

/// The real trainer: straight delegations, with nothing of their own to get wrong.
///
/// ⚠ `train` is spelled with the fully-qualified call rather than `self.train(conf, scratch)` — an
/// inherent method wins method resolution over a trait one, so the short form is this function
/// calling ITSELF, forever. (`crates/vike-ml/src/learner.rs`'s `ProbaModel` impl for `GbdtModel`
/// is written the way it is for the same hazard in the other direction.)
///
/// `save_binary` delegates to the model crate's `bin_dataset` with [`BinPreFilter::MatchFit`] —
/// NEVER `MinDataAgnostic`, whose bins measurably diverge from a CSV fit's trees whenever the
/// default pre-filter would have dropped a feature (the pre-filter shrinks the
/// `feature_fraction` sampling universe). The parity claim is gated against the real binary by
/// `the_binned_fit_matches_the_per_fit_csv_train_across_every_searched_axis_value` below.
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
/// The search hands the SAME [`TrainData`] to every trial of a fold, so within a fold the key is
/// stable and the fold's fits share one binary per `min_data_in_leaf` value. `min_data_in_leaf` is
/// in the key because it is DATASET-BAKED: under LightGBM's default `feature_pre_filter` it
/// decides which features survive binning, and a bin built at one value trains DIFFERENT trees at
/// another (measured on the real binary — see [`BinPreFilter`]'s doc and the matrix smoke below).
/// The other four searched axes are tree-growth parameters and deliberately absent.
///
/// `x_ptr`/lengths alone would be an ABA hazard — a freed fold's buffer address can be reused by a
/// later fold of identical size — so the key also carries a 64-bit content fingerprint of the
/// features and labels. A collision needs the same address, the same lengths AND the same
/// fingerprint over different bytes; with a handful of live keys per run that is astronomically
/// unlikely, and the fingerprint is the cheap insurance (a few ms on the fold sizes this study
/// trains), not a security boundary.
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
/// and does not need to be — see [`BinKey`]'s collision argument. Inline rather than a hasher
/// dependency: the workspace pins its dependency set, and eight lines do not buy a crate.
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
/// CSV the binning writes, so a value-wise fold would give two different datasets one key. It
/// packed `u8` labels eight-to-a-word until the study's labels became `f32` — the trainer's own
/// width — and the density is not worth a second spelling of the rule.
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
/// on the winner's binning instead of each writing their own, which matters because the TPE
/// startup batch arrives ten-at-once on a cold key.
///
/// ⚠ The `Err` arm is cached too: a binning failure (bad shape aside, that is a spawn or disk
/// failure) is answered to every later fit of the same key without retrying. That is the same
/// blast radius the per-fit CSV scheme had — a disk that cannot take the CSV fails every fit of
/// the fold individually — spelled as one failure instead of twenty-one.
#[derive(Default)]
struct BinSlot {
    once: OnceLock<Result<Arc<BinFile>, String>>,
}

/// The fold-keyed binning cache the `fit` doc always said this seam would grow.
///
/// Insertion-ordered with move-to-back on hit, evicting from the front past `capacity` — LRU over
/// a `Vec`, linear because `capacity` is small. Entries die in two steps: eviction drops the
/// cache's handle, and the directory is deleted when the last in-flight fit drops its own
/// ([`BinFile`]'s `Drop`). A fold's binaries therefore live for the fold (plus at most the LRU
/// tail) and are gone by the time the learner is — dropping the cache drops every entry.
/// How many binned datasets a [`GbdtLearner`] keeps resident by default.
///
/// ⚠ **A CONSTANT rather than a function of the thread pool, and the reason is a dependency.**
/// The study this cache was written for sized it at `(8 * rayon::current_num_threads()).max(64)`,
/// which is a fine number and would have made `rayon` a normal dependency of this crate — for a
/// cache bound, in a crate whose whole value is that it is feature-free and cheap to depend on. A
/// caller that knows its own pool width passes that number to
/// [`GbdtLearner::with_bin_capacity`] instead; an under-sized cache is a RE-BIN, never a wrong
/// answer, which is what makes a fixed floor a safe default rather than a silent behaviour change.
///
/// The floor itself is the old expression's own `.max(64)`: a live fold holds at most four keys
/// (one bin per `min_data_in_leaf` value its trials reach, plus the final whole-fold refit), so 64
/// keeps sixteen concurrent folds resident with slack.
pub const DEFAULT_BIN_CAPACITY: usize = 64;

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

/// The study's [`Learner`]: gradient-boosted trees, fitted by the pinned LightGBM binary.
///
/// ⚠ The plan sketched `new()`. The merged model crate takes the trainer binary as a PARAMETER and
/// reads no environment variable to find one (`crates/vike-ops/tests/settings_secrets/settings_registry.rs`'s
/// `LIBRARY_PIN` is why), and it needs somewhere to put a fit's scratch files — so the real
/// constructor takes both. A learner that could not name its binary would be a learner that had to
/// go looking for one.
///
/// `T` defaults to [`LightGbmCli`], so every construction site, every signature and every
/// `GbdtLearner` written with no type argument means the real learner and reads as it did before
/// the parameter existed. The only thing that ever names another `T` is this file's own
/// `RecordingTrainer`.
pub struct GbdtLearner<T = LightGbmCli> {
    cli: T,
    scratch: PathBuf,
    /// Makes each fit's scratch directory unique. `fit` is called from `rayon::par_iter`, and one
    /// shared config path across concurrent fits is two fits training each other's parameters.
    next: AtomicU64,
    /// The per-dataset binaries the fits of a fold share — see [`BinCache`].
    bins: BinCache,
    /// The parameters every fit starts from, before its [`GridPoint`] is applied — see
    /// [`params_for`]. A CONSTRUCTOR parameter rather than a constant, so this crate carries no
    /// caller's regularisation.
    base: GbdtParams,
    /// SHA-256 of the trainer binary's `PROVENANCE` file bytes, read ONCE at construction — the
    /// trainer's contribution to [`Learner::fit_identity`]. `None` means the identity cannot be
    /// proven and the cross-run fit cache never engages, which is the safe direction (a refit,
    /// never a wrong answer); the test constructors below use exactly that.
    provenance: Option<[u8; 32]>,
}

impl GbdtLearner<LightGbmCli> {
    /// `binary` is the pinned LightGBM CLI (`just lightgbm-build` writes one at
    /// `bin/lightgbm/lightgbm`); `scratch` is a directory this learner may create per-fit
    /// subdirectories under, and it should be on the NVMe; `base` is the parameter bag every fit
    /// starts from, before its [`GridPoint`] is applied ([`GbdtParams::default`] is LightGBM's own
    /// defaults, and is what a caller with no opinion should pass).
    ///
    /// The binary's provenance is checked here, ONCE, rather than per fit: a wrong release parses,
    /// predicts and is wrong in ways only `crates/vike-ml/tests/train_infer_equality.rs` would see.
    ///
    /// The bin cache is sized at [`DEFAULT_BIN_CAPACITY`]; [`Self::with_bin_capacity`] is how a
    /// caller that knows its own parallelism widens it.
    pub fn new(binary: &Path, scratch: &Path, base: GbdtParams) -> Result<Self, String> {
        let cli = LightGbmCli::new(binary).map_err(|e| e.to_string())?;
        std::fs::create_dir_all(scratch)
            .map_err(|e| format!("creating the scratch root {}: {e}", scratch.display()))?;
        // `LightGbmCli::new` just verified the file exists and records the pinned tag, so this
        // read succeeds in every ordinary construction; a race that removes it between the two
        // reads degrades to `None` — an uncached learner — rather than an error.
        let provenance = std::fs::read(crate::cli::provenance_path(binary))
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
    /// Resize the binned-dataset cache — see [`DEFAULT_BIN_CAPACITY`] for why the default is a
    /// fixed floor rather than a function of the caller's thread pool.
    ///
    /// ⚠ Capacity is a PERFORMANCE knob and nothing else: an evicted entry is re-binned, so no
    /// value of it can change a fitted model. A `0` is legal and means "bin every fit".
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
/// A free function rather than a method for the reason [`Trainer`]'s doc gives: an inherent
/// `impl<T: Trainer> GbdtLearner<T>` block would put a crate-private trait in the bounds of a `pub`
/// type's impl, which `private_bounds` refuses under `-D warnings`. A trait impl may carry the
/// bound; an inherent one may not.
fn bin_once<T: Trainer>(
    l: &GbdtLearner<T>,
    d: &TrainData<'_>,
    params: &GbdtParams,
) -> Result<Arc<BinFile>, String> {
    // ⚠ `d` goes STRAIGHT to the binning. Until the two `TrainData` types merged this line
    // rebuilt the labels into a fresh `Vec<f32>` and wrapped them in the library's own shape —
    // per fit, on the hot path — because the study's labels were `u8` and the trainer's are not.
    // `pack` now writes them in the trainer's width once.
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
    /// data at the same `min_data_in_leaf` — the fold-keyed binning cache this doc used to
    /// prophesy ("if the grid ever gets slow enough to matter"), keyed on the data's CONTENT
    /// because the seam hands `fit` no fold identity. A fold's 20 search trials therefore write
    /// and parse the ~8 MB training CSV at most three times (once per `min_data_in_leaf` value
    /// reached) instead of twenty, and every later fit reads a ~1 MB binary.
    ///
    /// The models are BIT-IDENTICAL to per-fit CSV construction — not assumed: binning bakes
    /// `min_data_in_leaf` (via LightGBM's default `feature_pre_filter`) and the categorical
    /// declaration, which is why both are in [`BinKey`]; the matrix smoke below proves
    /// byte-identical model text against a per-fit CSV train for all three values of every
    /// searched axis, and `crates/vike-ml/src/train.rs`'s `BinPreFilter` carries the measured
    /// divergence of the two schemes that DON'T hold it.
    fn fit(&self, d: &TrainData<'_>, p: &GridPoint, seed: u64) -> Result<Self::Model, String> {
        check_shape(d)?;
        let params = params_for(&self.base, p, seed, d.categorical)?;
        // Held for the whole fit: an LRU eviction drops only the cache's handle, so the binary
        // cannot be deleted out from under the child process reading it.
        let bin = binned_for(self, d, &params)?;
        let dir = self.fit_dir();
        std::fs::create_dir_all(&dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
        let out = fit_in(&self.cli, &dir, &bin.bin, &params);
        // Removed on BOTH outcomes: a rejected grid point is an ordinary event here (242 usable
        // points still beat 20 TPE trials), and 243 leaked directories per fold is a worse
        // diagnostic than the error string, which already carries LightGBM's own stderr tail.
        let _ = std::fs::remove_dir_all(&dir);
        out
    }

    /// The cross-run fit cache's train half: a digest over THE ACTUAL BYTES [`Self::fit`] would
    /// consume for `(d, p, seed)` — content-addressed, never a description
    /// (see [`crate::fit_cache`]'s module doc for the constraint this serves).
    ///
    /// # The enumeration — every input of a fit, and which part covers it
    ///
    /// A fit is: bin `d` at `params` ([`bin_once`] → `write_csv` → the child's `save_binary`),
    /// then train from the binary ([`fit_in`] → [`train_config`] → the child), then parse the
    /// model text. Its inputs, exhaustively:
    ///
    /// * **the training data** (`d.x`, `d.y`, `d.n_rows`, `d.n_cols`) — covered by the CSV part:
    ///   the EXACT bytes `TrainData::write_csv` streams to the binning (label first, features,
    ///   `nan` spelling, shortest-roundtrip f64 formatting), hashed through [`fit_cache::DigestWriter`]
    ///   without materialising. Keying the RENDER rather than the raw f64 bits is deliberate: a
    ///   formatter change in the model crate changes what the child reads, and must re-key.
    /// * **the grid point, the seed, the categorical declaration and every non-searched
    ///   parameter** — covered by the two RENDER parts: the `Task::Train` config and the
    ///   `Task::SaveBinary(MatchFit)` config, rendered exactly as a fit renders them but with the
    ///   two PATH lines normalised to placeholders (paths differ per run and provably do not
    ///   reach the trees — the matrix smoke below is that proof). Between them these carry all
    ///   five axes, `seed`, `categorical_feature`, `num_iterations`/`lambda_l1`/`lambda_l2`/
    ///   `bagging_*`, `objective`, `num_threads`, `deterministic`, `force_row_wise`,
    ///   `precise_float_parser` — and the [`BinPreFilter`] CHOICE, which is dataset-baked and
    ///   whose `MatchFit`-vs-`MinDataAgnostic` difference trains measurably different models.
    /// * **the trainer binary** — covered by the PROVENANCE part: the digest of the `PROVENANCE`
    ///   file bytes read at construction. A residual inherited from the pin check itself
    ///   (`crates/vike-ml/src/cli.rs`'s module doc): the file is the build recipe's self-report,
    ///   so a hand-swapped binary under a stale PROVENANCE defeats both the pin and this key.
    /// * **the model TEXT FORMAT the parse expects** — `crate::MODEL_VERSION`, one part.
    ///
    /// NOT inputs, deliberately excluded: the scratch paths (normalised, see above) and the
    /// in-process inference/scorer code (the score key's declared residual — the cache module doc
    /// carries it, with the schema tag to bump).
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
    /// whatever `want` asked for — its `split_feature=`/`split_gain=` arrays folded into a
    /// [`FitImportance`] (`crates/vike-ml/src/importance.rs`'s `importance_from_model_text`
    /// carries the verified format — it parses vike-ml's own `save_model` text and lives with
    /// it), and the text ITSELF for `--model-out`.
    ///
    /// ⚠ The text handed back is the trainer's own bytes, never a re-serialisation of the parsed
    /// [`GbdtModel`]: this workspace has no model WRITER, and adding one would be a second
    /// spelling of the format to keep in step with LightGBM's. The scratch `model.txt` the child
    /// wrote is deleted below with the rest of the fit directory — the `String` is the artifact
    /// that survives, which is exactly why it has to be handed UP rather than left on disk.
    ///
    /// ⚠ The body is deliberately spelled BESIDE [`Learner::fit`] rather than folded into it:
    /// `fit` is the per-trial hot path (244 calls per fold) and its orchestration is left
    /// byte-untouched, while this method runs once per fold and only under a capture flag.
    /// The two share every real step through the same helpers (`check_shape`, `params_for`,
    /// `binned_for`, `train_config`, `parse_model_text`), so the only duplicated lines are the
    /// scratch-directory choreography — same lifecycle, removed on both outcomes.
    ///
    /// Each half of `want` is answered on its own: a `--model-out` run pays no importance parse
    /// and — the part that matters — cannot be FAILED by one, while an `--importance-out` run
    /// drops the text instead of carrying a megabyte per fold up to a caller that keeps none.
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
