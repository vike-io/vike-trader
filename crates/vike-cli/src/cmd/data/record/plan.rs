//! What `data realtime record` resolves and plans — the target profile, the edit, the venue check.

use std::path::Path;

use vike_node_proto::auth::{NodeKeys, Scope};
use vike_secrets::profile_store::{
    ActiveProfile, ProfileKind, Profiles, RecorderBody, StoredProfile, SubscriptionRow,
    render_recorder_toml, toml_string_array,
};

use super::{Args, COMMAND, REC_VENUE_PREFIX, Spec, Verb, What, connect};
use crate::exit::{CliError, CmdResult};

/// Seconds since the Unix epoch, for the row's `updated_utc` stamp. A wall clock is the right one —
/// the column answers "when did an operator last write this", not an interval. `vike-cli` is not
/// one of `crates/vike-ops/tests/hygiene/clock_pin.rs`'s determinism-critical crates and already makes
/// several such reads, so this adds no row there.
pub(super) fn now_utc() -> i64 {
    vike_model::now_ms() / 1_000
}

// ─── resolving the profile — ruling 4 ────────────────────────────────────────────────────────────

/// **Which recorder profile this line acts on**, and, when there is none, WHICH no it is.
///
/// ⚠ The three noes name three different next commands, which is why
/// `vike_secrets::profile_store::Profiles::resolve_active` returns a reason rather than an
/// `Option` — and why this function does not collapse them. It splits the first of them FURTHER,
/// because `read_profiles` cannot: "no database" and "a database written before the profile tables
/// existed" both arrive as `NoProfileStore`, and only a filesystem probe can tell them apart. They
/// need different commands, so the probe is worth its line.
pub(super) fn resolve_target<'a>(
    db: &Path,
    profiles: &'a Profiles,
    named: Option<&str>,
) -> CmdResult<&'a StoredProfile> {
    if let Some(name) = named {
        let Some(found) = profiles.by_name(name) else {
            return Err(CliError::failed(format!(
                "no profile named `{name}` in {}. {}",
                db.display(),
                stored_recorder_names(profiles)
            )));
        };
        if found.row.kind != ProfileKind::Recorder {
            return Err(CliError::failed(format!(
                "profile `{name}` is a `{}` profile rather than a recorder one, so it has no \
                 subscription rows to edit. {}",
                found.row.kind.sql_word(),
                stored_recorder_names(profiles)
            )));
        }
        return Ok(found);
    }
    match profiles.resolve_active(ProfileKind::Recorder) {
        ActiveProfile::Row(p) => Ok(p),
        ActiveProfile::NoProfileStore => Err(CliError::failed(no_store_refusal(db))),
        ActiveProfile::NoneStored => Err(CliError::failed(format!(
            "{} holds no recorder profile at all, so there is no subscription set to edit. \
             `vike-cli config mirror --recorder <file>` is what creates one from the TOML profile \
             this box already records with — this verb never creates a profile, because inventing \
             one would be inventing which venue feeds OPEN",
            db.display()
        ))),
        ActiveProfile::NoneActive { stored } => Err(CliError::failed(format!(
            "{stored} recorder profile(s) are stored and NONE is marked active, so there is no \
             default to edit. Name one with `--profile NAME` — `{COMMAND} ls --profiles` lists \
             them. {}",
            stored_recorder_names(profiles)
        ))),
    }
}

/// The `NoProfileStore` refusal, split by a filesystem probe into the two states `read_profiles`
/// collapses — they name different next commands.
fn no_store_refusal(db: &Path) -> String {
    if vike_secrets::database_present(db) {
        format!(
            "{} exists but holds no profile tables, so nothing has been mirrored into it yet. \
             `vike-cli config mirror --recorder <file>` is what puts a recorder profile there; \
             until then the recorder profile this box uses is still the FILE its unit's --record \
             names",
            db.display()
        )
    } else {
        format!(
            "there is no settings database at {} — this box has not been migrated, so there is \
             nowhere to store a subscription row. `vike-cli secrets migrate` creates the store and \
             `vike-cli config mirror --recorder <file>` puts a recorder profile in it. This verb \
             creates neither",
            db.display()
        )
    }
}

