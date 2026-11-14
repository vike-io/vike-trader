//! The stored market funding series turned into what a run reads: the strategy's `Bar.funding`
//! signal ([`window_join_funding`]), the engine's charges ([`funding_charges`], applied by
//! `vike_simulator::StrategyEngineConfig::funding_schedule`) and the coverage holes a report names
//! ([`funding_gaps`]). Pure: the store read is `super::attach_funding_series`'s.
//!
//! **The schedule is the stored series' own timestamps, never a fixed interval.** A venue settles
//! each symbol on its own cadence (binance USDⓈ-M 4h, 8h or 1h by symbol and 1h at the rate cap,
//! bybit 8h, 4h or 1h, okx 1h to 8h, hyperliquid 1h) and re-cadences symbols over time; the history
//! lists every settlement, so a point IS a funding instant and nothing here assumes a spacing.

use vike_marketdata::{Bar, FundingRatePoint};

/// How a venue's stored funding point is owed, which decides how the run charges it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FundingAccrual {
    /// SETTLED at the point's timestamp on the position held at that instant (binance, bybit, okx,
    /// aster, hyperliquid): a position opened a second after pays nothing, one closed a second after
    /// pays the whole rate.
    AtSettlement,
    /// ACCRUED continuously over the `period_ms` ENDING at the point's timestamp, owed pro rata by
    /// whatever was held during it (deribit: `crates/bridges/deribit/src/history.rs`'s `FundingRateSourceDeribit`
    /// publishes the hour ending at each point). Charged in [`ACCRUAL_SLICE_MS`] slices.
    Continuous {
        /// How long one stored point accrues over.
        period_ms: i64,
    },
}

impl FundingAccrual {
    /// The accrual of `venue`'s stored funding series. Deribit is the one continuous venue; its hour
    /// is the bridge's `FUNDING_RECORD_MS` (this crate links no bridge, so the number is restated
    /// here and the bridge's doc names this function as the reader of its contract).
    #[must_use]
    pub fn of_venue(venue: &str) -> Self {
        match venue {
            "deribit" => FundingAccrual::Continuous { period_ms: 3_600_000 },
            _ => FundingAccrual::AtSettlement,
        }
    }
}

/// Does `symbol` on `venue` name a PERPETUAL, by the spelling that venue's funding-history source
/// accepts (each refuses every other name before asking the venue): `.P` on binance, bybit and aster
/// (`vike_catalog::split_perp_at`; a bare name is the spot pair there), `-SWAP` on okx, `-PERPETUAL`
/// on deribit, and a bare coin on hyperliquid (its spot pairs are `@<index>` or `BASE/QUOTE`). `false`
/// on every other venue: none publishes a funding history this tree reads. The default of
/// `engine.attach_funding` reads it (`super::super::profile::ProfileEngine::attaches_funding`).
#[must_use]
pub fn names_a_perpetual(venue: &str, symbol: &str) -> bool {
    match venue {
        "binance" | "bybit" | "aster" => vike_catalog::split_perp_at(venue, symbol).1,
        "okx" => symbol.ends_with("-SWAP"),
        "deribit" => symbol.ends_with("-PERPETUAL"),
        "hyperliquid" => !symbol.is_empty() && !symbol.contains('/') && !symbol.starts_with('@'),
        _ => false,
    }
}

/// Does a run read `venue:symbol`'s stored funding: `attach_funding` (the `engine.attach_funding` key)
/// when stated, else whether the symbol names a perpetual ([`names_a_perpetual`]). The ONE home of the
/// rule: the run's read (`super::funding_for_series`, both doors) and the data plan
/// (`ProfileEngine::attaches_funding`) both call it.
#[must_use]
pub(crate) fn attaches_funding(attach_funding: Option<bool>, venue: &str, symbol: &str) -> bool {
    attach_funding.unwrap_or_else(|| names_a_perpetual(venue, symbol))
}

/// The slice a continuous accrual is cut into: one minute. A position held for part of an hour pays
/// that part to the minute, and a run on bars of a minute or coarser charges each bar the slices
/// that ended inside it.
pub(crate) const ACCRUAL_SLICE_MS: i64 = 60_000;

/// The engine's charges for one stored series of `(ts, rate, mark)` points, ascending: a settlement
/// venue's points as they are, a continuous venue's each cut into `period / ACCRUAL_SLICE_MS` equal
/// slices ending one slice apart up to the point's timestamp, each carrying the point's mark.
#[must_use]
pub(crate) fn funding_charges(
    points: &[(i64, f64, Option<f64>)],
    accrual: FundingAccrual,
) -> Vec<FundingRatePoint> {
    match accrual {
        FundingAccrual::AtSettlement => points
            .iter()
            .map(|&(ts_ms, rate, mark)| FundingRatePoint {
                timestamp_ms: ts_ms,
                rate,
                premium: None,
                mark,
            })
            .collect(),
        FundingAccrual::Continuous { period_ms } => {
            let n = (period_ms / ACCRUAL_SLICE_MS).max(1);
            let slice = period_ms / n;
            points
                .iter()
                .flat_map(|&(ts, rate, mark)| {
                    (1..=n).map(move |k| FundingRatePoint {
                        timestamp_ms: ts - period_ms + k * slice,
                        rate: rate / n as f64,
                        premium: None,
                        mark,
                    })
                })
                .collect()
        }
    }
}

