//! `ls` and `gaps`: one inventory, filtered here, its account-kind exclusion and its renderings.
use vike_model::time::epoch_ms_to_utc_date;
use vike_node_proto::auth::{NodeKeys, Scope};

use super::{Args, ClassProbe, Coverage, SeriesRow};
use crate::cmd::data::shared::{
    CLASS_AS_OF_TS, col, connect, empty_note, interval_cell, scope_cell, span_cell,
};
use crate::exit::CmdResult;

/// `data hist ls` AND `data hist gaps` — one `inventory()` round trip, filtered client-side, plus
/// one `series_gaps` probe per MATCHED series under the latter.
///
/// ⚠ **A failed gap probe degrades the ROW; it does not fail the run.** The listing is what was
/// asked for and it is complete and correct; the gaps are an annotation on it, and one series whose
/// manifest cannot be read should not make `data hist list` unusable against a store of a thousand. The
/// failure is not swallowed either — it lands on the row it belongs to in both renderings, and
/// [`SeriesRow::gaps`] stays `None` so a machine reader can never mistake it for "no holes". Same
/// contract, and the same argument, as `crates/vike-app-core/src/data/stored_load.rs`'s
/// `load_stored_tree`, which the Data Manager runs over the same two verbs.
///
/// The rejected alternative was exiting on the run-failure rung with the table still emitted: that
/// makes a wrapper treat a complete listing as no listing, and there is no rung for "mostly".
pub(crate) fn execute_list(args: &Args, keys: Option<&NodeKeys>) -> CmdResult<()> {
    refuse_an_account_kind_filter(args)?;
    let mut client = connect(&args.addr, keys, Scope::Read)?;
    // A server-side `Response::Error` and a protocol desync both arrive as `Err(String)` and both
    // stay on the pre-existing rung: once the connection is open, a failure is the request's and
    // not the connection's.
    let inventory = client.inventory()?;
    let reported = inventory.len();

    // One entry per DISTINCT `(venue, symbol)`, so an instrument recorded as five series costs one
    // round trip rather than five — see [`CLASS_AS_OF_TS`] for why every row of one instrument is
    // genuinely asking the same question. A failed probe is cached like any other answer: a store
    // that cannot answer for an instrument will not answer on the retry either, and re-asking would
    // multiply one failure into one round trip per series of it.
    let mut class_cache: std::collections::BTreeMap<(String, String), ClassProbe> =
        std::collections::BTreeMap::new();

    // ⚠ The ACCOUNT-kind exclusion, and it is ACTIVE rather than a filter default — the surface
    // design's §9.3.2. The store is SHARED: `exec_fill`, `exec_order`, `exec_funding` and `equity`
    // sit in the same tree as market data, and they are YOUR OWN activity rather than the market's.
    // This plane does not serve them; `vike-cli account` will.
    //
    // ⚠ Counted rather than silently dropped, because an operator can see those series exist with
    // their own eyes — `ls` is what they would use to look. A silent filter would make this verb
    // UNDER-REPORT a store rather than scope itself, which is the failure §9.3.2 exists to prevent.
    // The disclosure below is therefore part of the contract, not a nicety.
    let mut excluded: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
    let mut excluded_count = 0usize;

    let mut rows: Vec<SeriesRow> = Vec::new();
    for (id, cov) in &inventory {
        if !args.filter.matches(Some(&id.kind), &id.venue, id.label()) {
            continue;
        }
        // AFTER the filter, deliberately: the note then describes what THIS listing withheld from
        // THIS operator, rather than reporting account series in venues they did not ask about.
        if vike_model::is_account_kind(&id.kind) {
            excluded.insert(id.kind.as_str());
            excluded_count += 1;
            continue;
        }
        // The id is handed BACK to the server exactly as it arrived — this side never constructs
        // a `SeriesId`, which is the whole reason `gaps` selects by FILTER rather than taking a
        // four-dimensional identity off the command line.
        let (gaps, gaps_error) = if args.gaps {
            match client.series_gaps(id) {
                Ok(ranges) => (Some(ranges), None),
                Err(e) => (None, Some(e)),
            }
        } else {
            (None, None)
        };
        // ⚠ Keyed on the RAW `symbol`, never on `label()`. A grouped series' label is its GROUP,
        // and two grouped series of different groups would otherwise share the one empty-symbol
        // cache slot — harmless today only because [`ClassProbe::Grouped`] carries no venue-specific
        // answer, which is the kind of accident that stops being harmless when a variant grows.
        let class = args.class.then(|| {
            if id.group.is_some() {
                return ClassProbe::Grouped;
            }
            class_cache
                .entry((id.venue.clone(), id.symbol.clone()))
                .or_insert_with(|| {
                    match client.properties_as_of(&id.venue, &id.symbol, CLASS_AS_OF_TS) {
                        // The READ this whole flag exists for: the field the venue producers write
                        // and, until now, nothing in the workspace read back.
                        Ok(Some(props)) => {
                            props.asset_class.map_or(ClassProbe::Unclassified, |c| {
                                ClassProbe::Classified(c.sql_word())
                            })
                        }
                        Ok(None) => ClassProbe::Unrecorded,
                        Err(e) => ClassProbe::Failed(e),
                    }
                })
                .clone()
        });
        rows.push(SeriesRow {
            kind: id.kind.clone(),
            venue: id.venue.clone(),
            name: id.label().to_string(),
            grouped: id.group.is_some(),
            symbol: id.symbol.clone(),
            group: id.group.clone(),
            interval: id.interval.clone(),
            coverage: Coverage {
                first_ts: cov.first_ts,
                last_ts: cov.last_ts,
                rows: cov.rows,
                bytes: cov.bytes,
                parts: cov.parts,
                dates: cov.dates,
            },
            gaps,
            gaps_error,
            class,
        });
    }

    if args.json {
        println!("{}", list_json(args, &rows, reported));
    } else {
        for line in list_lines(&rows, reported, !args.filter.is_empty(), args.gaps, args.class) {
            println!("{line}");
        }
        // ⚠ The disclosure goes to the HUMAN render only. `--json` is consumed by a program, and a
        // note appended to a document is noise at best and a parse error at worst; the JSON's
        // `reported` already carries the store's own total, so a consumer that wants the difference
        // can compute it. `account_exclusion_note` is pure and unit-tested beside the other
        // renderers.
        if let Some(note) = account_exclusion_note(excluded_count, &excluded) {
            println!("{note}");
        }
    }
    Ok(())
}

