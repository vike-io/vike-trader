use super::*;
use std::assert_matches;
use std::io::Cursor;
use std::net::ToSocketAddrs;
use vike_tradehub_client::proto::{NODE_PROTO_VERSION, read_frame_raw_capped};

fn resolve(addr: &str) -> Vec<SocketAddr> {
    addr.to_socket_addrs().map(|it| it.collect()).unwrap_or_default()
}

/// The DEFAULT is loopback and must stay so: it is the reachability barrier the plaintext
/// handshake depends on, not a convenience.
#[test]
fn the_default_address_is_loopback() {
    assert_eq!(bind_exposure(&resolve(DEFAULT_ADDR)), BindExposure::Loopback);
}

/// ⚠ The wildcards are the whole point. `0.0.0.0` / `::` are what an operator types when they
/// want "reachable from my laptop", and they are NOT loopback — `IpAddr::is_loopback` is false
/// for both — so they must classify as exposed, or the guard would pass the single most common
/// way this surface gets published to a network.
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

/// ⚠ **THE GUARD ITSELF.** Without an opt-in a non-loopback bind is REFUSED, not warned about:
/// a `warn!` on a surface that (with control on) places real orders is a line in a log file
/// nobody reads until afterwards. With the opt-in it proceeds, and says so.
#[test]
fn a_public_bind_is_refused_without_the_opt_in_and_announced_with_it() {
    let public = resolve("0.0.0.0:7879");
    let exposed = public[0];
    assert_eq!(
        bind_decision(&public, false),
        BindDecision::Refuse(exposed),
        "default (no flags.tradehub_allow_public_bind) must REFUSE a wildcard bind"
    );
    assert_eq!(
        bind_decision(&public, true),
        BindDecision::ProceedExposed(exposed),
        "the named opt-in permits it — and the decision still carries what is exposed, so the \
             daemon can name it"
    );
}

/// …and the opt-in changes NOTHING for a loopback bind: it is not a general "skip the checks"
/// switch, so leaving it set on a host later moved back to loopback is inert.
#[test]
fn a_loopback_bind_proceeds_identically_with_or_without_the_opt_in() {
    let local = resolve(DEFAULT_ADDR);
    assert_eq!(bind_decision(&local, false), BindDecision::Proceed);
    assert_eq!(bind_decision(&local, true), BindDecision::Proceed);
    // An address that resolves to nothing is left to `bind` to reject with its own message,
    // rather than being reported as an exposure it is not.
    assert_eq!(bind_decision(&[], false), BindDecision::Proceed);
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

/// PRE-AUTH ALLOCATION. A four-byte length prefix from a peer that has sent no key must not be
/// able to name a 64 MiB buffer. The guard fires on the LENGTH — note the body is never
/// supplied, so a passing read would have had to allocate first.
#[test]
fn an_unauthenticated_peer_cannot_name_a_huge_allocation() {
    let over = HANDSHAKE_MAX_FRAME_LEN + 1;
    let mut framed = over.to_be_bytes().to_vec();
    framed.push(0); // one byte of "body" — the refusal must not be a short-read artifact
    let err = read_frame_raw_capped(&mut Cursor::new(framed), HANDSHAKE_MAX_FRAME_LEN)
        .expect_err("a length past the handshake cap must be refused");
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);

    // The SAME frame is fine under the post-auth ceiling — proving the cap, not the framing, is
    // what refused it, and that the pre-auth phase is genuinely stricter than the session.
    let mut ok_framed = over.to_be_bytes().to_vec();
    ok_framed.extend(std::iter::repeat_n(b'x', over as usize));
    assert_eq!(
        read_frame_raw(&mut Cursor::new(ok_framed)).expect("under MAX_FRAME_LEN").len(),
        over as usize
    );
}

/// A real handshake fits the cap with orders of magnitude to spare — the cap must bound an
/// attacker, not a client. Both pre-auth frames are measured as they go on the wire.
#[test]
fn the_handshake_cap_is_far_larger_than_a_real_handshake() {
    let mut buf = Vec::new();
    write_frame(&mut buf, &Request::Hello { proto_version: NODE_PROTO_VERSION }).unwrap();
    write_frame(&mut buf, &Request::Auth { scope: Scope::Write, mac: vec![0u8; 32] }).unwrap();
    assert!(
        buf.len() * 8 < HANDSHAKE_MAX_FRAME_LEN as usize,
        "both handshake frames are {} bytes; the cap ({HANDSHAKE_MAX_FRAME_LEN}) must keep a \
             wide margin over them",
        buf.len()
    );
}

/// The slot guard must RELEASE — a counter that only goes up would wedge the server at
/// [`MAX_CONNECTIONS`] LIFETIME connections, turning a DoS guard into the DoS. (That the cap
/// itself is a sane positive bound is asserted at COMPILE time, beside the constant.)
#[test]
fn a_connection_slot_is_released_when_its_thread_ends() {
    let live = Arc::new(AtomicUsize::new(0));
    {
        let _a = ConnSlot(Arc::clone(&live));
        live.fetch_add(1, Ordering::AcqRel);
        let _b = ConnSlot(Arc::clone(&live));
        live.fetch_add(1, Ordering::AcqRel);
        assert_eq!(live.load(Ordering::Acquire), 2);
    }
    assert_eq!(live.load(Ordering::Acquire), 0, "both slots freed on drop");
}
