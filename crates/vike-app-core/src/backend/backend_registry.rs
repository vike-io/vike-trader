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
//! `crates/vike-app-core/src/ui/workspace/persist.rs` — its `base_dir`, the workspace family's one
//! resolver, whose env reads already carry their `vike_ops::settings::SETTINGS` rows — and the
//! only file it touches is `backends.json` itself. Key NAMES, not values, is the contract
//! rather than a preference, twice over (spec B7):
//!
//!   - **Key material must never land in a GUI-owned JSON file.** The credential store (the
//!     settings database) is the ONE home for secrets; this file is written by the GUI and read
//!     back as plain data, so a value stored here would be a second, ungated
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
//! **Why this is deliberately NOT a settings row (B8):** settings rows are OPERATOR-owned —
//! validated on write, unknown keys refused by name, every key gated by `vike_config::CONSUMPTION`.
//! This file is GUI-owned: the app WRITES it (add / remove / select a backend from the UI),
//! exactly like `workspace.json`, so it takes the same route — the workspace-persistence idiom
//! (one `base_dir`, forward-compat serde, sanitized names, never-brick loads) and NOT the settings
//! pipeline. It also must not reuse `config.tradehub_addr`: that key is the DAEMON's bind address,
//! not a client-side dial list.
//!
//! **Forward-compat:** every field is `#[serde(default)]` — the `WinSnap::asset_class` idiom
//! from `crates/vike-app-core/src/ui/workspace/persist.rs` — and unknown fields are ignored
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
    /// KEY NAME of the DATAHUB observe key this backend's market-data plane signs with. Blank ⇒
    /// [`DATAHUB_OBSERVE_KEY_NAME`], the platform default.
    ///
    /// ⚠ **This is a PLATFORM key and it does NOT live in the venue credential store.**
    /// `docs/decisions/0051-node-keys-live-in-their-own-store.md` moved the four
    /// `VIKE_*_{OBSERVE,CONTROL}_KEY` names into the node-key store — the settings database's
    /// `node_key` table — and `crates/vike-ops/tests/settings_secrets/node_key_store_gate.rs` holds
    /// it. So this name is resolved by [`datahub_observe_keys`] — process environment first, then
    /// that table — and never out of the map [`resolve_keys`] takes, which is the VENUE store.
    ///
    /// ⚠ **A custom name must be admitted by the read's SCOPE.** `vike_secrets::resolve_node_keys`
    /// hands back only the names its predicate admits, and [`datahub_observe_keys`] hands it *the
    /// datahub family OR this very name*. The bare `is_datahub_node_key` could never admit a CUSTOM
    /// name, so the key resolved to `None` and the market-data plane dialled unauthenticated.
    /// `datahub_observe_keys` carries the whole argument.
    ///
    /// ⚠ **It is honoured on a runtime SWITCH, not only at launch**, and that took wiring: the
    /// market-data session's ADDRESS follows `App::active_backend` per frame, so a key taken once at
    /// `App::new` would have signed the new backend's datahub with the previous record's name —
    /// `bad mac` at the handshake, which is the symptom
    /// [`crate::backend::backend_editor`]'s `an_edit_preserves_the_datahub_key_name_the_form_does_not_carry`
    /// names. `vike_app_core::data::md_session::MdSession::set_key_name` is the late-bound twin of
    /// `set_addr`, pushed from the same place, and the reconciler re-resolves off the frame thread.
    ///
    /// ⚠ There is deliberately no `datahub_control_key` twin. The desktop is an OBSERVER (ruling 1)
    /// and a market-data subscription is `VerbScope::Read` (`docs/decisions/0052`); the datahub's
    /// CONTROL scope also carries the verbs that COMPILE CLIENT-SUPPLIED RHAI, so a desktop that
    /// only wants a DOM ladder must never hold that key. It is the same observe/control asymmetry
    /// `fill_node_keys_from_env` already announces for the tradehub pair.
    ///
    /// ⚠ **One exception exists since 2026-09-26, and it is not here.** The owner ruled
    /// (`docs/decisions/0083-the-runtime-plugin-join-lands.md`, question 1, option (a)) that the
    /// desktop resolves the datahub Control key for Studio's COMPUTE dial only. It is NOT a record
    /// field and never passes through this module: `vike_studio::compute_key_from_vars` reads it
    /// from its own environment variable into a `vike_studio::ComputeKey`, a type none of the
    /// dials resolved here accepts. What this paragraph says is still the rule for every dial this
    /// registry keys — this module's
    /// `no_datahub_dial_here_resolves_a_write_key_whatever_the_environment_holds` is its test.
    #[serde(default)]
    pub datahub_observe_key: String,
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
///
/// `pub(crate)` so `crate::ui::workspace::persist`'s `FAMILY_MEMBERS` — the list of what a state-directory
/// relocation moves — names this file through the constant rather than through a second literal.
pub(crate) const BACKENDS_FILE: &str = "backends.json";

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
/// [`crate::backend::backend_conn::cli_observe_record`], whose key name is the fixed
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
    crate::ui::workspace::persist::sanitize_layout_name(name)
}

