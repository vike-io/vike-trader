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
//! back only the names its predicate admits, so the predicate IS the scope this process holds: no
//! other service's key (nor a retired person key an old store still holds, decision 0288) is ever
//! materialised here, and a key is never shared across services. (`is_platform_key`, the whole table, is not an alternative: it would put two other
//! services' keys in a process that holds every venue connection.) The datahub dials this daemon with
//! the SAME two names as a client, through its own predicate
//! (`crates/vike-datahub/src/datahub_cli/gateway.rs`'s `client_keys`).
//!
//! ⚠ **The family is FOUR names since wave 2**: the pair, the `Trade` key (`VIKE_GATEWAYHUB_TRADE_KEY`) and
//! the admin key (`VIKE_GATEWAYHUB_ADMIN_KEY`, named and minted by nothing). The map this read returns
//! holds whichever of them the store has, and [`keys_from_map`] builds the pair's `Read` and `Write` keys
//! and, when the store holds it, the `Trade` key (W9b: the trade surface is served, so the key arms its
//! scope): the `Account` scope stays without a key, and `super::super::server::handshake::scope_is_served`
//! refuses it. Absence of the `Trade` key is the switch: no key, no trade surface. The predicate is named by THIS file alone (the hub-key store gate's `FENCED_PREDICATES`):
//! a second file reading the family would hold order-scope key material in a process that does not
//! verify it.
//!
//! * **The store's own findings are reported** (`Resolved::report`): a group- or world-readable
//!   database is logged here, once per process, and not read in silence.
//! * **A store that EXISTS and cannot be READ is an ERROR, never "no keys configured".** It is logged
//!   at `error` with the cause and refused under its own words ([`GatewayKeysRefusal::StoreUnreadable`]): a
//!   permissions bug must not wear the unconfigured answer, because the parked daemon's one line
//!   would be the wrong diagnosis until somebody looked further. This is `crates/vike-datahub/src/datahub_cli.rs`'s `read_hub_keys`
//!   discipline, with the refusal the datahub turns into a flag made the return value (this daemon has
//!   no unauthenticated arm to fall back to).
//! * **An ABSENT store, or a store without the pair, is [`GatewayKeysRefusal::NotProvisioned`]**, and the words name
//!   the remedy: `vike-cli gatewayhub setup` mints the pair into the project the daemon runs from.
//!
//! # The construction
//!
//! [`keys_from_map`] is the canonical constructor, `HubKeys::from_vars_named`, over the two names this
//! service owns
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

use vike_hub_protocol::auth::HubKeys;

/// Why [`gateway_keys`] has no keys to serve under: two refusals because the remedies differ, and the
/// wire hook decides which remedy it logs by the variant, never by the words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum GatewayKeysRefusal {
    /// The store holds neither key of the pair (or there is no store): setup alone cures it.
    NotProvisioned { observe: &'static str, control: &'static str },
    /// The store exists and would not read: fix the store, do not mint a pair over it.
    StoreUnreadable,
}

impl std::fmt::Display for GatewayKeysRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GatewayKeysRefusal::NotProvisioned { observe, control } => write!(
                f,
                "the gatewayhub hub keys are not provisioned: this box's hub-key store holds neither \
                 {observe} nor {control}. Mint the pair with `vike-cli gatewayhub setup`, as the user \
                 and from the project folder this daemon runs in"
            ),
            GatewayKeysRefusal::StoreUnreadable => f.write_str(
                "the hub-key store is PRESENT but UNREADABLE, so the gatewayhub hub keys in it were \
                 NOT loaded",
            ),
        }
    }
}

/// The hub keys this daemon verifies its clients with, read from the NODE store the boot opened
/// (`store`: `vike_boot::Booted::store`, over the boot's one project walk) — never the venue
/// credential namespace. See the module doc for the scope, the findings and the two refusals.
///
/// # Errors
/// Why there are no keys to serve under, logged before the daemon exits
/// `vike_utility::cli::OPERATOR_NEEDED_EXIT` (parked, never restarted): [`GatewayKeysRefusal::NotProvisioned`]
/// (the store holds neither key) or [`GatewayKeysRefusal::StoreUnreadable`] (it could not be read at all).
pub(super) fn gateway_keys(
    store: &vike_secrets::CredentialStore,
) -> Result<HubKeys, GatewayKeysRefusal> {
    let credentials: HashMap<String, String> =
        match store.hub_keys(vike_venues::credential_keys::is_gatewayhub_daemon_key) {
            Ok(resolved) => {
                // The store's own findings: the reads return them as DATA and
                // `vike_secrets::CredentialRead::report` is the one place they become log lines.
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
                return Err(GatewayKeysRefusal::StoreUnreadable);
            }
        };
    keys_from_map(&credentials).ok_or_else(|| {
        let [observe, control] = pair_names();
        GatewayKeysRefusal::NotProvisioned { observe, control }
    })
}

/// The keys of THIS service's pair out of the map the daemon-family read returned, `None` when neither
/// is there.
fn keys_from_map(credentials: &HashMap<String, String>) -> Option<HubKeys> {
    let [observe, control] = pair_names();
    let keys = HubKeys::from_vars_named(credentials, observe, control)?;
    // The ORDER-scope key arms `Scope::Trade` when the store holds it (W9b): absence of the key is the
    // switch, so a box that never minted it serves no trade surface and advertises none.
    Some(match trade_key_from(credentials) {
        Some(trade) => keys.with_trade(trade),
        None => keys,
    })
}

/// The `Trade` key out of the family map, `None` when the store does not hold it (or holds it blank).
fn trade_key_from(credentials: &HashMap<String, String>) -> Option<Vec<u8>> {
    let value = credentials.get(vike_venues::credential_keys::gatewayhub_trade_key())?.trim();
    if value.is_empty() { None } else { Some(value.as_bytes().to_vec()) }
}

/// The pair's two names, observe first, out of the one table that classifies them. Not spelled
/// here: a hand copy of a hub-key name is the gap `vike_venues::credential_keys::PLATFORM_KEYS`
/// exists to close (and a whole `VIKE_..._KEY` literal in this crate would read to the settings
/// registry's harvest as a read of that variable).
pub(super) fn pair_names() -> [&'static str; 2] {
    vike_venues::credential_keys::gatewayhub_key_pair()
}
