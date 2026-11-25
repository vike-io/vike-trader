//! The instrument graph (the book's knowledge-graph theme): how the catalog's rows relate to each
//! other, and which of them lead the others' prices.
//!
//! **The structure, from the catalog's rows** ([`instrument_graph`] over `vike_model::Instrument`s,
//! the kept lists of every venue as the caller holds them):
//! - [`AssetGroup`]: the SAME ASSET across venues, every row whose base the row states as the same
//!   word. The word is the base upper-cased and trimmed, the way decision 0290's user names spell a
//!   base on every venue; a NAME is never read, so a row that states no base belongs to no group, and
//!   a scaled coin (`1000PEPE`, hyperliquid's `kPEPE`) is the asset its row states, not the coin.
//! - [`NegRiskSet`]: a Polymarket neg-risk set, every outcome token whose row carries the set's
//!   `negRiskMarketID` (the polymarket catalog's `wire.group`; it is read on outcome rows only,
//!   because okx and hyperliquid keep other ids there).
//! - [`UnderlyingLink`]: the spot–perpetual–future–option chain of one venue. A derivative links to
//!   the rows of its own venue with the same base and quote one step down: an option to the dated
//!   future of its expiry, else the perpetual, else the spot; a dated future to the perpetual, else
//!   the spot; a perpetual to the spot. Every row of the nearest step is linked (no tie is broken).
//!
//! **The features, from aligned return series** (one series per instrument, one entry per common
//! observation; plain vectors, so a study file needs no matrix type to call them):
//! - [`lead_lag_edges`] orients one edge per pair from leader to follower by
//!   `vike_math::time_series::lead_lag` (the cross-correlation of two series at every lag up to a
//!   bound, its peak and the lead-lag ratio of Huth and Abergel, a pure formula); [`net_leadership`]
//!   is each node's out-degree minus its in-degree.
//! - [`eigenvector_centrality`]: the Perron vector of a symmetric non-negative weight matrix, through
//!   `vike_math::linear_algebra::symmetric_eigen` (no graph crate: a dense matrix of a few hundred
//!   nodes is what a study holds).
//!
//! Pure: no clock, no file, no randomness; every index is a position in the caller's slice.
//! No transcendental is called.

use std::collections::BTreeMap;

use vike_math::linear_algebra::{Matrix, symmetric_eigen};
use vike_model::{Contract, Instrument};

use vike_math::time_series::lead_lag;

/// One asset across venues, from [`instrument_graph`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssetGroup {
    /// The base as every member states it, upper-cased and trimmed (`BTC`, `EUR`, `AAPL`).
    pub asset: String,
    /// The members' positions in the rows, ascending.
    pub members: Vec<usize>,
    /// The distinct venues of the members, sorted: more than one is the same asset across venues.
    pub venues: Vec<String>,
}

/// One Polymarket neg-risk set, from [`instrument_graph`]: the outcome tokens of its markets, whose
/// `Yes` prices sum to one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NegRiskSet {
    /// The set's `negRiskMarketID`, as the rows carry it.
    pub group: String,
    /// The members' positions in the rows, ascending.
    pub members: Vec<usize>,
}

/// A derivative and the row of its own venue it is written on, from [`instrument_graph`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnderlyingLink {
    /// The derivative's position in the rows.
    pub derivative: usize,
    /// The underlying's position in the rows.
    pub underlying: usize,
}

/// The catalog's structure, from [`instrument_graph`]. Every list is in a fixed order: groups by
/// their key, links by derivative then underlying.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InstrumentGraph {
    /// One group per asset, by its word.
    pub assets: Vec<AssetGroup>,
    /// One set per `negRiskMarketID`.
    pub neg_risk_sets: Vec<NegRiskSet>,
    /// Each derivative's links to its underlying rows.
    pub links: Vec<UnderlyingLink>,
}

/// Where a row stands in its venue's spot–perpetual–future–option chain; `None` off the chain (a
/// CFD, a combo, an outcome token, a bond, a fund, a warrant).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    Spot,
    Perpetual,
    Future { expiry_ms: i64 },
    Option { expiry_ms: i64 },
}

