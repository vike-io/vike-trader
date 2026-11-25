//! What the CLI's and the MCP server's tradehub dials sign with — the ONE resolver both order-write surfaces
//! (`trade`, `mcp`) authenticate through, and the Telegram channel beside them.
//!
//! # A device key, never a shared pair (decision 0288 item 8)
//!
//! A hub admits a PERSON by its access list alone: each device signs `PersonAuth` with its own Ed25519 key, and the
//! node's list says which role that key holds. The shipped binaries read NO shared tradehub pair any more
//! (`VIKE_TRADEHUB_OBSERVE_KEY` / `VIKE_TRADEHUB_CONTROL_KEY` simply stop being read; a row left in `hub_key` is
//! dead). A [`HubKeyring`] built for a dial ([`crate::resolved::hub_keyring`]) holds THIS box's device key, the
//! `device_key` row `vike_secrets::access::device_key_load` answers (`owner/server` on a server, the desktop's on a
//! PC), or a daemon's own named one ([`crate::resolved::hub_keyring_for`]: the Telegram channel's
//! `owner/telegram`). The one key serves both halves: the node's role decides what it reaches.
//!
//! # The tests' seam
//!
//! [`resolve`] builds a keyring over SHARED key bytes from a map. No production path calls it: it exists for the
//! scripted fake nodes of this workspace's suites, which verify an HMAC `Auth`. Its two map names are
//! [`OBSERVE_KEY_ENV`] and [`CONTROL_KEY_ENV`].
//!
//! # Everything here is PURE
//!
//! The device key arrives as a value: the composition root (`vike_cli::run`) owns the ONE store read, exactly as
//! the settings registry requires of a library.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use vike_hub_protocol::access::{DeviceKey, HubSigner, Signer};

use crate::refusals::DeviceKeyRefusal;

/// The tests' seam's map name for the observe (READ) key bytes ([`resolve`]). No production path reads it.
pub const OBSERVE_KEY_ENV: &str = "VIKE_TRADEHUB_OBSERVE_KEY";
/// The tests' seam's map name for the control (WRITE) key bytes; see [`OBSERVE_KEY_ENV`].
pub const CONTROL_KEY_ENV: &str = "VIKE_TRADEHUB_CONTROL_KEY";

/// How every message names what a dial signs with ("no device key", "this box's device key").
pub const DEVICE_KEY_WORDS: &str = "device key";

/// Where a resolved key came from — printed on the connection line so an operator debugging a
/// rejected handshake can see WHICH key was presented without echoing it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyOrigin {
    /// This box's (or this daemon's) own device key, a `device_key` row of its settings database.
    Device,
    /// Shared key bytes, from the tests' seam [`resolve`] alone.
    Store,
}

impl KeyOrigin {
    /// A short human tag for the connection line.
    pub fn label(self) -> &'static str {
        match self {
            KeyOrigin::Device => "device key",
            KeyOrigin::Store => "hub-key store",
        }
    }
}

/// What this invocation's tradehub dials sign with, plus the store that was consulted (so the "no key" message can
/// NAME it).
///
/// ⚠ Manual redacting [`std::fmt::Debug`] below: a private key or a shared key must never reach a log line, a panic
/// message or a `{:?}`.
#[derive(Clone, Default)]
pub struct HubKeyring {
    observe: Option<(HubSigner, KeyOrigin)>,
    control: Option<(HubSigner, KeyOrigin)>,
    /// The settings database this invocation read, whether or not it exists. `None` only when no project sits above
    /// the working directory.
    store: Option<PathBuf>,
    /// This box's patience on a bad node link (the two `preferences.*` rows, resolved by the boot
    /// walk). Carried HERE because the keyring is what every node-facing verb already holds when it
    /// dials, so each connect takes its window from the same hand that supplies its key; the
    /// default is the window every dial had before the rows existed, which is what a keyring built
    /// without the boot (a test, `HubKeyring::default()`) dials with.
    patience: vike_tradehub_client::liveness::LinkPatience,
    /// This box's `config.node_addr` row, the middle rung of every `--node`
    /// (`crate::resolved::node_dial_address`). Carried here for [`Self::patience`]'s reason; a keyring built
    /// without the boot (a test, `HubKeyring::default()`) holds none.
    node_addr: Option<String>,
}

