//! **The four shipped `settings/*.example.toml` templates, held to the real structs.**
//!
//! A fresh clone has no `settings/` directory and, until these templates, nothing to copy into one:
//! `vike-cli secrets path` prints an excellent `mkdir`/`$EDITOR`/`chmod 600` recipe for the
//! credential store, and the four TOMLs beside it had no equivalent at all. An operator's only route
//! to a `policy.toml` was reading `crates/vike-config/src/policy.rs`.
//!
//! A template is worth shipping only if it cannot drift, and a stale template is strictly worse than
//! none: it teaches a key that no longer exists, or silently omits one that now matters, with the
//! authority of a committed file. `recorder.example.toml` — the one example that predates these — is
//! gated by nothing and is free to rot today. So these are gated in BOTH directions, against
//! `vike_config::provenance::setting_keys()` — the same list `vike-cli config show` prints and the
//! same one the loader resolves:
//!
//! - [`every_setting_appears_in_its_template`] — a key with no line fails. This is the direction
//!   that catches a NEW setting: adding a field to `Config`/`Flags`/… turns this red until the
//!   template documents it.
//! - [`a_template_names_no_setting_that_does_not_exist`] — a line naming nothing fails. This is the
//!   direction that catches a REMOVED or RENAMED setting, including the two policy tombstones
//!   (`max_total_exposure`, `rate.max_utilization`) that the loader parses only in order to refuse
//!   them by name.
//! - [`every_template_line_is_one_the_loader_accepts`] — the strongest of the three. Every commented
//!   line is UNCOMMENTED, written into a throwaway settings directory under its real name, and put
//!   through the REAL `vike_config::load`. A wrong key, a wrong TYPE, or a value outside a validated
//!   range (`max_leverage >= 1.0`, `market_slippage` in 0.001..=0.05, `sweep_threads != 0`, an
//!   address with no `':'`) fails here. So the templates are not merely spelled right — every line
//!   in them is a line that WORKS if you uncomment it, which is the only promise a template makes.
//! - [`the_templates_as_shipped_configure_nothing`] — the promise the file headers make, and the
//!   only safety property that matters for a file somebody will copy without reading. AS SHIPPED
//!   every line is commented, so adopting all four verbatim must produce a `Settings` byte-equal to
//!   the compiled-in defaults: no flag on, no ceiling set, no path redirected. These templates list
//!   `allow_withdraw_keys` and `preflight_skip`, so one line that lost its `#` in an edit is exactly
//!   the mistake worth a test.
//!
//! # Why this file lives in `vike-cli`
//!
//! It gates data files at the repo ROOT against `vike-config`'s key list, so it belongs to neither
//! crate outright. `vike-cli` is where it earns its keep: this is the crate that owns `config show`,
//! the command the templates point at and the one an operator uses to check that a copied file did
//! what they expected. It also already links `vike-config`, so the gate costs no new edge, and
//! `vike-cli` rides the fast CI lane.
//!
//! # The line grammar these tests assume
//!
//! A SETTING line starts at column 0, is either `key = value` or `# key = value` (exactly one space
//! after the `#`), and its key is a bare identifier. Prose is any other `#` line. Keep prose from
//! starting at column 0 with `identifier =` and the extractor cannot be confused; nothing else about
//! the templates' formatting is load-bearing.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// The four templates, as `(the real file name, the template name)`.
///
/// Derived from `setting_keys()`'s own `file` field rather than typed out, so a FIFTH settings file
/// cannot be added to the loader while this test keeps checking four.
fn template_files() -> Vec<(&'static str, String)> {
    let mut files: Vec<&'static str> =
        vike_config::provenance::setting_keys().into_iter().map(|k| k.file).collect();
    files.sort_unstable();
    files.dedup();
    files.into_iter().map(|real| (real, real.replace(".toml", ".example.toml"))).collect()
}

/// The repo root — `crates/vike-cli/` is two levels down from it.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crates/vike-cli sits two levels below the repo root")
        .to_path_buf()
}

fn template_path(name: &str) -> PathBuf {
    repo_root().join("settings").join(name)
}

fn read_template(name: &str) -> String {
    let path = template_path(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "the shipped template {} must exist and be readable ({e}) — it is committed through \
             `.gitignore`'s `!/settings/*.example.toml` rule",
            path.display()
        )
    })
}

/// Every key a template NAMES, live or commented. See the module doc for the grammar.
fn keys_in(body: &str) -> BTreeSet<String> {
    body.lines().filter_map(setting_line).map(|(key, _)| key).collect()
}

/// `(key, the line with any leading `# ` removed)` for a setting line; `None` for prose or blanks.
fn setting_line(line: &str) -> Option<(String, String)> {
    let live = line.strip_prefix("# ").unwrap_or(line);
    // Column 0 only: an indented line is continuation prose, never a setting.
    if line.starts_with(' ') || line.starts_with('\t') {
        return None;
    }
    let (lhs, _) = live.split_once('=')?;
    let key = lhs.trim_end();
    let is_ident = !key.is_empty()
        && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
        && key.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_');
    is_ident.then(|| (key.to_string(), live.to_string()))
}