/// The recorder profiles a refusal names, so an operator is never told "no" without being told what
/// the alternatives are.
pub(super) fn stored_recorder_names(profiles: &Profiles) -> String {
    let names: Vec<&str> = profiles
        .all()
        .iter()
        .filter(|p| p.row.kind == ProfileKind::Recorder)
        .map(|p| p.row.name.as_str())
        .collect();
    if names.is_empty() {
        return "This store holds no recorder profile at all.".to_string();
    }
    format!("Recorder profiles in this store: {}.", names.join(", "))
}

/// One row's subscription, as `ls` and every refusal spell it.
pub(super) fn what_of(s: &SubscriptionRow) -> String {
    match (&s.family, &s.symbols) {
        (Some(f), _) => format!("family {f}"),
        (None, Some(syms)) => format!("symbols {syms}"),
        (None, None) => "(nothing)".to_string(),
    }
}

// ─── the symbols column ──────────────────────────────────────────────────────────────────────────

/// Parse the `symbols` column, which is stored as the TOML ARRAY RENDERING (`["BTCUSDT.P"]`) rather
/// than as a list — see `vike_secrets::profile_store::SubscriptionRow`, whose writer produces it
/// with `toml_string_array` so the renderer can emit the column verbatim.
///
/// # Errors
///
/// A `String` when the column is not a TOML array of strings — which a hand-edited row can be, and
/// which this module reports rather than silently treating as empty.
pub(super) fn parse_symbols(rendered: &str) -> Result<Vec<String>, String> {
    let doc: toml::Value = toml::from_str(&format!("v = {rendered}"))
        .map_err(|e| format!("the stored `symbols` column is not a TOML array: {e}"))?;
    let array = doc
        .get("v")
        .and_then(toml::Value::as_array)
        .ok_or_else(|| "the stored `symbols` column is not an ARRAY".to_string())?;
    let mut out = Vec::with_capacity(array.len());
    for item in array {
        out.push(
            item.as_str()
                .ok_or_else(|| "the stored `symbols` column holds a non-STRING".to_string())?
                .to_string(),
        );
    }
    Ok(out)
}

// ─── writing — `add` and `rm` ────────────────────────────────────────────────────────────────────

/// A write, planned but not performed: the whole profile as it WOULD be stored, plus the report.
#[derive(Debug)]
pub(super) struct Plan {
    pub(super) stored: StoredProfile,
    pub(super) report: String,
}