impl HubKeyring {
    /// A keyring over a device key (or none), naming `store` in its messages.
    pub fn of_device(device: Option<DeviceKey>, store: Option<&Path>) -> HubKeyring {
        let held = device.map(|key| (HubSigner::Device(key), KeyOrigin::Device));
        HubKeyring {
            observe: held.clone(),
            control: held,
            store: store.map(Path::to_path_buf),
            patience: vike_tradehub_client::liveness::LinkPatience::default(),
            node_addr: None,
        }
    }

    /// The same keyring with the box's resolved link patience attached — the one thing
    /// the key read cannot know (it takes the store's row, not the settings).
    #[must_use]
    pub fn with_patience(mut self, patience: vike_tradehub_client::liveness::LinkPatience) -> Self {
        self.patience = patience;
        self
    }

    /// The windows a connect from this keyring waits in: the observe stream's silence limit for
    /// `TradehubCoreHandle::connect_with_read_timeout` and a control reply's wait for
    /// `TradehubControlHandle::connect_with_reply_timeout`.
    pub fn patience(&self) -> vike_tradehub_client::liveness::LinkPatience {
        self.patience
    }

    /// The same keyring with this box's `config.node_addr` row attached.
    #[must_use]
    pub fn with_node_addr(mut self, node_addr: Option<&str>) -> Self {
        self.node_addr = node_addr.map(str::to_string);
        self
    }

    /// This box's `config.node_addr` row: the rung a dial takes when no `--node` names one.
    pub fn node_addr(&self) -> Option<&str> {
        self.node_addr.as_deref()
    }

    /// What an observe (READ) dial signs with and where it came from, or `None` when there is nothing.
    pub fn observe(&self) -> Option<(Signer<'_>, KeyOrigin)> {
        self.observe.as_ref().map(|(k, o)| (k.into(), *o))
    }

    /// What a control (WRITE) dial signs with and where it came from, or `None` when there is nothing.
    pub fn control(&self) -> Option<(Signer<'_>, KeyOrigin)> {
        self.control.as_ref().map(|(k, o)| (k.into(), *o))
    }

    /// Whether a settings directory resolved, so there was a store to read at all. `false`
    /// means a missing key says nothing about any store.
    pub fn store_resolved(&self) -> bool {
        self.store.is_some()
    }

    /// True when something resolved — i.e. there is something this session can do.
    pub fn has_any(&self) -> bool {
        self.observe.is_some() || self.control.is_some()
    }

    /// **THE answer to "nothing to sign with"**, every door's (`vike-cli` and the Telegram channel open it with
    /// their own line, `vike-mcp` answers it whole): no settings database was resolved at all (the key may well be
    /// there), or the one resolved, named with its path and whether the file is there, holds no device key, with
    /// the hub protocol's one cure for where `node` is (`None`: no node named, read as this box's own hubs).
    pub fn absent_refusal(&self, node: Option<&str>) -> DeviceKeyRefusal {
        use vike_hub_protocol::conn::{names_this_box, no_device_key_cure};
        match &self.store {
            None => DeviceKeyRefusal::NoSettingsDirectory,
            Some(p) => DeviceKeyRefusal::NotInStore {
                store: p.display().to_string(),
                state: if p.exists() {
                    "present, but it holds no `device_key` row"
                } else {
                    "absent"
                },
                cure: no_device_key_cure(node.is_none_or(names_this_box)),
            },
        }
    }

    /// The startup error of a `vike-cli` verb for "nothing to sign with": its opening line, then
    /// [`Self::absent_refusal`]. `surface` is the command reporting it (`trade`) so the first line reads as that
    /// command's own error; `node` is the address it would have dialled, which picks the cure.
    pub fn absent_message(&self, surface: &str, node: &str) -> String {
        format!(
            "vike-cli {surface}: no device key on this box — nothing to do.\n{}",
            self.absent_refusal(Some(node)).rendered()
        )
    }

    /// [`Self::absent_message`] for a READ-ONLY verb when nothing resolved for the observe half.
    ///
    /// When a control credential DID resolve (the tests' seam can build one alone), it says so and says why it does
    /// not help.
    pub fn observe_absent_message(&self, surface: &str, node: &str) -> String {
        let control_note = if self.control.is_some() {
            "\na control key DID resolve, but it cannot authenticate a read — the node verifies \
             each scope against its own key."
        } else {
            ""
        };
        format!(
            "vike-cli {surface}: no device key on this box — this is a READ verb, and reads \
             authenticate with this box's {DEVICE_KEY_WORDS}.\n{}{control_note}",
            self.absent_refusal(Some(node)).rendered()
        )
    }
}

