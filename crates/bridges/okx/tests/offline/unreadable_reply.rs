//! A 2xx whose body cannot be read, through the real agent on a loopback port (no venue is called): on
//! a write the venue took the request and what became of it is unknown, so it is the ambiguous sentinel;
//! `code: 0` said "it never left this host", which a submit reads as a terminal reject of a live order.
//! A read has no outcome to be unsure of and keeps `code: 0`. `UreqTransport::run` is the model.

use std::io::{Read, Write};

use vike_bridge_core::transport::E_TIMEOUT_AMBIGUOUS;
use vike_bridge_core::transport::json::RestTransportJson;
use vike_okx::signer::OkxV5Signer;
use vike_okx::transport::client::rest_transport;
use vike_secrets::credentials::Credentials;

const HTML: &str = "HTTP/1.1 200 OK\r\ncontent-length: 6\r\nconnection: close\r\n\r\n<html>";
const EMPTY: &str = "HTTP/1.1 200 OK\r\ncontent-length: 0\r\nconnection: close\r\n\r\n";

fn serve(reply: &'static str) -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buffer = [0u8; 2048];
            let _ = stream.read(&mut buffer);
            let _ = stream.write_all(reply.as_bytes());
        }
    });
    format!("http://{addr}")
}

fn signer() -> OkxV5Signer {
    let creds = Credentials {
        api_key: "k".to_string().into(),
        api_secret: "s".to_string().into(),
        passphrase: Some("p".to_string().into()),
    };
    OkxV5Signer::new(&creds, || 0)
}

#[test]
fn an_unreadable_2xx_reply_is_ambiguous_for_a_write_only() {
    let t = rest_transport(true);
    for reply in [HTML, EMPTY] {
        let e = t.signed(&serve(reply), "/x", "POST", &[], &signer()).unwrap_err();
        assert_eq!(e.code, E_TIMEOUT_AMBIGUOUS, "{e}");
        assert!(e.msg.contains("bad json"), "{e}");
        let e = t.signed_json(&serve(reply), "/x", "POST", "[]", &signer()).unwrap_err();
        assert_eq!(e.code, E_TIMEOUT_AMBIGUOUS, "the array-body POST too: {e}");
    }
    let e = t.signed(&serve(HTML), "/x", "GET", &[], &signer()).unwrap_err();
    assert_eq!(e.code, 0, "{e}");
    let e = t.signed_requery(&serve(HTML), "/x", "GET", &[], &signer()).unwrap_err();
    assert_eq!(e.code, 0, "{e}");
    let e = t.public(&serve(HTML), "/x", &[]).unwrap_err();
    assert_eq!(e.code, 0, "{e}");
}
