//! [`NeuralLearner`] — the learner seam over the network trainer PROCESS `vike-ml-neural-training`, driven
//! the way [`crate::GbdtLearner`] drives LightGBM: a separate binary, a per-fit scratch directory, files
//! in and files out. The trainer links `burn`; this module links nothing of it, so a host that fits a
//! network through the seam carries no `burn` and no ONNX runtime
//! (`docs/decisions/0292-neural-networks-train-research-side-on-burn-and-nothing-that-trades-links-the-trainer.md`).
//!
//! * **The network is the HOST's configuration** (`spec`, the trainer's TOML: which network, its
//!   widths, the loss and the epochs). The seam's [`GridPoint`] carries a TREE search's axes; a network
//!   has no leaves, so only its `learning_rate` applies (Adam's step) and a point that moves any of the
//!   other four from [`DEFAULT_POINT`] is REFUSED, an ordinary `Err` per the seam's rule, so a tree grid
//!   cannot silently search a network.
//! * **A fit** writes the data ([`TrainData::write_matrix`]: raw little-endian `f32`, no text) and
//!   the spec into its own scratch directory and runs `vike-ml-neural-training train`; the seed is the
//!   run's.
//! * **The model** ([`NeuralModel`]) is a `vike-ml-neural-training predict` child holding the exported ONNX:
//!   one row in, one answer out (one number, or one probability per class for a `multiclass` network,
//!   as its `ready` line declares), over its pipes; a batch (`Predictor::predict_batch`) streams its
//!   rows and reads the answers back, one stream instead of a round trip per row. The rows and the answers
//!   cross as `crate::train::matrix`'s predict frames (binary `f32`, the network's own number) when the
//!   child's `ready` line offers them (`matrix::PREDICT_WORD`), as comma-separated text lines to an older
//!   trainer: the trainer is a binary of its own, which a rollback of the host alone leaves newer. It is not `Text`:
//!   the ONNX runtime that could read it in-process is fenced out of the multicall binary, so the
//!   host HOLDS the model and answers rows.
//! * **[`Capture::text`]** is the STANDARD base64 of the trainer's own `model.onnx` bytes: the
//!   spelling a run's `model.onnx` crosses as text everywhere (`vike_runs::read_model_text`). No
//!   importance: a network reports none, and the seam leaves an absence to the caller.
//! * **No identity** ([`Learner::fit_identity`] stays `None`): the seed reproduces a network on one
//!   box only (the trainer's platform floats), which is not the bit-identity a fit cache needs.
//! * **A walk-forward chain** ([`Learner::fit_chained`]): with a spec that says `warm_start`, the
//!   trainer keeps each fit's weights, this learner keeps a chain's LAST fold's weights (a copy:
//!   the fit's directory goes with its model), and the chain's next fold starts from them with
//!   `--start-from`; the first fold of a chain starts from the seed. Only that fold's own weights
//!   are ever handed on: written by its trainer right after it trained on its own rows, which hold
//!   no row of any test week. A chain's folds run one at a time and strictly forward (each `step`
//!   after the last), refused otherwise; a failed fold (its trainer refused, a diverged network
//!   among them, or its model did not load) leaves the chain where it was, and its error names
//!   the chain and the step. Without
//!   `warm_start` the trainer keeps no weights, so every fold starts from the seed and a chained
//!   fit is the `fit_captured` it always was.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use vike_ml_prediction::{PredictionKind, Predictor};

use crate::seam::learner::{Capture, CapturedFit, Learner};
use crate::search::{DEFAULT_POINT, GridPoint};
use crate::train::TrainData;
use crate::train::matrix::{self, PREDICT_WORD};

/// The file the trainer writes the network to, in a fit's directory.
const MODEL_FILE: &str = "model.onnx";
/// Rows per predict frame of a batch: about 1 MiB of a 64-feature row's numbers, so the child answers one
/// frame while the next is written.
const ROWS_PER_PREDICT_FRAME: usize = 4096;
/// The fit's matrix the trainer reads (`crate::train::matrix`), in a fit's directory.
const DATA_FILE: &str = "data.matrix";
/// The trained network's weights, which the trainer writes beside it with a `warm_start` spec.
const WEIGHTS_FILE: &str = "weights";

