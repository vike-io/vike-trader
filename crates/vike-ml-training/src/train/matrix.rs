//! The data file of a network fit: what [`crate::NeuralLearner`] writes and the trainer process
//! `vike-ml-neural-training train --data` reads, the fit's matrix as raw little-endian `f32`, the type the
//! network trains in, so no number is spelled as text and parsed back
//! (`docs/decisions/0292-neural-networks-train-research-side-on-burn-and-nothing-that-trades-links-the-trainer.md`,
//! amended 2026-11-18). Both halves live here, so the two sides cannot disagree on the layout.
//!
//! | bytes | what |
//! |---|---|
//! | 8 | `MAGIC`, `vikerows` |
//! | 4 | `VERSION`, a `u32` (1) |
//! | 4 | `NUMBER_TYPE_F32`, a `u32` (1): the one number type the file holds |
//! | 8 | the rows, a `u64` |
//! | 8 | the columns (features a row), a `u64` |
//! | 4 x rows | the labels |
//! | 4 x rows x columns | the features, row-major |
//! | 32 | the SHA-256 of every byte before it |
//!
//! A number is the source's `f64` narrowed with `as f32`: exactly what the CSV route trained on (its
//! shortest decimal parsed back to the same `f64`, then narrowed), pinned by this module's tests. A
//! value whose narrowing is not finite is refused at the write and a non-finite number at the read: a
//! network has no missing-value branch. The reader still takes the CSV an older host writes (the
//! file then does not start with the magic), for a rollback of the host alone ([`read_matrix`]).
//!
//! **The predict pipe speaks the same form** ("JSON only at the edges", 2026-11-19): a batch of rows
//! the host asks a held network to score is [`predict_frame`], the file's header and the features with
//! no labels and no checksum (a pipe, not a file a disk keeps), and the child answers each batch with
//! [`answer_frame`], `columns` its answer's numbers per row; [`read_frame`] reads either. A number is its
//! `f64` narrowed with `as f32`, which is the network's own first step (`vike_ml_onnx_prediction`'s
//! `predict_into`), so the child scores the bits the text route's parse narrowed to, and an answer is the
//! `f32` the network computed. A predict number is NOT refused when it is not finite: the text route
//! scored such a row. The trainer process and the host can be different builds (a rollback puts back the
//! host's binaries alone), so the form is self-describing and negotiated: the child's `ready` line
//! advertises it ([`PREDICT_WORD`]), and a frame opens with the magic's first byte, which no text row
//! does, so the child answers each request in the form it came in.

use std::io::{Read, Write};

use sha2::{Digest, Sha256};
use vike_ml_prediction::MlError;

use crate::train::{TrainData, io};

const MAGIC: [u8; 8] = *b"vikerows";
const VERSION: u32 = 1;
const NUMBER_TYPE_F32: u32 = 1;
const HEADER: usize = 32;
const CHECKSUM: usize = 32;

/// A fit's matrix as the trainer trains on it: `labels.len()` rows of `columns` features each.
#[derive(Debug, Clone, PartialEq)]
pub struct Rows {
    /// The features, row-major.
    pub features: Vec<f32>,
    /// One label per row.
    pub labels: Vec<f32>,
    /// Features per row.
    pub columns: usize,
}

impl TrainData<'_> {
    /// Write the network trainer's data file (module doc): the header, the labels, the features and
    /// the checksum. A value that is not a finite `f32` once narrowed is refused, naming its row.
    pub fn write_matrix<W: Write>(&self, w: &mut W) -> Result<(), MlError> {
        self.validate()?;
        let mut hash = Sha256::new();
        let mut put = |bytes: &[u8]| {
            hash.update(bytes);
            io(w.write_all(bytes))
        };
        let mut bytes = Vec::with_capacity(HEADER.max(4 * self.n_cols.max(self.n_rows)));
        bytes.extend_from_slice(&MAGIC);
        bytes.extend_from_slice(&VERSION.to_le_bytes());
        bytes.extend_from_slice(&NUMBER_TYPE_F32.to_le_bytes());
        bytes.extend_from_slice(&(self.n_rows as u64).to_le_bytes());
        bytes.extend_from_slice(&(self.n_cols as u64).to_le_bytes());
        put(&bytes)?;
        let narrow = |row: usize, value: f64| {
            let narrowed = value as f32;
            if narrowed.is_finite() {
                Ok(narrowed.to_le_bytes())
            } else {
                Err(MlError::Shape(format!("row {}: `{value}` is not a finite f32", row + 1)))
            }
        };
        bytes.clear();
        for (row, label) in self.y.iter().enumerate() {
            bytes.extend(narrow(row, f64::from(*label))?);
        }
        put(&bytes)?;
        for (row, values) in self.x.chunks_exact(self.n_cols).enumerate() {
            bytes.clear();
            for value in values {
                bytes.extend(narrow(row, *value)?);
            }
            put(&bytes)?;
        }
        io(w.write_all(&hash.finalize()))
    }
}

