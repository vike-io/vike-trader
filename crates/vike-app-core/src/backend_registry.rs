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

/// Where a viewer looks when nothing else says: the daemon on this machine.
///
/// ⚠ NOT a guess at what the user wants — a floor under [`active_addr`] so the GUI OPENS. The thin
/// client used to exit(2) without `--observe <host>:<port>`, which made a mistyped flag close the
/// window instead of showing one. Nothing in a viewer can place an order, so a wrong address costs
/// a reconnect line in the status bar; a refusal to start costs the whole session.
///
/// `9099` is the port `docs/ops/tradehub-container.md` uses in every example, so the default agrees
/// with the documentation a first-time reader is following.
pub const DEFAULT_OBSERVE_ADDR: &str = "127.0.0.1:9099";

/// The ACTIVE backend's address, if the registry names one and it is usable.
///
/// The "configure once" half of the resolution order. Reads the file every call rather than caching
/// it: the Connections editor writes it while the app runs, and a viewer that kept a stale address
/// after the user changed it would be the same class of confusion this replaced.
///
/// A record whose address is blank is treated as absent — a half-filled row in the editor is a
/// row the user has not finished, not an instruction to dial nothing.
#[must_use]
pub fn active_addr() -> Option<String> {
    active_record().map(|b| b.addr.trim().to_string())
}

/// The ACTIVE backend's whole RECORD — [`active_addr`]'s twin, and the one a STARTUP connect must
/// use.
///
/// ⚠ An address alone loses the record's own `observe_key`/`control_key` NAMES and its `control`
/// arming, and the startup path had nothing else to build a connection from: it always used
/// [`crate::backend_conn::cli_observe_record`], whose key name is the fixed
/// [`OBSERVE_KEY_NAME`]. So a launch resolved the right ADDRESS from the
/// registry and then signed with the wrong key — `tradehub observe auth denied: bad mac`, on a
/// retry loop, forever. Measured 2026-09-03 against a node whose record named `PROD2_OBSERVE_KEY`:
/// the client dialled `127.0.0.1:9097` correctly and never authenticated once.
///
/// Shares [`active_addr`]'s rules, because that function is now this one plus a field read: the
/// pointer must name a record the registry holds, and a record with a blank address is treated as
/// absent (a half-filled editor row is unfinished, not an instruction to dial nothing).
#[must_use]
pub fn active_record() -> Option<BackendRecord> {
    pick_active(&load()).cloned()
}

/// [`active_record`]'s PURE half — the resolution with the file supplied, so the rules can be
/// tested without one on disk and the tests drive the real code rather than a copy of it.
fn pick_active(file: &BackendsFile) -> Option<&BackendRecord> {
    let active = file.active.as_deref()?;
    file.backends.iter().find(|b| b.name == active).filter(|b| !b.addr.trim().is_empty())
}

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
    /// The two key names `vike-app`'s `workspace_credentials` fills from must be the ones the
    /// client actually signs with.
    ///
    /// They are literals on purpose — the settings registry resolves constants crate-wide, so a
    /// name imported from `vike-tradehub-client` would be invisible to it and the read would pass
    /// its gate by blindness rather than by declaration. That trade is only safe with this test:
    /// without it, renaming the constant leaves this file looking up a key nobody sets, and the
    /// symptom is an endless `bad mac` at the node — the exact failure the gap-fill was added to
    /// remove.
    #[test]
    fn observe_and_control_key_names_match_the_client() {
        assert_eq!(OBSERVE_KEY_NAME, vike_tradehub_client::auth::OBSERVE_KEY_ENV);
        assert_eq!(CONTROL_KEY_NAME, vike_tradehub_client::auth::CONTROL_KEY_ENV);
    }

    use super::{CONTROL_KEY_NAME, NodeKeyFill, OBSERVE_KEY_NAME, fill_node_keys_from_env};

    fn map(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect()
    }

    /// THE fix: a container that passes the key in the environment and mounts no store now gets a
    /// usable key. Before this, the thin image authenticated with an empty one and the node
    /// answered `bad mac` for ever — measured on the published 0.1.16 image.
    #[test]
    fn an_env_key_fills_an_empty_store() {
        let mut creds = map(&[]);
        let got = fill_node_keys_from_env(&mut creds, &map(&[(OBSERVE_KEY_NAME, "sekrit")]));
        assert_eq!(got, NodeKeyFill::ObserveFromEnv);
        assert_eq!(creds.get(OBSERVE_KEY_NAME).map(String::as_str), Some("sekrit"));
    }

    /// ...and the direction that must NOT happen: a variable on the box silently replacing the key
    /// an operator wrote into `secrets.env`.
    #[test]
    fn a_store_entry_always_wins_over_the_environment() {
        let mut creds = map(&[(OBSERVE_KEY_NAME, "from-store")]);
        let got = fill_node_keys_from_env(&mut creds, &map(&[(OBSERVE_KEY_NAME, "from-env")]));
        assert_eq!(got, NodeKeyFill::Nothing);
        assert_eq!(creds.get(OBSERVE_KEY_NAME).map(String::as_str), Some("from-store"));
    }

    /// A blank value is no value. Inserting one would trade "no key, here is why" for `bad mac` at
    /// the reconnect cadence, which is a strictly worse message for the same broken state.
    #[test]
    fn a_blank_env_value_is_not_a_key() {
        for blank in ["", "   ", "\t"] {
            let mut creds = map(&[]);
            let got = fill_node_keys_from_env(&mut creds, &map(&[(OBSERVE_KEY_NAME, blank)]));
            assert_eq!(got, NodeKeyFill::Nothing, "{blank:?} was treated as a key");
            assert!(!creds.contains_key(OBSERVE_KEY_NAME));
        }
    }

    /// Control is never taken from the environment — it places orders — but the attempt is
    /// REPORTED, because an asymmetry nobody announces is one every user rediscovers.
    #[test]
    fn a_control_key_in_the_environment_is_refused_and_reported() {
        let mut creds = map(&[(OBSERVE_KEY_NAME, "s")]);
        let got = fill_node_keys_from_env(&mut creds, &map(&[(CONTROL_KEY_NAME, "control")]));
        assert_eq!(got, NodeKeyFill::ControlIgnored);
        assert!(
            !creds.contains_key(CONTROL_KEY_NAME),
            "the control key reached the credentials map from the environment — that would let a \
             variable on the box arm order placement"
        );
    }

    /// The ordinary case on a configured box: a store with both keys, nothing from the environment.
    #[test]
    fn a_complete_store_needs_nothing_from_the_environment() {
        let mut creds = map(&[(OBSERVE_KEY_NAME, "o"), (CONTROL_KEY_NAME, "c")]);
        let before = creds.clone();
        assert_eq!(fill_node_keys_from_env(&mut creds, &map(&[])), NodeKeyFill::Nothing);
        assert_eq!(creds, before);
    }
}