fn step_of(contract: &Contract) -> Option<Step> {
    match contract {
        Contract::CryptoSpot
        | Contract::Fx(_)
        | Contract::Equity(_)
        | Contract::Etf(_)
        | Contract::Index
        | Contract::Commodity => Some(Step::Spot),
        Contract::Perp(_) => Some(Step::Perpetual),
        Contract::Future(future) => Some(Step::Future { expiry_ms: future.expiry_ms }),
        Contract::Option(option) => Some(Step::Option { expiry_ms: option.expiry_ms }),
        Contract::Bond
        | Contract::Fund
        | Contract::Warrant
        | Contract::Cfd(_)
        | Contract::Combo(_)
        | Contract::PredictionOutcome(_) => None,
    }
}

/// A stated base or quote as one word on every venue; `None` when the row states none.
fn word(text: &str) -> Option<String> {
    let word = text.trim().to_ascii_uppercase();
    (!word.is_empty()).then_some(word)
}

/// The asset groups, the neg-risk sets and the underlying links of `rows` (module doc).
pub fn instrument_graph(rows: &[Instrument]) -> InstrumentGraph {
    let mut assets: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    let mut sets: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    let mut books: BTreeMap<(&str, String, String), Vec<usize>> = BTreeMap::new();
    for (i, row) in rows.iter().enumerate() {
        if matches!(row.contract, Contract::PredictionOutcome(_)) {
            // An outcome row's base is its outcome label (`Yes`), never an asset.
            if let Some(group) = row.wire.group.as_deref().filter(|g| !g.is_empty()) {
                sets.entry(group.to_string()).or_default().push(i);
            }
            continue;
        }
        let Some(base) = word(&row.base) else { continue };
        if let (Some(quote), Some(_)) = (word(&row.quote), step_of(&row.contract)) {
            books.entry((row.venue.as_str(), base.clone(), quote)).or_default().push(i);
        }
        assets.entry(base).or_default().push(i);
    }
    let assets = assets
        .into_iter()
        .map(|(asset, members)| {
            let mut venues: Vec<String> = members.iter().map(|&i| rows[i].venue.clone()).collect();
            venues.sort();
            venues.dedup();
            AssetGroup { asset, members, venues }
        })
        .collect();
    let neg_risk_sets =
        sets.into_iter().map(|(group, members)| NegRiskSet { group, members }).collect();
    let mut links = Vec::new();
    for book in books.values() {
        let at = |wanted: Step| -> Vec<usize> {
            book.iter().copied().filter(|&j| step_of(&rows[j].contract) == Some(wanted)).collect()
        };
        for &derivative in book {
            // The steps down, nearest first; the first that holds a row is the underlying.
            let steps = match step_of(&rows[derivative].contract) {
                Some(Step::Perpetual) => vec![at(Step::Spot)],
                Some(Step::Future { .. }) => vec![at(Step::Perpetual), at(Step::Spot)],
                Some(Step::Option { expiry_ms }) => {
                    vec![at(Step::Future { expiry_ms }), at(Step::Perpetual), at(Step::Spot)]
                }
                Some(Step::Spot) | None => Vec::new(),
            };
            let underlying = steps.into_iter().find(|step| !step.is_empty()).unwrap_or_default();
            links.extend(
                underlying.into_iter().map(|u| UnderlyingLink { derivative, underlying: u }),
            );
        }
    }
    links.sort_by_key(|link| (link.derivative, link.underlying));
    InstrumentGraph { assets, neg_risk_sets, links }
}

/// One directed lead-lag edge, from [`lead_lag_edges`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LeadLagEdge {
    /// The leading series' position.
    pub leader: usize,
    /// The following series' position.
    pub follower: usize,
    /// The peak lag seen from the leader: positive when the follower's peak comes after it.
    pub lag: i64,
    /// The cross-correlation at that lag.
    pub correlation: f64,
    /// The lead-lag ratio seen from the leader, at least one.
    pub ratio: f64,
}

