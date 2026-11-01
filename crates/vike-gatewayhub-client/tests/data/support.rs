//! The helpers more than one `data` member shares, compiled once in the group binary (`tests/data.rs`).

use std::sync::Arc;

use vike_data::{LiveDataSink, RecordingSink};
use vike_gatewayhub_client::client::GatewayLink;
use vike_gatewayhub_client::opener::GatewayOpener;
use vike_market_stream::market::RemoteDataClient;

/// A real `RemoteDataClient` for `binance` over `link`, and the recording sink it delivers into.
pub(crate) fn client(link: GatewayLink) -> (RemoteDataClient, Arc<RecordingSink>) {
    let sink = Arc::new(RecordingSink::default());
    let dyn_sink: Arc<dyn LiveDataSink> = sink.clone();
    (
        RemoteDataClient::new(GatewayOpener::new(link), "binance", dyn_sink)
            .expect("the reader thread"),
        sink,
    )
}