// ─── Node keys: where they may come from ──────────────────────────────────────────────────────

/// The node-key names the app looks up in its credential map.
///
/// ⚠ CONSTANTS HERE, not `vike_tradehub_client::auth`'s: the settings registry resolves a name
/// through a constant declared in THIS crate and no further, so importing the client's would make
/// the reads invisible to `crates/vike-ops/tests/settings_registry.rs` — passing its gate by
/// blindness rather than by declaration. `credential_key_names` at the bottom of this file holds
/// them equal to the client's, which is what makes the duplication safe.
pub const OBSERVE_KEY_NAME: &str = "VIKE_TRADEHUB_OBSERVE_KEY";
pub const CONTROL_KEY_NAME: &str = "VIKE_TRADEHUB_CONTROL_KEY";

/// What [`fill_node_keys_from_env`] did, so the caller can say it and a test can assert it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKeyFill {
    /// The store had no observe key and the environment supplied one.
    ObserveFromEnv,
    /// A control key is set in the environment and was deliberately not used.
    ControlIgnored,
    Nothing,
}

/// Fill the observe key from `env` when — and only when — the credential store has none.
///
/// PURE (no process state, no files), which is the whole reason it is a function rather than four
/// lines inside [`workspace_credentials`]: the behaviour below is a policy about credentials and
/// the tests at the bottom of this file are what hold it.
///
/// ⚠ **GAP-FILL, never an override.** A store entry wins. An environment variable that silently
/// replaced a key the operator wrote into `secrets.env` is the class this workspace already fought
/// over the risk ceilings (`vike_config::REMOVED_ENV`); the store stays the declared home.
///
/// ⚠ **The CONTROL key is deliberately NOT filled**, and the asymmetry is announced rather than
/// left to be discovered: observe is read-only, control PLACES ORDERS. The one surface that needed
/// the environment route is the thin image, which runs `--observe` and cannot trade at all, so
/// widening this would buy nothing and would let a variable on the box arm order placement.
pub fn fill_node_keys_from_env(
    creds: &mut HashMap<String, String>,
    env: &HashMap<String, String>,
) -> NodeKeyFill {
    // ⚠ The EXPLANATION is logged here, beside the policy, not by the caller. It was in the shell
    // and cost that crate a dozen lines it is rationed on
    // (`crates/vike-ops/tests/ci_excluded_gui_shell_ratchet.rs`) — but the better reason is that a
    // rule and the sentence describing it drift apart the moment they live in different files.
    if !creds.contains_key(OBSERVE_KEY_NAME) {
        // A blank value is no value: an empty key signs a MAC the node refuses, and inserting one
        // would replace "no key, here is why" with "bad mac" at the reconnect cadence — which is
        // the failure this whole function exists to remove.
        if let Some(v) = env.get(OBSERVE_KEY_NAME).filter(|v| !v.trim().is_empty()) {
            creds.insert(OBSERVE_KEY_NAME.to_string(), v.clone());
            tracing::info!(
                key = OBSERVE_KEY_NAME,
                "observe key taken from the environment — the credential store has none. This is \
                 how the thin-client container supplies it; a store entry, when one exists, wins."
            );
            return NodeKeyFill::ObserveFromEnv;
        }
    }
    if !creds.contains_key(CONTROL_KEY_NAME)
        && env.get(CONTROL_KEY_NAME).is_some_and(|v| !v.trim().is_empty())
    {
        tracing::warn!(
            key = CONTROL_KEY_NAME,
            "a control key is set in the environment and is NOT used: control places orders, so it \
             comes from the credential store only — put it in <project>/settings/secrets.env."
        );
        return NodeKeyFill::ControlIgnored;
    }
    NodeKeyFill::Nothing
}

