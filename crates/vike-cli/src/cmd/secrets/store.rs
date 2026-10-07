//! Which store a `secrets` verb reads: the `--file` / settings-directory resolution, and a refusal.

use std::path::{Path, PathBuf};

use vike_secrets::resolve;

use super::{Args, settings_dir_of};

/// The store this invocation inspects: `--file` if given, else the store inside the settings
/// directory the DISPATCHER resolved, else the walk from the working directory — under the SAME
/// `$VIKE_SETTINGS_DIR` value that dispatcher's boot was handed.
///
/// PURE — no I/O, so the whole override grammar is unit-tested. BOTH `settings_dir` and
/// `settings_dir_override` come from `crate::run`'s single environment sweep, not from a read of
/// this library file's own.
///
/// ⚠ **The last arm takes the override, and the override-BLIND
/// `vike_secrets::workspace_dotenv_path` it used to call is wrong there.** The tempting argument is
/// that this arm runs only when the dispatcher's boot resolved NO settings directory, and that a
/// boot resolving nothing means there was no override to honour — so the blind spelling is the same
/// pure resolver under the same `None` and cannot answer differently. **That was false**, because
/// `vike_boot::boot` resolved the directory as `spec.cwd.and_then(..)`: with no readable working
/// directory it yielded `None` *while still returning the override it was given*. `std::env::
/// current_dir()` fails whenever the directory a process started in has been removed, unmounted or
/// made unsearchable, so the reachable input was `$VIKE_SETTINGS_DIR=/srv/x/settings` plus a
/// vanished working directory — and there the two spellings diverge:
/// `vike_secrets::workspace_dotenv_path` falls through to the RELATIVE last resort
/// `settings/secrets.env` while every daemon on that box reads `/srv/x/settings/secrets.env`
/// (`vike_bridge_core::credentials::load_workspace_secrets_from_env` ->
/// `vike_secrets::resolve_project` -> `vike_secrets::workspace_dotenv_path_from`, all of which
/// carry the override).
///
/// Printing a location the rest of the program does not use is the one failure this command cannot
/// have: `path` exists to answer *which file are my keys actually coming from*, and it is the
/// command an operator runs when something is already wrong. `vike_secrets::dotenv_path_for` is
/// where that divergence is pinned.
///
/// # ⚠ The UPSTREAM cause is fixed, so this arm is now unreachable — and it stays
///
/// `vike_boot::boot` calls `vike_secrets::project_settings_dir_for`, which honours a name with no
/// walk, so a boot that returns `settings_dir: None` now necessarily returns
/// `settings_dir_override: None` as well. **This function's `settings_dir_override` parameter can
/// therefore only ever arrive as `None` from `crate::run`** — which makes the last arm
/// byte-identical to the blind spelling it replaced, on every input this dispatcher can produce.
///
/// It is KEPT, and that is a decision rather than an oversight. Three reasons, in order:
///
/// 1. **It is the belt.** The upstream fix is one expression in another crate. If it regresses to
///    an `and_then` on the working directory, this arm is what keeps `secrets path` naming the file
///    the daemons read, instead of quietly printing a relative last resort again.
/// 2. **The function is still CORRECT for the pairing**, and it is unit-tested for it directly
///    (`the_store_honours_the_override_when_no_settings_dir_was_resolved` calls it with synthetic
///    values, so it does not depend on the boot to reach that input at all).
/// 3. Deleting it would cost a parameter and buy nothing: the argument is a `Option<&str>` the
///    dispatcher already holds for `config check`'s origin verdict.
///
/// `a_boot_with_no_working_directory_resolves_the_named_directory` below is where the reachability
/// is measured — it now asserts the pairing is GONE, which is the assertion that would go red the
/// day the boot starts dropping names again.
pub(super) fn store_path(
    args: &Args,
    settings_dir: Option<&Path>,
    settings_dir_override: Option<&str>,
) -> PathBuf {
    if let Some(p) = &args.file {
        return p.clone();
    }
    vike_secrets::secrets_path_in(&settings_dir_of(settings_dir, settings_dir_override))
}

