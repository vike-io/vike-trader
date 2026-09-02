//! Backend registry — which remote tradehub daemons the GUI knows how to reach (split-plane
//! B7/B8/B9): the record model, the per-backend control-arming gate, and `backends.json`
//! persistence beside the workspace file.
//!
//! One [`BackendRecord`] per backend: a display name, the daemon's dial address, and the
//! CREDENTIAL-STORE **KEY NAMES** ([`BackendRecord::observe_key`], optional
//! [`BackendRecord::control_key`]) of the auth keys — **never key material**. Resolution to
//! bytes happens at CONNECT time, from a caller-supplied credentials map, via [`resolve_keys`].
//! This module obeys the settings-registry rule (libraries take configuration as parameters):
//! it reads NO environment variable itself — directory resolution delegates to
//! `crates/vike-app-core/src/workspace/persist.rs` — its `base_dir`, the workspace family's one
//! resolver, whose env reads already carry their `vike_ops::settings::SETTINGS` rows — and the
//! only file it touches is `backends.json` itself. Key NAMES, not values, is the contract
//! rather than a preference, twice over (spec B7):
//!
//!   - **Key material must never land in a GUI-owned JSON file.** The credential store
//!     (`<project>/settings/secrets.env`) is the ONE home for secrets; this file is written by
//!     the GUI and read back as plain data, so a value stored here would be a second, ungated
//!     copy of a live key that no redaction discipline covers.
//!   - **A NAME is grep-able where a resolved value is not.** The settings-registry scanner
//!     cannot resolve a map key the program assembles at runtime (a computed name is invisible
//!     to it by construction), so the auditable surface is the literal name sitting in the
//!     operator's own file: `vike-cli secrets list` prints the store's key names, and a
//!     `backends.json` naming `PROD2_OBSERVE_KEY` can be checked against that list by eye or by
//!     grep. A registry of resolved bytes would be auditable by nothing.
//!
//! **The arming gate (B9):** [`BackendRecord::control`] is the per-backend write-channel gate.
//! A record with `control = false` OR no `control_key` named can never arm a control
//! (order-writing) channel — [`resolve_keys`] answers `None` for the control half even when the
//! credentials map holds the named key — so disarming a backend in the file disarms it
//! everywhere downstream, and adding the key to the store arms nothing by itself.
//!
//! **Why this is deliberately NOT a vike-config settings file (B8):** the
//! `<project>/settings/*.toml` files are OPERATOR-owned — validated on load, unknown keys
//! refused by name, every key gated by `vike_config::CONSUMPTION`. This file is GUI-owned: the
//! app WRITES it (add / remove / select a backend from the UI), exactly like `workspace.json`,
//! so it takes the same route — the workspace-persistence idiom (one `base_dir`, forward-compat
//! serde, sanitized names, never-brick loads) and NOT the settings pipeline. It also must not
//! reuse `config.tradehub_addr`: that key is the DAEMON's bind address, not a client-side dial
//! list.
//!
//! **Forward-compat:** every field is `#[serde(default)]` — the `WinSnap::asset_class` idiom
//! from `crates/vike-app-core/src/workspace/persist.rs` — and unknown fields are ignored
//! (serde's default), so a v1 file loads under a newer build AND a newer build's file loads
//! here, rather than one bricking the other's startup.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// One remote backend daemon the GUI can dial.
///
/// `observe_key` / `control_key` hold credential-store KEY NAMES (e.g. `"PROD2_OBSERVE_KEY"`),
/// never key material — the module doc carries the argument. Every field is `#[serde(default)]`
/// so a file written before (or after) any given field existed still loads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackendRecord {
    /// Display name — user-entered, sanitized on save by [`sanitize_backend_name`] so it can
    /// never smuggle path separators into anything derived from it.
    #[serde(default)]
    pub name: String,
    /// The daemon's dial address (`host:port`). NOT `config.tradehub_addr` — that settings key
    /// is the daemon's own BIND address; this is the client-side inverse.
    #[serde(default)]
    pub addr: String,
    /// Credential-store KEY NAME of the observe (read-plane) auth key.
    #[serde(default)]
    pub observe_key: String,
    /// Credential-store KEY NAME of the control (write-plane) auth key, when one is configured
    /// at all. `None` ⇒ the write channel can never arm, whatever [`BackendRecord::control`]
    /// says.
    #[serde(default)]
    pub control_key: Option<String>,
    /// The per-backend arming gate (B9): `false` ⇒ [`resolve_keys`] never yields a control key
    /// for this record, even when `control_key` is named and present in the map.
    #[serde(default)]
    pub control: bool,
}

