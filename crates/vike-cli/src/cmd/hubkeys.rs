//! Where the two vike-tradehub NODE KEYS come from — the ONE resolver both order-write surfaces
//! (`trade`, `mcp`) authenticate through.
//!
//! # The bug this exists to close
//!
//! `vike-tradehub` — the daemon on the other end of the socket — reads its HMAC hub keys from the
//! CREDENTIAL STORE (`auth::from_vars(&workspace_credentials())`). `vike-cli trade` and
//! `vike-cli mcp` read the same two names from `std::env::var` and nothing else. So on a box
//! configured exactly as the daemon documents — both keys in the store, `vike-cli secrets list`
//! confirming them — the client answered:
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
//! ⚠ **That paragraph is HISTORY, and the store has since moved.** A hub key lives in the settings
//! database's `hub_key` table now (`docs/decisions/0051-node-keys-live-in-their-own-store.md`),
//! and both halves resolve it through the one read — `vike_secrets::CredentialStore::hub_keys`.
//! What did
//! NOT change is the property this module exists for: the client and the daemon consult the same
//! store, and this module's messages name it. The caller — `crate::hub_key_store` — is what opens
//! it; every path below is pure.
//!
//! # One source: the store, and nothing else
//!
//! The two keys are read from the settings database's `hub_key` table and from nowhere else. There
//! is no environment rung: the owner ruled on 2026-10-31 that every key lives ONLY as a row of
//! `<project>/settings/db/vike.db`, and
//! `docs/decisions/0111-no-setting-lives-in-the-environment-or-a-toml-file.md` (verdict 5, amended)
//! records it. Both names are `vike_config::REMOVED_ENV` rows, so a process that still exports
//! either one is refused at startup with the remedy, never read and never silently ignored. The
//! CONTROL key places orders; a shell variable that shadows the stored one (the old "an `export` is
//! narrow and deliberate" precedence) is exactly the second source the ruling closed.
//!
//! # Everything here is PURE
//!
//! The map arrives as a parameter: the composition root ([`crate::run`]) owns the ONE credential-store
//! read, exactly as the settings registry requires of a library. That is also why this module
//! replaced the two `std::env::var` call sites it inherited — they were `Layer::Library` rows on
//! `LIBRARY_PIN`, and they are now `Layer::Injected` map lookups.
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

/// The observe (READ) key's NAME — the row it is stored under in the hub-key store. (The `_ENV`
/// suffix is the name's history: it is not a variable any process reads, and a process that sets
/// it is refused at startup.)
pub const OBSERVE_KEY_ENV: &str = "VIKE_TRADEHUB_OBSERVE_KEY";
/// The control (WRITE) key's NAME — the row it is stored under in the hub-key store; see
/// [`OBSERVE_KEY_ENV`].
pub const CONTROL_KEY_ENV: &str = "VIKE_TRADEHUB_CONTROL_KEY";

/// Where a resolved key came from — printed on the connection line so an operator debugging a
/// rejected handshake can see WHICH key was presented without echoing it.
///
/// One value since 2026-10-31: the store is the only source. The type stays so every caller that
/// destructures `(key, origin)` and every connection line keeps its shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyOrigin {
    /// The settings database's `hub_key` table — read through
    /// `vike_secrets::CredentialStore::hub_keys`, the same read the daemon makes.
    Store,
}

impl KeyOrigin {
    /// A short human tag for the connection line.
    pub fn label(self) -> &'static str {
        match self {
            KeyOrigin::Store => "hub-key store",
        }
    }
}

/// The two hub keys as this invocation resolved them, plus the store that was consulted (so the
/// "no key anywhere" message can NAME it — the diagnostic the old message lacked).
///
/// ⚠ Manual redacting [`std::fmt::Debug`] below: these are HMAC signing keys and must never reach a
/// log line, a panic message or a `{:?}`. Mirrors `vike_hub_proto::auth::HubKeys` and
/// `vike_secrets::credentials::Credentials`.
#[derive(Clone, Default)]
pub struct HubKeyring {
    observe: Option<(String, KeyOrigin)>,
    control: Option<(String, KeyOrigin)>,
    /// The hub-key store this invocation would have read, whether or not it exists. `None` only
    /// when no project sits above the working directory.
    store: Option<PathBuf>,
    /// This box's patience on a bad node link (the two `preferences.*` rows, resolved by the boot
    /// walk). Carried HERE because the keyring is what every node-facing verb already holds when it
    /// dials, so each connect takes its window from the same hand that supplies its key; the
    /// default is the window every dial had before the rows existed, which is what a keyring built
    /// without the boot (a test, `HubKeyring::default()`) dials with.
    patience: vike_tradehub_client::liveness::LinkPatience,
}

