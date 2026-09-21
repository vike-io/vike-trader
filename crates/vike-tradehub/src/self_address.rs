//! **WHICH BOX this daemon is running on, discovered from the kernel's own routing table.**
//!
//! The daemon stamps the answer into every published frame
//! ([`vike_tradehub_client::wire::WireNodeIdentity::advertise_addr`]) so a thin client can say
//! which box it is attached to. It is the ONE fact in that block the client cannot derive.
//!
//! # ⚠ Why the CLIENT cannot answer this
//!
//! MEASURED on the production box: both listeners bind loopback (`127.0.0.1:7878`,
//! `127.0.0.1:7879`), so an SSH tunnel is the only route in — and through a tunnel the CLIENT side
//! of the socket is the tunnel mouth. Every thin client on every box reads `127.0.0.1:7879` for its
//! dial address, whichever daemon it is attached to, so two operators on two different production
//! boxes see the identical string. The connection carries no evidence of the far end's address; the
//! far end does, and is already sending a frame.
//!
//! # How the lookup works, and why NOTHING IS CONTACTED
//!
//! [`discover_ip`] binds a `UdpSocket` to the wildcard address and `connect`s it to a routable
//! destination. **`connect` on a UDP socket sends no packet.** The std library says so outright —
//! `std::net::UdpSocket::connect`'s documentation is "Connects this UDP socket to a remote address,
//! allowing the `send` and `recv` syscalls to be used to send data and also applies filters to only
//! receive data from the specified address", and `send`/`recv` are the calls that move bytes. What
//! `connect` does on a datagram socket is ask the kernel to resolve a ROUTE and bind the socket's
//! local end to the source address that route selects; `local_addr` then reads that address back.
//! No handshake, no datagram, no third party, no name server, nothing on the wire.
//!
//! It is the same question `ip route get` answers, and the answer is the same field:
//!
//! ```text
//! $ ip route get <dest>
//! <dest> via <gw> dev <iface> src <THIS ADDRESS>
//! ```
//!
//! A daemon that asked a third party what its own address was would be a defect — it would leak the
//! existence of the box to whoever it asked, and it would make a startup path depend on the
//! internet. This asks the kernel.
//!
//! # ⚠ What the lookup gets RIGHT, and what it cannot
//!
//! No count here, deliberately — each bullet argues its own case, and a tally in front of a list is
//! the shape of claim that goes stale the moment somebody adds a sixth.
//!
//! * **Multi-NIC** (the production box has a public interface AND a docker bridge) — CORRECT, and
//!   this is the reason the lookup is a route query rather than an interface enumeration. A route
//!   lookup for an off-box destination follows the DEFAULT route and so names the public interface's
//!   source address; the docker bridge wins only for destinations inside its own subnet. The
//!   tempting alternative — walk the interfaces and take the first non-loopback — returns
//!   the docker bridge's conventional `172.17.0.1` — silently, and reading exactly as plausibly as
//!   the right answer, which is what makes it the dangerous one rather than merely the wrong one. (It
//!   also needs `getifaddrs`, which `std` does not expose, so it would cost a dependency as well.)
//! * **Loopback-only box** (a container with no default route) — `connect` fails with
//!   `ENETUNREACH`, so this returns `None`. That is the honest answer: there IS no address that
//!   names this box to anyone else. It never falls back to `127.0.0.1`, which is the useless string
//!   this whole module exists to replace.
//! * **⚠ Behind NAT** — returns the box's PRIVATE address (`192.168.x.x`, `10.x.x.x`), not the
//!   public one the outside world would dial. The kernel does not know its own public address; only
//!   a third party can report it, and asking one is refused above. The private address still
//!   DISTINGUISHES the box, which is the job here — but an operator who needs the public one sets
//!   `config.toml`'s `tradehub_advertise_addr`, which wins outright ([`advertise_addr`]).
//! * **⚠ IPv6-only** — the v4 probe fails and the v6 probe answers, in that order. A dual-stack box
//!   therefore reports its v4 address, because that is the one an operator recognises; v6 is the
//!   fallback rather than the preference.
//! * **⚠ A source address that is not reachable from the client** — always possible, and not an
//!   error. A loopback-bound daemon's real address names its BOX; it is not an endpoint a tunnelled
//!   client can dial. Every render treats the value as the daemon's claim about where it is
//!   running, never as a dial address (`vike_app_core::backend_identity`).
//!
//! # ⚠ The probe destinations are DOCUMENTATION addresses, deliberately
//!
//! [`V4_PROBE`]/[`V6_PROBE`] are RFC 5737 / RFC 3849 documentation ranges. Nothing is sent to them,
//! so their only requirement is that a default route MATCH them — which `0.0.0.0/0` and `::/0` do,
//! being the routes whose source address this is asking for. Using a real public resolver's address
//! (`1.1.1.1`, `8.8.8.8`) would work identically and would put a third party's address in this
//! source tree, where every future reader has to re-derive that no packet reaches it. A reserved
//! range cannot be mistaken for a phone-home.
//!
//! # Read ONCE, at startup
//!
//! [`advertise_addr`] is called once by `crate::tradehub_cli`, and the result is moved into the
//! process-static identity block every frame carries. Re-reading would put a socket syscall on the
//! publish path to track a fact that changes only when the box's networking does — a DHCP lease
//! move or an interface failover — at which point the daemon's address has changed under a live
//! mount and a restart is the honest response anyway. A stale value cannot arise without a restart
//! having been earned.
//!
//! No environment is read here (the configured override arrives as a PARAMETER from the binary's
//! own settings load), so this module adds no `Layer::Library` row to
//! `vike_ops::settings::SETTINGS`.

