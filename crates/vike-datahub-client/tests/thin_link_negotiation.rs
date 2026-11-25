//! **A process behind a thin link asks LZ4 on every dial** (`DatahubClient::set_thin_link_for_this_process`, the
//! desktop's one switch): its connections negotiate `BodyCodec::MsgpackLz4` with a datahub that advertises the
//! codec, and still JSON with one that does not. Its own binary, because the switch is process-wide.

use std::sync::Arc;

use vike_data::HistoryStoreMemory;
use vike_datahub::test_support::spawn_server;
use vike_datahub_client::DatahubClient;
use vike_datahub_client::proto::{BarColumnsBody, Response};
use vike_datahub_client::test_support::spawn_fake_peer;
use vike_hub_protocol::frame::BodyCodec;

#[test]
fn a_thin_link_process_asks_lz4_of_a_server_that_serves_it_and_json_of_one_that_does_not() {
    DatahubClient::set_thin_link_for_this_process(true);
    let addr = spawn_server(Arc::new(HistoryStoreMemory::new()));
    let mut client = DatahubClient::connect(addr).expect("connect");
    assert_eq!(client.body_codec(), BodyCodec::MsgpackLz4);
    assert_eq!(client.bar_columns_body(), BarColumnsBody::Lz4, "its bar reads ask LZ4 too");
    client.ping().expect("Pong through LZ4");
    client.list_series().expect("a list through LZ4");

    let (old, _, _) = spawn_fake_peer(Vec::new(), |_| Some(Response::Pong));
    let client = DatahubClient::connect(old).expect("connect");
    assert_eq!(
        client.body_codec(),
        BodyCodec::Json,
        "an old server is spoken JSON, thin link or not"
    );
}