/// Build the mutated profile and the report, refusing every way the edit does not make sense.
///
/// ⚠ It clones the WHOLE `StoredProfile` rather than rebuilding one — see this module's doc. The
/// mounts, params, settings and profile-level note ride through untouched, and so does every
/// subscription `note` the rendered TOML cannot carry.
pub(super) fn plan_write(args: &Args, spec: &Spec, target: &StoredProfile) -> CmdResult<Plan> {
    let mut stored = target.clone();
    let Some(body) = stored.recorder.as_mut() else {
        return Err(CliError::failed(format!(
            "profile `{}` carries no recorder body, so it has no subscription set to edit. \
             `vike-cli config mirror --recorder <file>` is what creates one",
            target.row.name
        )));
    };
    let mut report = format!("profile: {}\n", target.row.name);
    match args.verb {
        Verb::Add => {
            if let Some(clash) = duplicate_of(&body.subscriptions, spec)? {
                return Err(CliError::failed(format!(
                    "`{}` is already a subscription of profile `{}`, at ord {} ({}). NOTHING was \
                     written. `{COMMAND} ls` prints the set; `{COMMAND} rm {}` removes it",
                    spec.render(),
                    target.row.name,
                    clash.ord,
                    what_of(&clash),
                    spec.render()
                )));
            }
            let ord = body.subscriptions.iter().map(|s| s.ord).max().map_or(0, |m| m + 1);
            let row = SubscriptionRow {
                ord,
                venue: spec.venue.clone(),
                family: match &spec.what {
                    What::Family(f) => Some(f.clone()),
                    What::Symbol(_) => None,
                },
                symbols: match &spec.what {
                    What::Family(_) => None,
                    What::Symbol(s) => Some(toml_string_array(std::slice::from_ref(s))),
                },
                backfill: args.backfill.clone(),
                note: args.note.clone(),
            };
            report.push_str(&format!("+ ord {ord}  {}  {}\n", row.venue, what_of(&row)));
            body.subscriptions.push(row);
        }
        Verb::Rm => {
            let removed = remove_one(&mut body.subscriptions, spec, args.ord, &target.row.name)?;
            report.push_str(&format!(
                "- ord {}  {}  {}\n",
                removed.ord,
                removed.venue,
                what_of(&removed)
            ));
        }
        Verb::Ls => unreachable!("`ls` never plans a write"),
    }
    // ⚠ THE PARSE CHECK, and it is a CHECK rather than the write: the rendered document is thrown
    // away. It exists because the mount's own read is `render_recorder_toml` →
    // `RecorderProfile::from_toml`, so a row this verb wrote that does not survive that round trip
    // is a daemon that refuses to start — and the place to find that out is here, before anything
    // is stored.
    parse_check(body)?;
    report.push_str(&format!("\nresulting subscriptions ({}):\n", body.subscriptions.len()));
    for s in &body.subscriptions {
        report.push_str(&format!("  ord {}  {}  {}\n", s.ord, s.venue, what_of(s)));
    }
    Ok(Plan { stored, report })
}

/// Render the mutated body and require the result to PARSE. Nothing is stored from it.
fn parse_check(body: &RecorderBody) -> CmdResult<()> {
    let rendered = render_recorder_toml(body);
    toml::from_str::<toml::Value>(&rendered).map_err(|e| {
        CliError::failed(format!(
            "the edited rows render a document that does not parse ({e}), and that document is \
             what the recording daemon reads at mount — so storing them would be storing a profile \
             that refuses to start. NOTHING was written.\n--- what would have been stored \
             ---\n{rendered}"
        ))
    })?;
    Ok(())
}

/// Is this SPEC already stored? `Ok(None)` when it is not.
///
/// # Errors
///
/// A [`CliError`] when a second FAMILY row on one venue was asked for — the store's partial unique
/// index refuses that, and refusing here names the existing row instead of handing back a SQLite
/// message — or when an existing row's `symbols` column cannot be read. A comparison that cannot be
/// made is reported rather than answered "no", because answering "no" writes a duplicate.
fn duplicate_of(rows: &[SubscriptionRow], spec: &Spec) -> CmdResult<Option<SubscriptionRow>> {
    for row in rows {
        if row.venue != spec.venue {
            continue;
        }
        match (&spec.what, &row.family) {
            (What::Family(f), Some(existing)) if f == existing => return Ok(Some(row.clone())),
            // ⚠ `subscription_one_family_per_venue` is UNIQUE over `(profile, venue, family)` WHERE
            // family IS NOT NULL, so a SECOND family row on this venue would be refused by SQLite
            // at INSERT — after `store_profile` had already deleted the whole body inside its
            // transaction. Refusing here names the row that is in the way instead.
            (What::Family(_), Some(existing)) => {
                return Err(CliError::failed(format!(
                    "venue `{}` already has a FAMILY subscription in this profile (`{existing}`, \
                     ord {}), and the store admits only one per venue \
                     (`subscription_one_family_per_venue`). NOTHING was written — remove that one \
                     first, or subscribe to symbols instead",
                    spec.venue, row.ord
                )));
            }
            (What::Symbol(s), None) => {
                let Some(rendered) = row.symbols.as_deref() else { continue };
                let list = parse_symbols(rendered).map_err(|e| {
                    CliError::failed(format!(
                        "profile row ord {} cannot be compared: {e}. NOTHING was written",
                        row.ord
                    ))
                })?;
                if list.iter().any(|x| x == s) {
                    return Ok(Some(row.clone()));
                }
            }
            _ => {}
        }
    }
    Ok(None)
}

