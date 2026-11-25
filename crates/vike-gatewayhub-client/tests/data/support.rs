//! The helpers more than one `data` member shares, compiled once in the group binary (`tests/data.rs`).

use std::sync::Arc;

use vike_data::{DataClientSink, RecordingSink};
use vike_gatewayhub_client::client::GatewayLink;
use vike_gatewayhub_client::opener::StreamOpenerGateway;
use vike_market_stream::market::DataClientStream;

/// A real `DataClientStream` for `binance` over `link`, and the recording sink it delivers into.
pub(crate) fn client(link: GatewayLink) -> (DataClientStream, Arc<RecordingSink>) {
    let sink = Arc::new(RecordingSink::default());
    let dyn_sink: Arc<dyn DataClientSink> = sink.clone();
    (
        DataClientStream::new(StreamOpenerGateway::new(link), "binance", dyn_sink)
            .expect("the reader thread"),
        sink,
    )
}