/// The template with every setting line uncommented — what an operator gets by copying it and
/// stripping the `#`s, which is exactly what a template invites.
fn uncommented(body: &str) -> String {
    body.lines()
        .map(|line| match setting_line(line) {
            Some((_, live)) => live,
            None => line.to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The keys `vike-config` says belong in `file`, as they are spelled INSIDE it (no section prefix).
fn expected_keys(file: &str) -> BTreeSet<String> {
    vike_config::provenance::setting_keys()
        .into_iter()
        .filter(|k| k.file == file)
        .map(|k| k.path.join("."))
        .collect()
}

/// A throwaway settings directory holding `contents`, written under the REAL file names.
fn settings_dir_with(files: &[(&str, String)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    for (name, body) in files {
        std::fs::write(dir.path().join(name), body).expect("write the settings file");
    }
    dir
}

/// Direction 1 — a setting with no template line fails. This is what turns red when a field is
/// ADDED: the template is where the new knob gets explained, or it ships undocumented.
#[test]
fn every_setting_appears_in_its_template() {
    for (real, template) in template_files() {
        let present = keys_in(&read_template(&template));
        let expected = expected_keys(real);
        let missing: Vec<&String> = expected.difference(&present).collect();
        assert!(
            missing.is_empty(),
            "settings/{template} does not mention {missing:?}. Every key `{real}` accepts must have \
             a line there (commented, at its default) — a template that silently omits a setting is \
             how an operator concludes the setting does not exist."
        );
    }
}

/// Direction 2 — a template line naming nothing fails. This is what turns red when a field is
/// REMOVED or RENAMED, which is the direction a hand-written template always rots in.
#[test]
fn a_template_names_no_setting_that_does_not_exist() {
    for (real, template) in template_files() {
        let present = keys_in(&read_template(&template));
        let expected = expected_keys(real);
        let unknown: Vec<&String> = present.difference(&expected).collect();
        assert!(
            unknown.is_empty(),
            "settings/{template} names {unknown:?}, which `{real}` does not accept. Delete the \
             line — a template documenting a key the loader rejects (or a TOMBSTONE it parses only \
             to refuse by name) sends an operator to configure something that cannot work."
        );
    }
}

/// Direction 3 — every line WORKS. Uncomment the lot, hand it to the real loader, and it must load:
/// right key, right type, and a value inside whatever range that key validates.
#[test]
fn every_template_line_is_one_the_loader_accepts() {
    let files: Vec<(&str, String)> = template_files()
        .into_iter()
        .map(|(real, template)| (real, uncommented(&read_template(&template))))
        .collect();
    let dir = settings_dir_with(&files);

    // No env layer: the templates are being judged as FILES. An ambient `VIKE_*` value would
    // otherwise mask a broken line by overriding it.
    let settings = vike_config::load(Some(dir.path()), &std::collections::HashMap::new());
    let settings = settings.unwrap_or_else(|e| {
        panic!(
            "an uncommented copy of the shipped templates must LOAD, and this one did not: {e}\n\
             Every line in a template is a line somebody will uncomment; if the loader rejects one, \
             the template is teaching a value that does not work."
        )
    });
    assert!(
        settings.warnings.is_empty(),
        "an uncommented copy of the templates must load without warnings, got: {:?}",
        settings.warnings
    );
}

/// **The templates AS SHIPPED configure nothing.** Adopt all four verbatim and the resolved
/// `Settings` must be byte-equal to the compiled-in defaults.
///
/// This is the whole safety contract of a file people copy before they read it, and it is what lets
/// the templates list EVERY key without any line reading as advice. The failure it guards is
/// mundane and entirely plausible: one line loses its `#` in an edit. These files list
/// `allow_withdraw_keys` and `preflight_skip` — two documented SAFETY OVERRIDES — plus
/// `tradehub_live`, `poly_exec` and a notional ceiling, so exactly which line lost its `#` decides
/// whether the accident is cosmetic or moves real money.
///
/// ⚠ Note what this does NOT claim: that uncommenting everything is a no-op. It is not, and it
/// cannot be — most keys here are `Option` whose default is "unset", and no TOML value means unset.
/// Uncommenting a line SETS that key; the shipped values are its default where it has one and an
/// illustrative value where its default is absence. [`every_template_line_is_one_the_loader_accepts`]
/// is what holds those illustrative values to being real, working ones.
#[test]
fn the_templates_as_shipped_configure_nothing() {
    let files: Vec<(&str, String)> = template_files()
        .into_iter()
        .map(|(real, template)| (real, read_template(&template)))
        .collect();
    let dir = settings_dir_with(&files);
    let env = std::collections::HashMap::new();

    let shipped = vike_config::load(Some(dir.path()), &env)
        .expect("the templates as shipped must load — every line in them is commented");
    let defaults = vike_config::load(None, &env).expect("defaults always load");

    assert_eq!(
        shipped.flags, defaults.flags,
        "settings/flags.example.toml turns a flag ON as shipped. Every line in it must be \
         commented: whoever copies this file gets whatever it sets, and it lists \
         `allow_withdraw_keys`, `preflight_skip`, `tradehub_live` and `poly_exec`."
    );
    assert_eq!(
        shipped.config, defaults.config,
        "settings/config.example.toml sets a key as shipped — a copied file that redirects the \
         store root or opens a node socket is not a template"
    );
    assert_eq!(
        shipped.preferences, defaults.preferences,
        "settings/preferences.example.toml sets a key as shipped"
    );
    assert_eq!(
        shipped.policy, defaults.policy,
        "settings/policy.example.toml sets a RISK CEILING as shipped. A copied file that caps \
         orders at somebody else's number is as wrong as one that uncaps them."
    );
    assert!(shipped.warnings.is_empty(), "shipped templates warn: {:?}", shipped.warnings);
}
