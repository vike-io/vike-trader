//! Where a bar run's bars come from (`[data] source` / `[data] file`, decision 0268): the one read
//! `load_profile_bars` and `load_profile_detail_bars` make per series, and what the report says about it.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use vike_data::csv_bars::CsvBarsColumns;
use vike_data::store::store_kind::FILE_SOURCE;
use vike_data::{HistoryStore, HistoryStoreCsv, PlannedRead, TimestampRange};
use vike_marketdata::Bar;
use vike_model::run_request::replay::ReplayKind;

use crate::HarnessError;
use crate::run::profile::BarSource;
use crate::run::profile::ProfileBacktest;

/// The bars of `(venue, symbol)` at `interval` in `range`, from where the profile says: the venue's own
/// candles (`HistoryStore::load_bars`, unchanged), a lane of `store` (`load_bars_sourced_capped`), or the
/// file (`HistoryStoreCsv`, read through the same sourced verb under [`FILE_SOURCE`]).
///
/// A lane or a file holding no bar in the window is REFUSED: naming one says the bars are there, and an
/// empty run would read as a strategy that did nothing. The venue's read keeps its old answer, split-adjusted
/// where the operator entered a split of the series (`KeptRows::adjust_for_splits`; each split's report line is
/// added to `split_lines` once). A lane's or a file's bars are read as they are: a file may come adjusted already.
pub(crate) fn load_series_bars(
    profile: &ProfileBacktest,
    store: &Arc<dyn HistoryStore + Send + Sync>,
    venue: &str,
    symbol: &str,
    interval: &str,
    range: TimestampRange,
    split_lines: &mut Vec<String>,
) -> Result<Vec<Bar>, HarnessError> {
    let data = |e: vike_data::HistoryStoreError| HarnessError::Data(e.to_string());
    let (bars, from) = match profile.data.bar_source()? {
        BarSource::Venue => {
            let mut bars = store.load_bars(venue, symbol, interval, range).map_err(data)?;
            for line in profile.kept.adjust_for_splits(venue, symbol, &mut bars)? {
                if !split_lines.contains(&line) {
                    split_lines.push(line);
                }
            }
            return Ok(bars);
        }
        BarSource::Lane(lane) => (
            store
                .load_bars_sourced_capped(venue, lane, symbol, interval, range, None)
                .map_err(data)?,
            format!("the lane `{lane}`"),
        ),
        BarSource::File(written) => {
            let path = file_path(profile, written);
            let format = profile.data.csv_format()?;
            let file = HistoryStoreCsv::open(
                &path,
                &CsvBarsColumns::Header,
                &format,
                venue,
                symbol,
                interval,
            )
            .map_err(data)?;
            let bars = file
                .load_bars_sourced_capped(venue, FILE_SOURCE, symbol, interval, range, None)
                .map_err(data)?;
            (bars, format!("the file {}", path.display()))
        }
    };
    if bars.is_empty() {
        return Err(HarnessError::Data(format!(
            "{from} holds no {interval} bars for ({venue:?}, {symbol:?}) between data.from and \
             data.to, so this run would trade nothing — check the window and the series it names"
        )));
    }
    Ok(bars)
}

/// The store reads `load_profile_bars` makes through [`load_series_bars`], named ahead
/// (`crate::run::data_plan::planned_reads`'s rule): each series' bars over the window, from the venue or the lane.
/// None on the tick lane, and none for a file's bars, which no store reads.
pub(crate) fn price_reads(profile: &ProfileBacktest) -> Vec<PlannedRead> {
    let (ReplayKind::Bar, Ok(range), Ok(source)) =
        (profile.data.kind, profile.range(), profile.data.bar_source())
    else {
        return Vec::new();
    };
    let interval = &profile.data.interval;
    profile
        .data
        .resolved_series()
        .into_iter()
        .filter_map(|s| {
            let (venue, symbol, interval) = (s.venue, s.symbol, interval.clone());
            match source {
                BarSource::Venue => Some(PlannedRead::Bars { venue, symbol, interval, range }),
                BarSource::Lane(lane) => Some(PlannedRead::BarsSourcedCapped {
                    venue,
                    source: lane.to_string(),
                    symbol,
                    interval,
                    range,
                    budget: None,
                }),
                BarSource::File(_) => None,
            }
        })
        .collect()
}

/// The file `data.file` names: a relative path resolves against the profile's own directory, as
/// `[engine.resolution] path` does.
fn file_path(profile: &ProfileBacktest, written: &str) -> PathBuf {
    match (Path::new(written).is_relative(), profile.base_dir.as_deref()) {
        (true, Some(dir)) => dir.join(written),
        _ => PathBuf::from(written),
    }
}

/// What the report says this run read, `None` for the venue's own candles: the lane, or `file:` and the
/// resolved path. The realism stamp's `data.source` value, and the subject of [`source_line`].
pub(crate) fn source_label(profile: &ProfileBacktest) -> Option<String> {
    match profile.data.bar_source().ok()? {
        BarSource::Venue => None,
        BarSource::Lane(lane) => Some(lane.to_string()),
        BarSource::File(written) => {
            Some(format!("{FILE_SOURCE}:{}", file_path(profile, written).display()))
        }
    }
}

/// The run's one report line naming where its bars came from, `None` for the venue's own candles.
pub(super) fn source_line(profile: &ProfileBacktest) -> Option<String> {
    let line = match profile.data.bar_source().ok()? {
        BarSource::Venue => return None,
        BarSource::Lane(lane) => {
            format!("bars read from the lane `{lane}` (data.source), not the venue's own candles")
        }
        BarSource::File(written) => format!(
            "bars read from the file {} (data.file), not imported, not the venue's own candles",
            file_path(profile, written).display()
        ),
    };
    Some(line)
}