/// The whole `backends.json`: the records plus which one is active.
///
/// Struct-level [`Default`] + per-field `#[serde(default)]` make an EMPTY or PARTIAL file load
/// as "no backends yet" rather than an error.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackendsFile {
    /// The known backends, in display order.
    #[serde(default)]
    pub backends: Vec<BackendRecord>,
    /// [`BackendRecord::name`] of the currently selected backend, if any. Sanitized on save
    /// with the same function as the record names, so the pointer follows them.
    #[serde(default)]
    pub active: Option<String>,
}

/// The registry file's basename, stated once.
const BACKENDS_FILE: &str = "backends.json";

/// Sanitize a user-entered backend name — the workspace layouts' sanitizer
/// (`sanitize_layout_name` — one authority, not a second copy), re-exported under the name this
/// module's callers reach for. Path separators, dots and control characters become `_`, so a
/// hostile name cannot escape the registry's directory through anything a caller later derives
/// from it. Returns `""` for a name that sanitizes to nothing.
pub fn sanitize_backend_name(name: &str) -> String {
    crate::workspace::persist::sanitize_layout_name(name)
}

/// Registry file path for READING: `<base_dir>/backends.json` — beside the workspace file, so
/// `$VIKE_WORKSPACE` (whose directory is the family's base) moves this file together with the
/// layouts family. `None` when no base resolves (no override, no project above the working
/// directory): there is no file to read then, and [`load`] reports that as "no backends yet".
pub fn path() -> Option<PathBuf> {
    Some(crate::workspace::persist::base_dir()?.join(BACKENDS_FILE))
}

/// Registry file path for WRITING — same location as [`path`], with the directory created
/// lazily and a symlinked target refused (`vike_model::state_path::write_path`, the same
/// resolver the workspace file's save uses).
fn save_path() -> std::io::Result<PathBuf> {
    vike_model::state_path::write_path(
        crate::workspace::persist::base_dir().as_deref(),
        BACKENDS_FILE,
    )
}

/// Read the registry; an ABSENT file (or no resolvable location) is the ordinary "no backends
/// yet" state and answers the empty default. A file that exists but does not parse also answers
/// the default — a corrupt registry must never brick startup (the workspace `load` contract) —
/// after a `tracing::warn!` naming the parse error.
pub fn load() -> BackendsFile {
    path().map(|p| load_or_default_from(&p)).unwrap_or_default()
}

/// [`load`]'s pure half: the same absent → default / corrupt → warn + default contract, over an
/// explicit path.
fn load_or_default_from(p: &Path) -> BackendsFile {
    load_from(p).unwrap_or_default()
}

/// Read + parse an explicit registry path; `None` if absent or unparseable. Crate-visible for
/// the same round-trip test as [`save_to`].
pub(crate) fn load_from(p: &Path) -> Option<BackendsFile> {
    let raw = std::fs::read_to_string(p).ok()?;
    match serde_json::from_str::<BackendsFile>(&raw) {
        Ok(f) => Some(f),
        Err(e) => {
            tracing::warn!("backends file unparseable, starting with an empty registry: {e}");
            None
        }
    }
}

/// Serialize + write the registry; returns the path written. Record names and the `active`
/// pointer are passed through [`sanitize_backend_name`] on the way out.
pub fn save(file: &BackendsFile) -> std::io::Result<PathBuf> {
    let p = save_path()?;
    save_to(file, &p)?;
    Ok(p)
}

