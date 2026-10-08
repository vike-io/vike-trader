//! `export --addr`: the walk over a REMOTE store, one fixed window at a time, into `--out`.
use vike_node_proto::auth::{NodeKeys, Scope};

use super::{Args, export};
use crate::cmd::data::shared::connect;
use crate::exit::{CliError, CmdResult};

/// `export --addr` — THE WALK: open `--out`, ask for one window at a time, append, repeat.
///
/// ⚠ **The file is opened BEFORE the first request and written window by window**, which is what
/// makes the memory bound real: at most one window's rows are alive at a time, and a row that has
/// been written is dropped. Collecting the whole range and writing once would be the same code
/// shape with none of the property — and would reintroduce exactly the unbounded client-side
/// buffer [`export`]'s module doc argues this design avoids.
///
/// ⚠ **A failure mid-walk leaves a PARTIAL file, and that is stated rather than cleaned up.**
/// Deleting it would destroy rows the operator may want (a 12-hour pull that died at hour 11 is
/// mostly good data), and writing to a temp file and renaming would hold the whole export in a
/// second copy on disk. So the error names the window that failed and the bound to resume from —
/// [`export::read_failed_note`] — and the file is left where it is.
///
/// ⚠ **The CSV header is written even when the walk returns nothing**, so an empty export is a
/// one-line file rather than a zero-byte one indistinguishable from a crash. `jsonl` has no header
/// and genuinely is empty; [`export::summary`] says which of the two happened.
pub(super) fn execute_export_remote(
    args: &Args,
    e: &export::Plan,
    keys: Option<&NodeKeys>,
) -> CmdResult<()> {
    use std::io::Write as _;

    let out_path = args.out.as_deref().expect("`parse` refuses an export without --out");
    // ⚠ Opened BEFORE the socket, deliberately: an unwritable destination is a fact this side can
    // establish for free, and discovering it after a multi-window pull would throw the pull away.
    let file = std::fs::File::create(out_path).map_err(|err| {
        CliError::usage(format!(
            "could not open --out {out_path:?} for writing ({err}). A remote export streams into \
             that file as it walks, so it is opened before the first request rather than after \
             the last."
        ))
    })?;
    let mut sink = std::io::BufWriter::new(file);

    // The header belongs to the FORMAT, not to the rows — see this function's doc for why it lands
    // before the walk rather than beside the first row that arrives.
    if e.wire == export::Wire::Csv {
        writeln!(sink, "{}", export::csv_header(e.kind)).map_err(write_failed(out_path))?;
    }

    let mut client = connect(&args.addr, keys, Scope::Read)?;
    let walk = export::windows(e.bounds.0, e.bounds.1, e.step_ms);
    let mut rows = 0usize;
    for (lo, hi) in &walk {
        // ⚠ ONE match rather than three loops, so the walk, the write and the counting are written
        // once and the KIND only chooses which RPC and which roster. Three copies is how a fix to
        // the resume hint would land in one lane and not the others.
        let cells: Vec<export::Cells> = match e.kind {
            export::Kind::Bar => {
                let interval =
                    e.spec.interval.as_deref().expect("`parse_spec` gives a bar spec an interval");
                client
                    .load_bars_ms(
                        &e.spec.venue,
                        &e.spec.symbol,
                        interval,
                        Some(*lo),
                        Some(*hi),
                        None,
                    )
                    .map_err(read_failed(e, *lo, *hi))?
                    .iter()
                    .map(|b| export::bar_cells(&e.spec, b))
                    .collect()
            }
            export::Kind::Quote => client
                .scan_quotes_ms(&e.spec.venue, &e.spec.symbol, Some(*lo), Some(*hi), None)
                .map_err(read_failed(e, *lo, *hi))?
                .iter()
                .map(|q| export::quote_cells(&e.spec, q))
                .collect(),
            export::Kind::Trade => client
                .scan_trades_ms(&e.spec.venue, &e.spec.symbol, Some(*lo), Some(*hi), None)
                .map_err(read_failed(e, *lo, *hi))?
                .iter()
                .map(|t| export::trade_cells(&e.spec, t))
                .collect(),
        };
        for row in &cells {
            let line = match e.wire {
                export::Wire::Jsonl => export::jsonl_line(row),
                export::Wire::Csv => export::csv_line(row),
            };
            writeln!(sink, "{line}").map_err(write_failed(out_path))?;
        }
        rows += cells.len();
    }
    sink.flush().map_err(write_failed(out_path))?;

    let written = export::Written { rows, windows: walk.len() };
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

/// The write-failure classifier — a closure so the path is named once and every `writeln!` in the
/// walk reports the same way.
///
/// ⚠ It says the partial file is DATA rather than wreckage, because that is the choice
/// [`execute_export_remote`]'s doc makes and an operator who is not told will delete it.
fn write_failed(out_path: &str) -> impl Fn(std::io::Error) -> CliError + '_ {
    move |err| {
        CliError::failed(format!(
            "writing to --out {out_path:?} failed ({err}). Rows already written are still there — \
             this verb streams, so a partial file is partial DATA rather than a corrupt one."
        ))
    }
}

/// The read-failure classifier, which renders [`export::read_failed_note`] at the run rung.
///
/// The rung is the RUN failure rather than [`Exit::Connect`] for [`connect`]'s stated reason: once
/// the connection is open a failure is the REQUEST's and not the connection's, which is the ladder
/// the whole read half follows.
fn read_failed(e: &export::Plan, lo: i64, hi: i64) -> impl Fn(String) -> CliError + '_ {
    move |why| CliError::failed(export::read_failed_note(&e.spec.text(), lo, hi, e.step_ms, &why))
}
