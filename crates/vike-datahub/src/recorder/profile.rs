//! The recording PROFILE, loaded and judged before a port binds: read, parse, one store root.

use std::path::{Path, PathBuf};

use vike_recorder::config::RecorderProfile;

/// ONE store, or refuse to start: the profile's `store` and the root the server resolved must name
/// the same directory.
///
/// **Why a refusal rather than a winner.** Before the merge these were two processes and two
/// answers was merely wasteful; ruling 10 makes them one process whose whole point is that the tape
/// it records is the tape it serves. If the profile won silently, an operator's `VIKE_DATAHUB_STORE`
/// would stop describing what the server answers from. If the server won silently, the tape would
/// start landing somewhere the operator's own profile does not name — and a store does not merge,
/// so the old one is simply no longer read and every query returns zero rows, which looks exactly
/// like an empty date range. Both silent answers produce a lie; the refusal names both paths and
/// both knobs and costs one edit.
///
/// **The comparison is TEXTUAL, after making both absolute against the working directory the
/// composition root swept** — not `canonicalize`, which requires both to exist and would refuse a
/// first run against a store root that has not been created yet. A symlinked alias is therefore
/// refused too; the message says so, and the fix is to make the two name one directory.
///
/// `cwd` is the composition root's ONE `current_dir()` answer, threaded rather than read, for the
/// same reason every other path in this daemon is: a second read is a second answer.
///
/// # ⚠ WHAT THE ROW MIGRATION DID TO THIS CHECK — the decision, written where the check is
///
/// `docs/decisions/0057-…` predicted that moving `recorder.toml` into the database would *"not
/// delete the check, but it does delete the reason anybody wrote it"*, because both sides would
/// become rows in one file. **That premise is FALSE, measured on the code, and the correction is
/// why this function is unchanged.**
///
/// Only ONE side became a row. The other is `Environment=VIKE_DATAHUB_STORE=…` on the unit's own
/// line — 0057 itself rules that the phases require no unit change, and the EROFS finding (no
/// shipped unit grants `settings/db`) is precisely the argument for leaving units alone. So after
/// the migration this still compares **a DATABASE ROW against a systemd `Environment=` LINE**,
/// which are as independently authored as the two files were.
///
/// **The asymmetry got WORSE, not better, which is the whole reason the refusal stays.** Before,
/// both sides needed an editor and a restart. Now the row is editable by `vike-cli` against a
/// running box while `VIKE_DATAHUB_STORE` still needs a unit edit, a `daemon-reload` and a restart.
/// Drift became MORE reachable, so the refusal became MORE load-bearing.
///
/// Two alternatives were considered and refused:
///
///   * **drop the `store` column and derive it from the server's resolved root.** That deletes the
///     refusal outright and takes away the profile's ability to STATE where it records — the one
///     claim `crates/vike-recorder/recorder.example.toml`'s own header tells an operator to get
///     right. It also silently changes the CI box, whose row and unit agree today on a DECLARED
///     DEPARTURE (`data/hist`): with the column gone, nothing would ever notice the unit's value
///     moving.
///   * **make the column NULLABLE, NULL meaning "whatever the server resolved".** Genuinely better
///     for a fresh install — one spelling instead of two — and it invents an "absence means agree"
///     semantic the FILE world does not have, so a migrated box and a file box would stop being
///     byte-identical. That is what the capability-map playbook's STEP 1 forbids. It is a real
///     improvement and it ships as its own argued change, never inside a migration.
///
/// This function needs no code change for any of that: it is pure over two `&Path`s and knows
/// nothing about where either came from. What DID change is the sentence its failure message used
/// to end with — *"edit the profile's `store` key"* names a file that may no longer answer on a
/// migrated box, the same defect `vike-cli secrets list`'s `source:` line was added to fix on the
/// credential side — so the message now names the ORIGIN it was given.
pub fn one_store_root(
    profile_store: &Path,
    server_root: &Path,
    cwd: Option<&Path>,
) -> Result<(), String> {
    let absolutize = |p: &Path| -> PathBuf {
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            match cwd {
                Some(base) => base.join(p),
                None => p.to_path_buf(),
            }
        }
    };
    let profile_abs = absolutize(profile_store);
    let server_abs = absolutize(server_root);
    if profile_abs == server_abs {
        return Ok(());
    }
    Err(format!(
        "the recording profile and this server disagree about the store root, and since the \
         recorder merged into the data daemon (ruling 10) there is only one:\n  \
         profile `store` = {}\n  server root    = {}\nOne process owns venue connections, the \
         store and serving, so a recording that landed in the first would be invisible to every \
         query answered from the second. Point them at one directory — change the profile's \
         `store` (⚠ WHICH profile: a `--record` file, or the `recorder` ROW a `--recorder-profile` \
         names; the refusal above this one names the origin this process actually read), or set \
         VIKE_DATAHUB_STORE (the unit's Environment= line) to the profile's path — and \
         start again. Both paths above are shown as this process resolved them, absolute against \
         its working directory; a symlink that makes two spellings the same directory is still \
         refused, because this comparison is textual by design (canonicalising would require a \
         store root that may not exist yet).",
        profile_abs.display(),
        server_abs.display()
    ))
}

