//! **Phase: the hub keys** — the ONE place this daemon reads the keys its clients authenticate
//! with. Always keyed: no keys, no start (the wire design's A.2; decision 0164, verdict 6). A daemon
//! that holds every venue connection and, in a later wave, the order path must not offer an
//! unauthenticated mode, so [`gateway_keys`] has no "absent is fine" arm and the wire hook
//! (`super::wire::serve_wire`) refuses to go on past an `Err`.
//!
//! # The read
//!
//! `store.hub_keys(vike_venues::credential_keys::is_gatewayhub_daemon_key)`: the `hub_key` table of
//! the settings database the boot's ONE project walk answered (`vike_boot::Booted::store`), never the
//! venue credential namespace, narrowed to THIS service's FAMILY. `CredentialStore::hub_keys` hands
//! back only the names its predicate admits, so the predicate IS the scope this process holds: the
//! tradehub's pair and the datahub's own pair are never materialised here, and a key is never shared
//! across services. (`is_platform_key`, the whole table, is not an alternative: it would put two other
//! services' keys in a process that holds every venue connection.) The datahub dials this daemon with
//! the SAME two names as a client, through its own predicate
//! (`crates/vike-datahub/src/datahub_cli/gateway.rs`'s `client_keys`).
//!
//! * **The store's own findings are reported** (`Resolved::report`): a group- or world-readable
//!   database is logged here, once per process, and not read in silence.
//! * **A store that EXISTS and cannot be READ is an ERROR, never "no keys configured".** It is logged
//!   at `error` with the cause and refused under its own words ([`KEYS_STORE_UNREADABLE`]): a
//!   permissions bug must not wear the unconfigured answer, because `Restart=on-failure` would repeat
//!   the wrong diagnosis forever. This is `crates/vike-datahub/src/datahub_cli.rs`'s `read_hub_keys`
//!   discipline, with the refusal the datahub turns into a flag made the return value (this daemon has
//!   no unauthenticated arm to fall back to).
//! * **An ABSENT store, or a store without the pair, is [`KEYS_NOT_PROVISIONED`]**, and the words name
//!   the remedy: `vike-cli gatewayhub setup` mints the pair into the project the daemon runs from.
//!
//! # The construction
//!
//! [`keys_from_map`] is the gatewayhub twin of `vike_hub_proto::auth::hub_keys_from_vars`: the same
//! canonical constructor, `HubKeys::from_vars_named`, over the two names this service owns
//! (`vike_venues::credential_keys::gatewayhub_key_pair`: observe first, control second, read out of the
//! table by that one accessor exactly as `vike-cli gatewayhub setup` reads them). The observe key signs the `Read`
//! scope and the control key the `Write` scope; the `Account` key stays EMPTY, so that scope is refused
//! structurally. A single key is enough to start (the other scope is then refused at the handshake and
//! the ready line says which key is `absent`); NEITHER key is a refusal.
//!
//! ⚠ **The accessor is not trusted, the family filter is the guard.** Were `PLATFORM_KEYS` reordered, the
//! names `gatewayhub_key_pair` returned would not be admitted by the daemon predicate, so the map would not hold them
//! and the daemon would refuse to start rather than serve under another service's name.
//! `the_pair_names_are_the_gatewayhubs_and_classified_as_such` holds the accessor to the classifier.
//!
//! # The hub-key store gate
//!
//! The read above is a probe row and the construction a declared row of
//! `crates/vike-ops/tests/settings_secrets/hub_key_store_gate/probe_rule.rs`'s `PROBE_SITES` and
//! `crates/vike-ops/tests/settings_secrets/hub_key_store_gate/construction_rule.rs`'s
//! `DECLARED_SITES`; the argument of the construction below (`credentials, observe, control`) is the
//! text that row matches, so it is not renamed in passing.

use std::collections::HashMap;

use vike_hub_proto::auth::HubKeys;

/// What [`gateway_keys`] answers when the store holds none of the daemon's keys. A constant so the
/// tests that pin the refusal and the line an operator greps for are one string; the full refusal
/// continues after it with the names and the remedy.
pub(super) const KEYS_NOT_PROVISIONED: &str = "the gatewayhub hub keys are not provisioned";

/// What [`gateway_keys`] answers when the hub-key store exists and would not read: a different
/// refusal from [`KEYS_NOT_PROVISIONED`] on purpose, because the remedy is different (fix the store,
/// do not mint a pair over it).
pub(super) const KEYS_STORE_UNREADABLE: &str =
    "the hub-key store is PRESENT but UNREADABLE, so the gatewayhub hub keys in it were NOT loaded";

/// The hub keys this daemon verifies its clients with, read from the NODE store the boot opened
/// (`store`: `vike_boot::Booted::store`, over the boot's one project walk) — never the venue
/// credential namespace. See the module doc for the scope, the findings and the two refusals.
///
/// # Errors
/// The words to log and exit 2 with when there are no keys to serve under: [`KEYS_NOT_PROVISIONED`]
/// (the store holds neither key) or [`KEYS_STORE_UNREADABLE`] (it could not be read at all).
pub(super) fn gateway_keys(store: &vike_secrets::CredentialStore) -> Result<HubKeys, String> {
    let credentials: HashMap<String, String> =
        match store.hub_keys(vike_venues::credential_keys::is_gatewayhub_daemon_key) {
            Ok(resolved) => {
                // The store's own findings: the reads return them as DATA and
                // `vike_secrets::Resolved::report` is the one place they become log lines.
                resolved.report();
                resolved.secrets.into_map()
            }
            Err(e) => {
                tracing::error!(
                    error = %e,
                    "vike-gatewayhub: hub-key store PRESENT but UNREADABLE — any configured \
                     gatewayhub hub keys were NOT loaded. This is NOT the same as no keys being \
                     provisioned: fix this store's permissions or contents and restart. The \
                     daemon is always keyed, so it will now refuse to start"
                );
                return Err(KEYS_STORE_UNREADABLE.to_string());
            }
        };
    keys_from_map(&credentials).ok_or_else(|| {
        let [observe, control] = pair_names();
        format!(
            "{KEYS_NOT_PROVISIONED}: this box's hub-key store holds neither {observe} nor \
             {control}. Mint the pair with `vike-cli gatewayhub setup`, as the user and from the \
             project folder this daemon runs in"
        )
    })
}

/// The gatewayhub twin of `vike_hub_proto::auth::hub_keys_from_vars`: the keys of THIS service's
/// pair out of the map the daemon-family read returned, `None` when neither is there.
fn keys_from_map(credentials: &HashMap<String, String>) -> Option<HubKeys> {
    let [observe, control] = pair_names();
    HubKeys::from_vars_named(credentials, observe, control)
}

/// The pair's two names, observe first, out of the one table that classifies them. Not spelled
/// here: a hand copy of a hub-key name is the gap `vike_venues::credential_keys::PLATFORM_KEYS`
/// exists to close (and a whole `VIKE_..._KEY` literal in this crate would read to the settings
/// registry's harvest as a read of that variable).
pub(super) fn pair_names() -> [&'static str; 2] {
    vike_venues::credential_keys::gatewayhub_key_pair()
}
