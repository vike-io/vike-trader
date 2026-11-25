//! `export --addr`, the DATAHUB route: the walk over a datahub's store, one fixed window at a time,
//! into `--out`.
use std::fs::File;
use std::io::BufWriter;
use std::path::PathBuf;

use vike_data::TimestampRange;
use vike_hub_protocol::auth::{HubKeys, Scope};

use super::super::{Args, export};
use crate::cmd::data::shared::connect;
use crate::exit::{CliError, CommandResult};

/// `export --addr` — THE WALK: open a temporary file beside `--out`, ask for one window at a time,
/// append, repeat, and replace `--out` when the whole range has landed.
///
/// ⚠ **The file is opened BEFORE the first request and written window by window**, which is what
/// makes the memory bound real: at most one window's rows are alive at a time, and a row that has
/// been written is dropped. Collecting the whole range and writing once would be the same code
/// shape with none of the property — and would reintroduce exactly the unbounded client-side
/// buffer [`export`]'s module doc argues this design avoids.
///
/// ⚠ **`--out` is all or nothing: a failed export leaves it byte-identical.** `File::create` on
/// `--out` itself took an operator's existing file to 0 bytes on a refused key and on a dead address
/// (measured 2026-11-07). The cost is stated rather than hidden: a pull that dies at hour 11 keeps
/// none of its rows, and until the rename the disk holds the old file and the new one. So the error
/// says `--out` was not touched and names the lever for the next try
/// ([`export::read_failed_note`]).
///
/// ⚠ **The CSV header is written even when the walk returns nothing**, so an empty export is a
/// one-line file rather than a zero-byte one indistinguishable from a crash. `jsonl` has no header
/// and genuinely is empty; [`export::summary`] says which of the two happened.
///
/// ⚠ **A walk that finds nothing does not replace a file that is already there**: a typo'd symbol
/// would otherwise cost the operator the file, the loss the rule above exists to prevent, with exit 0.
/// `--out` stays byte-identical and the summary and `out_kept` say so. With no file there the empty
/// one still lands.
pub(in crate::cmd::data::hist) fn execute_export_datahub(
    args: &Args,
    e: &export::Plan,
    keys: Option<&HubKeys>,
) -> CommandResult<()> {
    use std::io::Write as _;

    let out_path = args.out.as_deref().expect("`parse` refuses an export without --out");
    // ⚠ Opened BEFORE the socket, deliberately: an unwritable destination is a fact this side can
    // establish for free, and discovering it after a multi-window pull would throw the pull away.
    let mut out = OutFile::open(out_path).map_err(|err| {
        CliError::usage(format!(
            "could not open --out {out_path:?} for writing ({err}). A datahub export streams into \
             a temporary file beside it as it walks, so both are checked before the first request \
             rather than after the last."
        ))
    })?;

    // The header belongs to the FORMAT, not to the rows — see this function's doc for why it lands
    // before the walk rather than beside the first row that arrives.
    if e.wire == export::Wire::Csv {
        writeln!(out.sink, "{}", export::csv_header(e.kind)).map_err(write_failed(out_path))?;
    }

    let mut client = connect(&args.addr, keys, Scope::Read)?;
    let walk = export::windows(e.bounds.0, e.bounds.1, e.step_ms);
    let mut rows = 0usize;
    // `--levels N`: ONE fold across every window, so the walk's windows fold as one chain.
    let mut top = e.levels.map(export::BookTopLevels::new);
    for (lo, hi) in &walk {
        let range = TimestampRange { start: Some(*lo), end: Some(*hi) };
        // ⚠ ONE match rather than three loops, so the walk, the write and the counting are written
        // once and the KIND only chooses which RPC and which roster. Three copies is how a fix to
        // the resume hint would land in one lane and not the others.
        let cells: Vec<export::Cells> = match e.kind {
            export::Kind::Bar => {
                let interval =
                    e.spec.interval.as_deref().expect("`parse_spec` gives a bar spec an interval");
                match &e.source {
                    Some(lane) => client.load_bars_sourced(
                        &e.spec.venue,
                        lane,
                        &e.spec.symbol,
                        interval,
                        range,
                        None,
                    ),
                    None => client.load_bars(&e.spec.venue, &e.spec.symbol, interval, range, None),
                }
                .map_err(read_failed(e, *lo, *hi))?
                .iter()
                .map(|b| export::bar_cells(&e.spec, b))
                .collect()
            }
            export::Kind::Quote => match &e.source {
                Some(lane) => {
                    client.scan_quotes_sourced(&e.spec.venue, lane, &e.spec.symbol, range, None)
                }
                None => client.scan_quotes(&e.spec.venue, &e.spec.symbol, range, None),
            }
            .map_err(read_failed(e, *lo, *hi))?
            .iter()
            .map(|q| export::quote_cells(&e.spec, q))
            .collect(),
            export::Kind::Trade => client
                .scan_trades(&e.spec.venue, &e.spec.symbol, range, None)
                .map_err(read_failed(e, *lo, *hi))?
                .iter()
                .map(|t| export::trade_cells(&e.spec, t))
                .collect(),
            export::Kind::Book => client
                .scan_book_updates(&e.spec.venue, &e.spec.symbol, range, None)
                .map_err(read_failed(e, *lo, *hi))?
                .iter()
                .map(|u| book_row(&e.spec, &mut top, u))
                .collect(),
            export::Kind::Depth => client
                .scan_depth(&e.spec.venue, &e.spec.symbol, range, None)
                .map_err(read_failed(e, *lo, *hi))?
                .iter()
                .map(|u| book_row(&e.spec, &mut top, u))
                .collect(),
        };
        for row in &cells {
            let line = match e.wire {
                export::Wire::Jsonl => export::jsonl_line(row),
                export::Wire::Csv => export::csv_line(row),
            };
            writeln!(out.sink, "{line}").map_err(write_failed(out_path))?;
        }
        rows += cells.len();
    }
    let out_kept = rows == 0 && out.replaces_a_file();
    if out_kept {
        drop(out); // and the temporary with it
    } else {
        out.publish().map_err(write_failed(out_path))?;
    }

    let written = export::Written { rows, windows: walk.len(), out_kept };
    if args.json {
        println!(
            "{}",
            export::json_doc(
                // DERIVED from the verb, never re-typed — `universe_json`'s correction.
                args.sub.as_str(),
                &args.addr,
                out_path,
                e,
                &written,
            )
        );
    } else {
        for line in export::summary(e, out_path, &written) {
            println!("{line}");
        }
        if let Some(note) = export::step_note(e, &written) {
            println!("{note}");
        }
    }
    Ok(())
}

