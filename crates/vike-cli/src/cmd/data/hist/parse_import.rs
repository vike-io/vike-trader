//! `data hist import` — the arm of the per-subcommand `match` in `super::parse` that holds its
//! grammar.
//!
//! Moved here verbatim from that function (code-layout phase 2, task 11); see
//! `parse_fetch` for how the pieces fit.

use vike_model::time::epoch_ms_to_utc_date;

use super::{ImportArgs, Window, import};

/// `import`'s arm: resolves the format, dataset, days and bars so `execute_import` has no parse
/// left in it. Returns the `(spec, window)` pair `parse` builds its `Args` from, and the
/// `ImportArgs` it carries.
#[expect(clippy::type_complexity)] // the arm's `(spec, window)` pair plus the struct it builds
pub(super) fn parse(
    spec: Option<String>,
    dataset: Option<String>,
    from: Option<String>,
    to: Option<String>,
    bars: Option<String>,
    dry_run: bool,
    verify: bool,
    yes: bool,
) -> Result<(Option<String>, Option<Window>, Option<ImportArgs>), String> {
    let import_args;
    // The import door refused every foreign flag above; what is left is to RESOLVE what it
    // takes, here, so that every rule a request could break without a directory is a usage
    // error before a socket opens — `get`'s reason. The server re-checks all of it: the
    // dataset and the bars through the same shared validators, the days after it resolves an
    // omitted bound.
    let (spec, window) = {
        let format = spec.ok_or(
            "import needs a FORMAT and a DATASET — e.g. `vike-cli data hist import \
                 dukascopy-bi5 EURUSD --dry-run`. The FORMAT is an archive format the datahub \
                 advertises; the DATASET is ONE folder under <project>/market_data/imports/FORMAT/ \
                 on the DATAHUB's box",
        )?;
        if format.trim().is_empty() {
            return Err("import's FORMAT is EMPTY. Name an archive format the datahub \
                            advertises, such as dukascopy-bi5"
                .to_string());
        }
        let dataset = dataset.ok_or_else(|| {
            format!(
                "import needs a DATASET after the format: the vendor's own upper-case folder \
                     name under <project>/market_data/imports/{format}/ on the datahub's box — \
                     e.g. `vike-cli data hist import {format} EURUSD --dry-run`"
            )
        })?;
        // The wire's own validator — the one the client runs before it sends and the server
        // runs at its door — so a name that can never be a dataset is refused HERE, as a usage
        // error, rather than one round trip later.
        vike_datahub_client::archive::validate_import_dataset(&dataset)?;
        let from_day = from.as_deref().map(|raw| import::parse_day("--from", raw)).transpose()?;
        let to_day = to.as_deref().map(|raw| import::parse_day("--to", raw)).transpose()?;
        if let (Some(f), Some(t)) = (from_day, to_day)
            && f > t
        {
            return Err(format!(
                "--from ({}) is AFTER --to ({}) — both are inclusive days, and an inverted \
                     window holds none. Pass them the other way round.",
                epoch_ms_to_utc_date(f),
                epoch_ms_to_utc_date(t)
            ));
        }
        // Refused rather than read one way or the other — the wire's own rule
        // (`vike_datahub_client::archive::validate_import_window`), here so it is a usage error.
        if verify && !dry_run {
            return Err("--verify is a mode of --dry-run: it decodes every importable file and \
                            writes nothing, and this line is an import. Add --dry-run to verify, \
                            or drop --verify to import"
                .to_string());
        }
        let bars = import::parse_bars(bars.as_deref())?;
        import_args =
            Some(ImportArgs { format, dataset, from_day, to_day, bars, dry_run, verify, yes });
        (None, None)
    };
    Ok((spec, window, import_args))
}
