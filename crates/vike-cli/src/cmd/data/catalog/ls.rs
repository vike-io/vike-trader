//! `ls`, `refresh` and `show`: the venue's own list, and one instrument's recorded grid.

use vike_datahub_client::catalog::{CatalogListing, CatalogOutcome};
use vike_model::AssetClass;
use vike_node_proto::auth::{NodeKeys, Scope};

use crate::exit::{CliError, CmdResult};

use super::json::{ls_json, outcome_only_json, refresh_json, show_missing_json};
use super::{
    Args, CLASS_AS_OF_TS, InstrumentRef, InstrumentRow, col, connect, empty_note, show_json,
};

/// Does this row survive the client-side filters? Both are ANDed and an absent one matches
/// everything — the same browse-aid contract `crate::cmd::data`'s `Filter` states, and for the
/// same reason: nothing here reaches the wire, so a filter can never make the server or the venue
/// do less work.
pub(super) fn keeps(row: &InstrumentRow, class: Option<AssetClass>, search: Option<&str>) -> bool {
    if matches!(class, Some(c) if row.class != c) {
        return false;
    }
    match search {
        None => true,
        Some(needle) => {
            let needle = needle.to_lowercase();
            [&row.symbol, &row.base, &row.quote, &row.description]
                .iter()
                .any(|field| field.to_lowercase().contains(&needle))
        }
    }
}

/// A grid number, or `-` when the venue published none.
///
/// ⚠ **`0` is the model's ABSENT, not a tick size.** `vike_model::SymbolProperties`' own doc states
/// the convention — *"every field is `Default`-zero-or-`None` because the whole convention is
/// absent-is-`0`"* — and `vike_catalog::Instrument`'s says the properties *"may be `Default` until
/// fetched"*. Printing `0` for a tick size would read as a fact about the instrument, which is the
/// blank-cell defect `crate::cmd::data`'s `class_cell` argues against one group over.
///
/// The comparison is `<= 0.0` rather than `== 0.0`: a negative grid is nonsense from any venue, so
/// folding it into the same cell says the same true thing and costs no float-equality test.
pub(super) fn grid_cell(v: f64) -> String {
    if v <= 0.0 { "-".to_string() } else { v.to_string() }
}

/// `data catalog ls` — one `venue_catalog` round trip, filtered here, rendered here.
pub(super) fn execute_ls(args: &Args, keys: Option<&NodeKeys>) -> CmdResult<()> {
    let listing = fetch_listing(args, keys)?;
    let CatalogOutcome::Listed { instruments, truncated, .. } = &listing.outcome else {
        // [`outcome_only_json`], not [`ls_json`]: a refused listing carries no `instruments` key
        // at all, and an empty array there would be the collapse [`outcome_json`] prevents.
        return refuse_an_unlistable_venue(args, &listing, outcome_only_json);
    };
    // The one site where inference supplies `vike_catalog::Instrument`'s name — see
    // [`InstrumentRow`] for why it may not be written.
    let all: Vec<InstrumentRow> = instruments
        .iter()
        .map(|i| InstrumentRow {
            symbol: i.raw_symbol.clone(),
            class: i.asset_class,
            base: i.base.clone(),
            quote: i.quote.clone(),
            tick: i.properties.tick_size,
            lot: i.properties.step_size,
            description: i.description.clone(),
        })
        .collect();
    let narrowed = args.class.is_some() || args.search.is_some();
    let rows: Vec<InstrumentRow> =
        all.into_iter().filter(|r| keeps(r, args.class, args.search.as_deref())).collect();

    if args.json {
        println!("{}", ls_json(args, &listing, &rows));
    } else {
        for line in ls_lines(&rows, instruments.len(), narrowed, *truncated, &listing.describe()) {
            println!("{line}");
        }
    }
    Ok(())
}