/// The data file `file` (module doc) as rows. A short or long file, a version or number type this
/// reader does not know, no row, no feature, a checksum that does not match and a non-finite number
/// are each refused by name. A file that does not start with the magic is read as the CSV an older
/// host writes (`read_csv`).
pub fn read_matrix(file: &[u8]) -> Result<Rows, String> {
    if !file.starts_with(&MAGIC) {
        return read_csv(file);
    }
    let Some((head, _)) = file.split_first_chunk::<HEADER>() else {
        return Err(format!("the data file is {} bytes, shorter than its header", file.len()));
    };
    let word = |at: usize| u32::from_le_bytes([head[at], head[at + 1], head[at + 2], head[at + 3]]);
    let count = |at: usize| {
        let mut eight = [0; 8];
        eight.copy_from_slice(&head[at..at + 8]);
        usize::try_from(u64::from_le_bytes(eight)).unwrap_or(usize::MAX)
    };
    if word(8) != VERSION {
        return Err(format!("the data file is version {}; this trainer reads {VERSION}", word(8)));
    }
    if word(12) != NUMBER_TYPE_F32 {
        return Err(format!("the data file holds number type {}, not f32 (1)", word(12)));
    }
    let (rows, columns) = (count(16), count(24));
    if rows == 0 {
        return Err("the data file holds no row".into());
    }
    if columns == 0 {
        return Err("the data file's rows hold 0 features".into());
    }
    let need = columns
        .checked_add(1)
        .and_then(|width| width.checked_mul(rows))
        .and_then(|numbers| numbers.checked_mul(4))
        .and_then(|body| body.checked_add(HEADER + CHECKSUM));
    if need != Some(file.len()) {
        return Err(format!(
            "the data file is {} bytes, not the {rows} rows x {columns} features its header names",
            file.len()
        ));
    }
    let (signed, sum) = file.split_at(file.len() - CHECKSUM);
    if Sha256::digest(signed).as_slice() != sum {
        return Err("the data file's checksum does not match its contents".into());
    }
    let floats = |bytes: &[u8]| -> Vec<f32> {
        bytes.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect()
    };
    let (labels, features) = signed[HEADER..].split_at(4 * rows);
    let (labels, features) = (floats(labels), floats(features));
    let not_finite = |values: &[f32]| values.iter().position(|v| !v.is_finite());
    let bad = not_finite(&labels)
        .map(|row| (row, labels[row]))
        .or_else(|| not_finite(&features).map(|at| (at / columns, features[at])));
    if let Some((row, value)) = bad {
        return Err(format!("row {}: `{value}` is not a finite number", row + 1));
    }
    Ok(Rows { features, labels, columns })
}

/// The word a predicting child's `ready` line carries when it reads [`predict_frame`]s: `rows=vikerows`. A
/// child without it (an older trainer) is asked in text lines.
pub const PREDICT_WORD: &str = "rows=vikerows";

/// The most bytes one predict or answer frame's numbers may take, read: a peer streaming a stated
/// length forever stops here.
// shortcut: 1 GiB is far over a test block's rows; raise it when a batch is measured above.
const MAX_FRAME_NUMBERS_BYTES: usize = 1 << 30;

/// A batch of predict rows or of their answers, as [`read_frame`] reads it: `rows` rows of `columns`
/// numbers, row-major.
#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    pub rows: usize,
    pub columns: usize,
    pub values: Vec<f32>,
}

/// Whether a request or an answer that opens with `first` is a frame ([`read_frame`]) rather than a text
/// line: the magic's first byte, which no number's spelling and no `error` line opens with.
pub fn opens_frame(first: u8) -> bool {
    first == MAGIC[0]
}

