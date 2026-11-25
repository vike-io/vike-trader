//! `instrument_graph` — the book's chapter-49 case (W18): THE CATALOG AS A GRAPH, and which venue's
//! book leads the others.
//!
//! The study reads today's kept instrument rows (`StudyContext::kept_instruments`, the catalog the
//! host read for it) and builds the instrument graph (`vike_analytics::instrument_graph`: one asset
//! across venues, Polymarket neg-risk sets, each venue's spot–perpetual–future–option links). It then
//! takes the spot pairs and perpetuals of ONE asset, reads their bars, aligns them on their common
//! bar times and measures who leads whom: one lead-lag edge per pair whose lead-lag ratio reaches
//! `minimum_ratio`, each instrument's net leadership (out-degree minus in-degree, over the others),
//! and its eigenvector centrality in that network, each edge weighted by the size of its peak
//! correlation.
//!
//! ⚠ **Today's catalog over a past window.** The rows are the venues' lists as the host read them, so
//! an instrument delisted since the window is missing (survivorship bias) and one listed since is
//! present (and has no bars then). A row's bars are read under its STORE name, the store's spelling of the
//! user name the row stores (decision 0290: `vike_paths::store_path::store_name_of_user_name`, so okx's
//! `BTC-USDT-SWAP` is the series `BTCUSDT.P`); a row that stores no user name is a series refused.
//!
//! Recipe: `asset` and `interval` (REQUIRED, no default instrument), `max_lag` (3), `minimum_ratio`
//! (1.5).
//!
//! Metrics: `kept_rows`, `assets`, `assets_on_two_or_more_venues`, `neg_risk_sets`, `underlying_links`,
//! `asset_members`, `series_read`, `series_without_bars`, `series_refused`, `observations`, `edges`,
//! `centrality_defined`. Artifacts: `instruments.tsv` (a row per series read: its leadership and
//! centrality) and `edges.tsv` (a row per lead-lag edge).

use std::collections::BTreeMap;

use vike_analytics::instrument_graph::{
    eigenvector_centrality, instrument_graph, lead_lag_edges, net_leadership,
};
use vike_model::{Contract, Instrument, InstrumentNaming};
use vike_paths::store_path::store_name_of_user_name;
use vike_user_research::{StudyContext, StudyError, StudyOutcome};

#[cfg(test)]
pub mod fixture_store;

fn refuse(why: String) -> StudyError {
    StudyError::Study(why)
}

fn required<'a>(params: &'a toml::Value, key: &str) -> Result<&'a str, StudyError> {
    params.get(key).and_then(toml::Value::as_str).ok_or_else(|| {
        refuse(format!("the recipe must name `{key}` (the asset and the bars' interval)"))
    })
}

/// A kept row's series in the store: the store name of the user name it stores, `None` for a row the grammar
/// names not (or a list written before names were stored).
fn store_name(row: &Instrument) -> Option<String> {
    match &row.naming {
        Some(InstrumentNaming::User(user)) => Some(store_name_of_user_name(user)),
        _ => None,
    }
}

