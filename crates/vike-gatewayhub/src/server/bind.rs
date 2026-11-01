//! The bind guard: this daemon listens on loopback or it does not listen.
//!
//! The handshake is PLAINTEXT and authenticates the CONNECTION, not each frame
//! (`vike_hub_proto::auth`'s "What this does NOT buy"), and this daemon holds venue credentials and,
//! in a later wave, places orders. So where the datahub offers an opt-in for a non-loopback bind
//! (`vike_datahub_client::bind`), this daemon offers NONE: a non-loopback address is refused outright,
//! there is no allow-public flag, and a box that wants a remote reader reaches the loopback port over
//! an SSH tunnel. The classification of an address is `vike_hub_proto::bind::bind_exposure`'s (a
//! hostname resolving to a loopback and a LAN address is exposed on the LAN, and the wildcards are
//! public); the DECISION over it is this module's, and it has one arm.
//!
//! [`bind_loopback`] is how a root opens the listener, and `crate::server::accept::serve` checks the
//! listener it is handed again ([`check_listener`]), so a root that bound some other way cannot serve
//! on a public socket by passing it in.

use std::fmt;
use std::io;
use std::net::{SocketAddr, TcpListener, ToSocketAddrs};

use vike_hub_proto::bind::{BindExposure, bind_exposure};

/// Why the guard refused.
#[derive(Debug)]
pub enum BindRefusal {
    /// The address (or one of the addresses its name resolves to) is reachable off-box.
    NotLoopback(SocketAddr),
    /// The address resolved to nothing, or did not parse.
    Unresolvable(String),
    /// The address is loopback and the OS refused the bind.
    Io(io::Error),
}

impl fmt::Display for BindRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BindRefusal::NotLoopback(addr) => write!(
                f,
                "vike-gatewayhub: refusing to listen on {addr}: it is reachable off this box. The \
                 gateway binds LOOPBACK ONLY (a plaintext handshake in front of venue credentials); \
                 there is no flag that allows another address. Reach it through an SSH tunnel to \
                 the loopback port"
            ),
            BindRefusal::Unresolvable(why) => write!(f, "vike-gatewayhub: cannot bind: {why}"),
            BindRefusal::Io(e) => write!(f, "vike-gatewayhub: cannot bind: {e}"),
        }
    }
}

impl std::error::Error for BindRefusal {}

impl From<BindRefusal> for io::Error {
    fn from(refusal: BindRefusal) -> Self {
        match refusal {
            BindRefusal::Io(e) => e,
            BindRefusal::NotLoopback(_) => {
                io::Error::new(io::ErrorKind::PermissionDenied, refusal.to_string())
            }
            BindRefusal::Unresolvable(_) => {
                io::Error::new(io::ErrorKind::InvalidInput, refusal.to_string())
            }
        }
    }
}

/// The guard's one decision over a RESOLVED address set.
pub fn check_loopback(resolved: &[SocketAddr]) -> Result<(), BindRefusal> {
    match bind_exposure(resolved) {
        BindExposure::Loopback => Ok(()),
        BindExposure::Public(addr) => Err(BindRefusal::NotLoopback(addr)),
        BindExposure::Unresolvable => {
            Err(BindRefusal::Unresolvable("the address resolved to nothing".to_string()))
        }
    }
}

/// Resolve `addr`, refuse anything but loopback, and bind exactly the addresses that were checked.
pub fn bind_loopback(addr: &str) -> Result<TcpListener, BindRefusal> {
    let resolved: Vec<SocketAddr> = addr
        .to_socket_addrs()
        .map_err(|e| BindRefusal::Unresolvable(format!("`{addr}` does not resolve: {e}")))?
        .collect();
    check_loopback(&resolved)?;
    TcpListener::bind(resolved.as_slice()).map_err(BindRefusal::Io)
}

/// The same decision over a listener somebody else bound.
pub fn check_listener(listener: &TcpListener) -> Result<(), BindRefusal> {
    let local = listener.local_addr().map_err(BindRefusal::Io)?;
    check_loopback(&[local])
}

#[cfg(test)]
#[path = "bind_tests.rs"]
mod bind_tests;