/// The learner seam over `vike-ml-neural-training` (module doc).
#[derive(Debug)]
pub struct NeuralLearner {
    binary: PathBuf,
    scratch: PathBuf,
    spec: String,
    /// Makes each fit's directory unique: fits run from several threads at once.
    next: AtomicU64,
    /// Each walk-forward chain's last fold ([`Learner::fit_chained`]), by the chain's name.
    chains: Mutex<HashMap<String, Chain>>,
}

impl NeuralLearner {
    /// The trainer at `binary` (a host finds it in `<project>/bin/vike-ml-neural-training/`), per-fit
    /// directories under `scratch`, and the network `spec` (the trainer's TOML) every fit trains.
    pub fn new(binary: &Path, scratch: &Path, spec: &str) -> Result<Self, String> {
        if !binary.is_file() {
            return Err(format!("no network trainer at {}", binary.display()));
        }
        std::fs::create_dir_all(scratch)
            .map_err(|e| format!("creating the scratch root {}: {e}", scratch.display()))?;
        Ok(NeuralLearner {
            binary: binary.to_path_buf(),
            scratch: scratch.to_path_buf(),
            spec: spec.to_string(),
            next: AtomicU64::new(0),
            chains: Mutex::default(),
        })
    }

    /// A unique path under the scratch root, `<what>-<pid>-<n>`.
    fn fresh(&self, what: &str) -> PathBuf {
        let n = self.next.fetch_add(1, Ordering::Relaxed);
        self.scratch.join(format!("{what}-{}-{n}", std::process::id()))
    }

    /// Trains one network into its own directory, which holds `model.onnx` and `PROVENANCE` after
    /// (and `weights` with a `warm_start` spec), from the weights file `start` when one is given.
    fn train(
        &self,
        d: &TrainData<'_>,
        p: &GridPoint,
        seed: u64,
        start: Option<&Path>,
    ) -> Result<PathBuf, String> {
        if *p != (GridPoint { learning_rate: p.learning_rate, ..DEFAULT_POINT }) {
            return Err(format!(
                "a network has no leaves: only `learning_rate` of {p:?} applies, so the point must \
                 keep DEFAULT_POINT's tree axes"
            ));
        }
        let dir = self.fresh("network");
        let made = |e: std::io::Error| format!("{}: {e}", dir.display());
        std::fs::create_dir_all(&dir).map_err(made)?;
        let written = std::fs::File::create(dir.join(DATA_FILE)).map_err(made).and_then(|file| {
            let mut data = std::io::BufWriter::new(file);
            d.write_matrix(&mut data).map_err(|e| e.to_string())?;
            data.flush().map_err(made)?;
            std::fs::write(dir.join("spec.toml"), &self.spec).map_err(made)
        });
        if let Err(e) = written {
            let _ = std::fs::remove_dir_all(&dir);
            return Err(e);
        }
        let mut command = Command::new(&self.binary);
        command
            .arg("train")
            .arg("--data")
            .arg(dir.join(DATA_FILE))
            .arg("--spec")
            .arg(dir.join("spec.toml"))
            .arg("--seed")
            .arg(seed.to_string())
            .arg("--learning-rate")
            .arg(p.learning_rate.to_string())
            .arg("--out")
            .arg(&dir);
        if let Some(start) = start {
            command.arg("--start-from").arg(start);
        }
        let out = command.output().map_err(|e| format!("spawn {}: {e}", self.binary.display()))?;
        if !out.status.success() {
            let _ = std::fs::remove_dir_all(&dir);
            let err = String::from_utf8_lossy(&out.stderr);
            let last: Vec<&str> = err.lines().rev().take(10).collect();
            return Err(format!(
                "{} exited {}: {}",
                self.binary.display(),
                out.status,
                last.join("\n")
            ));
        }
        Ok(dir)
    }