/// Registry file path for READING: `<base_dir>/backends.json` — beside the workspace file, so
/// `$VIKE_WORKSPACE` (whose directory is the family's base) moves this file together with the
/// layouts family. `None` when no base resolves (no override, no project above the working
/// directory): there is no file to read then, and [`load`] reports that as "no backends yet".
pub fn path() -> Option<PathBuf> {
    Some(crate::ui::workspace::persist::base_dir()?.join(BACKENDS_FILE))
}

/// Registry file path for WRITING — same location as [`path`], with the directory created
/// lazily and a symlinked target refused (`vike_model::paths::state_path::write_path`, the same
/// resolver the workspace file's save uses).
fn save_path() -> std::io::Result<PathBuf> {
    vike_model::paths::state_path::write_path(
        crate::ui::workspace::persist::base_dir().as_deref(),
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

/// [`resolve_keys`]' OBSERVE half as the bytes a read sends, EMPTY when the map holds no key: the
/// read still runs, the node refuses its handshake, and the caller shows THAT refusal — the honest
/// answer every GUI read of a backend gives (its Backend settings section, the node's directory).
/// One spelling for what the shell's reads each restated in three lines.
pub fn observe_key_bytes(rec: &BackendRecord, vars: &HashMap<String, String>) -> Vec<u8> {
    resolve_keys(rec, vars).0.unwrap_or_default().as_bytes().to_vec()
}

/// [`resolve_keys`]' CONTROL half as the bytes a write sends, EMPTY on a record that is not armed or
/// whose control key the map does not hold: the write still runs and the node refuses it, which the
/// caller shows. Never another key: the B9 arming gate is `resolve_keys`' own.
pub fn control_key_bytes(rec: &BackendRecord, vars: &HashMap<String, String>) -> Vec<u8> {
    resolve_keys(rec, vars).1.unwrap_or_default().as_bytes().to_vec()
}

// ⚠ THE DATAHUB NODE-KEY BLOCK SITS ABOVE `mod tests`, AND THE POSITION USED TO BE LOAD-BEARING.
// `crates/vike-ops/tests/settings_secrets/node_key_store_gate/source_scan.rs`'s `code_lines` finds the end of a `#[cfg(test)]`
// item by COUNTING BRACES, and it counted them inside string literals too. This file's
// `missing_file_is_an_empty_default` writes the corrupt payload `"{ not json"` — one unmatched
// brace — so from that line to EOF the gate saw nothing at all, and a `node_keys_from_vars` call
// placed below it was invisible: the gate reported the DECLARED_SITES row as stale rather than
// reporting the call. That was a blind spot in the SCANNER, not in this file.
//
// It is CLOSED: that gate's `without_literals` now drops string, raw-string and char-literal spans
// before counting, and its `an_unmatched_brace_in_a_string_literal_does_not_hide_the_code_below_it`
// plants this exact payload as the kill proof. The block stays here because moving it back would be
// churn, not because it has to be — and this note stays because the workaround it describes is the
// reason the code is where it is.

/// The DATAHUB observe key's platform name — what a blank [`BackendRecord::datahub_observe_key`]
/// means. Declared HERE for the same reason [`OBSERVE_KEY_NAME`] is: the settings registry resolves
/// a name through a constant declared in THIS crate and no further, so importing
/// `vike_node_proto::auth::DATAHUB_OBSERVE_KEY_ENV` would pass its gate by BLINDNESS
/// rather than by declaration. `backend_registry_tests::the_datahub_observe_key_name_matches_the_client`
/// holds it equal to the client's.
pub const DATAHUB_OBSERVE_KEY_NAME: &str = "VIKE_DATAHUB_OBSERVE_KEY";

/// The key NAME a record's datahub connection signs with: its own override when it names one, else
/// [`DATAHUB_OBSERVE_KEY_NAME`].
#[must_use]
pub fn datahub_observe_key_name(rec: &BackendRecord) -> &str {
    let named = rec.datahub_observe_key.trim();
    if named.is_empty() { DATAHUB_OBSERVE_KEY_NAME } else { named }
}

/// **Resolve the DATAHUB observe key** — the credential gap the market-data wire's client half
/// could not open without (design §9 item 8).
///
/// Before this, `crates/vike-app-core` and `crates/vike-desktop` contained no call to
/// `node_keys_from_vars`, `RemoteHistStore::with_keys` or `DatahubClient::connect_authed`: every
/// dial from the desktop was UNAUTHENTICATED, and `bind_decision` requires a KEYED server for any
/// non-loopback bind — so against the only kind of datahub reachable off-box, every read failed at
/// the handshake.
///
/// Precedence: **process environment first, then the node-key store** (the settings database's
/// `node_key` table, through `vike_secrets::resolve_node_keys`) — the exact ladder
/// `crates/vike-cli/src/boot.rs`'s `datahub_keyring` implements, so the CLI and the GUI cannot
/// answer differently about where a key comes from.
///
/// ⚠ **`settings_dir` is a PARAMETER and this function reads NO environment.** The composition root
/// owns the one boot walk and the one `std::env::vars()` sweep; taking them here is what keeps this
/// out of `crates/vike-ops/tests/settings_secrets/settings_registry.rs`'s `LIBRARY_PIN` (a ratchet that may shrink,
/// never grow) and out of `CREDENTIAL_STORE_PIN` — `resolve_node_keys` is deliberately not one of
/// that pin's readers, which is how `crates/vike-datahub/src/datahub_cli.rs` LEFT it.
///
/// ⚠ **THE SCOPE IS THE SET THIS CALL WILL TAKE OUT — the datahub family OR `key_name`.**
/// `vike_secrets::resolve_node_keys` hands back only the names its predicate admits; this function
/// then takes `key_name` out of that map. So the predicate has to be neither wider nor narrower:
///
///   - **Wider** (`vike_model::credential_keys::is_platform_key`, all four platform names) would
///     materialise the TRADEHUB pair in a call that only ever needs a datahub key.
///   - **Narrower** (the bare `is_datahub_node_key`) can never match a record's CUSTOM
///     [`BackendRecord::datahub_observe_key`] — the key resolves to `None`, and the market-data
///     plane dials UNAUTHENTICATED and fails the handshake in silence: no notice fires for a name
///     outside the node-key families.
///
/// `|k| is_datahub_node_key(k) || k == key_name` is decision 0051's scope narrowed to the names
/// this caller actually takes out of the map. It keeps the two service families disjoint — a
/// custom name is in neither, so it can never widen this scope to the TRADEHUB pair, which is
/// resolved by `overlay_node_store` through its own family predicate.
/// `crates/vike-app-core/tests/node_key_store_spellings.rs`'s
/// `the_datahub_key_resolves_from_the_table_and_the_scope_holds` is the gate.
///
/// ⚠ **Only the OBSERVE half is ever populated.** The returned map carries exactly one entry, so
/// `NodeKeys`' control half loads as an empty `Vec` by construction rather than by a promise — see
/// [`BackendRecord::datahub_observe_key`] for why no dial this function feeds (the store, the chart
/// seed, the venue catalog, the market-data session, Studio's named run) may hold a datahub control
/// key. The one Control key the desktop holds is Studio's compute key, which never comes through
/// here.
///
/// Returns the keys (`None` = none configured, which is the ordinary unconfigured state and is what
/// makes a key-less loopback dev server work untouched) plus every NOTICE the resolution produced —
/// a store-permission finding, or an unreadable store — as data, for the caller to log once. Never
/// panics, never creates a file, never writes one.
#[must_use]
pub fn datahub_observe_keys(
    settings_dir: Option<&str>,
    env: &HashMap<String, String>,
    key_name: &str,
) -> (Option<vike_node_proto::auth::NodeKeys>, Vec<String>) {
    let mut notices = Vec::new();
    // A blank value is NO value — an empty key signs a mac the server refuses, which would replace
    // "no key, here is why" with `bad mac` on a reconnect loop.
    let non_blank = |v: &String| -> Option<String> {
        let t = v.trim();
        (!t.is_empty()).then(|| t.to_string())
    };
    // Rung 1: the process environment, which returns before any file is opened.
    let mut value = env.get(key_name).and_then(non_blank);
    if value.is_none() {
        // Rung 2: the NODE store. ⚠ NOT the venue store: that table holds 168 venue key names and
        // these are four, which is the whole of decision 0051.
        //
        // ⚠ SCOPE EXACTLY WHAT IS ABOUT TO BE TAKEN OUT — the datahub family OR `key_name` itself.
        // See this function's doc: the names this call takes out of the map are the datahub pair
        // and a record's own override, so the scope is that set and nothing wider.
        match vike_secrets::resolve_node_keys(settings_dir, |k: &str| {
            vike_model::credential_keys::is_datahub_node_key(k) || k == key_name
        }) {
            Ok(resolved) => {
                if let Some(w) = &resolved.warning {
                    // `PermissionWarning` is a STRUCT with a `Display` impl, not a string: it is
                    // returned as DATA because `vike-secrets` carries no logging dependency at
                    // all, so rendering it is this caller's job.
                    notices.push(w.to_string());
                }
                value = resolved.secrets.into_map().get(key_name).and_then(non_blank);
            }
            Err(e) => notices.push(format!("cannot read the node-key store: {e}")),
        }
    }
    let mut observe_only: HashMap<String, String> = HashMap::new();
    if let Some(v) = value {
        observe_only.insert(DATAHUB_OBSERVE_KEY_NAME.to_string(), v);
    }
    (vike_node_proto::auth::node_keys_from_vars(&observe_only), notices)
}

/// **The datahub STORE handle every hist-plane read dials through** — authenticated when this
/// desktop holds a datahub observe key, and the ONE spelling of that dial.
///
/// It lives beside [`datahub_observe_keys`] because the key resolution IS the dial's hard part:
/// every consumer (the Studio's one-shot open, the Data Manager's inventory walk, the chart's
/// store read in [`crate::data::store_bars`]) needs the identical pairing of "resolve the key for THIS
/// backend record's key name, then hand it to `with_keys`", and the binary's own wrapper is now
/// just the two composition-root facts — the settings directory and the process environment — fed
/// in as parameters.
///
/// ⚠ **There is no configuration in which the key-less constructor is the better choice**, which is
/// why this has no "plain" arm to pick: `RemoteHistStore::with_keys`' own contract is that against
/// a KEY-LESS server it degrades to an ordinary unauthenticated connect, "so one configured GUI
/// works against both a keyed production datahub and a local dev one". The `None` arm below is
/// reached only when this desktop holds NO key to offer, never as a preference. What the key-less
/// path cost when both call sites were on it — `No stored data · 0 rows · 0 B` from a datahub that
/// answered the same address with 7.49 billion rows on the CLI — is recorded at
/// [`datahub_observe_keys`].
///
/// Notices from the key resolution are LOGGED here rather than returned: every caller did the same
/// thing with them, and a store dial is not a place an operator is watching for a return value.
pub fn remote_hist_store(
    addr: String,
    settings_dir: Option<&str>,
    env: &HashMap<String, String>,
    key_name: &str,
) -> vike_datahub_client::RemoteHistStore {
    let (keys, notices) = datahub_observe_keys(settings_dir, env, key_name);
    for n in notices {
        tracing::warn!("datahub store: {n}");
    }
    match keys {
        Some(k) => vike_datahub_client::RemoteHistStore::with_keys(addr, k),
        None => vike_datahub_client::RemoteHistStore::new(addr),
    }
}

#[path = "backend_registry_tests.rs"]
#[cfg(test)]
mod backend_registry_tests;

// ─── Node keys: where they may come from ──────────────────────────────────────────────────────

/// The node-key names the app looks up in its credential map.
///
/// ⚠ CONSTANTS HERE, not `vike_tradehub_client::auth`'s: the settings registry resolves a name
/// through a constant declared in THIS crate and no further, so importing the client's would make
/// the reads invisible to `crates/vike-ops/tests/settings_secrets/settings_registry.rs` — passing its gate by
/// blindness rather than by declaration. `credential_key_names` at the bottom of this file holds
/// them equal to the client's, which is what makes the duplication safe.
pub const OBSERVE_KEY_NAME: &str = "VIKE_TRADEHUB_OBSERVE_KEY";
pub const CONTROL_KEY_NAME: &str = "VIKE_TRADEHUB_CONTROL_KEY";
/// What `fill_node_keys_from_env` did, so the caller can say it and a test can assert it.
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
/// replaced a key the operator wrote into the credential store is the class this workspace already
/// fought over the risk ceilings (`vike_config::REMOVED_ENV`); the store stays the declared home.
///
/// ⚠ **The CONTROL key is deliberately NOT filled**, and the asymmetry is announced rather than
/// left to be discovered: observe is read-only, control PLACES ORDERS. The one surface that needed
/// the environment route is the thin image, which runs `--observe` and cannot trade at all, so
/// widening this would buy nothing and would let a variable on the box arm order placement.
///
/// ⚠ **PRIVATE, and that is the gate for ONE revert — not a tidy-up, and not a fence around the
/// caller's line.** This was `pub`, and `vike-desktop`'s `workspace_credentials` called it DIRECTLY:
/// the process environment alone, with the node store never opened. That is precisely the defect
/// [`fill_node_keys`] exists to close, and while this function stayed reachable from outside the
/// crate, reverting the shell's one line reintroduced it and every test in
/// `crates/vike-app-core/tests/node_key_store_spellings.rs` and
/// `crates/vike-ops/tests/settings_secrets/node_key_store_gate.rs` still passed — `vike-desktop` sits in
/// `xtask/src/ci/tables/roster.rs`'s `EXCLUDE_FROM_CI`, so only `app-check` compiles it. Privacy makes
/// THAT revert an `E0603` in the `app-check` job, and needs no path-keyed row to rot. This repo
/// forbids a `pub use` kept so an old spelling compiles; a `pub fn` kept so a WRONG old call
/// compiles is the same thing wearing a different keyword.
///
/// ⚠ **It covers that one shape and no other, which is worth saying because privacy reads like a
/// fence.** Two other reverts of the same defect still COMPILE: deleting the [`fill_node_keys`]
/// call from the shell outright (it is `pub` — nothing warns, and the behavioural pin above calls it
/// directly and stays green), and passing `None` for its `settings_dir` (node-key resolution then
/// becomes a `$VIKE_SETTINGS_DIR`-BLIND walk from the working directory). Those two are held by a
/// TEXT rule instead — `crates/vike-ops/tests/settings_secrets/node_key_store_gate/shell_rule.rs`'s `SHELL_SITES` — which is
/// path-keyed and must be re-keyed if `crates/vike-desktop/src/main.rs` is renamed or moved.
fn fill_node_keys_from_env(
    creds: &mut HashMap<String, String>,
    env: &HashMap<String, String>,
) -> NodeKeyFill {
    // ⚠ The EXPLANATION is logged here, beside the policy, not by the caller. It was in the shell
    // and cost that crate a dozen lines it is rationed on
    // (`crates/vike-ops/tests/gui/ci_excluded_gui_shell_ratchet.rs`) — but the better reason is that a
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
             comes from the node-key store only — the settings database's node_key table \
             (`vike-cli backend setup` mints it there)."
        );
        return NodeKeyFill::ControlIgnored;
    }
    NodeKeyFill::Nothing
}