/// **EVERYTHING ABOUT A RECORDING THAT CAN BE REFUSED BEFORE A PORT IS BOUND** — read the profile,
/// parse it, and answer every question whose answer is a property of the FILE plus this build.
///
/// ⚠ **This exists because the refusals used to fire too late, and "too late" here means a
/// CRASH-LOOP rather than a refusal to start.** `record` performed the read, the parse and the
/// [`one_store_root`] comparison, and `datahub_cli` calls `record` only after the bind guard has
/// passed, the store has opened, the listener holds `VIKE_DATAHUB_ADDR` and the serve thread is
/// running. So a one-character typo in the profile's `store` key produced: bind, serve, refuse,
/// exit non-zero, `Restart=on-failure`, repeat — a daemon taking the data wire up and down every
/// five seconds instead of failing once and staying down where `systemctl status` can say why.
///
/// The four questions, in the order they are cheapest to answer:
///
///   1. the file READS (the message names the path — a `--record` typo);
///   2. the file PARSES (the message names the path FIRST: `ProfileError` is a library type that
///      never saw one, so its own text opens `recorder profile: TOML parse error at line 4`, and
///      this lands in a journal hours later where `--record` is a line in a unit nobody has open);
///   3. the profile and the server name ONE STORE ROOT ([`one_store_root`]);
///   4. the profile asks for something this BUILD can actually record — a non-empty `[[subscribe]]`
///      list, every venue of which has a feed compiled in (`crate::recording::supported`).
///      Both were startup errors already; what changes is that they are answered before a listener
///      exists rather than after, and (4) is the "records nothing, looks healthy" class this whole
///      daemon's design objects to.
///
/// ⚠ **ALL FOUR SURVIVE THE ROW MIGRATION, AND THREE OF THEM ARE THE SAME CODE.**
/// [`load_and_check_profile_row`] answers (1) differently — a profile NAME that the store does not
/// hold, rather than a path that does not read — and then joins this function at [`check_profile`],
/// which is (2)'s tail, (3) and (4). (2) itself is not duplicated at all: a row-loaded profile is
/// RENDERED back into a TOML document (`vike_secrets::profile_store::render_recorder_toml`) and
/// parsed by the same `RecorderProfile::from_toml`, so serde's `deny_unknown_fields` on four
/// structs and all seven `validate` rules apply to it with no second implementation.
///
/// `server_root` is the root the data server RESOLVED (`vike_model::paths::store_path`'s ladder), not one
/// it opened — deliberately, since this runs before the open — and `cwd` is the composition root's
/// one `current_dir()` answer.
pub fn load_and_check_profile(
    path: &Path,
    server_root: &Path,
    cwd: Option<&Path>,
) -> Result<RecorderProfile, String> {
    let text =
        std::fs::read_to_string(path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    let profile =
        RecorderProfile::from_toml(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    check_profile(profile, &path.display().to_string(), server_root, cwd)
}

/// **The same four questions, asked of a PROFILE ROW.** The row half of
/// [`load_and_check_profile`], and it runs at the same place in the startup sequence — before the
/// bind guard, before the store open, before the listener.
///
/// ⚠ **Question 1 NARROWS here, and the narrowing is deliberate.**
/// `vike_secrets::profile_store::read_profiles` collapses *no database*, *no profile tables* and
/// *no rows* to `Profiles::none()`, which is right for a SELECTION — every box that has not
/// migrated must behave as it did. It is wrong here: `--recorder-profile NAME` names a profile
/// OUTRIGHT, so "you named a profile and it is not there" must be a REFUSAL rather
/// than an empty answer. A daemon that started anyway would bind its port, serve every query and
/// record nothing, which is the exact class this function exists to refuse.
///
/// The message leads with the STORE PATH for the same reason the file half leads with the file
/// path: it lands in a journal hours later, where `--recorder-profile default` is a line in a unit
/// nobody has open, and "which store answered" is the question an operator cannot reconstruct.
/// `vike-cli config recorder` prints the same `source:` line from the other side.
///
/// `settings_dir` is `$VIKE_SETTINGS_DIR` as THIS boot resolved it — a parameter, never a second
/// walk, because a daemon with two answers for where its project is has none.
pub fn load_and_check_profile_row(
    name: &str,
    settings_dir: Option<&str>,
    server_root: &Path,
    cwd: Option<&Path>,
) -> Result<RecorderProfile, String> {
    // `db_path_for` is the PURE resolver: the settings override this boot already resolved, plus
    // the composition root's one `current_dir()` answer. No second walk, and no `env::var` in a
    // library — `crates/vike-ops/tests/settings/settings_registry.rs`'s `LIBRARY_PIN` is a ratchet.
    let db = vike_secrets::db_path_for(settings_dir, cwd);
    let store = db.display().to_string();
    let profiles = vike_secrets::profile_store::read_profiles(&db)
        .map_err(|e| format!("{store}: reading recorder profile `{name}`: {e}"))?;
    let stored = profiles.by_name(name).ok_or_else(|| {
        format!(
            "{store}: no recorder profile named `{name}`. A `--recorder-profile NAME` is an \
             explicit argument, so an absent profile is a refusal rather than a daemon that binds \
             its port, answers every query and records nothing. `vike-cli config recorder` lists \
             what this store holds; `vike-cli config bootstrap-recorder {name} --store <root> \
             --venue <v> (--family <f>|--symbols <a,b,c>)` builds one FROM ARGUMENTS (0086 — never \
             from a file) and activates it"
        )
    })?;
    profile_from_row(stored, name, &store, server_root, cwd)
}

/// **The same four questions asked of the ACTIVE recorder row** — what `--recorder-profile` with no
/// value resolves to (ruling 5 of
/// `docs/superpowers/specs/2026-09-22-data-realtime-record-design.md`: *"the `active` row of kind
/// `Recorder` is the default, on both sides, so the CLI and the daemon resolve the same thing and
/// cannot disagree"*).
///
/// It is [`load_and_check_profile_row`] with the NAME resolved rather than given, through
/// `vike_secrets::profile_store::Profiles::resolve_active` — the one resolution, which is why this
/// function computes nothing itself and matches on its answer instead. The CLI's default reaches
/// the same function from the other side, so "the profile the CLI calls the default" and "the
/// profile this daemon mounts" cannot become two answers.
///
/// # ⚠ The three noes are three DIFFERENT refusals, and that is the whole reason this exists
///
/// A single "no active recorder profile" would send an operator to the wrong command in two of the
/// three cases. `ActiveProfile`'s own doc carries the distinction; what this function adds is the
/// NEXT COMMAND for each, and one of them is deliberately not a command at all — see below.
///
/// # ⚠ MEASURED: on every box today this refuses, and the refusal is correct
///
/// Nothing in this tree sets an active recorder row. `vike_secrets::profile_store::set_active` has
/// no production caller (its only mentions outside its own file are fixtures in
/// `crates/vike-ops/tests/settings/profile_writer_gate.rs`), and
/// `crates/vike-cli/src/cmd/config/mirror_recorder.rs` consults `plan_active_row` with
/// `in_force: None`, which answers `Withhold { NothingSelectedToday }` — it PRESERVES an active row
/// and never creates one. So a migrated box holding recorder profiles holds no selection, and the
/// valueless flag lands on `NoneActive`. That is why `deploy/vike-datahub.service` still spells a
/// NAME, and why the `NoneActive` message below names `--recorder-profile <name>` rather than a
/// selection verb: **there is no verb that sets the active row**, and naming one that does not exist
/// is the exact defect `docs/superpowers/specs/2026-09-22-data-realtime-record-design.md` §6 found
/// three times in this file's neighbourhood.
pub fn load_and_check_active_profile_row(
    settings_dir: Option<&str>,
    server_root: &Path,
    cwd: Option<&Path>,
) -> Result<RecorderProfile, String> {
    use vike_secrets::profile_store::{ActiveProfile, ProfileKind};

    let db = vike_secrets::db_path_for(settings_dir, cwd);
    let store = db.display().to_string();
    let profiles = vike_secrets::profile_store::read_profiles(&db)
        .map_err(|e| format!("{store}: reading the ACTIVE recorder profile: {e}"))?;
    let stored = match profiles.resolve_active(ProfileKind::Recorder) {
        ActiveProfile::Row(p) => p,
        ActiveProfile::NoProfileStore => return Err(no_settings_database(&store)),
        ActiveProfile::NoneStored => return Err(no_recorder_profile_stored(&store)),
        ActiveProfile::NoneActive { stored } => {
            return Err(no_recorder_profile_selected(&store, stored));
        }
    };
    // The name is the ROW's own, not one an operator typed — every message below therefore reports
    // what was RESOLVED, which is the only thing that makes a valueless flag legible in a journal.
    let name = stored.row.name.clone();
    profile_from_row(stored, &name, &store, server_root, cwd)
}

/// `ActiveProfile::NoProfileStore` — no settings database at all, or one written before the profile
/// tables existed. `read_profiles` collapses those two and nothing downstream can tell them apart,
/// so the message must cover both without claiming to know which.
///
/// A pure function rather than an inline `format!` so the message is a NAMED symbol a test can
/// reach without building the store state that produces it — the other two below exist for the same
/// reason, and between them they are why the three noes cannot silently converge on one string.
pub(super) fn no_settings_database(store: &str) -> String {
    format!(
        "{store}: this box has no settings database, so there is no ACTIVE recorder profile for \
         `--recorder-profile` to resolve. `vike-cli secrets migrate` is the ONE thing that may \
         create the database (`vike-cli config bootstrap-recorder`, like every profile writer, \
         refuses an ABSENT store rather than creating one); once it exists, \
         `vike-cli config bootstrap-recorder <name> --store <root> --venue <v> \
         (--family <f>|--symbols <a,b,c>)` builds a first recorder profile FROM ARGUMENTS — never \
         from a file (0086) — and stores + activates it in one act"
    )
}

/// `ActiveProfile::NoneStored` — the tables are there and hold no recorder profile at all. The next
/// command WRITES one, which is the thing that makes this a different message from the one above.
pub(super) fn no_recorder_profile_stored(store: &str) -> String {
    format!(
        "{store}: the settings database holds NO recorder profile, so `--recorder-profile` with no \
         value resolves nothing. `vike-cli config bootstrap-recorder <name> --store <root> --venue \
         <v> (--family <f>|--symbols <a,b,c>)` builds one FROM ARGUMENTS and activates it (run it \
         with --dry-run first); `vike-cli config recorder` lists what this store holds"
    )
}

/// `ActiveProfile::NoneActive` — profiles ARE stored and none is selected. The ONLY one of the three
/// an operator caused, and the only one whose fix does not involve writing a profile first.
///
/// ⚠ It names `--recorder-profile <name>` rather than a selection verb ON PURPOSE. Nothing in this
/// tree sets an active recorder row today (`vike_secrets::profile_store::set_active` has no
/// production caller), and naming a command that does not exist is the exact defect
/// `docs/superpowers/specs/2026-09-22-data-realtime-record-design.md` §6 found three times in this
/// file's neighbourhood — in the refusal an operator hits, which is the worst possible place for it.
pub(super) fn no_recorder_profile_selected(store: &str, stored: usize) -> String {
    format!(
        "{store}: this store holds {stored} recorder profile(s) and NONE of them is marked active, \
         so `--recorder-profile` with no value resolves nothing. Name the one this box records \
         with — `--recorder-profile <name>` — and run `vike-cli config recorder` to see the names; \
         re-running `vike-cli config bootstrap-recorder <name> …` with that profile's own \
         arguments both replaces its body in place and activates it, if you would rather not name \
         it on the daemon's own argv"
    )
}

/// The tail both row loaders share: render the stored body back into a TOML document, parse it with
/// the SAME `RecorderProfile::from_toml` the file half uses, and ask [`check_profile`]'s questions
/// 3 and 4 of the result.
///
/// It is a separate function so the ACTIVE row and a NAMED row cannot be judged by different rules
/// — the same reason [`check_profile`] is shared with the file half one rung further down.
fn profile_from_row(
    stored: &vike_secrets::profile_store::StoredProfile,
    name: &str,
    store: &str,
    server_root: &Path,
    cwd: Option<&Path>,
) -> Result<RecorderProfile, String> {
    let body = stored.recorder.as_ref().ok_or_else(|| {
        format!(
            "{store}: profile `{name}` exists but carries no recorder body (its kind is `{}`). \
             Naming a daemon or run profile here would record nothing",
            stored.row.kind.sql_word()
        )
    })?;
    let doc = vike_secrets::profile_store::render_recorder_toml(body);
    let profile = RecorderProfile::from_toml(&doc)
        .map_err(|e| format!("{store}: recorder profile `{name}`: {e}"))?;
    check_profile(profile, &format!("{store}: recorder profile `{name}`"), server_root, cwd)
}

/// Questions 3 and 4, shared by both loaders so a row-loaded recording cannot be judged by a
/// different rule from a file-loaded one. `origin` is what an error message names — a path for the
/// file half, `<store>: recorder profile \`<name>\`` for the row half.
fn check_profile(
    profile: RecorderProfile,
    origin: &str,
    server_root: &Path,
    cwd: Option<&Path>,
) -> Result<RecorderProfile, String> {
    one_store_root(&profile.store, server_root, cwd)?;
    if profile.subscribe.is_empty() {
        return Err(format!(
            "{origin}: profile has no [[subscribe]] entries — nothing to record. A daemon started \
             this way would bind its port, serve every query and accumulate nothing, which is \
             indistinguishable from a venue that is merely quiet"
        ));
    }
    let supported = crate::recording::supported();
    for sub in &profile.subscribe {
        if !supported.contains(&sub.venue.as_str()) {
            return Err(format!(
                "{origin}: venue `{}` has no feed in this build. Supported here: [{}]. This is the \
                 SAME refusal `crate::recording::build_recording_feed` gives, asked before the \
                 listener binds rather than after — rebuild with that venue's Cargo feature (the \
                 shipped multicall carries `record-polymarket` and `record-binance`)",
                sub.venue,
                supported.join(", ")
            ));
        }
    }
    Ok(profile)
}