/// [`save`]'s pure half (crate-visible so `backend_editor`'s round-trip test drives the REAL
/// saver over a temp path, no env juggling): sanitize names, then an atomic-ish write to an
/// explicit path — the
/// JSON lands in a sibling `.tmp` first and is renamed over the target, so a crash mid-write
/// leaves the old file intact rather than a truncated one. Windows can refuse the rename when
/// the destination exists and is held open (POSIX would not — the root doctrine on `rename`);
/// the fallback is a direct write, which trades the atomicity back for succeeding at all.
pub(crate) fn save_to(file: &BackendsFile, p: &Path) -> std::io::Result<()> {
    let clean = sanitized(file);
    let json = serde_json::to_string_pretty(&clean).map_err(std::io::Error::other)?;
    let tmp = p.with_extension("json.tmp");
    std::fs::write(&tmp, &json)?;
    match std::fs::rename(&tmp, p) {
        Ok(()) => Ok(()),
        Err(_) => {
            let res = std::fs::write(p, &json);
            let _ = std::fs::remove_file(&tmp);
            res
        }
    }
}

/// The copy [`save_to`] actually writes: every record name and the `active` pointer sanitized,
/// so what lands on disk can never carry a path-hostile name (and the pointer still matches the
/// record it named, because both go through the same function).
fn sanitized(file: &BackendsFile) -> BackendsFile {
    BackendsFile {
        backends: file
            .backends
            .iter()
            .map(|r| BackendRecord { name: sanitize_backend_name(&r.name), ..r.clone() })
            .collect(),
        active: file.active.as_deref().map(sanitize_backend_name),
    }
}

