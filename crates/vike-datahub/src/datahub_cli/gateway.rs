//! The datahub's DIAL to the gatewayhub: where it is and how this process proves who it is.
//!
//! Decision 0164: the datahub opens no venue connection of its own. Every lane that used to name a
//! bridge (the broker's feed clients, backfill, the catalog, the options poll) asks the one daemon
//! that owns the venue connections, through `vike-gatewayhub-client`. The client crate reads no
//! store and no setting, so THIS root builds the one value they all dial from, a
//! [`GatewayLink`], and hands it down as a parameter.
//!
//! The address is the `config.gatewayhub_addr` row, defaulting to
//! [`vike_config::config::DEFAULT_GATEWAYHUB_ADDR`] (loopback: the daemon refuses anything else).
//!
//! # The keys: this daemon's gatewayhub CLIENT pair
//!
//! The link signs with the gatewayhub's `Observe` and `Control` keys (`Scope::Read` and
//! `Scope::Write` on the wire), the two names `vike-cli gatewayhub setup` mints into the
//! gatewayhub's hub-key store (`crates/vike-gatewayhub/src/gatewayhub_cli/keys.rs`'s
//! `gateway_keys` is the server's side of the same two names). They are read HERE, from the NODE
//! store the boot opened (`vike_boot::Booted::store`, over the boot's one project walk) and never
//! from the venue credential namespace, through `vike_venues::credential_keys`'s
//! `is_datahub_gatewayhub_client_key`: the datahub holds the CLIENT side of those two scopes and
//! nothing wider, so the order-scope key a later wave adds to the gatewayhub's family is not this
//! process's by that filing, and the link carries no `Account` key either.
//!
//! ⚠ **Nothing here falls back to the datahub's OWN keys** (the pair this daemon serves its own
//! clients with, read by `datahub_cli.rs`'s `read_hub_keys` through `is_datahub_pair_key`): a key is
//! never shared across services, so a leak of one service's pair opens nothing on the other, and the
//! two predicates admit disjoint names. This file names neither the datahub's predicate nor its
//! constants (`the_link_never_reaches_for_the_datahubs_own_pair` holds that).
//!
//! ⚠ **A store that EXISTS and cannot be READ is an ERROR, never "no keys".** It is logged at
//! `error` naming the consequence, exactly as `read_hub_keys` does. The link then carries no key
//! either way, and that is the DEGRADED state, not a crash: [`GatewayLink::can_sign`] answers false
//! for the scope without a key, every remote call names the missing key instead of dialling
//! unsigned, and a box that has not provisioned the pair (or has only one of the two) loses exactly
//! the lanes that need the missing scope, per call. Provisioning is one verb on the gatewayhub's
//! box (`vike-cli gatewayhub setup`), and `vike-cli secrets copy-hub-keys --only gatewayhub` brings
//! the SAME pair to a datahub that runs from another project folder.
//!
//! The hub-key store gate holds the two reads of this file: the probe is a row of
//! `crates/vike-ops/tests/settings_secrets/hub_key_store_gate/probe_rule.rs`'s `PROBE_SITES` and the
//! construction a row of `crates/vike-ops/tests/settings_secrets/hub_key_store_gate/construction_rule.rs`'s
//! `DECLARED_SITES`.

use std::collections::HashMap;

use vike_gatewayhub_client::client::GatewayLink;
use vike_hub_proto::auth::{HubKeys, Scope};

