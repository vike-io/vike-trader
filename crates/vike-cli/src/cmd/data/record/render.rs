//! How `data realtime record` renders — the `ls` listings, as a table or JSON.

use vike_secrets::profile_store::{ProfileKind, Profiles, StoredProfile, SubscriptionRow};

use super::plan::{parse_symbols, stored_recorder_names, what_of};
use super::{COMMAND, RESTART_NOTE, Render, col};

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
    let cells: Vec<[String; 4]> = subs
        .iter()
        .map(|s| {
            [
                s.ord.to_string(),
                s.venue.clone(),
                what_of(s),
                s.note.clone().unwrap_or_else(|| "-".to_string()),
            ]
        })
        .collect();
    let w_ord = col("ord", cells.iter().map(|c| c[0].chars().count()));
    let w_venue = col("venue", cells.iter().map(|c| c[1].chars().count()));
    let w_what = col("subscription", cells.iter().map(|c| c[2].chars().count()));
    out.push_str(&format!(
        "  {:<w_ord$}  {:<w_venue$}  {:<w_what$}  note\n",
        "ord", "venue", "subscription"
    ));
    for c in &cells {
        out.push_str(&format!(
            "  {:<w_ord$}  {:<w_venue$}  {:<w_what$}  {}\n",
            c[0], c[1], c[2], c[3]
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
        "note": s.note,
    });
    match s.symbols.as_deref().map(parse_symbols) {
        None => {}
        Some(Ok(list)) => row["symbols"] = serde_json::json!(list),
        Some(Err(_)) => row["symbols_unparsed"] = serde_json::json!(s.symbols),
    }
    row
}