impl HubKeyring {
    /// The same keyring with the box's resolved link patience attached — the one thing
    /// [`resolve`] cannot know (it takes the store's map, not the settings).
    #[must_use]
    pub fn with_patience(mut self, patience: vike_tradehub_client::liveness::LinkPatience) -> Self {
        self.patience = patience;
        self
    }

    /// The windows a connect from this keyring waits in: the observe stream's silence limit for
    /// `RemoteCoreHandle::connect_with_read_timeout` and a control reply's wait for
    /// `RemoteControlHandle::connect_with_reply_timeout`.
    pub fn patience(&self) -> vike_tradehub_client::liveness::LinkPatience {
        self.patience
    }

    /// The observe (READ) key and where it came from, or `None` when the store had none.
    pub fn observe(&self) -> Option<(&str, KeyOrigin)> {
        self.observe.as_ref().map(|(k, o)| (k.as_str(), *o))
    }

    /// The control (WRITE) key and where it came from, or `None` when the store had none.
    pub fn control(&self) -> Option<(&str, KeyOrigin)> {
        self.control.as_ref().map(|(k, o)| (k.as_str(), *o))
    }

    /// True when at least one key resolved — i.e. there is something this session can do.
    pub fn has_any(&self) -> bool {
        self.observe.is_some() || self.control.is_some()
    }

    /// The startup error for "neither key resolved". It NAMES the store, including its path and
    /// whether that file is even there, because the whole defect was a client that looked in one
    /// place and reported it as if it had looked everywhere.
    ///
    /// `surface` is the command reporting it (`trade` / `mcp`) so the first line reads as that
    /// command's own error.
    pub fn absent_message(&self, surface: &str) -> String {
        let store = self.store_line("present — but neither key is in it");
        format!(
            "vike-cli {surface}: no vike-tradehub hub key found — nothing to do.\n\
             looked for {OBSERVE_KEY_ENV} (read) and {CONTROL_KEY_ENV} (write) in:\n\
             {store}\n\
             store at least one to observe/control the node (a key is a row there and nowhere \
             else) — `vike-cli secrets path` prints the store's location."
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
             {store}{control_note}\n\
             `vike-cli secrets path` prints the store's location."
        )
    }

    /// The credential-store line both absent-key messages share: the path and whether the file is
    /// even there (`missing` is the present-but-lacking wording, which differs by surface).
    fn store_line(&self, missing: &str) -> String {
        match &self.store {
            Some(p) => format!(
                "  the hub-key store {} ({})",
                p.display(),
                if p.exists() { missing } else { "absent" }
            ),
            None => "  the hub-key store — NOT resolved (no project above the working \
                     directory; `cd` into the project, or set VIKE_SETTINGS_DIR)"
                .to_string(),
        }
    }
}

impl std::fmt::Debug for HubKeyring {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // NEVER the key bytes — only presence and provenance.
        let tag = |k: &Option<(String, KeyOrigin)>| match k {
            Some((_, o)) => o.label(),
            None => "absent",
        };
        f.debug_struct("HubKeyring")
            .field("observe", &tag(&self.observe))
            .field("control", &tag(&self.control))
            .field("store", &self.store)
            .field("patience", &self.patience)
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

/// Resolve both hub keys from the hub-key store — PURE, the store's map supplied by the caller.
///
/// Per-key, independently; a blank-after-trim value is treated as absent. `store_path` is carried
/// through only so [`HubKeyring::absent_message`] can name it.
pub fn resolve(store: &HashMap<String, String>, store_path: Option<&Path>) -> HubKeyring {
    let pick = |name: &str| -> Option<(String, KeyOrigin)> {
        store
            .get(name)
            .map(|v| v.trim())
            .filter(|v| !v.is_empty())
            .map(|k| (k.to_string(), KeyOrigin::Store))
    };
    HubKeyring {
        observe: pick(OBSERVE_KEY_ENV),
        control: pick(CONTROL_KEY_ENV),
        store: store_path.map(Path::to_path_buf),
        patience: vike_tradehub_client::liveness::LinkPatience::default(),
    }
}

#[path = "hubkeys_tests.rs"]
#[cfg(test)]
mod hubkeys_tests;