/// What ONE [`fill_node_keys`] call did — returned as DATA rather than only logged, so a test can
/// assert the resolution and a caller that has a subscriber can render the notices itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeKeyResolution {
    /// WHICH STORE answered for the tradehub pair — the settings database's `node_key` table, or
    /// none. `vike_secrets::resolve_node_keys`' own `source`, carried through unchanged.
    ///
    /// ⚠ A store that EXISTS and could not be READ also reads `None` here, because no store
    /// answered. The DISTINCTION is in [`Self::notices`], which is what reaches an operator; this
    /// field answers "where did the bytes come from", and for an unreadable store the answer is
    /// genuinely "nowhere".
    pub source: vike_secrets::Source,
    /// What the process-environment rung did afterwards — see `fill_node_keys_from_env`.
    pub fill: NodeKeyFill,
    /// A store-permission finding or an unreadable-store finding, already rendered. Empty on the
    /// overwhelmingly common path.
    pub notices: Vec<String>,
}

/// **Resolve the two TRADEHUB node keys into `creds` the way `vike-cli` and `vike-tradehub` already
/// resolve them**: the node-key store (the settings database's `node_key` table) first, then the
/// process environment as the gap-fill the thin-client image needs.
///
/// # The defect this closes
///
/// `docs/decisions/0051-node-keys-live-in-their-own-store.md` moved the platform node keys out of
/// the venue store into a store of their own. `vike-cli` was migrated (`node_key_store` in
/// `crates/vike-cli/src/lib.rs`) and so was the daemon (`start_observe_server` in
/// `crates/vike-tradehub/src/node.rs`); the DESKTOP was not. It read
/// [`OBSERVE_KEY_NAME`] straight out of the map `vike-desktop`'s `workspace_credentials` builds from
/// `vike_bridge_core::credentials::load_workspace_secrets_from_env` — the VENUE store — so an
/// operator who followed the decision and moved the pair kept a working CLI and got, on every
/// reconnect:
///
/// ```text
/// ERROR vike_app_core::backend::backend_conn: no observe key: VIKE_TRADEHUB_OBSERVE_KEY is absent …
/// WARN  vike_app_core::backend::observe_bridge: observe: … tradehub observe auth denied: bad mac
/// ```
///
/// `datahub_observe_keys` above already had the right shape for the datahub half; only the tradehub
/// half was left behind.
///
/// # ⚠ The node store answers WHOLLY — this is not a merge
///
/// Both names are taken from the one map `vike_secrets::resolve_node_keys` returned, and a name that
/// map does not carry is REMOVED from `creds` rather than left standing at whatever the venue store
/// held. Stitching an observe key out of one store together with a control key out of another is a
/// mismatched pair, and what an operator sees for it is an opaque `bad mac`.
///
/// ⚠ **"Wholly" is scoped to the TRADEHUB PAIR, not to all four platform names.**
/// `overlay_node_store`'s doc carries the gate.
///
/// # ⚠ `settings_dir` is a PARAMETER and this function reads NO environment
///
/// The same rule [`datahub_observe_keys`] states: the composition root owns the boot walk and the
/// one `std::env::vars()` sweep, so taking both here is what keeps this out of
/// `crates/vike-ops/tests/settings_secrets/settings_registry.rs`'s `LIBRARY_PIN` (a ratchet that may shrink, never
/// grow). The `env` map is the same sweep, used for the gap-fill rung and nothing else.
///
pub fn fill_node_keys(
    creds: &mut HashMap<String, String>,
    settings_dir: Option<&str>,
    env: &HashMap<String, String>,
) -> NodeKeyResolution {
    let (source, notices) = overlay_node_store(creds, settings_dir);
    report_node_store_once(&notices);
    NodeKeyResolution { source, fill: fill_node_keys_from_env(creds, env), notices }
}