/// **The settings directory whose database would SHADOW the store this invocation is reporting on**
/// — the `--file`'s own parent when one was given, else [`settings_dir_of`]'s answer.
///
/// # ⚠ Why this exists beside [`settings_dir_of`] rather than inside it
///
/// That function deliberately does NOT honour `--file`, and its doc argues why: a directory derived
/// from an operator-named path would let a `db/vike.db` sitting beside some unrelated `.env` ANSWER
/// for it — become the source of the credentials printed — which is a store nobody asked for. That
/// argument is about SOURCING and it is untouched: nothing below ever reads a row out of the
/// directory this returns.
///
/// What this asks is a different question with the opposite disposition: *is the file you named
/// still read?* `--file` names a text file, this command reads it as text, and on a MIGRATED box
/// that listing is a listing of something no process on the machine loads. The finding is
/// `vike_secrets::ShadowedStore`, the same type and the same sentence the project's own store gets
/// — and withholding it because the path came from a flag would make `--file` the one way to be
/// told a stale roster with no qualifier on it. A finding is never a source.
///
/// It is the SAME probe either way: one `vike_secrets::backend_in` on one directory, which is
/// 0054's per-RUN choice and not a per-key fallback. For an arbitrary path with no `db/vike.db`
/// beside it — the ordinary `--file /tmp/x.env` — it answers `Backend::Files` and every caller's
/// output is byte-identical to before this existed.
pub(super) fn shadowing_dir_of(
    args: &Args,
    settings_dir: Option<&Path>,
    settings_dir_override: Option<&str>,
) -> PathBuf {
    match &args.file {
        Some(p) => vike_secrets::settings_dir_of_store(p),
        None => settings_dir_of(settings_dir, settings_dir_override),
    }
}

/// **Refuse a `--file` that names a settings DATABASE, before anything tries to read it as text.**
///
/// `--file` is a credential-FILE flag: every path it reaches goes to `vike_secrets::resolve`, which
/// is the `KEY=VALUE` arm by definition. Handed `<settings>/db/vike.db` it does not fail usefully,
/// and often does not fail at all — a small database's pages are largely NUL bytes, which ARE valid
/// UTF-8, so `read_to_string` succeeds, the parser finds no assignment in the binary, and `list`
/// prints `0 secret(s)`: *the store is empty*, about the one artifact on the box holding every venue
/// key. `vike_secrets::db`'s `a_database_read_as_text_is_silent_rather_than_loud` pins that.
///
/// # Refused rather than taught to read it, and the reason is not the parser
///
/// Reading the `credential` table here is a dozen lines (`vike_secrets::read_table` takes a path),
/// so the argument has to be about what the OUTPUT would then mean. Everything this command prints
/// around the key names is derived from a settings DIRECTORY and not from the store file: which
/// database answers, whether a file is shadowed, where the node pair lives, `path`'s `nodes:` line.
/// A `--file` pointing at one artifact supplies none of it, so a listing sourced that way would be
/// right about venue keys and quietly wrong about every line beside them — and `path --file <db>`
/// would print a database under `store:` with no `answers:` line, which is the exact confusion
/// `docs/decisions/0054`'s work on this verb removed.
///
/// The spelling that works already exists and is the one the rest of the program agrees with:
/// `VIKE_SETTINGS_DIR=<project>/settings vike-cli secrets list` reaches
/// `vike_secrets::backend_in`, so the CLI answers exactly what a daemon booted on that box would.
/// The refusal names it.
///
/// Judged by the SQLite format's own 16-byte header (`vike_secrets::is_sqlite_file`), never by an
/// extension: an operator's migrated store may be named anything, and a credential file can never
/// begin with those bytes. Reading 16 bytes opens no row and no value.
pub(super) fn refuse_a_database_path(file: Option<&Path>) -> Result<(), String> {
    let Some(p) = file else { return Ok(()) };
    if !vike_secrets::is_sqlite_file(p) {
        return Ok(());
    }
    // `<settings>/db/vike.db` -> `<settings>`, so the suggestion is a directory the operator can
    // paste. A path with no grandparent gets the SHAPE instead of an invented directory: naming the
    // wrong one would be worse than naming none on the command that exists to end that class.
    let settings = p
        .parent()
        .and_then(Path::parent)
        .map_or_else(|| "<project>/settings".to_string(), |d| d.display().to_string());
    Err(format!(
        "{} holds a settings DATABASE, not a KEY=VALUE credential file, and --file reads a file. \
         Read a database by naming its PROJECT instead — the whole settings directory, so the key \
         names, the node pair and the shadowed file all come from one place:\n  \
         VIKE_SETTINGS_DIR={settings} vike-cli secrets list\n\
         (`vike-cli secrets path` prints which store answers for a project.)",
        p.display(),
    ))
}

/// Open the store this verb should report on: the explicit `--file`, or whichever store answers for
/// the project.
///
/// ⚠ The two arms are deliberately different FUNCTIONS rather than one with a flag. `--file` names a
/// text file and must stay a text-file read — `vike_secrets::resolve` is the FILE arm by definition
/// — while the project's store is whatever `vike_secrets::backend_in` says it is.
pub(super) fn resolve_store(
    args: &Args,
    settings_dir: Option<&Path>,
    settings_dir_override: Option<&str>,
) -> Result<vike_secrets::Resolved, vike_secrets::SecretsError> {
    match &args.file {
        Some(p) => resolve(p),
        None => vike_secrets::resolve_store_in(
            &settings_dir_of(settings_dir, settings_dir_override),
            vike_secrets::Table::Credential,
        ),
    }
}