    /// The trained network in `dir` as the seam hands it back: its held model, and its text when
    /// `want` asks.
    fn captured(&self, dir: PathBuf, want: Capture) -> Result<CapturedFit<NeuralModel>, String> {
        let mut text = None;
        if want.text {
            match std::fs::read(dir.join(MODEL_FILE)) {
                Ok(bytes) => text = Some(STANDARD.encode(bytes)),
                Err(e) => {
                    let _ = std::fs::remove_dir_all(&dir);
                    return Err(format!("{}: {e}", dir.join(MODEL_FILE).display()));
                }
            }
        }
        Ok((NeuralModel::start(&self.binary, dir)?, None, text))
    }
}

impl Learner for NeuralLearner {
    type Model = NeuralModel;

    fn fit(&self, d: &TrainData<'_>, p: &GridPoint, seed: u64) -> Result<NeuralModel, String> {
        Ok(self.fit_captured(d, p, seed, Capture::default())?.0)
    }

    fn fit_captured(
        &self,
        d: &TrainData<'_>,
        p: &GridPoint,
        seed: u64,
        want: Capture,
    ) -> Result<CapturedFit<NeuralModel>, String> {
        self.captured(self.train(d, p, seed, None)?, want)
    }

    /// The chain's next fold (module doc): from the weights its last fold kept, if any, and never
    /// while another fold of the chain trains or for a step that is not after the last one.
    fn fit_chained(
        &self,
        d: &TrainData<'_>,
        p: &GridPoint,
        seed: u64,
        want: Capture,
        chain: &str,
        step: i64,
    ) -> Result<CapturedFit<NeuralModel>, String> {
        let start = {
            let mut chains = self.chains.lock().unwrap_or_else(PoisonError::into_inner);
            let link = chains.entry(chain.to_string()).or_default();
            if link.training {
                return Err(format!(
                    "chain `{chain}`: its previous fold is still training; a chain's folds are \
                     fitted one at a time, in time order"
                ));
            }
            if let Some((last, _)) = &link.last
                && step <= *last
            {
                return Err(format!(
                    "chain `{chain}`: step {step} is not after its last fold's {last}; a chain runs \
                     strictly forward in time"
                ));
            }
            link.training = true;
            link.last.as_ref().and_then(|(_, weights)| weights.clone())
        };
        let trained = self.train(d, p, seed, start.as_deref()).and_then(|dir| {
            let weights = dir.join(WEIGHTS_FILE);
            if !weights.is_file() {
                return Ok((dir, None));
            }
            let kept = self.fresh("chain").with_extension("weights");
            match std::fs::copy(&weights, &kept) {
                Ok(_) => Ok((dir, Some(kept))),
                Err(e) => {
                    let _ = std::fs::remove_dir_all(&dir);
                    Err(format!("keeping {}: {e}", weights.display()))
                }
            }
        });
        // The fold counts only once its model is held: a fold whose model does not load leaves the
        // chain where it was, and its kept weights go.
        let held = trained.and_then(|(dir, kept)| match self.captured(dir, want) {
            Ok(fit) => Ok((fit, kept)),
            Err(e) => {
                if let Some(kept) = kept {
                    let _ = std::fs::remove_file(kept);
                }
                Err(e)
            }
        });
        let mut chains = self.chains.lock().unwrap_or_else(PoisonError::into_inner);
        let link = chains.entry(chain.to_string()).or_default();
        link.training = false;
        let (fit, kept) = held.map_err(|e| format!("chain `{chain}`, step {step}: {e}"))?;
        if let Some((_, Some(older))) = link.last.replace((step, kept)) {
            let _ = std::fs::remove_file(older);
        }
        Ok(fit)
    }
}