pub fn run(ctx: &StudyContext, params: &toml::Value) -> Result<StudyOutcome, StudyError> {
    let (asset, interval) = (required(params, "asset")?, required(params, "interval")?);
    let max_lag = params.get("max_lag").and_then(toml::Value::as_integer).unwrap_or(3);
    if max_lag < 1 {
        return Err(refuse(format!("`max_lag` must be at least 1, got {max_lag}")));
    }
    let max_lag = max_lag as usize;
    let minimum_ratio = params.get("minimum_ratio").and_then(toml::Value::as_float).unwrap_or(1.5);

    let kept = ctx.kept_instruments().map_err(StudyError::Data)?;
    let graph = instrument_graph(&kept.rows);
    let group = graph
        .assets
        .iter()
        .find(|group| group.asset.eq_ignore_ascii_case(asset.trim()))
        .ok_or_else(|| refuse(format!("no kept row states the asset `{asset}`")))?;

    // The asset's spot pairs and perpetuals, in the kept lists' order, each named `venue:store name` in the
    // tables, with its closes by time.
    let (mut without_bars, mut refused) = (0usize, 0usize);
    let mut read: Vec<(String, BTreeMap<i64, f64>)> = Vec::new();
    for row in group.members.iter().map(|&i| &kept.rows[i]) {
        if !matches!(row.contract, Contract::CryptoSpot | Contract::Perp(_)) {
            continue;
        }
        let Some(series) = store_name(row) else {
            refused += 1;
            continue;
        };
        match ctx.bars(&row.venue, &series, interval, ctx.window()) {
            Ok(bars) if bars.is_empty() => without_bars += 1,
            Ok(bars) => read.push((
                format!("{}:{series}", row.venue),
                bars.iter().map(|b| (b.timestamp, b.close)).collect(),
            )),
            // A name the store refuses (a path-hostile spelling) is a series this study cannot read.
            Err(_) => refused += 1,
        }
    }
    if read.len() < 2 {
        return Err(refuse(format!(
            "`{asset}` has {} spot or perpetual series with `{interval}` bars in the window; a \
             lead-lag needs two",
            read.len()
        )));
    }
    let common: Vec<i64> = read[0]
        .1
        .keys()
        .copied()
        .filter(|ts| read.iter().all(|(_, c)| c.contains_key(ts)))
        .collect();
    let series: Vec<Vec<f64>> = read
        .iter()
        .map(|(_, closes)| common.windows(2).map(|w| closes[&w[1]] / closes[&w[0]] - 1.0).collect())
        .collect();
    let edges = lead_lag_edges(&series, max_lag, minimum_ratio).ok_or_else(|| {
        refuse(format!(
            "{} common bars give returns too few (or not finite) for lags up to {max_lag}",
            common.len()
        ))
    })?;
    let nodes = series.len();
    let leadership = net_leadership(&edges, nodes);
    let mut weights = vec![vec![0.0; nodes]; nodes];
    for edge in &edges {
        weights[edge.leader][edge.follower] = edge.correlation.abs();
        weights[edge.follower][edge.leader] = edge.correlation.abs();
    }
    let centrality = eigenvector_centrality(&weights);

    let mut out = StudyOutcome::new();
    let on_two_or_more = graph.assets.iter().filter(|group| group.venues.len() > 1).count();
    for (metric, value) in [
        ("kept_rows", kept.rows.len()),
        ("assets", graph.assets.len()),
        ("assets_on_two_or_more_venues", on_two_or_more),
        ("neg_risk_sets", graph.neg_risk_sets.len()),
        ("underlying_links", graph.links.len()),
        ("asset_members", group.members.len()),
        ("series_read", nodes),
        ("series_without_bars", without_bars),
        ("series_refused", refused),
        ("observations", series[0].len()),
        ("edges", edges.len()),
        ("centrality_defined", usize::from(centrality.is_some())),
    ] {
        out.metric(metric, value as f64)?;
    }
    let mut table = String::from("instrument\tleadership\tcentrality\n");
    for (i, (name, _)) in read.iter().enumerate() {
        let score = centrality.as_ref().map_or_else(|| "-".to_string(), |c| format!("{:.6}", c[i]));
        table.push_str(&format!("{name}\t{:.6}\t{score}\n", leadership[i]));
    }
    out.artifact("instruments.tsv", table)?;
    let mut table = String::from("leader\tfollower\tlag\tcorrelation\tratio\n");
    for edge in &edges {
        table.push_str(&format!(
            "{}\t{}\t{}\t{:.6}\t{:.3}\n",
            read[edge.leader].0, read[edge.follower].0, edge.lag, edge.correlation, edge.ratio
        ));
    }
    out.artifact("edges.tsv", table)?;
    Ok(out)
}
