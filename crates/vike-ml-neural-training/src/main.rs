//! `vike-ml-neural-training` — the network trainer a study host DRIVES, as it drives LightGBM: a separate
//! process outside the study's jail, so `burn` is in no study build, no sandbox and no binary that trades
//! (decision 0292). The host's side is `vike_ml_training::NeuralLearner`.
//!
//! * `train --data FILE.matrix --spec FILE.toml --seed N --learning-rate X --out DIR` trains the network
//!   the spec names on the data (`vike_ml_neural_training::trainer`'s module doc has both formats) and
//!   writes `DIR/model.onnx` and `DIR/PROVENANCE` (one line, also printed on stdout). A spec with
//!   `warm_start = true` also writes `DIR/weights`, and `--start-from FILE` (a previous fit's `weights`)
//!   makes the fit continue that network.
//! * `predict --model DIR` loads `DIR/model.onnx` for one row and answers rows on stdin. The KIND of answer
//!   is the one `DIR/PROVENANCE`'s `loss=` (and, for `multiclass`, `classes=`) names
//!   (`trainer::prediction_kind`), declared to `OnnxModel::predictor`, which checks the graph against it:
//!   never assumed. Its first stdout line is `ready kind=probability|classes|value outputs=N columns=M
//!   rows=vikerows` (`classes`: N class probabilities). Then each request on stdin is answered in the form
//!   it came in: a BATCH of rows as `vike_ml_training::train::matrix`'s predict frame (what `rows=vikerows`
//!   offers, 2026-11-19) gets one answer frame of `N` numbers a row, or `error <reason>` for its first
//!   refused row; a line of `M` comma-separated numbers (an older host) gets one line of `N`
//!   comma-separated answers, or `error <reason>`. The process stays up after a refusal; end of input ends
//!   it.
//!
//! Exit 0 on success, 1 on a refusal (the reason on stderr), 2 on a command-line error.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use vike_ml_neural_training::trainer::{NetworkSpec, prediction_kind, train_from_spec};
use vike_ml_onnx_prediction::OnnxModel;
use vike_ml_prediction::{PredictionKind, Predictor};
use vike_ml_training::train::matrix::{
    PREDICT_WORD, answer_frame, opens_frame, read_frame, read_matrix,
};

const MODEL_FILE: &str = "model.onnx";
const PROVENANCE_FILE: &str = "PROVENANCE";
/// The trained network's weights (`vike_ml_neural_training::weights`), written with `warm_start` only.
const WEIGHTS_FILE: &str = "weights";

#[derive(clap::Parser, Debug)]
#[command(name = "vike-ml-neural-training")]
struct Args {
    #[command(subcommand)]
    command: Verb,
}

#[derive(clap::Subcommand, Debug)]
enum Verb {
    /// Train the network a spec names on a data file; write model.onnx and PROVENANCE.
    Train {
        /// The rows: `vike_ml_training::train::matrix`'s raw little-endian `f32` file (or an older
        /// host's CSV: the label, then the features, one row per line).
        #[arg(long, value_name = "FILE.matrix")]
        data: PathBuf,
        /// The network (TOML).
        #[arg(long, value_name = "FILE.toml")]
        spec: PathBuf,
        /// Seeds the initial weights and every epoch's batch order.
        #[arg(long)]
        seed: u64,
        /// Adam's step size.
        #[arg(long)]
        learning_rate: f64,
        /// The directory model.onnx and PROVENANCE (and, with `warm_start`, weights) are written to
        /// (it must exist).
        #[arg(long, value_name = "DIR")]
        out: PathBuf,
        /// A previous fit's `weights` to start from (a spec with `warm_start = true` only).
        #[arg(long, value_name = "FILE")]
        start_from: Option<PathBuf>,
    },
    /// Answer rows on stdin with a trained model.
    Predict {
        /// The directory `train` wrote.
        #[arg(long, value_name = "DIR")]
        model: PathBuf,
    },
}

fn main() -> ExitCode {
    let name = env!("CARGO_PKG_NAME");
    let version = vike_buildinfo::version_line(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
    let args: Args = vike_utility::cli::Program::new(name, &version).parse();
    let outcome = match args.command {
        Verb::Train { data, spec, seed, learning_rate, out, start_from } => {
            train(&data, &spec, seed, learning_rate, &out, start_from.as_deref())
        }
        Verb::Predict { model } => predict(&model),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(reason) => {
            eprintln!("vike-ml-neural-training: {reason}");
            ExitCode::from(1)
        }
    }
}

fn read(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))
}

fn bytes(path: &Path) -> Result<Vec<u8>, String> {
    std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))
}