impl Drop for NeuralLearner {
    fn drop(&mut self) {
        let chains = self.chains.get_mut().unwrap_or_else(PoisonError::into_inner);
        for (_, link) in chains.drain() {
            if let Some((_, Some(weights))) = link.last {
                let _ = std::fs::remove_file(weights);
            }
        }
    }
}

/// A walk-forward chain's state in a [`NeuralLearner`].
#[derive(Debug, Default)]
struct Chain {
    /// The last fold fitted: its step, and the weights its fit kept (none without `warm_start`).
    last: Option<(i64, Option<PathBuf>)>,
    /// A fold of this chain is training now.
    training: bool,
}

/// A trained network held by a `vike-ml-neural-training predict` child (module doc). Dropping it stops the
/// child and deletes the fit's directory.
#[derive(Debug)]
pub struct NeuralModel {
    pipes: Mutex<Pipes>,
    kind: PredictionKind,
    columns: usize,
    /// The child reads predict frames (its `ready` line said [`PREDICT_WORD`]); text lines otherwise.
    frames: bool,
    dir: PathBuf,
}

#[derive(Debug)]
struct Pipes {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
}

impl NeuralModel {
    /// Starts the predicting child on the network in `dir` and reads its `ready` line.
    fn start(binary: &Path, dir: PathBuf) -> Result<NeuralModel, String> {
        let spawned = Command::new(binary)
            .arg("predict")
            .arg("--model")
            .arg(&dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();
        let mut child = match spawned {
            Ok(child) => child,
            Err(e) => {
                let _ = std::fs::remove_dir_all(&dir);
                return Err(format!("spawn {}: {e}", binary.display()));
            }
        };
        let (Some(input), Some(output)) = (child.stdin.take(), child.stdout.take()) else {
            unreachable!("both pipes were asked for");
        };
        let mut output = BufReader::new(output);
        let mut ready = String::new();
        let _ = output.read_line(&mut ready);
        let words = |key: &str| {
            ready.split_whitespace().find_map(|word| word.strip_prefix(key)?.strip_prefix('='))
        };
        let outputs = words("outputs").and_then(|n| n.parse::<usize>().ok());
        let kind = match (words("kind"), outputs) {
            (Some("probability"), Some(1)) => Some(PredictionKind::Probability),
            (Some("value"), Some(1)) => Some(PredictionKind::Value),
            (Some("classes"), Some(class_count)) if class_count >= 2 => {
                Some(PredictionKind::ClassProbabilities { class_count })
            }
            _ => None,
        };
        let columns = words("columns").and_then(|n| n.parse::<usize>().ok());
        let model = NeuralModel {
            pipes: Mutex::new(Pipes { child, input, output }),
            kind: kind.unwrap_or(PredictionKind::Value),
            columns: columns.unwrap_or(0),
            frames: ready.split_whitespace().any(|word| word == PREDICT_WORD),
            dir,
        };
        if !ready.starts_with("ready ") || kind.is_none() || columns.is_none() {
            let mut pipes = model.pipes.lock().unwrap_or_else(PoisonError::into_inner);
            let mut why = String::new();
            if let Some(mut stderr) = pipes.child.stderr.take() {
                let _ = std::io::Read::read_to_string(&mut stderr, &mut why);
            }
            drop(pipes);
            return Err(format!("the network did not load: {} {}", ready.trim(), why.trim()));
        }
        Ok(model)
    }

    /// A row width or an answer room the network does not take, refused before the child is asked.
    fn check(&self, width: usize, room: usize) -> Result<(), String> {
        let outputs = self.kind.output_count();
        if width != self.columns || room != outputs {
            return Err(format!(
                "the network takes {} features and answers {outputs} number(s); got {width} and room for {room}",
                self.columns
            ));
        }
        Ok(())
    }
}

impl Predictor for NeuralModel {
    fn kind(&self) -> PredictionKind {
        self.kind
    }