impl std::fmt::Debug for HubKeyring {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // NEVER a key byte — only presence and provenance.
        let tag = |k: &Option<(HubSigner, KeyOrigin)>| match k {
            Some((_, o)) => o.label(),
            None => "absent",
        };
        f.debug_struct("HubKeyring")
            .field("observe", &tag(&self.observe))
            .field("control", &tag(&self.control))
            .field("store", &self.store)
            .field("patience", &self.patience)
            .field("node_addr", &self.node_addr)
            .finish()
    }
}

/// The stderr lines for an OBSERVE handshake the node refused
/// (`vike_hub_protocol::call::HubCallError::AccessDenied` from a `TradehubCoreHandle::connect` or a
/// one-shot read) — the ONE wording every read verb over the observe half shares. `command` is what
/// the operator typed (`"trade status"`, `"trade order"`), so the first line reads as that verb's own
/// error.
pub fn observe_permission_denied_lines(
    command: &str,
    node: &str,
    err: &vike_hub_protocol::call::HubCallError,
) -> Vec<String> {
    key_refused_lines(&format!("vike-cli {command}"), node, err)
}

/// [`observe_permission_denied_lines`] under any opening `who` (a command, an MCP tool's name).
fn key_refused_lines(
    who: &str,
    node: &str,
    err: &vike_hub_protocol::call::HubCallError,
) -> Vec<String> {
    vec![
        format!("{who}: the node at {node} refused the observe handshake: {err}"),
        format!(
            "this box's {DEVICE_KEY_WORDS} is not on that node's access list, is revoked there, or its role does \
             not admit this — `vike-cli access ls` on the server says which"
        ),
    ]
}

/// **A failed one-shot READ of a node, sorted ONCE for both doors** (`vike-cli`'s `trade status` family and
/// `vike-mcp`'s read tools), on the error's variant (decision 0301). `who` opens the first line (`vike-cli trade
/// status`, the tool's name) and `serves` is what a newer node serves.
///
/// Two sorts are facts about a REACHABLE box, so asking the same node again cannot succeed and the words say so: a
/// node too old for the verb (`HubCallError::NotOffered`: nothing was sent, the client's own sentence naming the
/// capability is kept) and a key it refused ([`observe_permission_denied_lines`]'s words). Anything else names the
/// address: the one sort worth asking again.
pub fn node_read_failure_lines(
    who: &str,
    node: &str,
    serves: &str,
    err: &vike_hub_protocol::call::HubCallError,
) -> Vec<String> {
    use vike_hub_protocol::call::HubCallError;
    match err {
        HubCallError::NotOffered { .. } => vec![
            format!("{who}: {err}"),
            format!(
                "this is a fact about the node, not the request: upgrade the node at {node} to a vike-tradehub \
                 build that serves {serves}; asking this node again cannot succeed"
            ),
        ],
        HubCallError::AccessDenied(_) => key_refused_lines(who, node, err),
        _ => vec![format!("{who}: cannot query the node at {node}: {err}")],
    }
}

/// **The tests' seam**: a keyring over SHARED key bytes from `store`'s [`OBSERVE_KEY_ENV`] / [`CONTROL_KEY_ENV`]
/// entries, for a scripted fake node that verifies an HMAC `Auth`. No production path calls it (decision 0288 item
/// 8: a person signs with its device key alone).
///
/// Per key, independently; a blank-after-trim value is treated as absent, and a value is trimmed.
pub fn resolve(store: &HashMap<String, String>, store_path: Option<&Path>) -> HubKeyring {
    let pick = |name: &str| -> Option<(HubSigner, KeyOrigin)> {
        store
            .get(name)
            .map(|v| v.trim())
            .filter(|v| !v.is_empty())
            .map(|k| (HubSigner::Shared(k.as_bytes().to_vec()), KeyOrigin::Store))
    };
    HubKeyring {
        observe: pick(OBSERVE_KEY_ENV),
        control: pick(CONTROL_KEY_ENV),
        store: store_path.map(Path::to_path_buf),
        patience: vike_tradehub_client::liveness::LinkPatience::default(),
        node_addr: None,
    }
}

#[path = "hubkeys_tests.rs"]
#[cfg(test)]
mod hubkeys_tests;
