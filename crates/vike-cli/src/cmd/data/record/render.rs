//! How `data realtime record` renders — its usage page and the `ls` listings, as a table or JSON.

use vike_secrets::profile_store::{ProfileKind, Profiles, StoredProfile, SubscriptionRow};

use super::grammar::verb_roster;
use super::plan::{parse_symbols, stored_recorder_names, what_of};
use super::{COMMAND, RESERVED_GRAIN_FLAG, RESTART_NOTE, Render, col};

// ─── the usage page ──────────────────────────────────────────────────────────────────────────────

/// This group's usage page.
///
/// A FUNCTION rather than a `const` because the verb roster and the reserved word are DECLARATIONS
/// above, and a copy typed here is exactly the shape this repository has watched rot.
pub(super) fn usage() -> String {
    const PAGE: &str = "\
usage: vike-cli data realtime record <verb> [options]

WHAT THIS BOX PERSISTS. `data realtime watch` streams to THIS terminal and keeps nothing;
these verbs edit the SUBSCRIPTION ROWS a recording datahub mounts, in this project's
settings database. A subscription is a VENUE plus a FAMILY or a SYMBOL plus a BACKFILL.

  ls           the stored subscriptions — ord, venue, what, backfill, note — leading with
               a `source:` line naming the store that answered. --profiles lists the
               recorder profiles instead, which is how you see what --profile may name
  add SPEC     write one subscription row. SPEC is VENUE:SYMBOL or VENUE:@FAMILY, where
               `@` marks a whole market FAMILY recorded as ONE grouped series
  rm SPEC      remove one. A SPEC is not an identity — two symbols-based rows on one venue
               are legal — so an ambiguous match is REFUSED with every candidate's `ord`
               printed, and --ord N is how you pick

options:
  --profile N  which recorder profile to read or edit. DEFAULT: the profile marked
               `active`, which is the same row the daemon resolves, so the two cannot
               disagree. Zero recorder profiles is an ERROR naming the verb that creates
               one — this verb never creates a profile
  --profiles   ls: list the recorder profiles (name, active, store, how many rows) rather
               than one profile's subscriptions
  --backfill B add: `venue` | `archive` | `off`. OMITTING it files NO key, which is not the
               same as `off` — the row stores the difference, and the daemon applies its
               own default to an absent one
  --note TEXT  add: the operator's own note for this subscription. It is a COLUMN, so it
               survives; the TOML rendering does not carry it and never did
  --ord N      rm: pick one of several candidates by its `ord`, as printed by `ls`
  --dry-run    add/rm: print the plan and write NOTHING
  --format F   ls: `table` or `json`
  --json       ls: shorthand for --format json
  -h, --help   this message

⚠ A ROW IS LIVE AT THE DAEMON'S NEXT RESTART, not now — and `add` and `rm` are identical
  in this. The recorder re-resolves a subscription's SYMBOLS every tick and does not
  re-read the PROFILE.
⚠ There is no --lane and no --stream: the recorder picks its stream set ONCE for the whole
  process, so there is no per-stream grain here to select. {reserved} is the reserved word
  if there ever is one.
⚠ --addr is accepted and REFUSED by name: editing another box's rows needs wire verbs that
  do not exist yet. Run this on the box whose daemon records.

verbs: {verbs}";
    PAGE.replace("{verbs}", &verb_roster()).replace("{reserved}", RESERVED_GRAIN_FLAG)
}

// ─── reading — `ls` ──────────────────────────────────────────────────────────────────────────────

/// The `--profiles` listing.
pub(super) fn render_profiles(source: &str, profiles: &Profiles, render: Render) -> String {
    let rows: Vec<&StoredProfile> =
        profiles.all().iter().filter(|p| p.row.kind == ProfileKind::Recorder).collect();
    if render == Render::Json {
        let docs: Vec<serde_json::Value> = rows
            .iter()
            .map(|p| {
                serde_json::json!({
                    "name": p.row.name,
                    "active": p.row.active,
                    "store": p.recorder.as_ref().map(|b| b.row.store.clone()),
                    "subscriptions": p.recorder.as_ref().map_or(0, |b| b.subscriptions.len()),
                })
            })
            .collect();
        return format!(
            "{}\n",
            serde_json::json!({
                "source": source,
                "listing": "profiles",
                "profiles": docs,
                "note": RESTART_NOTE,
            })
        );
    }
    let mut out = format!("source: {source}\n");
    if rows.is_empty() {
        out.push_str(&format!("  (no recorder profiles). {}\n", stored_recorder_names(profiles)));
        return out;
    }
    let w_name = col("profile", rows.iter().map(|p| p.row.name.chars().count()));
    out.push_str(&format!("  {:<w_name$}  active  rows  store\n", "profile"));
    for p in rows {
        out.push_str(&format!(
            "  {:<w_name$}  {:<6}  {:<4}  {}\n",
            p.row.name,
            if p.row.active { "yes" } else { "-" },
            p.recorder.as_ref().map_or(0, |b| b.subscriptions.len()),
            p.recorder.as_ref().map_or("(no recorder body)", |b| b.row.store.as_str()),
        ));
    }
    out
}