use std::net::{IpAddr, SocketAddr, UdpSocket};

/// The IPv4 route-lookup destination: RFC 5737 TEST-NET-1, discard port. See the module doc — no
/// packet is sent to it; a default route merely has to match it.
const V4_PROBE: &str = "192.0.2.1:9";

/// The IPv6 twin: RFC 3849's documentation prefix, discard port.
const V6_PROBE: &str = "[2001:db8::1]:9";

/// The box's own source address for an off-box destination, as the kernel's routing table answers
/// it — IPv4 if the box has one, else IPv6, else `None`.
///
/// `None` is a real and ordinary answer (a loopback-only container, a box with no default route).
/// It is never `127.0.0.1`: a loopback or unspecified result is discarded, because reporting it
/// would reproduce the exact string this module exists to replace.
///
/// Performs no I/O beyond the socket's own creation and route lookup; contacts nothing.
#[must_use]
pub fn discover_ip() -> Option<IpAddr> {
    route_source("0.0.0.0:0", V4_PROBE).or_else(|| route_source("[::]:0", V6_PROBE))
}

/// One route lookup: bind the wildcard, `connect` (a ROUTE RESOLUTION — no datagram leaves the
/// box), read back the source address the kernel chose.
///
/// Every failure is `None` rather than an error: a box with no route to `dest` has no address that
/// names it to anyone, which is information, not a fault. A loopback or unspecified result is
/// discarded for the same reason.
fn route_source(bind: &str, dest: &str) -> Option<IpAddr> {
    let sock = UdpSocket::bind(bind).ok()?;
    sock.connect(dest).ok()?;
    let ip = sock.local_addr().ok()?.ip();
    (!ip.is_unspecified() && !ip.is_loopback()).then_some(ip)
}

/// **The value the daemon puts on the wire, and the ORDER of the three answers.**
///
/// 1. `configured` — `config.toml`'s `tradehub_advertise_addr` (or `VIKE_TRADEHUB_ADVERTISE_ADDR`)
///    WINS outright. The operator knows things the kernel cannot: the public address in front of a
///    NAT, the name a tunnel is keyed on, which of two interfaces the people reading the screen
///    think of as "the box". A configured value is used verbatim, and `listen` is not consulted —
///    the whole point of an override is that it is not composed from something else.
/// 2. the DISCOVERED address ([`discover_ip`]), paired with the port `listen` binds so the result
///    reads like the address it names rather than a bare host.
/// 3. `""` — nothing to report. An empty string is what a client renders as "the daemon said
///    nothing", and a daemon that cannot name itself must say nothing rather than say
///    `127.0.0.1`.
///
/// `listen` is the daemon's own bind address (`config.tradehub_addr`, e.g. `127.0.0.1:7879`): its
/// HOST half is discarded (that is the useless half) and only its PORT is kept.
#[must_use]
pub fn advertise_addr(configured: Option<&str>, listen: Option<&str>) -> String {
    compose(configured, listen, discover_ip())
}