/// `values` (row-major, `columns` a row, whole rows: the caller's check) as ONE predict frame for the held
/// network (module doc): each narrowed with `as f32`.
pub fn predict_frame(values: &[f64], columns: usize) -> Vec<u8> {
    frame_of(
        values.len().checked_div(columns).unwrap_or(0),
        columns,
        values.iter().map(|&v| v as f32),
    )
}

/// A batch's answers (row-major, `columns` a row: the network's outputs, whole rows) as ONE answer frame.
pub fn answer_frame(values: &[f32], columns: usize) -> Vec<u8> {
    frame_of(values.len().checked_div(columns).unwrap_or(0), columns, values.iter().copied())
}

fn frame_of(rows: usize, columns: usize, values: impl Iterator<Item = f32>) -> Vec<u8> {
    let mut frame = Vec::with_capacity(HEADER + 4 * rows * columns);
    frame.extend_from_slice(&MAGIC);
    frame.extend_from_slice(&VERSION.to_le_bytes());
    frame.extend_from_slice(&NUMBER_TYPE_F32.to_le_bytes());
    frame.extend_from_slice(&(rows as u64).to_le_bytes());
    frame.extend_from_slice(&(columns as u64).to_le_bytes());
    values.for_each(|v| frame.extend_from_slice(&v.to_le_bytes()));
    frame
}

/// ONE predict or answer frame read from `r` (module doc). A header that is not the magic, a version or
/// number type this reader does not know, no column, or numbers over [`MAX_FRAME_NUMBERS_BYTES`] are
/// refused by name; a stream cut short is the read's own error.
pub fn read_frame(r: &mut impl Read) -> Result<Frame, String> {
    let mut head = [0u8; HEADER];
    r.read_exact(&mut head).map_err(|e| format!("a predict frame's header: {e}"))?;
    if head[..8] != MAGIC {
        return Err("a predict frame that does not start `vikerows`".into());
    }
    let word = |at: usize| u32::from_le_bytes([head[at], head[at + 1], head[at + 2], head[at + 3]]);
    let count = |at: usize| {
        let mut eight = [0; 8];
        eight.copy_from_slice(&head[at..at + 8]);
        usize::try_from(u64::from_le_bytes(eight)).unwrap_or(usize::MAX)
    };
    if word(8) != VERSION || word(12) != NUMBER_TYPE_F32 {
        return Err(format!(
            "a predict frame of version {} and number type {}; this side reads {VERSION} and f32 (1)",
            word(8),
            word(12)
        ));
    }
    let (rows, columns) = (count(16), count(24));
    if columns == 0 {
        return Err("a predict frame whose rows hold 0 numbers".into());
    }
    let bytes = rows
        .checked_mul(columns)
        .and_then(|n| n.checked_mul(4))
        .filter(|bytes| *bytes <= MAX_FRAME_NUMBERS_BYTES)
        .ok_or_else(|| {
            format!("a predict frame of {rows} rows x {columns} numbers is over the cap")
        })?;
    let mut body = vec![0u8; bytes];
    r.read_exact(&mut body).map_err(|e| format!("a predict frame's {rows} rows: {e}"))?;
    let values = body.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect();
    Ok(Frame { rows, columns, values })
}