/// `data catalog refresh` — the same wire call, rendering the COUNTS and which arm answered.
///
/// ⚠ It ships no instrument rows deliberately. `ls` is the listing; this verb's whole product is
/// the answer to "did that actually reach the venue", and printing twenty thousand symbols under
/// it would bury the one line it exists for.
pub(super) fn execute_refresh(args: &Args, keys: Option<&NodeKeys>) -> CmdResult<()> {
    let listing = fetch_listing(args, keys)?;
    let CatalogOutcome::Listed { instruments, cached, .. } = &listing.outcome else {
        // ⚠ **[`refresh_json`], not [`outcome_only_json`], and the difference is this verb's whole
        // product.** This arm used to emit the shared refusal document — the one `ls` emits — so a
        // `refresh --json` against an un-enumerable venue carried NO `reasked` key at all, while
        // [`refresh_json`]'s own ⚠ described a `reasked: null` document that nothing ever emitted.
        // A consumer of THIS verb branches on that field; making it absent on exactly the answers
        // where "did anything reach the venue" is most in doubt is the shape that teaches one to
        // treat a refusal as a `false`.
        return refuse_an_unlistable_venue(args, &listing, refresh_json);
    };
    if args.json {
        println!("{}", refresh_json(args, &listing));
    } else {
        for line in refresh_lines(instruments.len(), *cached, &listing.describe()) {
            println!("{line}");
        }
    }
    Ok(())
}

/// The one `venue_catalog` call both verbs make.
///
/// ⚠ The two failure classes are kept apart exactly as [`connect`] keeps them: a socket that could
/// not be reached is [`crate::exit::Exit::Connect`] — the rung a wrapper retries on — while a
/// SERVED error (the server does not advertise the lane, the venue slug was refused, the provider
/// itself failed) rides `?` onto [`crate::exit::Exit::Failed`], because the far side spoke.
/// ⚠ The venue is `unwrap_or_default`ed rather than `expect`ed, and the degenerate path is a clean
/// refusal rather than a panic: [`parse`] already requires `--venue` on both callers, and an empty
/// slug that somehow reached here is refused by
/// `vike_datahub_client::catalog::validate_catalog_venue` inside the client with a sentence naming
/// what a venue slug is. A renderer has no business aborting a run over a state its own parser
/// forbids.
fn fetch_listing(args: &Args, keys: Option<&NodeKeys>) -> CmdResult<CatalogListing> {
    let venue = args.venue.as_deref().unwrap_or_default();
    let mut client = connect(&args.addr, keys, Scope::Read)?;
    Ok(client.venue_catalog(venue)?)
}

/// The three NON-`Listed` outcomes, answered once for both verbs.
///
/// `document` is the verb's own `--json` renderer, passed in rather than chosen here: `ls` owes
/// [`outcome_only_json`] and `refresh` owes [`refresh_json`], whose `reasked` field is the one
/// thing a consumer reads that verb for. Both have the same signature, so the choice is a
/// function ITEM and costs nothing at the call site.
///
/// ⚠ **[`crate::exit::Exit::Empty`], not `Failed`, and not success either.** That rung exists so
/// that "the answer was nothing" and "nothing was evaluated" stop sharing a number, which is
/// precisely this distinction: a venue listed with zero instruments EXITS ZERO, while a venue that
/// could not be enumerated at all exits here. Collapsing them would put
/// `vike_datahub_client::catalog::CatalogOutcome`'s whole reason for being an enum back into a
/// pipeline's exit code — `data catalog ls --venue ig | wc -l` would read `0` for both.
///
/// It is not `Failed` because none of the three is a failure: `NotArmed` is the server's operator
/// doing what they wrote down, and both venue-shaped refusals are facts about the world. The
/// sentence is the WIRE's (`CatalogListing::describe`), never one assembled here, so the daemon,
/// the desktop and this CLI cannot describe one refusal three ways.
fn refuse_an_unlistable_venue(
    args: &Args,
    listing: &CatalogListing,
    document: fn(&Args, &CatalogListing) -> String,
) -> CmdResult<()> {
    if args.json {
        // stdout stays the document and nothing else; the sentence lands on stderr through
        // [`run`], which is the split `crate::cmd::data`'s module doc already states for `--json`.
        println!("{}", document(args, listing));
    }
    Err(CliError::empty(listing.describe()))
}