/// [`advertise_addr`]'s pure core, with the discovered address INJECTED — so every branch of the
/// precedence above is testable without depending on the box the test runs on (which would either
/// be unassertable or would commit a real address to this repository).
#[must_use]
pub fn compose(
    configured: Option<&str>,
    listen: Option<&str>,
    discovered: Option<IpAddr>,
) -> String {
    // A blank configured value is an ABSENT one: `tradehub_advertise_addr = ""` in a file, or an
    // `Environment=` line someone emptied rather than deleted, must fall through to discovery
    // rather than advertise a blank address.
    if let Some(v) = configured.map(str::trim).filter(|v| !v.is_empty()) {
        return v.to_string();
    }
    let Some(ip) = discovered else {
        return String::new();
    };
    match listen_port(listen) {
        // `SocketAddr`'s own Display brackets an IPv6 host, which is the spelling every other
        // address in this workspace uses and the one `vike_config`'s `is_host_port` accepts.
        Some(port) => SocketAddr::new(ip, port).to_string(),
        None => ip.to_string(),
    }
}

/// The PORT half of a bind address, or `None` when there is no bind address or its port does not
/// parse. `rsplit_once` rather than `split_once` so an IPv6 bind (`[::1]:7879`) yields the port and
/// not the first hextet.
fn listen_port(listen: Option<&str>) -> Option<u16> {
    listen?.trim().rsplit_once(':')?.1.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    /// ⚠ Documentation addresses (RFC 5737 / RFC 3849) throughout. A real address measured on a
    /// real box must never reach a tracked file — `scripts/forbidden_tokens.ere` refuses one, and
    /// it refuses it at TAG time, which is the worst moment to find out.
    const DOC_V4: Ipv4Addr = Ipv4Addr::new(203, 0, 113, 7);

    /// **THE DISCOVERY ITSELF, asserted as PROPERTIES rather than as an address.** The address is a
    /// property of whatever box the test runs on — CI's runners, a developer's laptop, the
    /// production daemon — so pinning one would either be unassertable or would commit a real
    /// address to this repository.
    ///
    /// What is asserted is everything that is true everywhere: it does not panic, it does not hang,
    /// whatever it returns is a parseable IP that round-trips its own rendering, and it is NEVER
    /// loopback or unspecified — because `127.0.0.1` is the answer this whole module exists to stop
    /// a client from being shown.
    #[test]
    fn the_discovery_returns_a_real_address_or_nothing_and_never_loopback() {
        let found = discover_ip();
        if let Some(ip) = found {
            assert!(!ip.is_loopback(), "{ip}: loopback is the non-answer this module replaces");
            assert!(!ip.is_unspecified(), "{ip}: the wildcard names no box");
            let round: IpAddr = ip.to_string().parse().expect("renders to a parseable address");
            assert_eq!(round, ip);
        }
        // …and it is STABLE: it is read once at startup and must not depend on when it was asked.
        assert_eq!(found, discover_ip(), "the route lookup is a function of the box, not the call");
    }

    /// A box with ONLY loopback reports NOTHING — it does not error, and it does not report
    /// `127.0.0.1`. The lookup's own failure path is exercised directly here (a destination inside
    /// the loopback net resolves to a loopback source on every platform, which is precisely the
    /// result `route_source` discards), because a test cannot take the machine's routes away.
    #[test]
    fn a_lookup_that_can_only_reach_loopback_reports_nothing_rather_than_erroring() {
        assert_eq!(
            route_source("0.0.0.0:0", "127.0.0.1:9"),
            None,
            "a loopback source is discarded — reporting it would hand back the useless string"
        );
    }

    /// **THE OVERRIDE ORDER, all three rungs.** The operator's configured value wins over a
    /// discovered one, because they know what the kernel cannot (a NAT's public face, a tunnel's
    /// name, which of two interfaces the people reading the screen mean).
    #[test]
    fn a_configured_value_beats_the_discovered_one_and_is_used_verbatim() {
        let discovered = Some(IpAddr::V4(DOC_V4));
        assert_eq!(
            compose(Some("198.51.100.4:7879"), Some("127.0.0.1:7879"), discovered),
            "198.51.100.4:7879",
            "the operator's own value, verbatim — not composed with anything"
        );
        // …and a blank one is an ABSENT one, not an advertised blank.
        for blank in ["", "   "] {
            assert_eq!(
                compose(Some(blank), Some("127.0.0.1:7879"), discovered),
                "203.0.113.7:7879",
                "an emptied setting falls through to discovery"
            );
        }
    }

    /// The discovered address is paired with the LISTEN PORT, and only the port: the host half of a
    /// bind address is the loopback string this module exists to replace.
    #[test]
    fn the_discovered_address_takes_the_listen_port_and_discards_the_listen_host() {
        let discovered = Some(IpAddr::V4(DOC_V4));
        assert_eq!(compose(None, Some("127.0.0.1:7879"), discovered), "203.0.113.7:7879");
        assert_eq!(
            compose(None, Some("0.0.0.0:7879"), discovered),
            "203.0.113.7:7879",
            "a wildcard bind contributes its port and nothing else"
        );
        // No bind address (a headless daemon with no node server) — the address alone still names
        // the box, which is the whole question being answered.
        assert_eq!(compose(None, None, discovered), "203.0.113.7");
        assert_eq!(
            compose(None, Some("not-an-address"), discovered),
            "203.0.113.7",
            "an unparseable port is dropped rather than guessed at"
        );
    }

    /// ⚠ An IPv6 result is BRACKETED, because `[addr]:port` is the only spelling in which a v6
    /// address and a port are distinguishable — and it is the spelling `vike_config`'s address
    /// validator accepts, so a discovered v6 address renders the same way a configured one must be
    /// written. `rsplit_once` on the LISTEN side is the same trap from the other end: a v6 bind
    /// address is full of colons and only the last one is the port's.
    #[test]
    fn an_ipv6_result_is_bracketed_and_an_ipv6_bind_yields_its_port_not_its_first_hextet() {
        let v6 = Some(IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1)));
        assert_eq!(compose(None, Some("[::1]:7879"), v6), "[2001:db8::1]:7879");
        assert_eq!(listen_port(Some("[::1]:7879")), Some(7879), "the LAST colon is the port's");
    }

    /// **Nothing discovered and nothing configured is EMPTY** — never a placeholder, and never
    /// `127.0.0.1`. A client reads empty as "this daemon reported no address", which is a different
    /// statement from any address at all.
    #[test]
    fn nothing_known_is_empty_rather_than_a_loopback_placeholder() {
        assert_eq!(compose(None, Some("127.0.0.1:7879"), None), "");
        assert_eq!(compose(None, None, None), "");
    }

    /// **The operator's probe: what would THIS box report?** `#[ignore]`d, so it runs only when
    /// invoked by name — the answer is a property of the machine, so there is nothing for CI to
    /// assert and an address printed on a runner would be noise.
    ///
    /// ⚠ It PRINTS and asserts nothing about the value, deliberately: an assertion would have to
    /// name a real address, and a real box's address must never reach a tracked file.
    ///
    /// ```sh
    /// cargo test -p vike-tradehub --lib self_address -- --ignored --nocapture
    /// ```
    ///
    /// Compare the answer against the kernel's own, which is the same question asked another way:
    ///
    /// ```sh
    /// ip route get 192.0.2.1     # …and read the `src` field
    /// ```
    #[test]
    #[ignore = "prints this box's own address — an operator probe, not an assertion"]
    fn what_this_box_would_report() {
        println!("discovered: {:?}", discover_ip());
        println!(
            "would advertise (no override, listening on :7879): {}",
            advertise_addr(None, Some("127.0.0.1:7879"))
        );
    }

    /// The probe destinations stay inside the reserved documentation ranges. Reddens on a future
    /// edit that "fixes" the lookup by pointing it at a public resolver — which would work, and
    /// would put a third party's address in a startup path where every later reader has to
    /// re-derive that no packet reaches it.
    #[test]
    fn the_probe_destinations_are_reserved_documentation_addresses() {
        let v4: SocketAddr = V4_PROBE.parse().expect("a literal socket address");
        assert_eq!(v4.ip(), IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)), "RFC 5737 TEST-NET-1");
        let v6: SocketAddr = V6_PROBE.parse().expect("a literal socket address");
        assert_eq!(
            v6.ip(),
            IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1)),
            "RFC 3849 documentation prefix"
        );
    }
}
