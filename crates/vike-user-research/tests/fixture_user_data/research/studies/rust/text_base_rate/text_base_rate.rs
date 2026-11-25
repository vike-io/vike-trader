//! `text_base_rate` — the book's chapter-48 case (W17): FIND THE PAST TEXTS LIKE THIS ONE, and turn
//! what followed them into a base-rate feature.
//!
//! For every text received in the study window (the query): retrieve, among the texts received before
//! it, the `neighbours` most similar ones (`vike_rag`'s hybrid ranking: BM25 over the chunks plus cosine
//! over their stored embeddings, fused by reciprocal rank fusion), one vote per text, keeping only those
//! whose outcome was already known at the query's time. The base rate is the share of them after which
//! the instrument ROSE over `horizon_bars` bars. The study reports the base rates and how often their
//! side (above or below one half) matched the query's own outcome.
//!
//! **Point in time, every step.** The texts and their vectors are read as of the window's end
//! (`texts_as_of`, `text_scores_as_of`), then every ranking is asked AS OF the query's received time, so
//! a later text is neither a neighbour nor a statistic of the keyword ranking. A neighbour's outcome
//! counts only once its last bar had CLOSED (the bar after it had opened) by the query's time.
//!
//! **The query is a STORED text.** Its vector was computed host-side by `vike-nlp score`, like every
//! other: a study cannot run the model, so a free-form question is not a query here. The outcome is a
//! price series; a Polymarket market text with its resolution as the outcome needs that text kind in
//! the store first (the plan's W38).
//!
//! Recipe: `venue`, `symbol`, `interval` (the outcome's bars; REQUIRED, no default instrument),
//! `horizon_bars` (60), `neighbours` (10), `text_kind` (`news`), `collector` (`rss`), `model`
//! (`all-minilm-l6-v2`, the embedding series), `model_label` (the exact `name@revision`; needed only
//! when the series holds two revisions, whose vectors are not comparable), `history_days` (30: how long
//! before the window a text may be a neighbour).
//!
//! Metrics: `texts`, `chunks`, `embedded_chunks`, `queries`, `with_base_rate`, `mean_base_rate`,
//! `scored`, `side_hits`, `side_hit_rate`. Artifacts: `base_rates.tsv` (a row per query) and
//! `context.txt` (the last query's neighbours, assembled under their citations).

use vike_data::TimestampRange;
use vike_marketdata::Bar;
use vike_rag::context::assemble_context;
use vike_rag::search::{Corpus, Hit, TextSource, attach_vectors, chunks_of};
use vike_user_research::{StudyContext, StudyError, StudyOutcome};

#[cfg(test)]
pub mod fixture_store;

const DAY_MS: i64 = 86_400_000;

fn refuse(why: String) -> StudyError {
    StudyError::Study(why)
}

fn required<'a>(params: &'a toml::Value, key: &str) -> Result<&'a str, StudyError> {
    params.get(key).and_then(toml::Value::as_str).ok_or_else(|| {
        refuse(format!(
            "the recipe must name `{key}` (the outcome's bars: venue, symbol, interval)"
        ))
    })
}

fn text<'a>(params: &'a toml::Value, key: &str, default: &'a str) -> &'a str {
    params.get(key).and_then(toml::Value::as_str).unwrap_or(default)
}

fn count(params: &toml::Value, key: &str, default: i64, least: i64) -> Result<i64, StudyError> {
    let value = params.get(key).and_then(toml::Value::as_integer).unwrap_or(default);
    if value < least {
        return Err(refuse(format!("`{key}` must be at least {least}, got {value}")));
    }
    Ok(value)
}

/// The return over `horizon` bars after `received_ms` and when it became known: in at the open of the
/// first bar opening at or after the text, out at the close of the bar `horizon - 1` later, known when
/// the bar after that opened. `None` when the bars do not reach that far.
fn outcome(bars: &[Bar], received_ms: i64, horizon: usize) -> Option<(f64, i64)> {
    let first = bars.partition_point(|b| b.timestamp < received_ms);
    let last = first + horizon - 1;
    let known_ms = bars.get(last + 1)?.timestamp;
    let entry = bars[first].open;
    (entry > 0.0).then(|| (bars[last].close / entry - 1.0, known_ms))
}