/// Resolve a record's key NAMES to key VALUES against a caller-supplied credentials map — the
/// connect-time half of the names-not-values contract (module doc). Pure: no env, no store.
///
/// Returns `(observe, control)`. The observe half is a plain lookup. The control half is the
/// B9 arming gate: `None` unless the record is armed (`control = true`) AND names a
/// `control_key` AND the map holds it — so an unarmed record never yields a control key, even
/// when the credentials exist.
pub fn resolve_keys<'a>(
    rec: &BackendRecord,
    vars: &'a HashMap<String, String>,
) -> (Option<&'a str>, Option<&'a str>) {
    let observe = vars.get(&rec.observe_key).map(String::as_str);
    let control = if rec.control {
        rec.control_key.as_ref().and_then(|k| vars.get(k)).map(String::as_str)
    } else {
        None
    };
    (observe, control)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(name: &str) -> BackendRecord {
        BackendRecord {
            name: name.to_string(),
            addr: "the CI box.example:9040".to_string(),
            observe_key: "PROD2_OBSERVE_KEY".to_string(),
            control_key: Some("PROD2_CONTROL_KEY".to_string()),
            control: false,
        }
    }

    fn vars(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    /// A populated registry survives a disk round-trip byte-for-value: records, order, the
    /// active pointer, and every field including the arming gate.
    #[test]
    fn round_trips_through_disk() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let p = tmp.path().join(BACKENDS_FILE);
        let mut armed = rec("the build runner");
        armed.control = true;
        let file = BackendsFile {
            backends: vec![rec("the build runner"), armed],
            active: Some("the build runner".to_string()),
        };
        save_to(&file, &p).expect("save");
        let back = load_from(&p).expect("load");
        assert_eq!(back, file);
    }

    /// Unknown fields — a FUTURE build's file read by THIS build — parse rather than brick:
    /// stray keys at both the top level and inside a record are ignored.
    #[test]
    fn unknown_fields_are_tolerated() {
        let raw = r#"{
            "backends": [{
                "name": "the CI box",
                "addr": "h:1",
                "observe_key": "K_OBS",
                "control_key": null,
                "control": false,
                "from_the_future": {"nested": true}
            }],
            "active": "the CI box",
            "schema_note": "a v3 field this build has never heard of"
        }"#;
        let f: BackendsFile = serde_json::from_str(raw).expect("future file must parse");
        assert_eq!(f.backends.len(), 1);
        assert_eq!(f.backends[0].name, "the CI box");
        assert_eq!(f.active.as_deref(), Some("the CI box"));
    }

    /// Missing fields — a v1 file read by a build that has since grown fields — fill with
    /// defaults (the `WinSnap::asset_class` idiom): no `control` key loads as UNARMED, no
    /// `control_key` as `None`, no `active` as `None`. The default being the DISARMED state is
    /// load-bearing: an old file can never arm a write channel by omission.
    #[test]
    fn a_v1_file_with_missing_fields_loads_disarmed() {
        let raw = r#"{"backends": [{"name": "a", "addr": "h:1", "observe_key": "K"}]}"#;
        let f: BackendsFile = serde_json::from_str(raw).expect("v1 file must parse");
        assert_eq!(f.backends.len(), 1);
        assert!(!f.backends[0].control, "absent `control` must default to DISARMED");
        assert_eq!(f.backends[0].control_key, None);
        assert_eq!(f.active, None);
    }

    /// An absent file is the ordinary "no backends yet" state — the empty default, not an
    /// error. A present-but-corrupt file answers the same default (never brick startup).
    #[test]
    fn missing_file_is_an_empty_default() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let absent = tmp.path().join(BACKENDS_FILE);
        assert_eq!(load_or_default_from(&absent), BackendsFile::default());

        let corrupt = tmp.path().join("corrupt.json");
        std::fs::write(&corrupt, "{ not json").expect("write corrupt");
        assert_eq!(load_or_default_from(&corrupt), BackendsFile::default());
    }

    /// The B9 arming gate, all four refuse-paths and the one accept-path: an unarmed record
    /// never yields a control key EVEN WHEN the map holds it; `control_key: None` never arms;
    /// a named-but-absent key resolves to nothing; and only armed + named + present answers.
    #[test]
    fn an_unarmed_record_never_yields_a_control_key() {
        let m = vars(&[("PROD2_OBSERVE_KEY", "obs-bytes"), ("PROD2_CONTROL_KEY", "ctl-bytes")]);

        // control = false, key named AND present in the map: still None.
        let disarmed = rec("the CI box");
        assert_eq!(resolve_keys(&disarmed, &m), (Some("obs-bytes"), None));

        // control = true but no control_key named: None.
        let mut keyless = rec("the CI box");
        keyless.control = true;
        keyless.control_key = None;
        assert_eq!(resolve_keys(&keyless, &m), (Some("obs-bytes"), None));

        // control = true, key named, but absent from the map: None (and observe likewise
        // answers only what the map holds).
        let mut armed = rec("the CI box");
        armed.control = true;
        let empty = vars(&[]);
        assert_eq!(resolve_keys(&armed, &empty), (None, None));

        // The one path that arms: control = true + key named + present.
        assert_eq!(resolve_keys(&armed, &m), (Some("obs-bytes"), Some("ctl-bytes")));
    }

    /// A hostile name full of path separators is neutralized on save: what lands on disk
    /// carries no separator, the `active` pointer is sanitized with the same function so it
    /// still matches, and the registry directory holds exactly the one file — nothing escaped,
    /// and no `.tmp` residue survives the write.
    #[test]
    fn a_hostile_name_cannot_escape_the_directory() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let p = tmp.path().join(BACKENDS_FILE);
        let hostile = "../../evil\\..\\name";
        let file = BackendsFile { backends: vec![rec(hostile)], active: Some(hostile.to_string()) };
        save_to(&file, &p).expect("save");

        let back = load_from(&p).expect("load");
        let name = &back.backends[0].name;
        assert!(
            !name.contains('/') && !name.contains('\\') && !name.contains(".."),
            "sanitized name still carries a path component: {name:?}"
        );
        assert_eq!(
            back.active.as_deref(),
            Some(name.as_str()),
            "the active pointer must be sanitized by the same function, so it still matches"
        );
        let entries: Vec<_> = std::fs::read_dir(tmp.path())
            .expect("read dir")
            .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(entries, vec![BACKENDS_FILE.to_string()], "exactly one file, no escapees");
    }
}
