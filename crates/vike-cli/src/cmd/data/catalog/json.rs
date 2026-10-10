//! The `--json` documents this group's verbs print.

use vike_datahub_client::catalog::{CatalogListing, CatalogOutcome, CatalogRefusal};
use vike_datahub_client::{FEATURE_BACKFILL, FEATURE_VENUE_CATALOG};
use vike_model::AssetClass;

use super::venues::skew;
use super::{Args, InstrumentRef, InstrumentRow, ServerView, VenueRow};

// ─── the `--json` documents ─────────────────────────────────────────────────────────────────────

/// The fields every document in this group opens with.
///
/// ⚠ `verb` is DERIVED from [`Verb::as_str`], never typed as a literal. `crate::cmd::data`'s
/// `tape_health_json` carries the incident that rule comes from: four renderers each spelled their
/// verb as a literal, the group split renamed two of them, and the documents went on naming
/// spellings the parser refuses.
pub(super) fn document_head(args: &Args) -> serde_json::Value {
    serde_json::json!({ "group": "catalog", "verb": args.verb.as_str(), "addr": args.addr })
}

/// One `CatalogOutcome`, as a STABLE machine token plus the numbers it carries.
///
/// ⚠ **`listed` is a number for `Listed` and `null` for everything else, and that is the whole
/// point.** An empty listing is `{"outcome":"listed","listed":0}` while an un-enumerable venue is
/// `{"outcome":"refused","listed":null}` — the type-level distinction
/// `vike_datahub_client::catalog`'s module doc exists to preserve, carried across the last hop to
/// a consumer rather than flattened on the way out.
pub(super) fn outcome_json(outcome: &CatalogOutcome) -> serde_json::Value {
    match outcome {
        CatalogOutcome::Listed { instruments, truncated, cached } => serde_json::json!({
            "outcome": "listed",
            "listed": instruments.len(),
            "truncated": truncated,
            "cached": cached,
            "refusal": serde_json::Value::Null,
            "supported": serde_json::Value::Null,
        }),
        CatalogOutcome::NotArmed => serde_json::json!({
            "outcome": "not_armed",
            "listed": serde_json::Value::Null,
            "truncated": serde_json::Value::Null,
            "cached": serde_json::Value::Null,
            "refusal": serde_json::Value::Null,
            "supported": serde_json::Value::Null,
        }),
        CatalogOutcome::Refused(refusal) => {
            let (token, supported) = match refusal {
                CatalogRefusal::NoBulkList { .. } => ("no_bulk_list", serde_json::Value::Null),
                CatalogRefusal::NeedsCredentials => ("needs_credentials", serde_json::Value::Null),
                CatalogRefusal::NotServed { supported } => {
                    ("not_served", serde_json::json!(supported))
                }
            };
            serde_json::json!({
                "outcome": "refused",
                "listed": serde_json::Value::Null,
                "truncated": serde_json::Value::Null,
                "cached": serde_json::Value::Null,
                "refusal": token,
                "supported": supported,
            })
        }
    }
}

/// Merge `extra` into `head` — both are objects by construction, so this is total.
pub(super) fn merged(mut head: serde_json::Value, extra: serde_json::Value) -> String {
    if let (Some(h), Some(e)) = (head.as_object_mut(), extra.as_object()) {
        for (k, v) in e {
            h.insert(k.clone(), v.clone());
        }
    }
    serde_json::to_string_pretty(&head)
        .expect("a tree of strings, numbers, bools and nulls; serialization is total")
}

/// `ls --json`: the outcome, the counts, and the rows that survived the filters.
///
/// ⚠ The `venue` field is `CatalogListing::venue` — the slug the SERVER echoed back — rather than
/// this side's `--venue` value. That type's own doc says it is echoed verbatim so a client holding
/// several in flight can match them up, and taking it from the answer rather than from the request
/// is what makes the document describe what arrived.
pub(super) fn ls_json(args: &Args, listing: &CatalogListing, rows: &[InstrumentRow]) -> String {
    let instruments: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            serde_json::json!({
                "symbol": r.symbol,
                // The model's own stored word, never a second spelling minted here.
                "asset_class": r.class.sql_word(),
                "base": r.base,
                "quote": r.quote,
                // ⚠ The RAW numbers, including the `0.0` the human table renders as `-`: a machine
                // reader asked for the grid in order to fold it, and a dash is this side's reading
                // rather than the datum.
                "tick_size": r.tick,
                "step_size": r.lot,
                "description": r.description,
            })
        })
        .collect();
    merged(
        document_head(args),
        serde_json::json!({
            "venue": listing.venue,
            "outcome_detail": outcome_json(&listing.outcome),
            "message": listing.describe(),
            "filter": { "class": args.class.map(AssetClass::sql_word), "search": args.search },
            "shown": instruments.len(),
            "instruments": instruments,
        }),
    )
}