/// One profile's subscription rows.
pub(super) fn render_subscriptions(source: &str, target: &StoredProfile, render: Render) -> String {
    let body = target.recorder.as_ref();
    let subs: &[SubscriptionRow] = body.map_or(&[], |b| b.subscriptions.as_slice());
    if render == Render::Json {
        let docs: Vec<serde_json::Value> = subs.iter().map(subscription_json).collect();
        return format!(
            "{}\n",
            serde_json::json!({
                "source": source,
                "listing": "subscriptions",
                "profile": target.row.name,
                "active": target.row.active,
                "store": body.map(|b| b.row.store.clone()),
                "subscriptions": docs,
                "note": RESTART_NOTE,
            })
        );
    }
    let mut out = format!("source: {source}\n");
    out.push_str(&format!(
        "profile: {}{}\n",
        target.row.name,
        if target.row.active { " (active)" } else { " (NOT the active profile)" }
    ));
    let Some(body) = body else {
        out.push_str(
            "  ⚠ no recorder body — this profile row carries none, so a profile flag naming it \
             would be refused at startup\n",
        );
        return out;
    };
    out.push_str(&format!("  store: {}\n", body.row.store));
    if subs.is_empty() {
        out.push_str(&format!(
            "  (no subscriptions — this profile records NOTHING). `{COMMAND} add VENUE:SYMBOL` \
             writes one.\n"
        ));
        return out;
    }
    let cells: Vec<[String; 5]> = subs
        .iter()
        .map(|s| {
            [
                s.ord.to_string(),
                s.venue.clone(),
                what_of(s),
                s.backfill.clone().unwrap_or_else(|| "-".to_string()),
                s.note.clone().unwrap_or_else(|| "-".to_string()),
            ]
        })
        .collect();
    let w_ord = col("ord", cells.iter().map(|c| c[0].chars().count()));
    let w_venue = col("venue", cells.iter().map(|c| c[1].chars().count()));
    let w_what = col("subscription", cells.iter().map(|c| c[2].chars().count()));
    let w_back = col("backfill", cells.iter().map(|c| c[3].chars().count()));
    out.push_str(&format!(
        "  {:<w_ord$}  {:<w_venue$}  {:<w_what$}  {:<w_back$}  note\n",
        "ord", "venue", "subscription", "backfill"
    ));
    for c in &cells {
        out.push_str(&format!(
            "  {:<w_ord$}  {:<w_venue$}  {:<w_what$}  {:<w_back$}  {}\n",
            c[0], c[1], c[2], c[3], c[4]
        ));
    }
    out.push_str(&format!("{RESTART_NOTE}\n"));
    out
}

/// One subscription row as a JSON object.
///
/// ⚠ **`symbols` is an ARRAY or null and never a string.** A hand-edited row whose column does not
/// parse changes the KEY rather than the TYPE — `symbols_unparsed` carries the raw text — so a
/// consumer's shape cannot be poisoned by a row nobody wrote through this verb.
fn subscription_json(s: &SubscriptionRow) -> serde_json::Value {
    let mut row = serde_json::json!({
        "ord": s.ord,
        "venue": s.venue,
        "family": s.family,
        "symbols": serde_json::Value::Null,
        "backfill": s.backfill,
        "note": s.note,
    });
    match s.symbols.as_deref().map(parse_symbols) {
        None => {}
        Some(Ok(list)) => row["symbols"] = serde_json::json!(list),
        Some(Err(_)) => row["symbols_unparsed"] = serde_json::json!(s.symbols),
    }
    row
}