/// One edge per pair of `series` (each aligned, one entry per common observation), oriented from the
/// leader to the follower by [`lead_lag`]'s ratio, kept when the ratio reaches `minimum_ratio` (pass
/// more than one: at exactly one neither leads, and the earlier series is named the leader). In the
/// order of the pair's positions.
///
/// `None` where [`lead_lag`] is for any pair.
pub fn lead_lag_edges(
    series: &[Vec<f64>],
    max_lag: usize,
    minimum_ratio: f64,
) -> Option<Vec<LeadLagEdge>> {
    let mut edges = Vec::new();
    for i in 0..series.len() {
        for j in i + 1..series.len() {
            let pair = lead_lag(&series[i], &series[j], max_lag)?;
            let edge = if pair.lead_lag_ratio >= 1.0 {
                LeadLagEdge {
                    leader: i,
                    follower: j,
                    lag: pair.peak_lag,
                    correlation: pair.peak_correlation,
                    ratio: pair.lead_lag_ratio,
                }
            } else {
                LeadLagEdge {
                    leader: j,
                    follower: i,
                    lag: -pair.peak_lag,
                    correlation: pair.peak_correlation,
                    ratio: 1.0 / pair.lead_lag_ratio,
                }
            };
            if edge.ratio >= minimum_ratio {
                edges.push(edge);
            }
        }
    }
    Some(edges)
}

/// Each of `nodes` nodes' out-degree minus its in-degree over `edges`, over `nodes - 1`: `1` leads
/// every other node, `-1` follows every other node (the net degree of a directed graph, normalised).
/// All zero for fewer than two nodes.
///
/// # Panics
/// An edge names a node at or past `nodes`.
pub fn net_leadership(edges: &[LeadLagEdge], nodes: usize) -> Vec<f64> {
    let mut net = vec![0.0; nodes];
    if nodes < 2 {
        return net;
    }
    for edge in edges {
        net[edge.leader] += 1.0;
        net[edge.follower] -= 1.0;
    }
    let others = (nodes - 1) as f64;
    net.iter().map(|v| v / others).collect()
}

/// How close the two largest eigenvalues may come, relative to the largest, before the leading
/// eigenvector is no longer one vector (two equal components of a disconnected graph).
const REPEATED_EIGENVALUE: f64 = 1e-12;

/// The eigenvector centrality of the undirected graph whose symmetric weights are the lower triangle
/// of `weights` (one row per node; the diagonal is each node's weight to itself, pass zeros for a
/// plain graph): the unit eigenvector of the largest eigenvalue, every entry non-negative (Bonacich,
/// "Factoring and weighting approaches to status scores and clique identification", Journal of
/// Mathematical Sociology 2(1), 1972). On a disconnected graph the nodes outside the strongest
/// component score zero.
///
/// `None` when `weights` is empty or not square, a weight is negative or not finite, no weight is
/// positive, or the largest eigenvalue is repeated (the leading eigenvector is not one vector).
pub fn eigenvector_centrality(weights: &[Vec<f64>]) -> Option<Vec<f64>> {
    let n = weights.len();
    if n == 0 || weights.iter().any(|row| row.len() != n) {
        return None;
    }
    let weights = Matrix::from_rows(weights)?;
    for i in 0..n {
        for j in 0..=i {
            let weight = weights.get(i, j);
            if !(weight.is_finite() && weight >= 0.0) {
                return None;
            }
        }
    }
    let eigen = symmetric_eigen(&weights)?;
    let largest = eigen.values[0];
    if largest <= 0.0
        || eigen.values.get(1).is_some_and(|&next| next >= largest * (1.0 - REPEATED_EIGENVALUE))
    {
        return None;
    }
    // Perron–Frobenius: a simple largest eigenvalue of a non-negative matrix has a non-negative
    // eigenvector, so a negative entry is a rounding of zero.
    Some(eigen.vectors.column(0).iter().map(|v| v.abs()).collect())
}

#[path = "instrument_graph_tests.rs"]
#[cfg(test)]
mod instrument_graph_tests;
