//! Where the two vike-tradehub NODE KEYS come from — the ONE resolver both order-write surfaces
//! (`trade`, `mcp`) authenticate through.
//!
//! # The bug this exists to close
//!
//! `vike-tradehub` — the daemon on the other end of the socket — reads its HMAC node keys from the
//! CREDENTIAL STORE (`auth::from_vars(&workspace_credentials())`, i.e.
//! `<project>/settings/secrets.env`). `vike-cli trade` and `vike-cli mcp` read the same two names
//! from `std::env::var` and nothing else. So on a box configured exactly as the daemon documents —
//! both keys in the store, `vike-cli secrets list` confirming them — the client answered:
//!
//! ```text
//! vike-cli trade: neither VIKE_TRADEHUB_OBSERVE_KEY nor VIKE_TRADEHUB_CONTROL_KEY is set in the
//! environment — nothing to do
//! ```
//!
//! …and exited. The store is never exported to the process environment (the workspace's
//! `.env`-not-exported gotcha), so the two halves of one feature disagreed about where its
//! credentials live.
//!
//! ⚠ **That paragraph is HISTORY, and the file it names has since moved.** A node key lives in
//! `<project>/settings/node.env` now (`docs/decisions/0051-node-keys-live-in-their-own-store.md`),
//! or in the settings database's `node_key` table once a database exists, and both halves resolve
//! it through the one call — `vike_secrets::resolve_node_keys`, which reads that store and, on a box
//! with no database, falls back to `secrets.env` for a pair that has not moved. What did NOT change
//! is the property this module exists for: the client and the daemon consult the same store, and
//! this module's messages name it. The caller — `crate::node_key_store` — is what opens it; every
//! path below is pure.
//!
//! # Precedence: the PROCESS ENVIRONMENT WINS, the store is the standing default
//!
//! Two candidate orders, and this one is chosen deliberately:
//!
//! - It is STRICTLY ADDITIVE. Every operator who exports the keys today keeps the exact behaviour
//!   they have; the store only answers where the environment was silent, which is precisely the
//!   case that used to fail. A fix that can regress a working setup is a worse fix.
//! - An `export` is EPHEMERAL and NARROW — one shell, one invocation, typed on purpose — while the
//!   store is the box's standing configuration. The narrower, more deliberate, more visible source
//!   winning is the least surprising rule, and it is the one that makes "point at a different node
//!   for one command" possible at all.
//! - It matches how this workspace already treats the two sources elsewhere: operator TOGGLES
//!   (`VIKE_RECONCILE`, `VIKE_TRADEHUB_CONTROL`) are read off the real process env precisely so a
//!   shell export is authoritative for the session.
//!
//! ⚠ This is NOT the `policy` settings case, where an environment override was DELETED because a
//! ceiling any exported variable can raise is not a ceiling. A node key is a CREDENTIAL, not a risk
//! limit: presenting a different key cannot widen what an authenticated peer may do — the node's
//! `Scope`, `ControlLimits` and `RiskGate` are unmoved by it. The worst an exported key achieves is
//! failing the handshake.
//!
//! # Everything here is PURE
//!
//! Both maps arrive as parameters: the composition root ([`crate::run`]) owns the `std::env::vars()`
//! sweep and the ONE credential-store read, exactly as the settings registry requires of a library.
//! That is also why this module replaced the two `std::env::var` call sites it inherited — they were
//! `Layer::Library` rows on `LIBRARY_PIN`, and they are now `Layer::Injected` map lookups.
//!
//! # Trimming is load-bearing, not tidiness
//!
//! `vike_tradehub_client::auth::from_vars` — what the SERVER builds its keys with — trims
//! each value. A client that did not would sign with `"secret\n"` against a server verifying
//! `"secret"`, and an HMAC mismatch surfaces as an opaque `AuthDenied`, not as "your key file has a
//! trailing space". So this trims too, and the two sides agree by construction. (The old
//! `trade.rs` reader did not trim; `mcp.rs` did not even reject an EMPTY value, and would open a
//! connection signing with a zero-length key.)

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// The observe (READ) key's name, in both the process environment and the node-key store.
pub const OBSERVE_KEY_ENV: &str = "VIKE_TRADEHUB_OBSERVE_KEY";
/// The control (WRITE) key's name, in both the process environment and the node-key store.
pub const CONTROL_KEY_ENV: &str = "VIKE_TRADEHUB_CONTROL_KEY";