/// `ls`'s human rendering.
///
/// `listed` is what the SERVER sent, before the client-side filters; `narrowed` says whether any
/// filter ran. The pair is what lets [`empty_note`] tell "this venue lists nothing" apart from
/// "your filter selected nothing" — two opposite answers that a bare "nothing found" merges.
pub(super) fn ls_lines(
    rows: &[InstrumentRow],
    listed: usize,
    narrowed: bool,
    truncated: bool,
    describe: &str,
) -> Vec<String> {
    let mut lines = Vec::new();
    if rows.is_empty() {
        lines.push(empty_note("instruments", listed, narrowed));
        lines.push(describe.to_string());
        lines.extend(truncation_warning(truncated, narrowed));
        return lines;
    }
    let sym_w = col("SYMBOL", rows.iter().map(|r| r.symbol.len()));
    let class_w = col("CLASS", rows.iter().map(|r| r.class.sql_word().len()));
    let base_w = col("BASE", rows.iter().map(|r| r.base.len()));
    let quote_w = col("QUOTE", rows.iter().map(|r| r.quote.len()));
    let tick_w = col("TICK", rows.iter().map(|r| grid_cell(r.tick).len()));
    let lot_w = col("LOT", rows.iter().map(|r| grid_cell(r.lot).len()));
    lines.push(format!(
        "{:<sym_w$}  {:<class_w$}  {:<base_w$}  {:<quote_w$}  {:>tick_w$}  {:>lot_w$}  DESCRIPTION",
        "SYMBOL", "CLASS", "BASE", "QUOTE", "TICK", "LOT"
    ));
    for r in rows {
        lines.push(format!(
            "{:<sym_w$}  {:<class_w$}  {:<base_w$}  {:<quote_w$}  {:>tick_w$}  {:>lot_w$}  {}",
            r.symbol,
            r.class.sql_word(),
            r.base,
            r.quote,
            grid_cell(r.tick),
            grid_cell(r.lot),
            r.description
        ));
    }
    lines.push(String::new());
    lines.push(if narrowed {
        format!("{} of {listed} instruments · {describe}", rows.len())
    } else {
        describe.to_string()
    });
    lines.extend(truncation_warning(truncated, narrowed));
    lines
}

/// The line a TRUNCATED listing owes a FILTERED reader, and that the wire's own sentence cannot
/// give.
///
/// `CatalogListing::describe` already says a listing was truncated. What it cannot know is that a
/// filter then ran on this side: the instrument somebody searched for may be in the tail the
/// server never sent, so an empty or short result under a filter is NOT evidence the venue does
/// not list it. Without a filter the wire's own sentence is the whole story and this adds nothing.
pub(super) fn truncation_warning(truncated: bool, narrowed: bool) -> Vec<String> {
    if truncated && narrowed {
        vec![
            "⚠ the listing was TRUNCATED before this filter ran, so an instrument missing here \
             may be in the tail the server did not send rather than absent from the venue."
                .to_string(),
        ]
    } else {
        Vec::new()
    }
}

/// `refresh`'s human rendering — the wire's own sentence, plus the one fact it does not state
/// outright.
pub(super) fn refresh_lines(count: usize, cached: bool, describe: &str) -> Vec<String> {
    let mut lines = vec![describe.to_string()];
    lines.push(if cached {
        // The honest half. See the module doc: this wire carries no force bit, so a `cached`
        // answer means this verb re-asked NOTHING and an operator must not read it as a fetch.
        "⚠ NOTHING was re-asked: the server answered from its own in-process memo, whose TTL is \
         that server's and which no flag on this wire can bypass. The count above is up to that \
         TTL old."
            .to_string()
    } else {
        format!("the server called the venue and now holds {count} instruments for it.")
    });
    lines
}