/// The link every remote lane of this process dials from, built once by [`super::run`] from the
/// boot's settings and the boot's hub-key store (`store`: `vike_boot::Booted::store`).
///
/// It dials nothing and builds no venue client: the address is a string off the settings, the keys
/// are the bytes of the datahub's gatewayhub client pair (possibly none, see the module doc), and the
/// timings are the client crate's own. The pair's PRESENCE is logged, never a value.
pub(super) fn gateway_link(
    settings: &vike_config::Settings,
    store: &vike_secrets::CredentialStore,
) -> GatewayLink {
    let addr = settings
        .config
        .gatewayhub_addr
        .clone()
        .unwrap_or_else(|| vike_config::config::DEFAULT_GATEWAYHUB_ADDR.to_string());
    let ClientKeys { keys, store_unreadable } = client_keys(store);
    let link = GatewayLink::new(addr, keys);
    let [observe, control] = pair_names();
    if link.can_sign(Scope::Read) || link.can_sign(Scope::Write) {
        tracing::info!("vike-datahub: gatewayhub link: {link:?}");
    } else if store_unreadable {
        // `client_keys` already said, at `error`, that the store could not be read and that this is NOT
        // the pair being absent; saying "absent, mint the pair" now would send an operator to create
        // keys they may already own (and a second `gatewayhub setup` would not fix a store this
        // process cannot read).
        tracing::debug!(
            "vike-datahub: gatewayhub link: no key loaded because the hub-key store is unreadable \
             (see the error above); every call to {} is refused locally",
            link.addr()
        );
    } else {
        tracing::warn!(
            "vike-datahub: no gatewayhub client key ({observe} / {control}) in this box's hub-key \
             store, so every call to the gatewayhub at {} is refused locally for want of a key and \
             no venue lane works. Mint the pair on the gatewayhub's box with `vike-cli gatewayhub \
             setup`; a datahub in another project folder takes the SAME pair with `vike-cli secrets \
             copy-hub-keys --only gatewayhub`",
            link.addr()
        );
    }
    link
}

/// What [`client_keys`] read: the pair (possibly none) and WHY there may be none.
struct ClientKeys {
    keys: HubKeys,
    /// The store EXISTS and could not be read (already logged at `error`). Distinct from the pair being
    /// absent, and the caller must not say "absent" after it.
    store_unreadable: bool,
}

/// The datahub's gatewayhub CLIENT pair, from the NODE store: the observe key for `Scope::Read` and
/// the control key for `Scope::Write`, `Account` left empty. An unreadable store is an `error!`, gives
/// no keys and says so in the result; an absent one gives none silently (the caller says so once). See
/// the module doc.
fn client_keys(store: &vike_secrets::CredentialStore) -> ClientKeys {
    let mut store_unreadable = false;
    let credentials: HashMap<String, String> =
        match store.hub_keys(vike_venues::credential_keys::is_datahub_gatewayhub_client_key) {
            Ok(resolved) => {
                // The store's own findings. The reads return them as DATA and
                // `vike_secrets::Resolved::report` is the one place they become log lines, which
                // warns once per distinct finding per process (`read_hub_keys` reports the same
                // store's finding and the two do not double it).
                resolved.report();
                resolved.secrets.into_map()
            }
            Err(e) => {
                store_unreadable = true;
                tracing::error!(
                    error = %e,
                    "vike-datahub: hub-key store PRESENT but UNREADABLE — the gatewayhub client \
                     keys were NOT loaded. This is NOT the same as the pair being absent: every \
                     call to the gatewayhub is about to be refused locally for want of a key. Fix \
                     this store's permissions or contents and restart"
                );
                Default::default()
            }
        };
    let keys = keys_from_map(&credentials).unwrap_or_else(|| HubKeys::new(Vec::new(), Vec::new()));
    ClientKeys { keys, store_unreadable }
}

/// The client pair out of the map the read returned, `None` when neither key is there: the
/// gatewayhub twin of `vike_hub_proto::auth::hub_keys_from_vars` (which reads the datahub's OWN
/// two names and is deliberately not called here).
fn keys_from_map(credentials: &HashMap<String, String>) -> Option<HubKeys> {
    let [observe, control] = pair_names();
    HubKeys::from_vars_named(credentials, observe, control)
}

/// The pair's two names, observe first, out of the one table that classifies them
/// (`vike_venues::credential_keys::gatewayhub_key_pair`, read through that one accessor exactly as
/// `vike-cli gatewayhub setup` and the gatewayhub's own read do). Not spelled here: a hand copy of a
/// hub-key name is the gap that table exists to close.
///
/// ⚠ The accessor is not trusted, the predicate is the guard: were the table reordered, the names
/// `gatewayhub_key_pair` returned would not be admitted by `is_datahub_gatewayhub_client_key`, the map would not hold
/// them, and the link would carry no key rather than another service's.
fn pair_names() -> [&'static str; 2] {
    vike_venues::credential_keys::gatewayhub_key_pair()
}

#[path = "gateway_tests.rs"]
#[cfg(test)]
mod gateway_tests;
