//! `data hist export` — the arm of the per-subcommand `match` in `super::parse` that holds its
//! grammar, both routes (the engine's Parquet writer and the remote walk).
//!
//! Moved here verbatim from that function (code-layout phase 2, task 11); see
//! `parse_fetch` for how the pieces fit. The module this verb's pure half lives in is `export`,
//! which is why this file is not simply called that.

use vike_model::{parse_date_label, time::epoch_ms_to_utc_date};

use super::{
    ExportRange, Filter, Sub, Window, check_spec, export, refuse_an_account_kind_on_a_read,
    refuse_foreign_flags,
};

/// `export`'s arm. Returns the `(spec, window)` pair `parse` builds its `Args` from, then the
/// ENGINE route's [`ExportRange`] and the REMOTE route's `export::Plan` — exactly one of the two is
/// `Some`, and which one is what selects the route.
#[allow(clippy::too_many_arguments)] // one parameter per local the arm read inside `parse`
#[allow(clippy::type_complexity)] // the arm's `(spec, window)` pair plus the two structs it builds
pub(super) fn parse(
    sub: Sub,
    spec: Option<String>,
    out: &Option<String>,
    days: Option<String>,
    addr: &Option<String>,
    engine: &Option<String>,
    filter: &Filter,
    format_raw: Option<String>,
    from: Option<String>,
    to: Option<String>,
    window_raw: Option<String>,
) -> Result<(Option<String>, Option<Window>, Option<ExportRange>, Option<export::Plan>), String> {
    let mut export_range = None;
    let mut export_args: Option<export::Plan> = None;
    let (spec, window) = {
        let raw_spec = spec.ok_or(
            "export needs a spec: VENUE:SYMBOL:INTERVAL, e.g. `vike-cli data hist export \
                 demo:DEMOUSDT:1h --out slice.parquet` — or VENUE:SYMBOL with \
                 `--kind quote|trade` over --addr, whose lanes have no interval",
        )?;
        if out.is_none() {
            return Err("export needs --out FILE: it writes a standalone file, and there is no \
                     default name a store could supply"
                .into());
        }
        // ⚠ `--days` is refused BY NAME rather than folded into a range — see [`ExportRange`].
        // It is refused on BOTH routes and for one reason, so the check sits above the split.
        if days.is_some() {
            return Err(
                "--days bounds a FETCH: it counts back from NOW, which says nothing about \
                     what a store already holds. An export slices what is on disk — use \
                     --from/--to"
                    .into(),
            );
        }
        // ⚠ **THE ROUTE SPLIT.** `--addr` names the datahub THIS process walks; `--engine`
        // names the binary that writes Parquet instead. This is `rm`'s rule arriving on the
        // second verb that has two routes, and it is spelled here rather than shared because
        // the two verbs' OTHER flags differ — what follows below is entirely per-route grammar.
        //
        // ⚠ It named `--store` beside `--engine` and called the pair "two DIFFERENT stores"
        // until 2026-09-26. `--store` cannot reach this line any more — the flag loop refuses it
        // on `export` before its value is read — and the two routes no longer read different
        // stores at all: both ask a datahub (decision 0084's amendment). What they still differ
        // in is WHO writes the file, which is what the sentence says now.
        if addr.is_some() && engine.is_some() {
            return Err(
                "--addr and --engine are two DIFFERENT routes: --addr has THIS binary walk \
                     that datahub and write the rows itself (--format jsonl|csv), while --engine \
                     names the engine that writes Parquet, reading its bars from the datahub its \
                     own settings name (config.datahub_addr, or VIKE_DATAHUB_ADDR above it). \
                     Pass one."
                    .to_string(),
            );
        }
        // ⚠ `addr.is_some()` rather than the resolved [`Args::addr`], which is always `Some`:
        // the route is a question about what the operator TYPED. This is the same local the
        // `addr_given` field is built from, read before that field exists.
        if addr.is_some() {
            let kind = export::parse_kind(filter.kind.as_deref())?;
            let parsed = export::parse_spec(&raw_spec, kind)?;
            let wire = export::parse_wire(format_raw.as_deref())?;
            // ⚠ BOTH bounds, and the refusal names the verb that PRINTS them — see
            // [`export::Plan::bounds`] for why a walk needs what the engine route's paged read
            // does not.
            let (Some(from_raw), Some(to_raw)) = (from.as_deref(), to.as_deref()) else {
                return Err(format!(
                    "a remote export needs BOTH --from and --to. It walks the range in \
                         windows, so it has to know where the first step begins and where to \
                         stop, and an unbounded side has neither — unlike the engine route, \
                         which asks for whatever the store holds in one paged read. \
                         `vike-cli data hist ls --venue {} --name {}` prints this series' \
                         recorded span, which is the two numbers to pass.",
                    parsed.venue, parsed.symbol
                ));
            };
            let start = parse_date_label(from_raw).map_err(|e| {
                format!("--from {from_raw:?} is not a timestamp this side can read ({e})")
            })?;
            let end = parse_date_label(to_raw).map_err(|e| {
                format!("--to {to_raw:?} is not a timestamp this side can read ({e})")
            })?;
            // Refused rather than swapped, for `membership_window`'s reason: a range whose
            // ends are the wrong way round has two readable meanings, and picking one discards
            // half of what the operator typed. Here it would also produce an EMPTY file, which
            // reads exactly like a store that holds nothing.
            if start > end {
                return Err(format!(
                    "--from ({}) is AFTER --to ({}) — an inverted range holds nothing, so the \
                         export would write a file with no rows in it, which reads exactly like \
                         an empty store. Pass them the other way round.",
                    epoch_ms_to_utc_date(start),
                    epoch_ms_to_utc_date(end)
                ));
            }
            let step_defaulted = window_raw.is_none();
            let step_ms = match window_raw.as_deref() {
                Some(raw) => export::parse_window_step(raw, kind)?,
                None => kind.default_window_ms(),
            };
            export_args = Some(export::Plan {
                kind,
                spec: parsed,
                wire,
                bounds: (start, end),
                step_ms,
                step_defaulted,
            });
        } else {
            // The ENGINE route keeps the grammar it shipped with: three mandatory spec parts
            // (the engine's export writes bars), two INDEPENDENT optional bounds, Parquet.
            // ⚠ What it lost on 2026-09-26 is `--store`: the engine reads the bars through a
            // datahub now (decision 0084's amendment), and the flag loop refuses the flag here.
            check_spec(&raw_spec)?;
            if let Some(raw) = format_raw.as_deref() {
                export::refuse_a_wire_on_the_local_route(raw)?;
            }
            // ⚠ Both refused rather than ignored, and each names --addr, because both are
            // meaningful ONLY on the walk. `--kind` USED to be refused one block up with "that
            // flag belongs to the READ half", which is a true no for a false reason — it
            // belongs to a route this verb did not have. The reason moves; the refusal stays.
            if let Some(kind) = filter.kind.as_deref() {
                refuse_an_account_kind_on_a_read(kind)?;
                return Err(format!(
                    "--kind does not apply to an ENGINE export: the engine writes BARS, and the \
                         bar step is the spec's third part ({kind:?} would be a different row \
                         shape). The remote route reads three kinds — add --addr HOST:PORT to \
                         reach it"
                ));
            }
            refuse_foreign_flags(
                sub,
                &[(export::WINDOW_FLAG, window_raw.is_some())],
                "that flag sets the step of a WINDOWED WALK, and this route does not walk: it \
                     spawns the engine, which asks for the whole range as one paged read. The walk \
                     is the remote route's — add --addr HOST:PORT",
            )?;
            export_range = Some(ExportRange { from, to });
        }
        (Some(raw_spec), None)
    };
    Ok((spec, window, export_range, export_args))
}