// ─── `show`: one instrument's recorded grid ─────────────────────────────────────────────────────

/// `data catalog show VENUE:SYMBOL` — one `properties_as_of` round trip against the datahub's own
/// store.
///
/// ⚠ **The as-of instant is `crate::cmd::data`'s [`CLASS_AS_OF_TS`], reused rather than re-chosen.**
/// That constant's doc argues the whole decision — a PIT tape answered at `i64::MAX` is "the
/// latest row on record", and every row of one instrument must ask the same question or two
/// renderings of it can disagree. `data hist ls --class` and this verb answer about the same tape,
/// so a second instant here would be a second answer to one question.
pub(super) fn execute_show(args: &Args, keys: Option<&NodeKeys>) -> CmdResult<()> {
    // [`parse`] requires the positional on this verb, so the `else` is unreachable — spelled as a
    // usage refusal rather than a panic, for [`fetch_listing`]'s reason.
    let Some(target) = args.instrument.as_ref() else {
        return Err(CliError::usage(
            "`data catalog show` needs an instrument: VENUE:SYMBOL (e.g. binance:BTCUSDT)",
        ));
    };
    let mut client = connect(&args.addr, keys, Scope::Read)?;
    let props = client.properties_as_of(&target.venue, &target.symbol, CLASS_AS_OF_TS)?;
    let Some(props) = props else {
        if args.json {
            println!("{}", show_missing_json(args, target));
        }
        // ⚠ The EMPTY rung, for [`refuse_an_unlistable_venue`]'s reason: nothing was evaluated.
        // The sentence names the OTHER source deliberately — an absence here is a statement about
        // what has been RECORDED, and the venue may well list this instrument.
        return Err(CliError::empty(format!(
            "nothing has recorded an instrument grid for `{}:{}` in the store that datahub \
             opened, so there is nothing to show. That is a RECORDER fact, not a venue one — ask \
             the venue itself with `data catalog ls --venue {} --search {}`",
            target.venue, target.symbol, target.venue, target.symbol
        )));
    };
    if args.json {
        println!("{}", show_json(args, target, &props));
    } else {
        for line in show_lines(target, &props) {
            println!("{line}");
        }
    }
    Ok(())
}

/// `show`'s human rendering: a label/value block rather than a table, because there is one row and
/// a one-row table is a table nobody can read down.
pub fn show_lines(target: &InstrumentRef, props: &vike_model::SymbolProperties) -> Vec<String> {
    let class = match props.asset_class {
        Some(c) => c.sql_word(),
        // ⚠ The wiring signal, SAID rather than left blank — the identical verdict
        // `crate::cmd::data`'s `class_cell` renders for the same `None`: a grid was recorded and
        // the venue producer named no class for it.
        None => "unclassified",
    };
    let mut lines = vec![
        format!("{}:{}", target.venue, target.symbol),
        String::new(),
        format!("  asset class    {class}"),
        format!("  tick size      {}", grid_cell(props.tick_size)),
        format!("  lot step       {}", grid_cell(props.step_size)),
        format!("  min qty        {}", grid_cell(props.min_qty)),
        format!("  max qty        {}", grid_cell(props.max_qty)),
        format!("  min notional   {}", grid_cell(props.min_notional)),
        format!("  contract size  {}", grid_cell(props.contract_size)),
        String::new(),
        // The module doc's ⚠, at the one place a reader can act on it.
        "recorded in the datahub's kind=properties tape, latest row on record — NOT a live venue \
         query. `data catalog ls --venue <V>` asks the venue itself."
            .to_string(),
    ];
    if props.tick_size <= 0.0 || props.step_size <= 0.0 {
        lines.push(
            "⚠ the recorded grid names no tick size or no lot step. A row exists, so something \
             recorded it — a producer writing a DEFAULT grid is the shape that leaves these empty."
                .to_string(),
        );
    }
    lines
}
