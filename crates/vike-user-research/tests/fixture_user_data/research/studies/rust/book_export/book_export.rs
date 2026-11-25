//! `book_export` — a RECORDED order book exported as DeepLOB-style training rows (the book's W20).
//!
//! The consumer of `vike_analytics::book_export`: it reads the recorded book through the study API
//! (`StudyContext::book_updates`, the lossless lane, or `StudyContext::depth`, the conflating one),
//! hands it to `export_book_training_data` and writes every row to `book_samples.tsv` beside the run,
//! with the class balance as metrics. A network trainer reads the TSV; no new storage is involved.
//!
//! The recipe names the market (`venue`, `symbol`: no default, a study names its own market) and may
//! set `lane` (`"book"` or `"depth"`, default `"book"`), `levels` (default 10), `horizon_events`
//! (default 10), `interval_ms` (omitted: a row after every book event), `label` (`"mid_after"`, the
//! default, `"mean_of_next"` or `"mean_of_next_against_mean_of_last"`: `BookLabel`'s three forms),
//! `up_threshold` and `down_threshold` (default 0.0001 each, a relative mid change).

use vike_analytics::book_export::{
    BookExportParameters, BookLabel, BookSampling, export_book_training_data,
};
use vike_user_research::{StudyContext, StudyError, StudyOutcome};

fn text<'a>(params: &'a toml::Value, key: &str) -> Option<&'a str> {
    params.get(key).and_then(toml::Value::as_str)
}

fn whole(params: &toml::Value, key: &str, default: i64) -> i64 {
    params.get(key).and_then(toml::Value::as_integer).unwrap_or(default)
}

/// A float key; a whole number written without a point (`up_threshold = 0`) reads too.
fn number(params: &toml::Value, key: &str, default: f64) -> f64 {
    let value = params.get(key);
    value
        .and_then(toml::Value::as_float)
        .or_else(|| value.and_then(toml::Value::as_integer).map(|v| v as f64))
        .unwrap_or(default)
}

pub fn run(ctx: &StudyContext, params: &toml::Value) -> Result<StudyOutcome, StudyError> {
    let (Some(venue), Some(symbol)) = (text(params, "venue"), text(params, "symbol")) else {
        return Err(StudyError::Study("the recipe must name `venue` and `symbol`".to_string()));
    };
    let updates = match text(params, "lane").unwrap_or("book") {
        "book" => ctx.book_updates(venue, symbol, ctx.window())?,
        "depth" => ctx.depth(venue, symbol, ctx.window())?,
        other => {
            return Err(StudyError::Study(format!(
                "lane {other:?}: expected \"book\" or \"depth\""
            )));
        }
    };
    if updates.is_empty() {
        return Err(StudyError::Study(format!(
            "no recorded book for {venue}/{symbol} in the requested window"
        )));
    }
    let sampling = match params.get("interval_ms") {
        None => BookSampling::EveryEvent,
        Some(_) => BookSampling::Interval { milliseconds: whole(params, "interval_ms", 0) },
    };
    let label = match text(params, "label").unwrap_or("mid_after") {
        "mid_after" => BookLabel::MidAfter,
        "mean_of_next" => BookLabel::MeanOfNext,
        "mean_of_next_against_mean_of_last" => BookLabel::MeanOfNextAgainstMeanOfLast,
        other => {
            return Err(StudyError::Study(format!(
                "label {other:?}: expected \"mid_after\", \"mean_of_next\" or \
                 \"mean_of_next_against_mean_of_last\""
            )));
        }
    };
    let parameters = BookExportParameters {
        levels: usize::try_from(whole(params, "levels", 10)).unwrap_or(0),
        sampling,
        horizon_events: usize::try_from(whole(params, "horizon_events", 10)).unwrap_or(0),
        label,
        up_threshold: number(params, "up_threshold", 0.0001),
        down_threshold: number(params, "down_threshold", 0.0001),
    };
    let data = export_book_training_data(&updates, &parameters).map_err(StudyError::Study)?;

    let count = |label: i8| data.labels.iter().filter(|&&l| l == label).count() as f64;
    let mut out = StudyOutcome::new();
    out.metric("rows", data.rows() as f64)?;
    out.metric("columns", data.columns() as f64)?;
    out.metric("up", count(1))?;
    out.metric("flat", count(0))?;
    out.metric("down", count(-1))?;
    out.metric("skipped_without_book", data.skipped_without_book as f64)?;
    out.metric("skipped_without_label", data.skipped_without_label as f64)?;

    let mut tsv =
        format!("timestamp\tlabel\thorizon_return\tmid\t{}\n", data.column_names().join("\t"));
    for (row, features) in data.features.chunks_exact(data.columns()).enumerate() {
        tsv.push_str(&format!(
            "{}\t{}\t{}\t{}",
            data.timestamps[row], data.labels[row], data.horizon_returns[row], data.mids[row]
        ));
        for value in features {
            tsv.push_str(&format!("\t{value}"));
        }
        tsv.push('\n');
    }
    out.artifact("book_samples.tsv", tsv)?;
    Ok(out)
}