/// Remove the ONE row a SPEC names — ruling 5.
///
/// ⚠ **A SPEC IS NOT AN IDENTITY and this function never guesses.** Two symbols-based rows on one
/// venue are legal (the unique index is partial), so a match of more than one is REFUSED with every
/// candidate's `ord` printed, and `--ord N` is how the operator picks. A silent wrong removal
/// leaves a perfectly healthy-looking reconcile.
///
/// ⚠ A symbol that is one of SEVERAL on a row is a NEAR MISS rather than a match, and is reported
/// as one: this verb removes a whole subscription ROW, and removing a four-symbol row because one
/// of its symbols was named would be a silent over-removal. Editing a multi-symbol row's list is
/// not built, and the message says so rather than leaving the operator to infer it.
pub(super) fn remove_one(
    rows: &mut Vec<SubscriptionRow>,
    spec: &Spec,
    ord: Option<i64>,
    profile: &str,
) -> CmdResult<SubscriptionRow> {
    let (exact, near) = match_rows(rows, spec)?;
    let candidates: Vec<SubscriptionRow> = match ord {
        Some(n) => exact.iter().filter(|r| r.ord == n).cloned().collect(),
        None => exact.clone(),
    };
    if candidates.is_empty() {
        return Err(CliError::failed(no_match_refusal(spec, ord, profile, &exact, &near)));
    }
    if candidates.len() > 1 {
        let mut msg = format!(
            "`{}` matches {} subscriptions of profile `{profile}`, and a SPEC is not an identity \
             here — the store's unique index covers FAMILY rows only, so two symbols-based rows on \
             one venue are legal. NOTHING was removed. Pick one with --ord N:",
            spec.render(),
            candidates.len()
        );
        for r in &candidates {
            msg.push_str(&format!("\n    ord {}  {}  {}", r.ord, r.venue, what_of(r)));
        }
        return Err(CliError::failed(msg));
    }
    let chosen = candidates[0].ord;
    let at = rows.iter().position(|r| r.ord == chosen).expect("the candidate came from these rows");
    Ok(rows.remove(at))
}

/// The refusal when nothing matched — it names the NEAR misses, because the commonest way to get
/// here is naming one symbol of a row that lists several.
fn no_match_refusal(
    spec: &Spec,
    ord: Option<i64>,
    profile: &str,
    exact: &[SubscriptionRow],
    near: &[SubscriptionRow],
) -> String {
    let mut msg = format!(
        "no subscription of profile `{profile}` matches `{}`{}. NOTHING was removed",
        spec.render(),
        ord.map_or_else(String::new, |n| format!(" at ord {n}"))
    );
    if !near.is_empty() {
        msg.push_str(
            ". ⚠ It IS listed on a row that names other symbols too, and this verb removes a whole \
             subscription ROW rather than one symbol of one — editing a row's symbol list is not \
             built:",
        );
        for r in near {
            msg.push_str(&format!("\n    ord {}  {}  {}", r.ord, r.venue, what_of(r)));
        }
    } else if !exact.is_empty() {
        msg.push_str(". The rows this spec DOES match:");
        for r in exact {
            msg.push_str(&format!("\n    ord {}  {}  {}", r.ord, r.venue, what_of(r)));
        }
    }
    msg
}