fn train(
    data: &Path,
    spec: &Path,
    seed: u64,
    rate: f64,
    out: &Path,
    start_from: Option<&Path>,
) -> Result<(), String> {
    let spec = NetworkSpec::parse(&read(spec)?).map_err(|e| e.0)?;
    let rows = read_matrix(&bytes(data)?)?;
    let start = start_from.map(bytes).transpose()?;
    let model = train_from_spec(&spec, &rows, seed, rate, start.as_deref()).map_err(|e| e.0)?;
    let write = |name: &str, bytes: &[u8]| {
        let path = out.join(name);
        std::fs::write(&path, bytes).map_err(|e| format!("{}: {e}", path.display()))
    };
    write(MODEL_FILE, &model.onnx)?;
    write(PROVENANCE_FILE, format!("{}\n", model.provenance).as_bytes())?;
    if let Some(weights) = &model.weights {
        write(WEIGHTS_FILE, weights)?;
    }
    println!("{}", model.provenance);
    Ok(())
}

fn predict(dir: &Path) -> Result<(), String> {
    let provenance = read(&dir.join(PROVENANCE_FILE))?;
    let word = |key: &str| {
        provenance
            .split_whitespace()
            .find_map(|word| word.strip_prefix(key)?.strip_prefix('='))
            .ok_or_else(|| format!("{PROVENANCE_FILE} has no `{key}=`"))
    };
    let shape = word("input_shape")?
        .split(',')
        .map(|n| n.parse::<usize>().map_err(|e| format!("input_shape `{n}`: {e}")))
        .collect::<Result<Vec<usize>, String>>()?;
    let classes = word("classes")
        .ok()
        .map(|n| n.parse::<usize>().map_err(|e| format!("classes `{n}`: {e}")))
        .transpose()?;
    let kind = prediction_kind(word("loss")?, classes).map_err(|e| e.0)?;
    let kind_word = match kind {
        PredictionKind::Probability => "probability",
        PredictionKind::Value => "value",
        PredictionKind::ClassProbabilities { .. } => "classes",
    };
    let bytes = std::fs::read(dir.join(MODEL_FILE)).map_err(|e| format!("{MODEL_FILE}: {e}"))?;
    let model = OnnxModel::from_bytes(&bytes, &shape)?.predictor(kind)?;
    let columns: usize = shape.iter().product();
    let outputs = kind.output_count();
    let mut out = std::io::stdout().lock();
    let mut answer = |bytes: &[u8]| -> Result<(), String> {
        out.write_all(bytes).and_then(|()| out.flush()).map_err(|e| format!("stdout: {e}"))
    };
    let ready =
        format!("ready kind={kind_word} outputs={outputs} columns={columns} {PREDICT_WORD}\n");
    answer(ready.as_bytes())?;
    let mut values = vec![0.0; outputs];
    let mut input = std::io::stdin().lock();
    loop {
        let first = match input.fill_buf().map_err(|e| format!("stdin: {e}"))? {
            [] => break,
            [first, ..] => *first,
        };
        // A frame (the host saw `PREDICT_WORD`): every row scored alone, as a line is, the answers as one frame,
        // or the first row's refusal as a line. An unreadable frame leaves the pipe out of step: it ends here.
        if opens_frame(first) {
            let frame = read_frame(&mut input)?;
            let scored = if frame.columns == columns {
                let mut scores = Vec::with_capacity(frame.rows * outputs);
                frame
                    .values
                    .chunks_exact(columns)
                    .try_for_each(|row| {
                        let row: Vec<f64> = row.iter().copied().map(f64::from).collect();
                        model.predict_into(&row, &mut values)?;
                        scores.extend(values.iter().map(|&v| v as f32));
                        Ok::<(), String>(())
                    })
                    .map(|()| answer_frame(&scores, outputs))
            } else {
                Err(format!(
                    "the network takes {columns} features, the frame's rows hold {}",
                    frame.columns
                ))
            };
            answer(&scored.unwrap_or_else(|reason| format!("error {reason}\n").into_bytes()))?;
            continue;
        }
        let mut line = String::new();
        input.read_line(&mut line).map_err(|e| format!("stdin: {e}"))?;
        let line = line.trim_end_matches(['\n', '\r']);
        let row: Result<Vec<f64>, String> = line
            .split(',')
            .map(|v| v.trim().parse::<f64>().map_err(|e| format!("`{v}`: {e}")))
            .collect();
        // The predictor computes in `f32` and widens; the `f32` spelling is that number exactly.
        let reply = row.and_then(|row| model.predict_into(&row, &mut values)).map_or_else(
            |reason| format!("error {reason}"),
            |()| values.iter().map(|&v| (v as f32).to_string()).collect::<Vec<_>>().join(","),
        );
        answer(format!("{reply}\n").as_bytes())?;
    }
    Ok(())
}
