//! The credential file cannot ARM real money — and the refusal list cannot drift away from the
//! venues that actually have a mainnet switch.
//!
//! `vike_config::arming::CREDENTIAL_FILE_ARMING_REFUSED` names the variables refused in
//! `<project>/settings/secrets.env`; `vike_bridge_core::mainnet::mainnet_switch_for` decides which
//! venues HAVE a `{VENUE}_MAINNET` switch at all. Those two tables live in crates that cannot see
//! each other (vike-config sits below the bridge layer), so this is the only place the mirror can
//! be asserted — vike-mount depends on both, and is also the crate that consumes both on the real
//! path (`cex_mainnet_enabled`, `hyperliquid::hl_env`).
//!
//! Without this gate, adding a venue to the switch table would leave its flag armable from the
//! credential file with nothing failing.

use std::collections::HashMap;

use vike_config::arming::{CREDENTIAL_FILE_ARMING_REFUSED, refuse_credential_file_arming};

fn map(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect()
}

/// EVERY venue with a declared mainnet switch has its flag on the refusal list. A switched venue
/// missing here is a venue a one-line append can still put on mainnet.
#[test]
fn every_switched_venue_flag_is_refused_in_the_credential_file() {
    let refused: Vec<&str> = CREDENTIAL_FILE_ARMING_REFUSED.iter().map(|s| s.var).collect();
    for &venue in vike_model::VENUES {
        if vike_bridge_core::mainnet::mainnet_switch_for(venue).is_some() {
            let flag = format!("{}_MAINNET", venue.to_uppercase());
            assert!(
                refused.contains(&flag.as_str()),
                "{venue} has a declared mainnet switch, so {flag} must be refused in the \
                 credential file — see vike_config::arming"
            );
        }
    }
}

/// ...and nothing is refused for a venue that has NO switch. A stray row would refuse a line that
/// arms nothing, which is the shape that teaches operators to route around the check. (It would
/// also be a lie about the venue's gating: aster, for instance, is gated by credential PRESENCE —
/// which prefix the store holds, `ASTER_LIVE_*` or `ASTER_TESTNET_*` — and there is no
/// `ASTER_MAINNET` anywhere in the tree to refuse.)
#[test]
fn no_mainnet_flag_is_refused_for_a_switchless_venue() {
    for s in CREDENTIAL_FILE_ARMING_REFUSED {
        let Some(venue) = s.var.strip_suffix("_MAINNET") else { continue };
        let venue = venue.to_lowercase();
        assert!(
            vike_bridge_core::mainnet::mainnet_switch_for(&venue).is_some(),
            "{} is refused but {venue} has no declared mainnet switch — either the row is stale \
             or the switch table lost a venue",
            s.var
        );
    }
}

/// The end-to-end property, asserted against the REAL arming fold: a value that
/// `mainnet_for` would ARM from the credential-map source is exactly a value the refusal
/// rejects. The two must agree, or a line arms while the check waves it through.
#[test]
fn anything_the_fold_would_arm_from_the_map_is_refused() {
    for &venue in vike_model::VENUES {
        if vike_bridge_core::mainnet::mainnet_switch_for(venue).is_none() {
            continue;
        }
        let flag = format!("{}_MAINNET", venue.to_uppercase());
        // the map-only source — the exact shape a `secrets.env` append produces (the file is
        // never exported to the process env)
        assert!(
            vike_bridge_core::mainnet::mainnet_for(venue, None, Some("1")),
            "precondition: {venue} arms from a credential-map `1`"
        );
        assert!(
            refuse_credential_file_arming(&map(&[(flag.as_str(), "1")])).is_err(),
            "{flag} arms {venue} from the credential map, so it must be refused there"
        );
    }
}

/// A credential file with real-looking credentials and no arming flags starts normally — the
/// check must not fire on the file's actual job.
#[test]
fn an_ordinary_credential_file_starts() {
    assert!(
        refuse_credential_file_arming(&map(&[
            ("BINANCE_DEMO_API_KEY", "k"),
            ("OKX_DEMO_API_SECRET", "s"),
            ("ASTER_LIVE_USER", "0xabc"),
            ("BYBIT_MAINNET", "0"),
        ]))
        .is_ok()
    );
}
