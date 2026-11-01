//! The ONE reader of a stored RUN profile: store rows back into a [`RunProfile`].
//!
//! It lived in `vike-tradehub`'s `profile_rows` beside its daemon-profile twin
//! (`rows_to_daemon_profile`), which stays there because `DaemonProfile` is the daemon's. It moved
//! down with the schema (decision 0157) because everything it needs is here: the rows are
//! `vike-secrets`', the document is [`RunProfile`]'s, and `vike-cli` reads the same rows without
//! linking the daemon.

use vike_secrets::profile_store::{StoredProfile, render_run_toml};

use super::RunProfile;

/// **Store rows back into a `RunProfile`, THROUGH THE EXISTING PARSER** — the exact twin of
/// `rows_to_daemon_profile`, and the READER 0057's Phase 2 never had.
///
/// [`vike_secrets::profile_store::render_run_toml`] assembles the document and
/// `RunProfile::from_toml_str` parses AND validates it, so a row-loaded run profile meets every
/// refusal the file meets: `deny_unknown_fields` on all seven structs (which is what catches a
/// `profile_setting` path naming nothing real — see that renderer's doc for why it carries no
/// vocabulary of its own), the `[event_source]`/`[broker]` tombstones, `max_leverage >= 1.0`,
/// `required_free_bp_pct` in `[0.0, 1.0)`. **No second validator is written**, and none may be.
///
/// ⚠ **A row the loader refuses BY PARSE — a key the type does not have, such as the `mode` a run
/// profile carried until decision 0156 deleted it — gets its fix appended**: [`refused_row_fix`]
/// names the ONE stored row without which the body loads, and the exact `vike-cli config
/// bootstrap-run` command that re-writes the body without it (that verb REPLACES the body stored
/// under a name, and it is the run profile's only writer). It asks the same loader once per row, so
/// it adds no vocabulary; with no single such row (two bad rows, or a refusal of a value) the
/// loader's own words stand alone.
///
/// ⚠ The caller must treat an `Err` as a HARD startup failure, never as "no run profile". Falling
/// through to the file rung would make a corrupt row indistinguishable from an absent one, and an
/// absent one is what a live mount REFUSES on — so the fall-through would convert a refusal into a
/// mount running on the file's ceilings while the operator believes the row is in force.
///
/// # Errors
///
/// Whatever the renderer or the loader refuses, verbatim.
pub fn rows_to_run_profile(stored: &StoredProfile) -> Result<RunProfile, String> {
    RunProfile::from_toml_str(&render_run_toml(stored)).map_err(|e| match e {
        vike_model::ProfileError::Parse(_) => match refused_row_fix(stored) {
            Some(fix) => format!("{}{fix}", e.to_string().trim_end()),
            None => e.to_string(),
        },
        vike_model::ProfileError::Validation(_) => e.to_string(),
    })
}

/// **The fix for a run body the loader refuses at PARSE**: the ONE stored `profile_setting` row
/// without which the body loads, named, and the `vike-cli config bootstrap-run` command that
/// re-writes the body with every OTHER row — which is how a row is deleted, since that verb replaces
/// the body stored under a name. `None` when no single row's absence makes the body load.
///
/// The row is found by asking [`RunProfile::from_toml_str`] — the one loader — once per row with
/// that row left out, never by a vocabulary of its own. The command carries each other row as
/// `--<path> <value>`, the value unquoted from its stored TOML scalar (and shell-quoted when it is
/// not a plain word), which is exactly what `bootstrap-run` lowers back to the same stored scalar.
/// A `name` row is left out of the command: that verb takes no `--name` (the ROW's name is the
/// profile's name), so carrying it would earn the operator an "unknown option" instead of a fix.
fn refused_row_fix(stored: &StoredProfile) -> Option<String> {
    let culprits: Vec<&String> = stored
        .settings
        .keys()
        .filter(|path| {
            let mut without = stored.clone();
            without.settings.remove(path.as_str());
            RunProfile::from_toml_str(&render_run_toml(&without)).is_ok()
        })
        .collect();
    let [row] = culprits.as_slice() else { return None };
    let name = &stored.row.name;
    let flags: String = stored
        .settings
        .iter()
        .filter(|(path, _)| path != row && path.as_str() != "name")
        .map(|(path, value)| format!(" --{path} {}", flag_value(value)))
        .collect();
    Some(format!(
        "\n\nThe stored row `{row}` of run profile `{name}` is what the loader refuses — without it \
         the body loads. Delete it by re-writing the body without that row (`bootstrap-run` \
         REPLACES the body stored under a name, so every other row is carried over below):\n\n    \
         vike-cli config bootstrap-run {name}{flags}\n\nthen restart the daemon."
    ))
}

/// A stored `profile_setting` value — the TOML rendering of one scalar — as the command-line
/// argument `vike-cli config bootstrap-run` lowers back to that same scalar: a TOML string loses its
/// quotes, anything else is verbatim, and a value that is not a plain word is single-quoted for the
/// shell.
fn flag_value(stored: &str) -> String {
    let raw = toml::from_str::<toml::Table>(&format!("v = {stored}"))
        .ok()
        .and_then(|t| t.get("v").and_then(|v| v.as_str().map(str::to_string)))
        .unwrap_or_else(|| stored.to_string());
    let plain =
        !raw.is_empty() && raw.chars().all(|c| c.is_ascii_alphanumeric() || "_-./:@+".contains(c));
    if plain { raw } else { format!("'{}'", raw.replace('\'', "'\\''")) }
}

#[path = "rows_tests.rs"]
#[cfg(test)]
mod rows_tests;