/// Which rows a SPEC matches EXACTLY, and which it merely appears on.
///
/// A family spec matches a row whose `family` equals it. A symbol spec matches a row whose
/// `symbols` list is EXACTLY that one symbol; a row that lists it among others is a NEAR MISS.
///
/// # Errors
///
/// A [`CliError`] when a row's `symbols` column cannot be parsed — reported rather than skipped,
/// because a row silently treated as non-matching is a row `rm` cannot reach.
fn match_rows(
    rows: &[SubscriptionRow],
    spec: &Spec,
) -> CmdResult<(Vec<SubscriptionRow>, Vec<SubscriptionRow>)> {
    let mut exact = Vec::new();
    let mut near = Vec::new();
    for row in rows {
        if row.venue != spec.venue {
            continue;
        }
        match (&spec.what, &row.family) {
            (What::Family(f), Some(existing)) if f == existing => exact.push(row.clone()),
            (What::Symbol(s), None) => {
                let Some(rendered) = row.symbols.as_deref() else { continue };
                let list = parse_symbols(rendered).map_err(|e| {
                    CliError::failed(format!(
                        "profile row ord {} cannot be matched: {e}. NOTHING was removed — \
                         `vike-cli config recorder` prints the stored column",
                        row.ord
                    ))
                })?;
                if list.len() == 1 && list[0] == *s {
                    exact.push(row.clone());
                } else if list.iter().any(|x| x == s) {
                    near.push(row.clone());
                }
            }
            _ => {}
        }
    }
    Ok((exact, near))
}

// ─── the venue check — ruling 7 ──────────────────────────────────────────────────────────────────

/// What the handshake said about a venue's recordability.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum VenueVerdict {
    /// The datahub advertises this venue as recordable.
    Recordable,
    /// It advertises some venues and NOT this one — the one verdict that refuses.
    NotRecordable(Vec<String>),
    /// It could not be asked, or it advertises none at all. WARN and write.
    CannotSay(String),
}

/// Every venue slug a server advertised as RECORDABLE — the read half of the [`REC_VENUE_PREFIX`]
/// pair, written the way `vike_datahub_client::advertised_md_venues` reads its own: each
/// value trimmed, an EMPTY value dropped, because an empty advertisement advertises nothing.
pub(super) fn advertised_rec_venues(features: &[String]) -> Vec<String> {
    features
        .iter()
        .filter_map(|f| f.strip_prefix(REC_VENUE_PREFIX))
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .collect()
}

/// Ask the configured datahub whether it can record this venue. BEST EFFORT — this module's doc
/// carries the three-row table it implements.
pub(super) fn probe_venue(addr: &str, keys: Option<&NodeKeys>, venue: &str) -> VenueVerdict {
    let client = match connect(addr, keys, Scope::Read) {
        Ok(c) => c,
        Err(e) => {
            return VenueVerdict::CannotSay(format!(
                "the datahub at {addr} could not be asked which venues it can record ({}), so this \
                 row was written UNCHECKED",
                e.msg
            ));
        }
    };
    let advertised = advertised_rec_venues(client.features());
    if advertised.is_empty() {
        return VenueVerdict::CannotSay(format!(
            "the datahub at {addr} advertises no recordable venue at all — it predates the \
             `{REC_VENUE_PREFIX}` advertisement, or it was built with no recorder venue feature. \
             This row was written UNCHECKED"
        ));
    }
    if advertised.iter().any(|v| v == venue) {
        return VenueVerdict::Recordable;
    }
    VenueVerdict::NotRecordable(advertised)
}

/// The refusal a `NotRecordable` verdict produces — named so the message and the verdict cannot
/// drift, and so it states the CONSEQUENCE rather than a preference.
pub(super) fn unrecordable_refusal(venue: &str, advertised: &[String]) -> String {
    format!(
        "the datahub this box records with does NOT record `{venue}` — it advertises [{}]. NOTHING \
         was written. A row naming a venue the daemon's build has no feed for is legal in the store \
         and takes the daemon down at its NEXT RESTART (the daemon's recording table errors \
         by name, and the unit restarts on failure), so this is refused rather than warned about. \
         Rebuild the datahub with that venue's recorder feature, or record it from a box that has \
         one",
        advertised.join(", ")
    )
}