/// Which of the two sources a resolved key came from — printed on the connection line so an
/// operator debugging a rejected handshake can see WHICH key was presented without echoing it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyOrigin {
    /// A shell `export` / systemd `Environment=` — the narrow, deliberate override.
    ProcessEnv,
    /// `<project>/settings/node.env` (or, on a box that has not migrated, the credential store the
    /// daemon falls back to) — resolved by `vike_secrets::resolve_node_keys`, the same call the
    /// daemon makes.
    Store,
}

impl KeyOrigin {
    /// A short human tag for the connection line.
    pub fn label(self) -> &'static str {
        match self {
            KeyOrigin::ProcessEnv => "process env",
            KeyOrigin::Store => "node-key store",
        }
    }
}

/// The two node keys as this invocation resolved them, plus the store that was consulted (so the
/// "no key anywhere" message can NAME it — the diagnostic the old message lacked).
///
/// ⚠ Manual redacting [`std::fmt::Debug`] below: these are HMAC signing keys and must never reach a
/// log line, a panic message or a `{:?}`. Mirrors `vike_tradehub_client::auth::NodeKeys` and
/// `vike_bridge_core::credentials::Credentials`.
#[derive(Clone, Default)]
pub struct NodeKeyring {
    observe: Option<(String, KeyOrigin)>,
    control: Option<(String, KeyOrigin)>,
    /// The node-key store this invocation would have read, whether or not it exists. `None` only
    /// when no project sits above the working directory.
    store: Option<PathBuf>,
}

impl NodeKeyring {
    /// The observe (READ) key and where it came from, or `None` when neither source had one.
    pub fn observe(&self) -> Option<(&str, KeyOrigin)> {
        self.observe.as_ref().map(|(k, o)| (k.as_str(), *o))
    }

    /// The control (WRITE) key and where it came from, or `None` when neither source had one.
    pub fn control(&self) -> Option<(&str, KeyOrigin)> {
        self.control.as_ref().map(|(k, o)| (k.as_str(), *o))
    }

    /// True when at least one key resolved — i.e. there is something this session can do.
    pub fn has_any(&self) -> bool {
        self.observe.is_some() || self.control.is_some()
    }

    /// The startup error for "neither key resolved". It NAMES both sources, including the store
    /// path and whether that file is even there, because the whole defect was a client that looked
    /// in one place and reported it as if it had looked everywhere.
    ///
    /// `surface` is the command reporting it (`trade` / `mcp`) so the first line reads as that
    /// command's own error.
    pub fn absent_message(&self, surface: &str) -> String {
        let store = self.store_line("present — but neither key is in it");
        format!(
            "vike-cli {surface}: no vike-tradehub node key found — nothing to do.\n\
             looked for {OBSERVE_KEY_ENV} (read) and {CONTROL_KEY_ENV} (write) in:\n\
             \x20 1. the process environment (a shell `export`, a systemd `Environment=`)\n\
             {store}\n\
             set at least one to observe/control the node — \
             `vike-cli secrets path` prints the store's location."
        )
    }

