//! The server's half of the frame discipline: how it waits for a frame without ever resuming a half-read
//! one, and how it reads a client frame.
//!
//! The codec is `vike_hub_proto::frame`'s and is not repeated here, and nor is the capped write: a server
//! frame is written under `GW_DOWN_FRAME_CEILING_BYTES`, not under `MAX_FRAME_LEN`'s 64 MiB (the client
//! reads under the smaller number and treats a longer frame as a framing fault that kills the session),
//! by `vike_market_stream::frame::write_capped`, which takes the ceiling as an argument. What is the
//! server's own: a server that must also notice a peer going away (a running fetch, a silent
//! stream) waits for readability with `peek` (`vike_market_stream::frame::wait_readable`, shared with the
//! datahub's strategy sessions), never with a timed-out frame read, because a read timeout that fires mid-frame
//! loses the bytes already consumed and desyncs the stream.

use std::io;
use std::net::TcpStream;
use std::time::Duration;

use vike_gatewayhub_client::proto::{GW_UP_FRAME_CEILING_BYTES, read_frame_raw_capped};

/// Read the body of a frame whose first byte `vike_market_stream::frame::wait_readable` reported, under the client-frame
/// ceiling and with `timeout` for the rest of it. A framing fault or a timeout is `Err`: the stream is
/// desynced and the caller closes the connection.
pub fn read_client_body(stream: &mut TcpStream, timeout: Duration) -> io::Result<Vec<u8>> {
    stream.set_read_timeout(Some(timeout.max(Duration::from_millis(1))))?;
    read_frame_raw_capped(stream, GW_UP_FRAME_CEILING_BYTES)
}

#[cfg(test)]
#[path = "frame_tests.rs"]
mod frame_tests;