/// `refresh --json`: the counts and — the field this verb exists for — whether anything was
/// actually re-asked.
///
/// ⚠ It answers for BOTH of [`execute_refresh`]'s exits, and that is a correction: the refusal
/// path used to print [`outcome_only_json`] instead, so the `reasked: null` document the ⚠ below
/// describes was emitted by nothing and the non-`Listed` arm was dead in production. `refresh
/// --json` now always carries the one field a consumer reads this verb for.
pub(super) fn refresh_json(args: &Args, listing: &CatalogListing) -> String {
    let reasked = match &listing.outcome {
        CatalogOutcome::Listed { cached, .. } => serde_json::json!(!cached),
        _ => serde_json::Value::Null,
    };
    merged(
        document_head(args),
        serde_json::json!({
            "venue": listing.venue,
            "outcome_detail": outcome_json(&listing.outcome),
            "message": listing.describe(),
            // ⚠ NOT a synonym for `!cached`: it is `null` when no listing happened at all, so a
            // consumer cannot read a refusal as "did not re-ask" and retry forever.
            "reasked": reasked,
        }),
    )
}

/// The document a refused `ls`/`refresh` still owes a machine reader. It carries no `instruments`
/// key at all — an empty array there would be the very collapse [`outcome_json`] prevents.
pub(super) fn outcome_only_json(args: &Args, listing: &CatalogListing) -> String {
    merged(
        document_head(args),
        serde_json::json!({
            "venue": listing.venue,
            "outcome_detail": outcome_json(&listing.outcome),
            "message": listing.describe(),
        }),
    )
}

/// `show --json` when nothing was recorded. `recorded: false` with every grid field ABSENT rather
/// than zeroed: a zero here would be indistinguishable from a recorded default grid, which is a
/// real and different state this verb warns about separately.
pub(super) fn show_missing_json(args: &Args, target: &InstrumentRef) -> String {
    merged(
        document_head(args),
        serde_json::json!({
            "venue": target.venue,
            "symbol": target.symbol,
            "recorded": false,
            "source": "datahub store, kind=properties, latest row on record",
        }),
    )
}

/// `venues --json`: two SEPARATE objects and the difference between them.
///
/// ⚠ There is deliberately no merged per-venue verdict. §8.1 wants the skew visible, and a
/// consumer that wants "can I watch binance live" ANDs `build` and `server` itself — having read,
/// in the document, which of the two said no.
///
/// ⚠ **`server.state` is a THREE-token field where this shipped `server.reachable`, a boolean.**
/// See [`ServerView::state`]: a server that answered the handshake and then refused the connection
/// is reached, and reporting `"reachable": false` for it was not lossy but false. The tokens are
/// `answered` / `refused` / `unreachable`, and every other field of this object is `null` on the
/// last two — including on `refused`, because a discarded handshake advertised nothing this side
/// may read.
pub(super) fn venues_json(args: &Args, rows: &[VenueRow], view: &ServerView) -> String {
    let venues: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            serde_json::json!({
                "venue": r.venue,
                "live_lanes": r.live,
                "backfill_bars": r.backfill_bars,
                "backfill_ticks": r.backfill_ticks,
            })
        })
        .collect();
    let server = match view {
        ServerView::Answered(features) => serde_json::json!({
            "state": view.state(),
            "error": serde_json::Value::Null,
            "features": features,
            "md_venues": view.md_venues(),
            "serves_backfill": view.serves(FEATURE_BACKFILL),
            "serves_venue_catalog": view.serves(FEATURE_VENUE_CATALOG),
        }),
        // The two NON-answers are ONE shape and differ only in their token and their sentence, so
        // the nulls are typed once: a second copy of them is how two documents of one verb come to
        // disagree about which keys a consumer may expect to be present.
        ServerView::Refused(why) | ServerView::Unreachable(why) => serde_json::json!({
            "state": view.state(),
            "error": why,
            // ⚠ `null`, not `[]`: this side holds no advertisement it may read, and an empty list
            // would say the SERVER advertised nothing — a different and much stronger claim.
            //
            // ⚠ **This read "nothing was asked", which is the one claim the `refused` half of this
            // shared arm contradicts** — the sentence deleted from `venues_lines`'s operator-facing
            // text and from [`ServerView::serves`]'s doc, left standing on the arm that BUILDS the
            // `refused` document. A keyed datahub met with no node keys completes `handshake()` and
            // only then refuses, so on `refused` the question was asked, the far side answered, and
            // THIS side dropped the `Welcome` with the connection ([`ServerView::md_venues`] is
            // where). Only [`ServerView::Unreachable`] means nobody was asked — which is why these
            // two share a SHAPE and not a reason.
            "features": serde_json::Value::Null,
            "md_venues": serde_json::Value::Null,
            "serves_backfill": serde_json::Value::Null,
            "serves_venue_catalog": serde_json::Value::Null,
        }),
    };
    let skew_doc = match skew(rows, view) {
        Some(s) => serde_json::json!({
            "declared_here_unserved_there": s.declared_here_unserved_there,
            "served_there_unknown_here": s.served_there_unknown_here,
        }),
        None => serde_json::Value::Null,
    };
    merged(
        document_head(args),
        serde_json::json!({ "build": { "venues": venues }, "server": server, "skew": skew_doc }),
    )
}