#[cfg(test)]
mod active_addr_tests {
    use super::*;

    fn rec(name: &str, addr: &str) -> BackendRecord {
        BackendRecord {
            name: name.to_string(),
            addr: addr.to_string(),
            observe_key: "K".to_string(),
            control_key: None,
            control: false,
        }
    }

    /// `active_addr` without a file on disk — the REAL resolution ([`pick_active`]) plus the field
    /// read, so these tests cannot pass while the shipped function disagrees with them.
    fn pick(file: &BackendsFile) -> Option<String> {
        pick_active(file).map(|b| b.addr.trim().to_string())
    }

    #[test]
    fn the_active_record_is_the_one_answered() {
        let f = BackendsFile {
            backends: vec![rec("home", "<host>:9099"), rec("vps", "example:9099")],
            active: Some("vps".to_string()),
        };
        assert_eq!(pick(&f).as_deref(), Some("example:9099"));
    }

    /// An empty registry answers nothing, so the caller falls through to its default rather than
    /// dialling an empty string — which would look like a hang instead of a first run.
    #[test]
    fn an_empty_or_unpointed_registry_answers_nothing() {
        assert_eq!(pick(&BackendsFile::default()), None);
        let orphan = BackendsFile {
            backends: vec![rec("home", "<host>:9099")],
            active: Some("deleted".to_string()),
        };
        assert_eq!(pick(&orphan), None, "an `active` naming no record must not answer");
    }

    /// The record answer carries the KEY NAMES, which is the whole reason `active_record` exists:
    /// a startup connect that kept only the address signed with the CLI-synthetic
    /// `VIKE_TRADEHUB_OBSERVE_KEY` and failed `bad mac` against a node whose record named its own.
    #[test]
    fn the_record_answer_carries_the_key_names_not_just_the_address() {
        let mut vps = rec("vps", "example:9099");
        vps.observe_key = "PROD2_OBSERVE_KEY".to_string();
        vps.control_key = Some("PROD2_CONTROL_KEY".to_string());
        vps.control = true;
        let f = BackendsFile {
            backends: vec![rec("home", "<host>:9099"), vps],
            active: Some("vps".to_string()),
        };

        let picked = pick_active(&f).expect("the active record answers");
        assert_eq!(picked.addr, "example:9099");
        assert_eq!(picked.observe_key, "PROD2_OBSERVE_KEY");
        assert_eq!(picked.control_key.as_deref(), Some("PROD2_CONTROL_KEY"));
        assert!(picked.control, "the record's own arming rides along with it");
    }

    /// A blank address is treated as absent by BOTH answers, because they are one function: a
    /// half-filled editor row is unfinished, not an instruction to dial nothing.
    #[test]
    fn a_blank_address_is_absent_for_the_record_answer_too() {
        let f =
            BackendsFile { backends: vec![rec("half", "   ")], active: Some("half".to_string()) };
        assert_eq!(pick(&f), None);
        assert!(pick_active(&f).is_none(), "the two answers cannot disagree — same resolution");
    }

    /// A half-filled row in the Connections editor is a row the user has not finished, not an
    /// instruction to dial nothing.
    #[test]
    fn a_blank_address_is_treated_as_absent() {
        for blank in ["", "   "] {
            let f = BackendsFile {
                backends: vec![rec("home", blank)],
                active: Some("home".to_string()),
            };
            assert_eq!(pick(&f), None, "{blank:?} was taken as an address");
        }
    }

    /// The default must be loopback. A viewer that defaulted to a PUBLIC address would dial a
    /// stranger's machine on first launch, which is the one first-run behaviour that could be
    /// worse than refusing to start.
    #[test]
    fn the_default_is_loopback_and_carries_a_port() {
        assert!(
            DEFAULT_OBSERVE_ADDR.starts_with("127.0.0.1:")
                || DEFAULT_OBSERVE_ADDR.starts_with("localhost:"),
            "{DEFAULT_OBSERVE_ADDR} is not loopback — a viewer must not dial off-box by default"
        );
        let port = DEFAULT_OBSERVE_ADDR.rsplit(':').next().unwrap_or("");
        assert!(port.parse::<u16>().is_ok(), "{DEFAULT_OBSERVE_ADDR} carries no usable port");
    }
}