/// Refuse a READ whose `--kind` names an account kind outright — the second of §9.3.2's rules.
///
/// ⚠ The exclusion and this refusal answer DIFFERENT questions and both are needed. An unfiltered
/// listing withholds account series and says so (the note below); a listing that ASKED for one by
/// name must not answer "no series match", because that sentence is false — the series exist and
/// this plane declines to serve them. Silence there would send an operator to check the wrong end
/// of the pipe, which is the same failure [`empty_note`] exists to prevent one level up.
///
/// ⚠ EXACT match only. `--kind` on a listing is a SUBSTRING filter (the module doc's rule: an
/// unknown kind is a filter that matched nothing, not a roster error), so `--kind exec` stays a
/// filter and simply matches nothing after the exclusion. Refusing every substring that could
/// reach an account kind would put a roster's worth of guessing into a filter.
pub(super) fn refuse_an_account_kind_filter(args: &Args) -> Result<(), String> {
    let Some(kind) = args.filter.kind.as_deref() else { return Ok(()) };
    refuse_an_account_kind_on_a_read(kind)
}

/// The SENTENCE the rule above answers with, spelled once for every read-side flag that names a
/// kind.
///
/// ⚠ Two flags reach it and they are refused at different RUNGS, which is deliberate rather than an
/// oversight: `ls --kind` is a filter over an answer that has already arrived, so its refusal sits
/// in [`execute_list`]; `gate --require-kind` is a criterion, so [`parse`] refuses it before a
/// socket is opened. What must NOT differ is the words — an operator who meets the same rule twice
/// and reads two sentences learns that one of them is a different KIND of no, which is false.
/// Distinct from [`refuse_an_account_kind`], which is `rm`'s and carries the extra paragraph
/// `docs/decisions/0080-the-account-funding-kind-takes-the-qualified-name.md` requires for a
/// DELETE.
pub(super) fn refuse_an_account_kind_on_a_read(kind: &str) -> Result<(), String> {
    if !vike_model::is_account_kind(kind) {
        return Ok(());
    }
    Err(format!(
        "`{kind}` is ACCOUNT data, not market data, and no verb in this CLI plane reads it.\n\
         Your fills, orders, funding payments and equity belong to `vike-cli account`, which is \
         not built yet. Two of those four — equity and exec_fill — ARE on the datahub wire now, \
         so what is missing for them is the CLI verb rather than the read; orders and funding \
         payments are on neither. The market funding RATE is not an account kind — it is \
         `--kind bar --interval funding`."
    ))
}

