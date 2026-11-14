//! `import csv-bars --file PATH`: every line an UPLOAD prints (decision 0272), as pure functions over
//! the wire's own answer, so the wording is tested without a socket. `super::super::import_execution`'s
//! `execute_upload` is the dial, the sending and the prints.

use vike_datahub_client::archive::{ArchiveDayResultWire, ImportTarget};
use vike_datahub_client::upload::CsvUploadDone;
use vike_model::time::{epoch_ms_to_utc_date, epoch_ms_to_utc_timestamp};
use vike_utility::bytes::iec_bytes;

use super::{counted, day_count, thousands};

/// The lines the answer prints: what was sent where, how the datahub read it, and what it stored (or,
/// under `--dry-run`, what it would) — or the decoder's refusal and the file's own column names to map.
pub(crate) fn upload_lines(
    done: &CsvUploadDone,
    target: &ImportTarget,
    file: &str,
    bytes: u64,
    addr: &str,
    dry_run: bool,
) -> Vec<String> {
    let lane = format!("user-{}", target.label);
    let mut out = vec![
        format!("csv-bars · {file} ({}) sent to the datahub at {addr}", iec_bytes(bytes)),
        format!(
            "  series      {} {} {} → lane {lane}, never the venue's own series",
            target.venue, target.symbol, target.interval
        ),
    ];
    if let Some(why) = &done.refused {
        out.push(format!("  refused     {why}"));
        out.push("              nothing was stored".to_string());
        if !done.header.is_empty() {
            out.push(format!(
                "  columns     the file's header: {} — name them with --columns \
                 time=<name>,close=<name> (and open=, high=, low=, volume= when it has them)",
                done.header.join(", ")
            ));
        }
        return out;
    }
    let span = match (done.first_day, done.last_day) {
        (Some(a), Some(b)) => {
            format!(", {} .. {}", epoch_ms_to_utc_date(a), epoch_ms_to_utc_date(b))
        }
        _ => String::new(),
    };
    out.push(format!(
        "  read        {} over {}{span}",
        counted(done.bars, "bar"),
        day_count(done.days)
    ));
    if !done.preview.is_empty() {
        out.push(
            "  read as     the first rows as the datahub's decoder read them (time in UTC):".into(),
        );
        out.extend(done.preview.iter().map(|b| {
            format!(
                "              {}  open {}  high {}  low {}  close {}  volume {}",
                epoch_ms_to_utc_timestamp(b.timestamp),
                b.open,
                b.high,
                b.low,
                b.close,
                b.volume
            )
        }));
    }
    if dry_run {
        out.push(format!(
            "  --dry-run   nothing was stored. Run it without --dry-run to store {} into {lane}",
            day_count(done.days.saturating_sub(done.not_stored.len() as u64))
        ));
    } else {
        out.push(format!(
            "  stored      {} ({} bars) into {lane}",
            day_count(done.stored_days),
            thousands(done.stored_bars)
        ));
    }
    for d in &done.not_stored {
        let why = match &d.result {
            ArchiveDayResultWire::Refused(r) => format!("{}: {}", r.class, r.detail),
            other => format!("{other:?}"),
        };
        out.push(format!("  not stored  {}  {why}", epoch_ms_to_utc_date(d.day)));
    }
    out
}

/// The progress line one acknowledged chunk prints, on stderr.
pub(crate) fn progress_line(sent: u64, total: u64) -> String {
    format!("  sent {} of {}", iec_bytes(sent), iec_bytes(total))
}

#[cfg(test)]
mod tests {
    use vike_data::archive_day::ArchiveDayRefusal;
    use vike_data::csv_bars::CsvBarsFormatWords;
    use vike_datahub_client::archive::{ImportOutcomeDay, ImportPlanPreviewRow};

    use super::*;

    fn target() -> ImportTarget {
        ImportTarget {
            venue: "user".to_string(),
            symbol: "USDT-USD".to_string(),
            interval: "1d".to_string(),
            label: "coinbase".to_string(),
            file_format: CsvBarsFormatWords::default(),
            columns: None,
        }
    }

    const DAY: i64 = 19_737 * vike_model::MS_PER_DAY;

    #[test]
    fn a_stored_upload_says_where_it_went_and_what_it_left() {
        let done = CsvUploadDone {
            preview: vec![ImportPlanPreviewRow {
                timestamp: DAY,
                open: "1".into(),
                high: "1".into(),
                low: "1".into(),
                close: "1".into(),
                volume: "0".into(),
            }],
            bars: 3,
            days: 3,
            first_day: Some(DAY),
            last_day: Some(DAY + 2 * vike_model::MS_PER_DAY),
            stored_days: 2,
            stored_bars: 2,
            not_stored: vec![ImportOutcomeDay {
                day: DAY + 2 * vike_model::MS_PER_DAY,
                result: ArchiveDayResultWire::Refused(ArchiveDayRefusal {
                    class: "TooRecent".into(),
                    detail: "the day has not ended yet".into(),
                }),
            }],
            ..CsvUploadDone::default()
        };
        let lines = upload_lines(&done, &target(), "rates.csv", 2048, "127.0.0.1:7878", false);
        let text = lines.join("\n");
        assert!(text.contains("user USDT-USD 1d → lane user-coinbase"), "{text}");
        assert!(text.contains("3 bars over 3 days, 2024-01-15 .. 2024-01-17"), "{text}");
        assert!(text.contains("stored      2 days (2 bars) into user-coinbase"), "{text}");
        assert!(text.contains("not stored  2024-01-17  TooRecent"), "{text}");
        let dry = upload_lines(&done, &target(), "rates.csv", 2048, "127.0.0.1:7878", true);
        assert!(
            dry.join("\n").contains("nothing was stored. Run it without --dry-run to store 2 days")
        );
    }

    #[test]
    fn a_refused_file_names_the_line_and_offers_its_columns() {
        let done = CsvUploadDone {
            header: vec!["timestamp_ms".into(), "rate".into()],
            refused: Some("line 1: header: no time column".into()),
            ..CsvUploadDone::default()
        };
        let text = upload_lines(&done, &target(), "rates.csv", 10, "h:1", false).join("\n");
        assert!(text.contains("refused     line 1: header: no time column"), "{text}");
        assert!(
            text.contains("the file's header: timestamp_ms, rate — name them with --columns"),
            "{text}"
        );
        assert!(!text.contains("stored      "), "{text}");
    }
}
