//! `vike-cli config operator-instrument rm --venue <VENUE> <RAW_SYMBOL> [--dry-run]` — **delete ONE instrument
//! the operator added by hand** (its `operator_instrument` row, `vike_secrets::operator_instrument`), with no hub
//! running.
//!
//! # Why an offline twin of `data instruments rm`
//!
//! `data instruments rm` asks the running datahub, which deletes the row AND folds the kept list again. A release's
//! row steps (`deploy/row-steps`) run with no hub up, on each pre-flight probe's copy and then on the box, so they
//! need a verb that writes the settings database alone. The kept list drops the row at its next refresh (the datahub
//! folds the operator's rows back in from the database at each one,
//! `crates/vike-datahub/src/instrument_lists.rs`'s `with_operator_rows`). Decision
//! 0290's deploy retired IG's 15 Mini rows this way (its row steps of 2026-11-16, under that day's ruling: the standard
//! lot `CS.D.<PAIR>.CFD.IP` was the pair, and a Mini row of the old shape folded beside it would have made every IG pair
//! name two rows). Since item 6's amendment (the owner's ruling of 2026-11-17) IG's own list names both forms itself,
//! the Mini `CS.D.<PAIR>.MINI.IP` the pair (`EURUSD`) and the standard lot the pair + `.CFD` (`EURUSD.CFD`), in
//! `crates/bridges/ig/src/catalog.rs`'s `naming_of`, so neither needs an operator's row.
//!
//! # The fences
//!
//! - **One row, addressed by the venue and the venue's own name** (`raw_symbol`), through the store's one deleter
//!   (`delete_operator_instrument`). No other row is touched, and nothing here writes SQL.
//! - **A row the store does not hold answers `unchanged`** and exits 0, so a re-run is harmless; a venue outside the
//!   roster is refused, so a typo is not read as "already gone".
//! - `data instruments add` brings a row back, so no `--yes` is asked; `--dry-run` says what would go.
//! - No change-journal record: an instrument is not a credential, an account or a setting (the store module's doc).

use std::path::Path;
use std::process::ExitCode;

use vike_secrets::operator_instrument::{delete_operator_instrument, read_operator_instruments};

use crate::exit::{CliError, CommandResult};

/// The program name this verb is read and reported under.
const BIN: &str = "vike-cli config operator-instrument rm";

/// `config operator-instrument rm`'s arguments, as `clap` reads them.
#[derive(clap::Args, Debug)]
#[command(
    about = "Delete ONE instrument the operator added by hand, with no hub running",
    long_about = "Delete ONE instrument the operator added by hand (`data instruments add`), by its venue and \
                  the\nvenue's own name, from the settings database alone: the offline twin of `data \
                  instruments rm`,\nfor a release's row steps. The kept list drops it at its next refresh. \
                  A row the store does\nnot hold is left unchanged (exit 0)."
)]
pub(crate) struct DeleteOperatorInstrumentCommand {
    #[arg(long, value_name = "VENUE", help = "the venue the row belongs to")]
    venue: String,
    #[arg(
        value_name = "RAW_SYMBOL",
        help = "the venue's own name of the row (`CS.D.EURUSD.MINI.IP`)"
    )]
    raw_symbol: String,
    #[arg(long, help = "say what would be deleted and delete nothing")]
    dry_run: bool,
}

/// Entry point: gate, delete, report.
pub(crate) fn run(cmd: DeleteOperatorInstrumentCommand, settings_dir: Option<&Path>) -> ExitCode {
    match execute(&cmd, settings_dir) {
        Ok(report) => {
            println!("{report}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{BIN}: {}", e.message);
            e.exit.into()
        }
    }
}

fn execute(
    cmd: &DeleteOperatorInstrumentCommand,
    settings_dir: Option<&Path>,
) -> CommandResult<String> {
    let (venue, name) = (cmd.venue.as_str(), cmd.raw_symbol.as_str());
    if !vike_model::VENUES.contains(&venue) {
        return Err(CliError::usage(format!(
            "`{venue}` is not a venue (one of {}). Nothing was deleted.",
            vike_model::VENUES.join(", ")
        )));
    }
    let dir = settings_dir.ok_or_else(|| {
        CliError::failed(
            "no settings directory resolved — there is no store to delete a row from. Set \
             VIKE_SETTINGS_DIR, or run from a project that has one.",
        )
    })?;
    let row = format!("the operator's {venue} row `{name}`");
    if cmd.dry_run {
        let rows = read_operator_instruments(dir).map_err(|e| CliError::failed(e.to_string()))?;
        let held =
            rows.iter().any(|r| r.instrument.venue == venue && r.instrument.raw_symbol == name);
        return Ok(if held {
            format!("would delete {row} — NOTHING WAS DELETED (--dry-run).")
        } else {
            format!("unchanged — the store holds no {row}. Nothing was deleted.")
        });
    }
    if delete_operator_instrument(dir, venue, name).map_err(|e| CliError::failed(e.to_string()))? {
        Ok(format!(
            "deleted {row}; the kept list drops it at its next refresh (`vike-cli data catalog refresh \
             --venue {venue}`)."
        ))
    } else {
        Ok(format!("unchanged — the store holds no {row}. Nothing was deleted."))
    }
}

#[cfg(test)]
mod tests {
    use vike_model::{Contract, ContractFx, Instrument};
    use vike_secrets::operator_instrument::{OperatorInstrument, write_operator_instrument};

    use super::*;

    fn command(venue: &str, name: &str, dry_run: bool) -> DeleteOperatorInstrumentCommand {
        DeleteOperatorInstrumentCommand { venue: venue.into(), raw_symbol: name.into(), dry_run }
    }

    /// A store holding IG's EURUSD Mini as the operator added it.
    fn store_with_the_mini() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        vike_secrets::create_empty_store_for_test(&vike_secrets::db_path_in(dir.path())).unwrap();
        let mini = Instrument::new(
            "ig",
            "CS.D.EURUSD.MINI.IP",
            "EUR",
            "USD",
            Contract::Fx(ContractFx::default()),
        );
        let row = OperatorInstrument {
            asked: "EURUSD".into(),
            added: "2026-11-16".into(),
            instrument: mini,
        };
        write_operator_instrument(dir.path(), &row).unwrap();
        dir
    }

    fn held(dir: &Path) -> usize {
        read_operator_instruments(dir).unwrap().len()
    }

    /// The row goes; the same step again is `unchanged`; a dry run deletes nothing; a typo'd venue is refused.
    #[test]
    fn one_row_is_deleted_and_a_rerun_is_unchanged() {
        let d = store_with_the_mini();
        let dry = execute(&command("ig", "CS.D.EURUSD.MINI.IP", true), Some(d.path())).unwrap();
        assert!(dry.contains("would delete") && dry.contains("NOTHING WAS DELETED"), "{dry}");
        assert_eq!(held(d.path()), 1);

        let done = execute(&command("ig", "CS.D.EURUSD.MINI.IP", false), Some(d.path())).unwrap();
        assert!(done.starts_with("deleted the operator's ig row `CS.D.EURUSD.MINI.IP`"), "{done}");
        assert_eq!(held(d.path()), 0);

        let again = execute(&command("ig", "CS.D.EURUSD.MINI.IP", false), Some(d.path())).unwrap();
        assert!(again.starts_with("unchanged"), "{again}");

        let typo = execute(&command("igg", "CS.D.EURUSD.MINI.IP", false), Some(d.path()));
        assert!(typo.unwrap_err().message.contains("is not a venue"));
    }
}