/// The `§9.3.2` disclosure: what this ANSWER withheld, and why, in the operator's own terms.
///
/// `None` when nothing was withheld — a note that fires on every run stops being read.
///
/// ⚠ It names the KINDS rather than only a count, because the count alone cannot be acted on: an
/// operator who sees "3 series are hidden" has to guess whether the thing they are looking for is
/// among them. It also names the plane that WILL serve them, so the answer to "where did my fills
/// go" is in the message rather than in a document.
///
/// ⚠ **TWO verbs reach it now, and the sentence moved to stay true of both.** It read "…and
/// {is_are} not LISTED here", which was exact while [`execute_list`] was the only caller.
/// [`execute_gate`] withholds the same series from a VERDICT's evidence and lists nothing at all,
/// so the clause is "not part of this answer" — one rule keeping one sentence, which is the whole
/// reason [`refuse_an_account_kind_on_a_read`] exists as a shared function one rung up.
pub(super) fn account_exclusion_note(
    count: usize,
    kinds: &std::collections::BTreeSet<&str>,
) -> Option<String> {
    if count == 0 {
        return None;
    }
    // `series` is its own plural, so only the KIND noun and the verb inflect.
    let names: Vec<&str> = kinds.iter().copied().collect();
    let is_are = if count == 1 { "is" } else { "are" };
    Some(format!(
        "note: {count} series of kind{} {} {is_are} in this store and {is_are} not part of this \
         answer — they are account data (`vike-cli account`, not built yet).",
        if names.len() == 1 { "" } else { "s" },
        names.join(", "),
    ))
}

/// The human `list` table — PURE, so every column rule below is unit-tested rather than only seen.
///
/// One row per series, with `kind` and a `SCOPE` cell (`symbol` | `group`) as their own columns:
/// the identity is four dimensions with an alternative inside it, and joining them into a
/// `VENUE:SYMBOL:INTERVAL` string would render every grouped series as a venue and two empties.
///
/// ⚠ `FIRST`/`LAST` are `-` for a ZERO-ROW series rather than a date. A store folds an empty series
/// to an all-zero coverage, and `epoch_ms_to_utc_date(0)` is a perfectly well-formed `1970-01-01`
/// that reads as data. [`list_json`] carries the store's own numbers untouched — a render may
/// decline to show a sentinel, but a document may not edit one.
///
/// ⚠ The CLASS column is appended only under `--class`, and the widths are arranged so that WITHOUT
/// it every line is byte-identical to a build that had never heard of the flag: `last_w` is 0 there,
/// and `{:<0$}` pads to at least zero characters, i.e. not at all. A trailing column that was always
/// present would have had to render an unasked question in every cell.
pub(super) fn list_lines(
    rows: &[SeriesRow],
    reported: usize,
    filtered: bool,
    gaps_requested: bool,
    class_requested: bool,
) -> Vec<String> {
    if rows.is_empty() {
        return vec![empty_note("series", reported, filtered)];
    }
    let kind_w = col("KIND", rows.iter().map(|r| r.kind.len()));
    let venue_w = col("VENUE", rows.iter().map(|r| r.venue.len()));
    let scope_w = col("SCOPE", rows.iter().map(|r| scope_cell(r.grouped).len()));
    let name_w = col("NAME", rows.iter().map(|r| r.name.len()));
    let ivl_w = col("INTERVAL", rows.iter().map(|r| interval_cell(r).len()));
    let rows_w = col("ROWS", rows.iter().map(|r| r.coverage.rows.to_string().len()));
    let days_w = col("DAYS", rows.iter().map(|r| r.coverage.dates.to_string().len()));
    // Only the LAST column needs a measured width once something follows it — see this function's
    // doc for why zero is the right "no class asked for" value rather than a second format string.
    let last_w = if class_requested {
        col("LAST", rows.iter().map(|r| span_cell(r.coverage.last_ts, r.coverage.rows).len()))
    } else {
        0
    };
    let class_head = if class_requested { "  CLASS" } else { "" };

    let mut lines = vec![format!(
        "{:<kind_w$}  {:<venue_w$}  {:<scope_w$}  {:<name_w$}  {:<ivl_w$}  {:>rows_w$}  \
         {:>days_w$}  {:<10}  {:<last_w$}{class_head}",
        "KIND", "VENUE", "SCOPE", "NAME", "INTERVAL", "ROWS", "DAYS", "FIRST", "LAST"
    )];
    for r in rows {
        let class_tail =
            if class_requested { format!("  {}", class_cell(r)) } else { String::new() };
        lines.push(format!(
            "{:<kind_w$}  {:<venue_w$}  {:<scope_w$}  {:<name_w$}  {:<ivl_w$}  {:>rows_w$}  \
             {:>days_w$}  {:<10}  {:<last_w$}{class_tail}",
            r.kind,
            r.venue,
            scope_cell(r.grouped),
            r.name,
            interval_cell(r),
            r.coverage.rows,
            r.coverage.dates,
            span_cell(r.coverage.first_ts, r.coverage.rows),
            span_cell(r.coverage.last_ts, r.coverage.rows),
        ));
        if class_requested {
            lines.extend(class_error_line(r));
        }
        if gaps_requested {
            lines.extend(gap_lines(r));
        }
    }
    lines.push(String::new());
    let shown_rows: u64 = rows.iter().map(|r| r.coverage.rows).sum();
    let head = if filtered {
        format!("{} of {reported} series", rows.len())
    } else {
        format!("{reported} series")
    };
    lines.push(format!("{head} · {shown_rows} rows"));
    lines
}

