use std::net::{TcpListener, TcpStream};

use vike_gatewayhub_client::proto::Response;
use vike_market_stream::frame::write_capped;

use super::*;

fn pair() -> (TcpStream, TcpStream) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let client = TcpStream::connect(listener.local_addr().expect("addr")).expect("connect");
    let (server, _) = listener.accept().expect("accept");
    (server, client)
}

#[test]
fn waiting_for_a_frame_never_consumes_it() {
    let (server, mut client) = pair();
    assert_eq!(wait_readable(&server, Duration::from_millis(20)).expect("wait"), Ready::Timeout);
    write_capped(&mut client, &Response::Pong, 1024).expect("write");
    assert_eq!(wait_readable(&server, Duration::from_secs(2)).expect("wait"), Ready::Data);
    // Peeked, not consumed: the whole frame is still there.
    let mut server = server;
    let body = read_client_body(&mut server, Duration::from_secs(2)).expect("body");
    assert!(serde_json::from_slice::<Response>(&body).is_ok());
    drop(client);
    assert_eq!(wait_readable(&server, Duration::from_secs(2)).expect("wait"), Ready::Eof);
}
