//! `store export`: one series read over the wire, encoded as a standalone Parquet file.

use std::path::Path;
use std::process::ExitCode;

use vike_datahub_client::route::{history_route, open_routed_history, routed_hub_keys};

use crate::datahub_cli::args::StoreFlags;

/// `store export` — one series out of the store as a standalone Parquet file.
///
/// The producer half of the starter dataset (`scripts/publish_starter_data.sh` calls exactly this),
/// and useful on its own: handing somebody a slice without handing them the store's internals.
///
/// # ⚠ It READS OVER THE WIRE, and only the ENCODING is local
///
/// Decision 0084's 2026-09-25 amendment closed the local READ door on every history reader, and this
/// verb is one: a verb that opened the store and called its own export would be a second reader,
/// whatever file it writes afterwards. So the bars come from the datahub the SETTINGS name
/// (`config.datahub_addr`) — so one box has one answer to "where is my history" — and
/// `vike_store::write_bars_parquet` encodes them. The wire carries a `Bar` bit-exactly:
/// `crates/vike-datahub/tests/datafusion_serve/store_cli/export.rs`'s
/// `data_export_reads_its_bars_through_a_datahub_and_writes_what_the_store_holds` holds the
/// exported bars bit-equal to the store read DIRECTLY, through a real datahub.
///
/// ⚠ **The server does not do this half, and that is why the encoding stays here** rather than the
/// verb moving onto the wire whole: `crates/vike-datahub/src/server.rs` holds an
/// `Arc<dyn HistoryStore>` and is backend-agnostic by design, so it has no Parquet encoder to drive —
/// the measurement `crates/vike-cli/src/cmd/data/hist/export.rs`'s `parquet_refusal` states for the
/// same reason. What crosses the wire is the ordinary paged `LoadBars`, so nothing about the
/// protocol moved and any datahub serves it.
///
/// ⚠ **`--store` is refused BEFORE this function runs** (`super`'s `refuse_a_store_on_a_read`), so
/// nothing here resolves a store root, and a store that is absent is never CREATED by an export.
///
/// `booted` is the daemon's own boot, run by `super`'s `run_store`: a retired variable and a malformed
/// settings store refuse there as they do for the daemon, and the hub keys come off its one handle.
pub(super) fn run_export(
    booted: &vike_boot::Booted,
    flags: &StoreFlags,
    raw_spec: &str,
) -> ExitCode {
    let Some(out) = flags.out.clone() else {
        eprintln!("vike-datahub store export: it needs --out FILE");
        return ExitCode::from(2);
    };
    // The same VENUE:SYMBOL:INTERVAL grammar a fetch takes, but WITHOUT its venue roster: this
    // reads the store, so any venue the store holds is exportable, including `demo`.
    let parts: Vec<&str> = raw_spec.split(':').collect();
    if parts.len() != 3 || parts.iter().any(|p| p.is_empty()) {
        eprintln!("vike-datahub store export: it takes VENUE:SYMBOL:INTERVAL, not {raw_spec:?}");
        return ExitCode::from(2);
    }
    let range = match (flags.from.clone(), flags.to.clone()) {
        (None, None) => vike_data::TimestampRange::all(),
        (from, to) => {
            let conv = |v: Option<String>| -> Result<Option<i64>, String> {
                v.map(|s| vike_data::store::hist::parse_timestamp(&s)).transpose()
            };
            match (conv(from), conv(to)) {
                (Ok(start), Ok(end)) => vike_data::TimestampRange { start, end },
                (Err(e), _) | (_, Err(e)) => {
                    eprintln!("vike-datahub store export: {e}");
                    return ExitCode::from(2);
                }
            }
        }
    };

    for w in &booted.settings.warnings {
        eprintln!("vike-datahub store export: {w}");
    }
    let route = history_route(booted.settings.config.datahub_addr.as_deref());
    // The datahub family alone, and an unreadable store warns and continues, which
    // `routed_hub_keys`' doc argues.
    let keys = routed_hub_keys(
        booted.store.hub_keys(vike_venues::credential_keys::is_datahub_pair_key),
        route.hub(),
    );
    let store = open_routed_history(&route, keys);
    let bars = match store.load_bars(parts[0], parts[1], parts[2], range) {
        Ok(b) => b,
        Err(e) => {
            // The replacement is named on the FAILURE too, not only on the `--store` refusal: the
            // commonest reason this read fails on a fresh box is that no datahub is running at
            // all, and the operator who used to pass a directory needs the one command that serves
            // it rather than a connect error alone.
            eprintln!(
                "vike-datahub store export: reading {raw_spec} from {} failed: {e}\n  For files on \
                 this machine, start a key-less datahub on them first: vike-backend datahub --store \
                 DIR — it binds loopback only, which is where this verb dials by default.",
                route.label()
            );
            return ExitCode::from(2);
        }
    };
    match vike_store::write_bars_parquet(Path::new(&out), &bars) {
        Ok(rows) => {
            // The row count is the load-bearing half of this line: a publishing script that
            // exported an EMPTY slice would otherwise upload a valid file nobody can use. It LEADS
            // the line (`scripts/publish_starter_data.sh` greps `^exported N`); the provenance
            // follows it, because a file whose source is not named is a file nobody can re-derive.
            println!(
                "exported {rows} bars of {}/{} {} to {out} (read from {})",
                parts[0],
                parts[1],
                parts[2],
                route.label()
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("vike-datahub store export: exporting {raw_spec} failed: {e}");
            ExitCode::from(2)
        }
    }
}
