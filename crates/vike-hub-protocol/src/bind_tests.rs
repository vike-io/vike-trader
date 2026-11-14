//! The address classification, unit-tested once for every server that calls it. Each server's own
//! tests keep the one fact that is theirs: that its DEFAULT address classifies as loopback.
use std::assert_matches;
use std::net::{SocketAddr, ToSocketAddrs};

use super::*;

fn resolve(addr: &str) -> Vec<SocketAddr> {
    addr.to_socket_addrs().map(|it| it.collect()).unwrap_or_default()
}

/// ⚠ The wildcards are the whole point. `0.0.0.0` / `::` are what an operator types when they
/// want "reachable from my laptop", and they are NOT loopback — `IpAddr::is_loopback` is false
/// for both — so they must classify as exposed, or a server's guard would pass the single most
/// common way its surface gets published to a network.
#[test]
fn the_wildcard_binds_are_public_not_loopback() {
    for addr in ["0.0.0.0:7879", "[::]:7879"] {
        assert_matches!(
            bind_exposure(&resolve(addr)),
            BindExposure::Public(_),
            "{addr} must classify as exposed — it listens on EVERY interface"
        );
    }
}

#[test]
fn loopback_spellings_are_all_loopback_and_a_routable_ip_is_not() {
    // 127.0.0.0/8 in full, both families, and the name.
    for addr in ["127.0.0.1:7879", "127.9.9.9:7879", "[::1]:7879", "localhost:7879"] {
        assert_eq!(bind_exposure(&resolve(addr)), BindExposure::Loopback, "{addr}");
    }
    for addr in ["<host>:7879", "<host>:7879", "[2001:db8::1]:7879"] {
        assert_matches!(bind_exposure(&resolve(addr)), BindExposure::Public(_), "{addr}");
    }
}

/// A name resolving to BOTH loopback and a routable address is exposed on that address, so the
/// strictest reading is the true one. (Constructed directly: no DNS in a unit test.)
#[test]
fn one_routable_address_among_loopbacks_is_still_public() {
    let mixed = vec![
        "127.0.0.1:7879".parse::<SocketAddr>().unwrap(),
        "<host>:7879".parse::<SocketAddr>().unwrap(),
    ];
    assert_matches!(bind_exposure(&mixed), BindExposure::Public(_));
    // …and "resolved to nothing" is its own answer, never silently read as exposed.
    assert_eq!(bind_exposure(&[]), BindExposure::Unresolvable);
}
