use vike_gatewayhub_client::proto::Response;
use vike_market_stream::frame::{Ready, wait_readable, write_capped};

use super::*;
use crate::server::ctl::tests::pair;

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