/// One row's CLASS cell, under `--class` only.
///
/// ⚠ **The three absences are three different words, and none of them is blank.** `unclassified`
/// means a grid was recorded and named no class — the producer is not wired, which is the thing
/// this column was added to make visible; `no-properties` means nothing has recorded a grid for
/// this instrument at all; `(group)` means the question was not asked because a grouped series'
/// name is a GROUP and `properties_as_of` takes a symbol. Rendering any of them as an empty cell
/// would put the model's carefully-preserved `Option` back into the state its own doc refuses —
/// a guess and a fetch indistinguishable once stored.
///
/// The parenthesised two are the ones that are NOT facts about the instrument (nothing was asked;
/// something broke), which is why they wear brackets and the two real verdicts do not.
pub(super) fn class_cell(row: &SeriesRow) -> &'static str {
    match &row.class {
        Some(ClassProbe::Classified(word)) => word,
        Some(ClassProbe::Unclassified) => "unclassified",
        Some(ClassProbe::Unrecorded) => "no-properties",
        Some(ClassProbe::Grouped) => "(group)",
        Some(ClassProbe::Failed(_)) => "(error)",
        // UNREACHABLE from [`list_lines`], which only calls this under `class_requested`. Spelled
        // as a cell rather than a panic, for the reason [`empty_note`]'s last arm gives: a renderer
        // has no business aborting a run that already succeeded.
        None => "-",
    }
}

/// The reason under a row whose class probe FAILED, and nothing at all for any other outcome.
///
/// The asymmetry with [`gap_lines`] is deliberate: a gap probe's three outcomes all need a line
/// because "no gaps" is invisible in the row itself, whereas every class verdict is already IN the
/// row as a word. Only the failure carries text the cell cannot hold.
pub(super) fn class_error_line(row: &SeriesRow) -> Vec<String> {
    match &row.class {
        Some(ClassProbe::Failed(e)) => vec![format!("      class unavailable: {e}")],
        _ => Vec::new(),
    }
}

/// The gap annotation under one row, under `gaps` only.
///
/// Three outcomes, and all three are SAID rather than implied by an absence: holes, no holes, and
/// a probe this store could not answer. Printing nothing for the middle case would leave the
/// operator who ran `gaps` unable to tell an answered "clean" from an unasked question.
pub(super) fn gap_lines(row: &SeriesRow) -> Vec<String> {
    const INDENT: &str = "      ";
    match (&row.gaps, &row.gaps_error) {
        (_, Some(e)) => vec![format!("{INDENT}gaps unavailable: {e}")],
        (Some(ranges), None) if ranges.is_empty() => vec![format!("{INDENT}no gaps")],
        (Some(ranges), None) => ranges
            .iter()
            .map(|(from, to)| {
                format!(
                    "{INDENT}gap {} .. {}",
                    epoch_ms_to_utc_date(*from),
                    epoch_ms_to_utc_date(*to)
                )
            })
            .collect(),
        (None, None) => Vec::new(),
    }
}