/// Where the walk writes. A regular `--out` is replaced by the policy of `vike_utility::atomic_file`
/// (decision 0141): a temporary file in the destination's own directory, synced, then
/// `std::fs::rename` over the destination, which replaces it on Windows too, and the destination
/// never removed first. It is spelled here because `write_atomic` takes the whole file in memory,
/// the buffer [`execute_export_datahub`]'s memory bound refuses.
///
/// shortcut: no directory fsync, so a power loss just after the rename can bring the old file back;
/// an export can be run again.
struct OutFile {
    // Declared first, so a failed walk closes the file before [`TempOut`] removes it.
    sink: BufWriter<File>,
    temp: Option<TempOut>,
    dest: PathBuf,
}

/// The temporary file: dropped, it is gone, so every failure path and a panic leave no litter. Like
/// `write_atomic`'s, it stays behind only when the process is killed.
struct TempOut(PathBuf);

impl Drop for TempOut {
    fn drop(&mut self) {
        // After the rename there is nothing left to remove, which is not worth an error either.
        let _ = std::fs::remove_file(&self.0);
    }
}

impl OutFile {
    fn open(out_path: &str) -> std::io::Result<Self> {
        // Through a link, as `File::create` wrote: the rename must replace the file, not the link.
        let dest = std::fs::canonicalize(out_path).unwrap_or_else(|_| PathBuf::from(out_path));
        // A FIFO or a device holds no bytes to lose and a rename would replace the node itself, so
        // it stays a stream; a directory stays the refusal it always was.
        if dest.exists() && !dest.is_file() {
            return Ok(Self { sink: BufWriter::new(File::create(&dest)?), temp: None, dest });
        }
        // A rename needs only the directory's permission, so a read-only file is asked directly.
        // `write` without `truncate` changes nothing in it.
        let mode = dest
            .exists()
            .then(|| std::fs::OpenOptions::new().write(true).open(&dest)?.metadata())
            .transpose()?
            .map(|metadata| metadata.permissions());
        let mut name = dest
            .file_name()
            .ok_or_else(|| std::io::Error::other("it names no file"))?
            .to_os_string();
        name.push(format!(".{}.tmp", std::process::id()));
        let tmp = dest.with_file_name(name);
        let file = File::create(&tmp)?;
        let temp = TempOut(tmp);
        if let Some(mode) = mode {
            // Best effort, as a filesystem may keep no mode: the export is worth more than the bits.
            let _ = file.set_permissions(mode);
        }
        Ok(Self { sink: BufWriter::new(file), temp: Some(temp), dest })
    }

    /// Whether [`Self::publish`] would replace a regular file that is already at `--out`.
    fn replaces_a_file(&self) -> bool {
        self.temp.is_some() && self.dest.exists()
    }

    /// The walk landed whole: flush and, for a regular file, sync and replace `--out`.
    fn publish(self) -> std::io::Result<()> {
        let Self { sink, temp, dest } = self;
        let file = sink.into_inner().map_err(std::io::IntoInnerError::into_error)?;
        let Some(temp) = temp else { return Ok(()) };
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temp.0, &dest)
    }
}

/// One book event's cells: as recorded, or its `--levels N` row ([`export::BookTopLevels`]).
fn book_row(
    spec: &export::Spec,
    top: &mut Option<export::BookTopLevels>,
    event: &vike_marketdata::L2BookUpdate,
) -> export::Cells {
    match top {
        Some(top) => export::book_cells(spec, &top.row(event)),
        None => export::book_cells(spec, event),
    }
}

/// The write-failure classifier — a closure so the path is named once and every `writeln!` in the
/// walk reports the same way.
fn write_failed(out_path: &str) -> impl Fn(std::io::Error) -> CliError + '_ {
    move |err| {
        CliError::failed(format!(
            "writing to --out {out_path:?} failed ({err}). --out is replaced only when the whole \
             walk has landed, so it still holds what it held before this run."
        ))
    }
}

/// The read-failure classifier, which renders [`export::read_failed_note`] at the run rung.
///
/// The rung is the RUN failure rather than [`Exit::Connect`] for [`connect`]'s stated reason: once
/// the connection is open a failure is the REQUEST's and not the connection's, which is the ladder
/// the whole read half follows.
fn read_failed(e: &export::Plan, lo: i64, hi: i64) -> impl Fn(String) -> CliError + '_ {
    move |why| CliError::failed(export::read_failed_note(&e.series_text(), lo, hi, e.step_ms, &why))
}
