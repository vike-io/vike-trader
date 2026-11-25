//! **A proxy variable in the process environment is never used** by any agent this crate builds or finishes.
//!
//! ureq's default config reads `ALL_PROXY` / `HTTPS_PROXY` / `HTTP_PROXY` (first hit wins, for every scheme, and
//! nothing exempts loopback). The workspace forbids `unsafe`, and setting a variable in-process is `unsafe`, so the
//! test runs ITSELF again as a child with the six variables set and `NO_PROXY` removed
//! (`crates/vike-backtesthub/tests/sandbox_child_memory.rs`'s `the_child` is the pattern). The "proxy" is a listener
//! nothing may dial.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::time::Duration;

use vike_transport::http;

/// What the child half is started with: a second name filter that matches no test (libtest refuses an unknown
/// `--` option).
const CHILD_MARK: &str = "vike-transport-proxy-environment-child";

#[test]
fn a_proxy_variable_in_the_environment_is_never_used() {
    let proxy = TcpListener::bind("127.0.0.1:0").expect("bind");
    proxy.set_nonblocking(true).expect("nonblocking");
    let address = proxy.local_addr().expect("addr");
    let mut child = std::process::Command::new(std::env::current_exe().expect("this test binary"));
    child.args(["the_child", "--exact", "--ignored", "--nocapture", CHILD_MARK]);
    for name in ["ALL_PROXY", "all_proxy", "HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy"]
    {
        child.env(name, format!("http://{address}"));
    }
    let out = child.env_remove("NO_PROXY").env_remove("no_proxy").output().expect("run the child");
    let said =
        format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(out.status.success(), "the child failed:\n{said}");
    assert!(said.contains("1 passed"), "the child ran no test:\n{said}");
    assert!(proxy.accept().is_err(), "a request went through the environment's proxy");
}

/// The child half: every agent has no proxy, and a GET to a loopback target is answered directly.
#[test]
#[ignore = "the child half of a_proxy_variable_in_the_environment_is_never_used, which starts it"]
fn the_child() {
    if !std::env::args().any(|a| a == CHILD_MARK) {
        return;
    }
    assert!(ureq::Proxy::try_from_env().is_some(), "the parent set the proxy variables");
    let target = TcpListener::bind("127.0.0.1:0").expect("bind target");
    let url = format!("http://{}/x", target.local_addr().expect("addr"));
    std::thread::spawn(move || {
        for conn in target.incoming() {
            let Ok(mut s) = conn else { continue };
            let _ = s.read(&mut [0u8; 4096]);
            let _ =
                s.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok");
        }
    });
    let five = Duration::from_secs(5);
    for (name, agent) in [
        ("blocking_agent", http::blocking_agent()),
        ("blocking_agent_with_timeout", http::blocking_agent_with_timeout(five)),
        (
            "blocking_agent_with_connect_timeout",
            http::blocking_agent_with_connect_timeout(five, five),
        ),
        ("blocking_agent_for_lanes", http::blocking_agent_for_lanes(4)),
        ("public_get_agent", http::public_get_agent()),
        ("blocking_agent_loopback_insecure", http::blocking_agent_loopback_insecure()),
        ("agent_no_redirect", http::agent_no_redirect(ureq::Agent::config_builder())),
        ("agent_public_get", http::agent_public_get(ureq::Agent::config_builder())),
    ] {
        assert!(agent.config().proxy().is_none(), "{name}: {:?}", agent.config().proxy());
        let response = agent.get(&url).call().unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(response.status().as_u16(), 200, "{name}");
    }
}