pub fn run(ctx: &StudyContext, params: &toml::Value) -> Result<StudyOutcome, StudyError> {
    let (venue, symbol, interval) =
        (required(params, "venue")?, required(params, "symbol")?, required(params, "interval")?);
    let horizon = count(params, "horizon_bars", 60, 1)? as usize;
    let neighbours = count(params, "neighbours", 10, 1)? as usize;
    let history_ms = count(params, "history_days", 30, 0)? * DAY_MS;
    let (text_kind, collector) =
        (text(params, "text_kind", "news"), text(params, "collector", "rss"));
    let model = text(params, "model", "all-minilm-l6-v2");
    let (Some(start), Some(end)) = (ctx.window().start, ctx.window().end) else {
        return Err(refuse("the study needs a bounded window (--from and --to)".to_string()));
    };
    let lookback = end - start + history_ms;

    // Everything a decision at the window's end could read; each query below sees only its own past.
    let texts = ctx.texts_as_of(text_kind, collector, end, lookback)?;
    let mut scores = ctx.text_scores_as_of(text_kind, model, end, lookback)?;
    if let Some(label) = params.get("model_label").and_then(toml::Value::as_str) {
        scores.retain(|s| s.model == label);
    }
    let mut labels: Vec<&str> = scores.iter().map(|s| s.model.as_str()).collect();
    labels.sort_unstable();
    labels.dedup();
    if labels.len() > 1 {
        return Err(refuse(format!(
            "the {model} series holds {labels:?}: vectors of two revisions are not comparable, so \
             name one with `model_label`"
        )));
    }

    let mut chunks = Vec::new();
    for row in &texts {
        let of = TextSource {
            text_id: row.text_id(),
            source: row.source.clone(),
            published_ms: row.published_ms,
            received_ms: row.received_ms,
            url: row.url.clone(),
        };
        chunks.extend(chunks_of(&of, &row.title, &row.summary, &row.body));
    }
    let mut vectors = Vec::with_capacity(scores.len());
    for score in &scores {
        let vector = score.value_list().map_err(refuse)?;
        vectors.push((score.text_id.clone(), score.received_ms, score.chunk, vector));
    }
    let embedded = attach_vectors(&mut chunks, vectors);
    let corpus = Corpus::new(chunks);

    let bars = ctx.bars(venue, symbol, interval, TimestampRange::of(end - lookback, end))?;
    let outcomes: Vec<Option<(f64, i64)>> =
        corpus.chunks().iter().map(|c| outcome(&bars, c.of.received_ms, horizon)).collect();

    let mut table = String::from("received_ms\tsource\turl\tneighbours\tbase_rate\toutcome\n");
    let (mut queries, mut rates, mut scored, mut hits) = (0usize, Vec::new(), 0usize, 0usize);
    let mut context = String::new();
    for (at, query) in corpus.chunks().iter().enumerate() {
        let as_of = query.of.received_ms;
        if query.index != 0 || query.vector.is_empty() || as_of < start {
            continue;
        }
        queries += 1;
        let mut picked: Vec<Hit> = Vec::new();
        let mut voted: Vec<&str> = vec![query.of.text_id.as_str()];
        for hit in corpus.hybrid_ranking(&query.text, &query.vector, as_of) {
            let of = &corpus.chunks()[hit.chunk].of;
            let known = matches!(outcomes[hit.chunk], Some((_, known_ms)) if known_ms <= as_of);
            if known && !voted.contains(&of.text_id.as_str()) {
                voted.push(of.text_id.as_str());
                picked.push(hit);
                if picked.len() == neighbours {
                    break;
                }
            }
        }
        let rose =
            picked.iter().filter(|h| outcomes[h.chunk].is_some_and(|(r, _)| r > 0.0)).count();
        let rate = if picked.is_empty() { f64::NAN } else { rose as f64 / picked.len() as f64 };
        let own = outcomes[at].map_or(f64::NAN, |(r, _)| r);
        if !picked.is_empty() {
            rates.push(rate);
            if rate != 0.5 && own.is_finite() && own != 0.0 {
                scored += 1;
                hits += usize::from((rate > 0.5) == (own > 0.0));
            }
            context = format!("query: {}\n\n{}", query.text, assemble_context(&corpus, &picked));
        }
        table.push_str(&format!(
            "{as_of}\t{}\t{}\t{}\t{rate}\t{own}\n",
            query.of.source,
            query.of.url,
            picked.len()
        ));
    }

    let mean_rate = if rates.is_empty() {
        f64::NAN
    } else {
        vike_math::summation::compensated_sum(rates.iter().copied()) / rates.len() as f64
    };
    let mut out = StudyOutcome::new();
    out.metric("texts", texts.len() as f64)?;
    out.metric("chunks", corpus.chunks().len() as f64)?;
    out.metric("embedded_chunks", embedded as f64)?;
    out.metric("queries", queries as f64)?;
    out.metric("with_base_rate", rates.len() as f64)?;
    out.metric("mean_base_rate", mean_rate)?;
    out.metric("scored", scored as f64)?;
    out.metric("side_hits", hits as f64)?;
    out.metric("side_hit_rate", if scored == 0 { f64::NAN } else { hits as f64 / scored as f64 })?;
    out.artifact("base_rates.tsv", table)?;
    out.artifact("context.txt", context)?;
    Ok(out)
}