/// The CSV a host from before 2026-11-18 writes (`TrainData::write_csv`: the label, then the features,
/// one row per line, no header), read as the trainer read it then, refusals included: a rollback
/// puts back the host's binaries but not the trainer (`deploy/docker/vikectl`'s `HOST_FILES`), so an
/// older host's fits still train on this trainer.
/// shortcut: kept for that rollback only; delete it once no release a box can roll back to writes CSV.
fn read_csv(file: &[u8]) -> Result<Rows, String> {
    let text = std::str::from_utf8(file).map_err(|_| {
        "the data file is neither a training matrix (it does not start `vikerows`) nor CSV text"
            .to_string()
    })?;
    let mut rows = Rows { features: Vec::new(), labels: Vec::new(), columns: 0 };
    for (index, line) in text.lines().filter(|line| !line.trim().is_empty()).enumerate() {
        let values = line
            .split(',')
            .map(|field| match field.trim().parse::<f64>() {
                Ok(value) if value.is_finite() => Ok(value as f32),
                _ => Err(format!("row {}: `{field}` is not a finite number", index + 1)),
            })
            .collect::<Result<Vec<f32>, String>>()?;
        let columns = values.len() - 1;
        if columns == 0 || (index > 0 && columns != rows.columns) {
            return Err(format!("row {}: {columns} features", index + 1));
        }
        rows.columns = columns;
        rows.labels.push(values[0]);
        rows.features.extend_from_slice(&values[1..]);
    }
    if rows.labels.is_empty() {
        return Err("the data file holds no row".into());
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file_of(x: &[f64], y: &[f32], columns: usize) -> Vec<u8> {
        let mut file = Vec::new();
        TrainData::new(x, y, columns).expect("data").write_matrix(&mut file).expect("written");
        file
    }

    /// The CSV route's file: what the host wrote before 2026-11-18 (`write_csv`).
    fn csv_of(x: &[f64], y: &[f32], columns: usize) -> Vec<u8> {
        let mut text = Vec::new();
        TrainData::new(x, y, columns).expect("data").write_csv(&mut text).expect("csv");
        text
    }

    fn bits(values: &[f32]) -> Vec<u32> {
        values.iter().map(|v| v.to_bits()).collect()
    }

    /// Full-mantissa `f64`s from a fixed stream, and the narrowing's edges: signed zeros, `f64`
    /// subnormals, `f32` subnormals, the `f32` extremes, halfway and just-off-halfway values.
    fn values(count: usize) -> Vec<f64> {
        let mut state = 0x9E37_79B9_7F4A_7C15_u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let edges = [
            0.0,
            -0.0,
            f64::MIN_POSITIVE,
            -5e-324,
            1e-45,
            -1.4e-45,
            f64::from(f32::MIN_POSITIVE) / 3.0,
            f64::from(f32::MAX),
            f64::from(f32::MIN),
            1.0 + f64::EPSILON,
            1.0 + 2f64.powi(-24),
            1.0 + 2f64.powi(-24) + 2f64.powi(-50),
            0.1,
            -123_456.789_012_345_6,
        ];
        let random = (edges.len()..count).map(|_| {
            let unit = (next() >> 11) as f64 / (1u64 << 53) as f64;
            (unit - 0.5) * 10f64.powi((next() % 13) as i32 - 6)
        });
        edges.into_iter().chain(random).take(count).collect()
    }

    #[test]
    fn the_trainer_reads_from_the_matrix_the_bits_it_read_from_an_older_hosts_csv() {
        let (rows, columns) = (200, 1200);
        let x = values(rows * columns);
        let y: Vec<f32> = (0..rows).map(|i| [(i % 3) as f32, -0.0, 1e-40][i % 3]).collect();
        let read = read_matrix(&file_of(&x, &y, columns)).expect("read");
        let csv = read_matrix(&csv_of(&x, &y, columns)).expect("the CSV an older host writes");
        assert_eq!(read.columns, csv.columns);
        assert_eq!(bits(&read.labels), bits(&csv.labels), "the labels");
        assert_eq!(bits(&read.features), bits(&csv.features), "the features");
        let narrowed: Vec<f32> = x.iter().map(|&v| v as f32).collect();
        assert_eq!(bits(&read.features), bits(&narrowed), "each feature is its f64 narrowed");
    }

    #[test]
    fn a_value_that_does_not_narrow_to_a_finite_f32_is_refused_at_the_write() {
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 1e300] {
            let x = [0.5, 0.25, bad, 1.0];
            let error = TrainData::new(&x, &[0.0, 1.0], 2)
                .expect("data")
                .write_matrix(&mut Vec::new())
                .expect_err("refused");
            assert!(error.to_string().contains("row 2:"), "{error}");
        }
        let error = TrainData::new(&[0.5], &[f32::NAN], 1)
            .expect("data")
            .write_matrix(&mut Vec::new())
            .expect_err("a label too");
        assert!(error.to_string().contains("row 1:"), "{error}");
    }

    /// A predict frame carries each feature as the text route delivered it to the network (its shortest
    /// spelling parsed as `f64`, then narrowed by the network's first step), non-finite numbers included; an
    /// answer frame carries the network's `f32`s as the text route's parse of their spelling did.
    #[test]
    fn a_predict_frame_carries_the_bits_the_text_lines_did() {
        let mut features = values(4 * 300);
        features.extend([f64::INFINITY, f64::NEG_INFINITY, 1e300, -1e-320]);
        let frame = read_frame(&mut predict_frame(&features, 4).as_slice()).expect("read");
        assert_eq!((frame.rows, frame.columns), (301, 4));
        let text: Vec<f32> = features
            .iter()
            .map(|v| v.to_string().parse::<f64>().expect("spelling") as f32)
            .collect();
        assert_eq!(bits(&frame.values), bits(&text), "the text route's narrowed parse");
        let nan = read_frame(&mut predict_frame(&[f64::NAN], 1).as_slice()).expect("read");
        assert!(nan.values[0].is_nan(), "a NaN feature scores as NaN, as its text did");

        let answers = [0.25_f32, -0.0, 1e-40, f32::MAX, 0.1, 7.5];
        let back = read_frame(&mut answer_frame(&answers, 3).as_slice()).expect("read");
        assert_eq!((back.rows, back.columns), (2, 3));
        let spelled: Vec<f32> =
            answers.iter().map(|v| v.to_string().parse::<f32>().expect("spelling")).collect();
        assert_eq!(bits(&back.values), bits(&spelled), "the text route's parse of the answer");
        assert!(opens_frame(answer_frame(&answers, 3)[0]));
        assert!(
            !opens_frame(b'e') && !opens_frame(b'-') && !opens_frame(b'N') && !opens_frame(b'i')
        );
    }

    #[test]
    fn a_malformed_predict_frame_is_refused_by_name() {
        let good = predict_frame(&[0.5, 0.25], 2);
        let refusal = |frame: &[u8]| read_frame(&mut &frame[..]).expect_err("refused");
        assert!(refusal(b"1.5,2.5\n").contains("header"));
        let mut magic = good.clone();
        magic[0] = b'w';
        assert!(refusal(&magic).contains("does not start"));
        let mut version = good.clone();
        version[8] = 2;
        assert!(refusal(&version).contains("version 2"));
        let mut columns = good.clone();
        columns[24..32].copy_from_slice(&0u64.to_le_bytes());
        assert!(refusal(&columns).contains("0 numbers"));
        let mut huge = good.clone();
        huge[16..24].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(refusal(&huge).contains("over the cap"));
        assert!(refusal(&good[..good.len() - 1]).contains("1 rows"));
    }

    #[test]
    fn a_malformed_file_is_refused_by_name() {
        let good = file_of(&[0.5, 0.25, 1.0, 2.0, 3.0, 4.0], &[0.0, 1.0], 3);
        assert_eq!(read_matrix(&good).expect("good").labels, vec![0.0, 1.0]);
        let refusal = |file: &[u8]| read_matrix(file).expect_err("refused");
        let resigned = |mut file: Vec<u8>| {
            let at = file.len() - CHECKSUM;
            let sum = Sha256::digest(&file[..at]);
            file[at..].copy_from_slice(&sum);
            file
        };
        assert!(refusal(&good[..20]).contains("shorter than its header"));
        let older = Rows {
            features: vec![0.5, 0.25, 1.0, 2.0, 3.0, 4.0],
            labels: vec![0.0, 1.0],
            columns: 3,
        };
        assert_eq!(read_matrix(b"0,0.5,0.25,1\n1,2,3,4\n"), Ok(older), "an older host's CSV");
        assert!(refusal(&[0xff, 0xfe, 0]).contains("neither a training matrix"));
        assert!(refusal(b"0,1,2\n1,2\n").contains("row 2: 1 features"));
        assert!(refusal(b"0,nan,1\n").contains("row 1: `nan` is not a finite number"));
        assert!(refusal(b"\n").contains("no row"));
        assert!(refusal(&good[..good.len() - 1]).contains("not the 2 rows x 3 features"));
        let mut long = good.clone();
        long.push(0);
        assert!(refusal(&long).contains("not the 2 rows x 3 features"));
        let mut version = good.clone();
        version[8] = 2;
        assert!(refusal(&resigned(version)).contains("version 2"));
        let mut number_type = good.clone();
        number_type[12] = 2;
        assert!(refusal(&resigned(number_type)).contains("not f32"));
        let mut flipped = good.clone();
        flipped[HEADER + 9] ^= 1;
        assert!(refusal(&flipped).contains("checksum"));
        let mut not_finite = good.clone();
        not_finite[HEADER + 8 + 4 * 4..HEADER + 8 + 4 * 5].copy_from_slice(&f32::NAN.to_le_bytes());
        assert!(refusal(&resigned(not_finite)).contains("row 2: `NaN` is not a finite number"));
        let none = file_of(&[], &[], 3);
        assert!(refusal(&none).contains("no row"));
        let mut no_feature = none.clone();
        no_feature[16] = 1;
        no_feature[24] = 0;
        assert!(refusal(&no_feature).contains("0 features"));
    }
}