/// The STORE rung of [`fill_node_keys`]: replace the tradehub pair in `creds` with whatever the
/// node-key store answers, wholly, and hand back the verdict plus anything worth saying about it.
///
/// ⚠ An unreadable store REMOVES the pair rather than degrading to the venue store's copy, and says
/// so. `vike-cli`'s `node_key_store` treats the same error as an empty store for the same reason:
/// an unreadable store and a missing one must never look the same to an operator.
///
/// ⚠ **The scope is the TRADEHUB FAMILY**: `vike_secrets::resolve_node_keys` hands back the names
/// the predicate admits, and this function then takes its own two names out of that map — removing
/// any it does not carry.
/// `crates/vike-app-core/tests/node_key_store_spellings.rs` pins the agreement with the CLI and
/// the daemon.
///
/// ⚠ **The unreadable-store NOTICE names the path the error carries.** `vike_secrets::SecretsError` names
/// the real path in both its `Display` and its `path` field, so the notice states THAT and claims
/// nothing else. `the_unreadable_store_notice_names_the_store_and_removes_the_pair` is the gate, and
/// it plants bytes that are not a database at the database's path rather than playing with
/// permissions — an open of one fails on every platform.
fn overlay_node_store(
    creds: &mut HashMap<String, String>,
    settings_dir: Option<&str>,
) -> (vike_secrets::Source, Vec<String>) {
    let mut notices = Vec::new();
    let answered = match vike_secrets::resolve_node_keys(
        settings_dir,
        vike_model::credential_keys::is_tradehub_node_key,
    ) {
        Ok(resolved) => {
            if let Some(w) = &resolved.warning {
                // `PermissionWarning` is a struct with a `Display` impl: `vike-secrets` carries no
                // logging dependency, so rendering it is this caller's job — the same division
                // `datahub_observe_keys` above already makes.
                notices.push(w.to_string());
            }
            Some((resolved.secrets.into_map(), resolved.source))
        }
        Err(e) => {
            // ⚠ NAME THE STORE THAT FAILED — the path the error carries. See this function's doc.
            notices.push(format!(
                "{} could not be READ while resolving the tradehub node keys ({e}) — no tradehub \
                 node key is taken from it this run, and nothing is silently read in its place. \
                 An unreadable store is NOT the absent-credential gate: fix its permissions and \
                 restart.",
                e.path.display()
            ));
            None
        }
    };
    let (answer, source) = answered.unwrap_or_else(|| (HashMap::new(), vike_secrets::Source::None));
    for name in [OBSERVE_KEY_NAME, CONTROL_KEY_NAME] {
        // Trimmed, because `vike_tradehub_client::auth::from_vars` — what the DAEMON verifies with
        // — trims: a client signing with `"secret\n"` against a server verifying `"secret"` fails
        // as `bad mac` rather than as "your key file has a trailing space".
        match answer.get(name).map(|v| v.trim()).filter(|v| !v.is_empty()) {
            Some(v) => creds.insert(name.to_string(), v.to_string()),
            None => creds.remove(name),
        };
    }
    (source, notices)
}

/// Log the node-store notices at most ONCE per process, and only once a SUBSCRIBER exists.
///
/// ⚠ **Both halves of that are load-bearing.** `vike-desktop`'s `workspace_credentials` is called
/// per FRAME by the Connections tool, so an unlatched notice would be a wall of identical warnings;
/// and that same function is handed to `vike_boot::boot`, which runs BEFORE `vike_log::init` (the
/// log directory is itself a setting), so a latch armed on that first call would swallow the one
/// line the operator needs — a store notice emitted into a subscriber-less void and never
/// repeated. The notices are returned as data regardless, so a test asserts them and never this.
fn report_node_store_once(notices: &[String]) {
    static SAID: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if notices.is_empty() || !tracing::dispatcher::has_been_set() {
        return;
    }
    if SAID.swap(true, std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    for n in notices {
        tracing::warn!("node keys: {n}");
    }
}

#[path = "active_addr_tests.rs"]
#[cfg(test)]
mod active_addr_tests;