    /// The startup error for a READ-ONLY verb when the OBSERVE key specifically did not resolve.
    ///
    /// [`Self::absent_message`] is the wrong message for that surface twice over: its "no key
    /// found" is FALSE when a control key resolved, and its "set at least one" advice is wrong for
    /// a verb a control key cannot serve — the node verifies each scope against its own key, so
    /// only the observe key opens a read. When a control key DID resolve, this says so and says
    /// why it does not help, because "the key is right there in the store" is exactly what an
    /// operator holding one would otherwise conclude.
    pub fn observe_absent_message(&self, surface: &str) -> String {
        let store = self.store_line("present — but the observe key is not in it");
        let control_note = if self.control.is_some() {
            "\na control key DID resolve, but it cannot authenticate a read — the node verifies \
             each scope against its own key."
        } else {
            ""
        };
        format!(
            "vike-cli {surface}: no observe key found — this is a READ verb, and reads \
             authenticate with {OBSERVE_KEY_ENV}.\nlooked in:\n\
             \x20 1. the process environment (a shell `export`, a systemd `Environment=`)\n\
             {store}{control_note}\n\
             `vike-cli secrets path` prints the store's location."
        )
    }

    /// The credential-store line both absent-key messages share: the path and whether the file is
    /// even there (`missing` is the present-but-lacking wording, which differs by surface).
    fn store_line(&self, missing: &str) -> String {
        match &self.store {
            Some(p) => format!(
                "  2. the node-key store {} ({})",
                p.display(),
                if p.exists() { missing } else { "absent" }
            ),
            None => "  2. the node-key store — NOT resolved (no project above the working \
                     directory; `cd` into the project, or set VIKE_SETTINGS_DIR)"
                .to_string(),
        }
    }
}

impl std::fmt::Debug for NodeKeyring {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // NEVER the key bytes — only presence and provenance.
        let tag = |k: &Option<(String, KeyOrigin)>| match k {
            Some((_, o)) => o.label(),
            None => "absent",
        };
        f.debug_struct("NodeKeyring")
            .field("observe", &tag(&self.observe))
            .field("control", &tag(&self.control))
            .field("store", &self.store)
            .finish()
    }
}

/// The stderr lines for an OBSERVE handshake a resolved key could not pass
/// (`io::ErrorKind::PermissionDenied` from a `RemoteCoreHandle::connect`) — the ONE wording every
/// read verb over the observe half shares, so a per-verb copy of this sentence cannot drift into
/// two. `command` is what the operator typed (`"trade status"`, `"trade order"`), so the first line
/// reads as that verb's own error.
///
/// ⚠ Extracted after `crate::cmd::trade::order`'s `ls` grew a second, near-verbatim copy of
/// `crate::cmd::trade::status`'s wording — both now call this.
pub(crate) fn observe_permission_denied_lines(
    command: &str,
    node: &str,
    err: &std::io::Error,
) -> Vec<String> {
    vec![
        format!("vike-cli {command}: the node at {node} refused the observe handshake: {err}"),
        format!(
            "the presented {OBSERVE_KEY_ENV} does not match that node's — `vike-cli secrets path` \
             prints the store this side read it from"
        ),
    ]
}

/// Resolve both node keys from the process environment and the node-key store — PURE, both maps
/// supplied by the caller.
///
/// Per-key, independently: process env first, store second, blank-after-trim treated as absent in
/// BOTH (a `VIKE_TRADEHUB_CONTROL_KEY=` line configures nothing, and letting an empty export shadow
/// a real stored key would be the precedence rule doing harm). `store_path` is carried through only
/// so [`NodeKeyring::absent_message`] can name it.
pub fn resolve(
    env: &HashMap<String, String>,
    store: &HashMap<String, String>,
    store_path: Option<&Path>,
) -> NodeKeyring {
    let pick = |name: &str| -> Option<(String, KeyOrigin)> {
        let non_blank = |map: &HashMap<String, String>| {
            map.get(name).map(|v| v.trim()).filter(|v| !v.is_empty()).map(str::to_string)
        };
        non_blank(env)
            .map(|k| (k, KeyOrigin::ProcessEnv))
            .or_else(|| non_blank(store).map(|k| (k, KeyOrigin::Store)))
    };
    NodeKeyring {
        observe: pick(OBSERVE_KEY_ENV),
        control: pick(CONTROL_KEY_ENV),
        store: store_path.map(Path::to_path_buf),
    }
}

#[path = "nodekeys_tests.rs"]
#[cfg(test)]
mod nodekeys_tests;