/// The `list --json` document.
///
/// It carries `symbol` AND `group` beside the derived `name`/`grouped`, because the four dimensions
/// ARE the identity and a caller that wants to ask the same server about the same series needs them
/// rather than this side's rendering. `interval` is `null` for every tick-shaped kind, which is the
/// store's own answer and not an omission.
///
/// Gap ranges are OBJECTS (`from_ts`/`to_ts`), not two-element arrays: the wire shape is a tuple,
/// and a caller reading `g[0]`/`g[1]` has to remember an order that nothing in the document states.
///
/// ⚠ `coverage` carries the store's numbers UNTOUCHED — including a zero-row series' all-zero
/// timestamps, which [`list_lines`] renders as `-`. A document that substituted `null` there would
/// be reporting a judgement, and a caller folding several stores' inventories would have no way to
/// tell that judgement from a field the server never sent.
///
/// # ⚠ The class fields are a WIRE, and an absent class may not read as a present-but-empty one
///
/// `asset_class` alone could not carry this: a `null` there would mean "not asked", "no grid
/// recorded", "a grid that named no class" and "the probe failed" all at once, and the last three
/// are what an operator acts on differently. So the document carries `asset_class_status`
/// ([`ClassProbe::status`]) beside it, and `asset_class` is non-null for EXACTLY the `classified`
/// verdict — a relation pinned by `the_class_fields_are_a_status_and_a_word_that_cannot_disagree`.
/// `asset_class_error` carries the sentence for `error` and is null otherwise.
///
/// Under no `--class`, all three are `null` on every row and the top-level `class_requested` is
/// `false` — the exact shape `gaps`/`gaps_requested` already has, so a caller that learned the one
/// has not been handed a second convention. That pairing is also what keeps this ADDITIVE: a
/// pre-existing consumer sees three new always-null keys and a `false`.
pub(crate) fn list_json(args: &Args, rows: &[SeriesRow], reported: usize) -> String {
    let series: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            serde_json::json!({
                "kind": r.kind,
                "venue": r.venue,
                "name": r.name,
                "grouped": r.grouped,
                "symbol": r.symbol,
                "group": r.group,
                "interval": r.interval,
                "coverage": {
                    "first_ts": r.coverage.first_ts,
                    "last_ts": r.coverage.last_ts,
                    "rows": r.coverage.rows,
                    "bytes": r.coverage.bytes,
                    "parts": r.coverage.parts,
                    "dates": r.coverage.dates,
                },
                "gaps": r.gaps.as_ref().map(|ranges| {
                    ranges
                        .iter()
                        .map(|(from, to)| serde_json::json!({ "from_ts": from, "to_ts": to }))
                        .collect::<Vec<_>>()
                }),
                "gaps_error": r.gaps_error,
                "asset_class": r.class.as_ref().and_then(ClassProbe::word),
                "asset_class_status": r.class.as_ref().map(ClassProbe::status),
                "asset_class_error": match &r.class {
                    Some(ClassProbe::Failed(e)) => Some(e.as_str()),
                    _ => None,
                },
            })
        })
        .collect();
    let doc = serde_json::json!({
        // ⚠ DERIVED from the verb, never re-typed. These four renderers each carried the name as a
        // LITERAL, so the group split renamed `list` -> `ls` and `tape-health` -> `health` in the
        // CLI while the JSON document kept saying the old word — a document naming a spelling the
        // parser refuses. Four copies of one fact, found by the rename rather than by reading.
        "subcommand": args.sub.as_str(),
        "addr": args.addr,
        "filter": {
            "kind": args.filter.kind,
            "venue": args.filter.venue,
            "name": args.filter.name,
        },
        "gaps_requested": args.gaps,
        "class_requested": args.class,
        // What the SERVER reported, beside what the filter kept — so a caller can tell an empty
        // store from an over-narrow filter without a second call.
        "series_reported": reported,
        "count": series.len(),
        "series": series,
    });
    serde_json::to_string_pretty(&doc)
        .expect("a tree of strings, numbers, bools and nulls; serialization is total")
}