/// Summary of one [`window_join_funding`] pass, for caller logging and tests.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FundingJoinStats {
    /// price bars that received a funding rate (one per bar, however many points fell in it).
    pub placed: usize,
    /// funding points that fell into a price bar's window ALREADY holding an earlier point this pass
    /// — a price interval COARSER than the funding schedule. Their rates are SUMMED onto the bar
    /// (each is also charged at its own instant by the engine's schedule); counted for the log.
    pub collisions: usize,
    /// funding points whose ts predates the FIRST price bar (no window) — dropped from the signal.
    pub dropped_before: usize,
}

/// Window-join a sorted market `funding` series onto the replayed PRICE bars: bar `i` carries the
/// SUM of the rates of every point in `(bar[i-1].ts, bar[i].ts]` (bar 0: the point at its own ts), so
/// a bar shows the funding that came due by its open. A bar with no point in its window is left with
/// `funding = None`; a point after the last bar belongs to no bar of this run.
///
/// **Never forward-fills, and never drops a point inside the run.** One rate spread across every bar
/// until the next point would read as many fundings as bars; a coarse bar keeping only its LAST point
/// (what this did until 2026-11-08) read a quarter of hyperliquid's hourly funding on 4h bars and a
/// 24th on 1d bars. The strategy's signal is now the bar's whole due funding, and the CHARGE is the
/// engine's schedule ([`funding_charges`]), each point at its own instant.
///
/// Both inputs MUST be sorted ascending by ts; the walk is a single two-pointer merge — O(n+m). `funding`
/// is `(ts, rate)`. Mutates `price_bars` in place and returns a [`FundingJoinStats`].
pub fn window_join_funding(price_bars: &mut [Bar], funding: &[(i64, f64)]) -> FundingJoinStats {
    let mut stats = FundingJoinStats::default();
    let mut fi = 0usize;
    let Some(first_timestamp) = price_bars.first().map(|b| b.timestamp) else { return stats };
    while fi < funding.len() && funding[fi].0 < first_timestamp {
        stats.dropped_before += 1;
        fi += 1;
    }
    for bar in price_bars.iter_mut() {
        let mut due: Option<f64> = None;
        while fi < funding.len() && funding[fi].0 <= bar.timestamp {
            if due.is_some() {
                stats.collisions += 1;
            }
            due = Some(due.unwrap_or(0.0) + funding[fi].1);
            fi += 1;
        }
        if let Some(rate) = due {
            bar.funding = Some(rate);
            stats.placed += 1;
        }
    }
    stats
}

/// The holes in a stored funding series over the run's window `[from, to]`, as report lines: an
/// interior spacing more than one and a half times the WIDER of its neighbouring spacings, and a
/// leading or trailing stretch more than one and a half times the first or last spacing. The
/// yardstick is the series' own local spacing, never a fixed interval, so a symbol re-cadenced from
/// 8h to 4h (or capped down to 1h) is not a hole, and a missing settlement on any schedule is. A hole
/// is charged nothing, so a run must say it has one. Empty when the series has fewer than two points
/// (no spacing to measure) or none is found.
#[must_use]
pub(crate) fn funding_gaps(symbol: &str, ts: &[i64], from: i64, to: i64) -> Vec<String> {
    if ts.len() < 2 {
        return Vec::new();
    }
    let sp: Vec<i64> = ts.windows(2).map(|w| w[1] - w[0]).collect();
    let wider_than = |gap: i64, yardstick: i64| yardstick > 0 && gap * 2 > yardstick * 3;
    let hours = |ms: i64| ms as f64 / 3_600_000.0;
    let interior: Vec<usize> = (0..sp.len())
        .filter(|&i| {
            let previous = if i > 0 { sp[i - 1] } else { 0 };
            wider_than(sp[i], previous.max(sp.get(i + 1).copied().unwrap_or(0)))
        })
        .collect();
    let mut out = Vec::new();
    if let Some(&i) = interior.iter().max_by_key(|&&i| sp[i]) {
        out.push(format!(
            "{symbol}: the stored funding series has {} gap(s) wider than 1.5x the spacing around \
             them (the widest {:.1}h, after {}); no funding is charged inside a gap — refetch \
             `<venue>:{symbol}:funding` over it",
            interior.len(),
            hours(sp[i]),
            ts[i],
        ));
    }
    let (first, last) = (ts[0], ts[ts.len() - 1]);
    if wider_than(first - from, sp[0]) {
        out.push(format!(
            "{symbol}: the stored funding series starts {:.1}h after the run does (first point \
             {first}); the run charges no funding before it",
            hours(first - from),
        ));
    }
    if wider_than(to - last, sp[sp.len() - 1]) {
        out.push(format!(
            "{symbol}: the stored funding series ends {:.1}h before the run does (last point \
             {last}); the run charges no funding after it",
            hours(to - last),
        ));
    }
    out
}

#[cfg(test)]
#[path = "funding_tests.rs"]
mod funding_tests;