    fn predict_into(&self, row: &[f64], out: &mut [f64]) -> Result<(), String> {
        self.check(row.len(), out.len())?;
        let mut pipes = self.pipes.lock().unwrap_or_else(PoisonError::into_inner);
        let sent = if self.frames {
            pipes.input.write_all(&matrix::predict_frame(row, row.len()))
        } else {
            writeln!(pipes.input, "{}", line_of(row))
        };
        sent.and_then(|()| pipes.input.flush())
            .map_err(|e| format!("the network's process: {e}"))?;
        read_scored(&mut pipes.output, out).map_err(|(why, _)| why)
    }

    /// Every row streamed to the child while its answers are read back, instead of one round trip
    /// per row: a writer thread feeds stdin, so neither pipe fills while the other side waits on it
    /// (frames of [`ROWS_PER_PREDICT_FRAME`] rows, or one line a row to an older trainer). The child still
    /// runs each row alone, so each answer is the bits `predict_into` answers; every answer is read before
    /// the first row's refusal is answered, so the pipes stay in step.
    fn predict_batch(&self, flat_x: &[f64], n_features: usize) -> Result<Vec<f64>, String> {
        if n_features == 0 || !flat_x.len().is_multiple_of(n_features) {
            return Err(format!("{} numbers are not whole rows of {n_features}", flat_x.len()));
        }
        let outputs = self.kind.output_count();
        self.check(n_features, outputs)?;
        let mut out = vec![0.0; flat_x.len() / n_features * outputs];
        let mut pipes = self.pipes.lock().unwrap_or_else(PoisonError::into_inner);
        let Pipes { input, output, .. } = &mut *pipes;
        let (frames, mut refusal) = (self.frames, None);
        let written = std::thread::scope(|scope| {
            let writer = scope.spawn(move || {
                let mut input = std::io::BufWriter::new(input);
                if frames {
                    for rows in flat_x.chunks(ROWS_PER_PREDICT_FRAME * n_features) {
                        input.write_all(&matrix::predict_frame(rows, n_features))?;
                    }
                } else {
                    for row in flat_x.chunks_exact(n_features) {
                        writeln!(input, "{}", line_of(row))?;
                    }
                }
                input.flush()
            });
            let per_answer = if frames { ROWS_PER_PREDICT_FRAME * outputs } else { outputs };
            for slots in out.chunks_mut(per_answer) {
                if let Err((why, gone)) = read_scored(output, slots) {
                    refusal.get_or_insert(why);
                    if gone {
                        break; // no answer follows, and the writer meets a closed pipe
                    }
                }
            }
            writer.join()
        });
        if let Some(refusal) = refusal {
            return Err(refusal);
        }
        match written {
            Ok(Ok(())) => Ok(out),
            Ok(Err(e)) => Err(format!("the network's process: {e}")),
            Err(_) => Err("the network's row writer panicked".to_string()),
        }
    }
}

/// One row as the child reads it: each number's shortest spelling, comma-separated.
fn line_of(row: &[f64]) -> String {
    let line: Vec<String> = row.iter().map(f64::to_string).collect();
    line.join(",")
}

/// The child's next answer, written into `slots`: an answer frame of exactly `slots.len()` numbers, or a
/// line (its numbers, or its `error <reason>` as the refusal). The `Err`'s flag says the child is gone and
/// nothing follows (its output ended, broke, or a frame did not read).
fn read_scored(output: &mut impl BufRead, slots: &mut [f64]) -> Result<(), (String, bool)> {
    let gone = |why: String| (why, true);
    let first = match output.fill_buf() {
        Ok([first, ..]) => *first,
        Ok([]) => return Err(gone("the network's process ended".to_string())),
        Err(e) => return Err(gone(format!("the network's process: {e}"))),
    };
    if matrix::opens_frame(first) {
        let frame = matrix::read_frame(output).map_err(gone)?;
        if frame.values.len() != slots.len() {
            let why = format!(
                "the network answered {} number(s), not {}",
                frame.values.len(),
                slots.len()
            );
            return Err((why, false));
        }
        slots.iter_mut().zip(frame.values).for_each(|(slot, value)| *slot = f64::from(value));
        return Ok(());
    }
    let mut line = String::new();
    output.read_line(&mut line).map_err(|e| gone(format!("the network's process: {e}")))?;
    read_answer(&line, slots).map_err(|why| (why, false))
}

/// The child's answer line, written into `out`: its numbers, or its `error <reason>` as the refusal.
fn read_answer(answer: &str, out: &mut [f64]) -> Result<(), String> {
    if let Some(reason) = answer.strip_prefix("error ") {
        return Err(reason.trim().to_string());
    }
    // Read as the `f32`s the network computed, then widened: the child prints each float's shortest
    // spelling, and reading the spelling as `f64` would answer a nearby decimal instead.
    let numbers: Vec<&str> = answer.trim().split(',').collect();
    if numbers.len() != out.len() {
        return Err(format!(
            "the network answered `{}`, not {} number(s)",
            answer.trim(),
            out.len()
        ));
    }
    for (slot, number) in out.iter_mut().zip(numbers) {
        *slot = number
            .parse::<f32>()
            .map(f64::from)
            .map_err(|e| format!("the network answered `{}`: {e}", answer.trim()))?;
    }
    Ok(())
}

impl Drop for NeuralModel {
    fn drop(&mut self) {
        let pipes = self.pipes.get_mut().unwrap_or_else(PoisonError::into_inner);
        let _ = pipes.child.kill();
        let _ = pipes.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Exec the freshly planted fake trainer once with `--vike-settle` (which it answers with exit 0 and
    /// nothing else), retrying ONLY `ETXTBSY`: this binary's cases run as threads of one process, and a
    /// sibling's fork can hold the just-written file open for a moment, so the learner's exec of it would
    /// otherwise race that. A local copy of `crates/vike-ops/tests/common/mod.rs`'s `plant_execution`
    /// settle: this crate takes no `vike-ops` dependency for a test helper.
    #[cfg(unix)]
    fn settle_planted_execution(path: &Path) {
        use vike_model::test_support::etxtbsy::{ATTEMPTS, FIRST_BACKOFF, spawn_error_is_etxtbsy};
        let mut backoff = FIRST_BACKOFF;
        for attempt in 1..=ATTEMPTS {
            match std::process::Command::new(path).arg("--vike-settle").output() {
                Ok(out) if out.status.success() => return,
                Ok(out) => panic!("settle {}: it ran and exited {:?}", path.display(), out.status),
                Err(e) if spawn_error_is_etxtbsy(&e) => {}
                Err(e) => panic!("settle {}: not the ETXTBSY race: {e}", path.display()),
            }
            if attempt < ATTEMPTS {
                std::thread::sleep(backoff);
                backoff *= 2;
            }
        }
        panic!("settle {}: still ETXTBSY after {ATTEMPTS} attempts", path.display());
    }

    #[test]
    fn a_missing_trainer_is_refused_at_construction() {
        let missing = Path::new(env!("CARGO_MANIFEST_DIR")).join("no-such-trainer");
        let error = NeuralLearner::new(&missing, Path::new("."), "").expect_err("refused");
        assert!(error.contains("no network trainer"), "{error}");
    }

    #[test]
    fn a_point_that_moves_a_tree_axis_is_refused_before_any_process() {
        // Built by hand, never run: the refusal comes before a directory or a process exists.
        let learner = NeuralLearner {
            binary: PathBuf::from("vike-no-such-network-trainer"),
            scratch: PathBuf::from("vike-no-such-scratch"),
            spec: String::new(),
            next: AtomicU64::new(0),
            chains: Mutex::default(),
        };
        let (x, y) = ([0.0, 1.0], [0.0_f32, 1.0]);
        let data = TrainData::new(&x, &y, 1).expect("data");
        let point = GridPoint { num_leaves: 7, ..DEFAULT_POINT };
        let error = learner.fit(&data, &point, 0).expect_err("refused");
        assert!(error.contains("no leaves"), "{error}");
    }

    /// A fold that trains but whose model does not load (a trainer whose `predict` never comes up)
    /// leaves the chain where it was: the same step is still the chain's next, the error names the
    /// fold, and nothing it kept stays behind.
    #[cfg(unix)]
    #[test]
    fn a_fold_whose_model_does_not_load_leaves_the_chain_where_it_was() {
        use std::os::unix::fs::PermissionsExt;
        let root = vike_paths::scratch::ScratchDir::create_in(
            &std::env::temp_dir(),
            "vike_ml_neural_unloadable",
        )
        .expect("scratch dir");
        let binary = root.path().join("trainer");
        let script = "#!/bin/sh\n\
            [ \"$1\" = --vike-settle ] && exit 0\n\
            if [ \"$1\" = train ]; then\n\
            while [ $# -gt 0 ]; do [ \"$1\" = --out ] && out=\"$2\"; shift; done\n\
            printf onnx > \"$out/model.onnx\"; printf weights > \"$out/weights\"\n\
            echo trainer=fake > \"$out/PROVENANCE\"; exit 0\n\
            fi\n\
            echo 'no model here' >&2; exit 1\n";
        std::fs::write(&binary, script).expect("the fake trainer");
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).expect("mode");
        settle_planted_execution(&binary);
        let scratch = root.path().join("fits");
        let learner = NeuralLearner::new(&binary, &scratch, "").expect("a learner");
        let (x, y) = ([0.0, 1.0], [0.0_f32, 1.0]);
        let data = TrainData::new(&x, &y, 1).expect("data");
        for attempt in 1..=2 {
            let error = learner
                .fit_chained(&data, &DEFAULT_POINT, 0, Capture::default(), "long", 1)
                .map(|_| ())
                .expect_err("the model does not load");
            assert!(
                error.contains("chain `long`, step 1: the network did not load"),
                "attempt {attempt}: {error}"
            );
        }
        let left: Vec<_> = std::fs::read_dir(&scratch).expect("the fits root").collect();
        assert!(left.is_empty(), "no fit directory and no kept weights stay: {left:?}");
    }

    /// An OLDER trainer (its `ready` line offers no predict frames) is asked in text lines, a row and a batch
    /// alike, and its line answers are read as they always were.
    #[cfg(unix)]
    #[test]
    fn an_older_trainer_is_asked_in_text_lines() {
        use std::os::unix::fs::PermissionsExt;
        let root = vike_paths::scratch::ScratchDir::create_in(
            &std::env::temp_dir(),
            "vike_ml_neural_older",
        )
        .expect("scratch dir");
        let binary = root.path().join("trainer");
        let script = "#!/bin/sh\n\
            [ \"$1\" = --vike-settle ] && exit 0\n\
            if [ \"$1\" = train ]; then\n\
            while [ $# -gt 0 ]; do [ \"$1\" = --out ] && out=\"$2\"; shift; done\n\
            printf onnx > \"$out/model.onnx\"; echo trainer=fake > \"$out/PROVENANCE\"; exit 0\n\
            fi\n\
            echo 'ready kind=value outputs=1 columns=2'\n\
            while read -r line; do case \"$line\" in *,*) echo 0.25;; *) echo \"error not a line\";; esac; done\n";
        std::fs::write(&binary, script).expect("the fake trainer");
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).expect("mode");
        settle_planted_execution(&binary);
        let learner =
            NeuralLearner::new(&binary, &root.path().join("fits"), "").expect("a learner");
        let (x, y) = ([0.0, 1.0, 2.0, 3.0], [0.0_f32, 1.0]);
        let model =
            learner.fit(&TrainData::new(&x, &y, 2).expect("data"), &DEFAULT_POINT, 0).expect("fit");
        assert!(!model.frames, "no predict frames offered");
        assert_eq!(model.predict_one(&[1.5, -2.0]), Ok(0.25));
        assert_eq!(model.predict_batch(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0], 2), Ok(vec![0.25; 3]));
    }
}
