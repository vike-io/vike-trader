use super::*;

#[test]
fn loopback_binds() {
    let listener = bind_loopback("127.0.0.1:0").expect("loopback binds");
    assert!(listener.local_addr().expect("addr").ip().is_loopback());
    assert!(check_listener(&listener).is_ok());
}

#[test]
fn every_non_loopback_spelling_is_refused() {
    for addr in ["0.0.0.0:0", "[::]:0", "192.0.2.7:7883", "<host>:7883"] {
        match bind_loopback(addr) {
            Err(BindRefusal::NotLoopback(a)) => assert!(!a.ip().is_loopback(), "{addr}"),
            other => panic!("`{addr}` was not refused as non-loopback: {other:?}"),
        }
    }
}

#[test]
fn the_refusal_names_the_address_and_offers_no_flag() {
    let text = bind_loopback("0.0.0.0:7883").expect_err("refused").to_string();
    assert!(text.contains("0.0.0.0:7883"), "{text}");
    assert!(text.contains("LOOPBACK ONLY"), "{text}");
    assert!(text.contains("no flag"), "{text}");
    assert!(text.contains("SSH tunnel"), "{text}");
}

#[test]
fn an_unparseable_address_is_refused_not_bound() {
    assert!(matches!(bind_loopback("not-an-address"), Err(BindRefusal::Unresolvable(_))));
    assert!(matches!(check_loopback(&[]), Err(BindRefusal::Unresolvable(_))));
}

#[test]
fn a_name_resolving_to_a_loopback_and_a_lan_address_is_exposed() {
    let mixed: Vec<SocketAddr> =
        vec!["127.0.0.1:1".parse().unwrap(), "192.0.2.9:1".parse().unwrap()];
    assert!(
        matches!(check_loopback(&mixed), Err(BindRefusal::NotLoopback(a)) if a.ip().to_string() == "192.0.2.9")
    );
}

#[test]
fn a_listener_bound_elsewhere_is_checked_again() {
    let public = TcpListener::bind("0.0.0.0:0").expect("wildcard binds in a test");
    assert!(matches!(check_listener(&public), Err(BindRefusal::NotLoopback(_))));
}

#[test]
fn the_refusal_converts_to_an_io_error_a_root_can_return() {
    let e: io::Error = bind_loopback("0.0.0.0:0").expect_err("refused").into();
    assert_eq!(e.kind(), io::ErrorKind::PermissionDenied);
}
