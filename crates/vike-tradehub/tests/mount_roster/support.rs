//! What the moved tests had from `vike-mount`'s own `#[cfg(test)]` helpers, which no other crate
//! can reach, plus the one-line map builder several of them repeated.
#![allow(dead_code)]

use std::collections::HashMap;

use vike_mount::{MountPolicy, VenueMode, VenuePolicy};

/// A policy that arms ONE venue at `mode` — the ceiling most of these tests must open, or the
/// mount returns at its paper early return and the gate under test is never reached.
pub fn armed_policy(venue: &str, mode: VenueMode) -> MountPolicy {
    MountPolicy { venues: VenuePolicy::default().declare(venue, mode), ..MountPolicy::default() }
}

/// A policy that arms the WHOLE roster at `mode` — for the tests whose subject is the OTHER gate
/// (absent credentials), which the ceiling would otherwise short-circuit.
pub fn all_armed_policy(mode: VenueMode) -> MountPolicy {
    MountPolicy {
        venues: vike_model::VENUES.iter().fold(VenuePolicy::default(), |p, v| p.declare(v, mode)),
        ..MountPolicy::default()
    }
}

/// A credential map from `(key, value)` pairs.
pub fn vars(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect()
}
