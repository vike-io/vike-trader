//! The server's half of the frame discipline: how it waits for a frame without ever resuming a half-read
//! one, and how it reads a client frame.
//!
//! The codec is `vike_hub_proto::frame`'s and is not repeated here, and nor is the capped write: a server
//! frame is written under `GW_DOWN_FRAME_CEILING_BYTES`, not under `MAX_FRAME_LEN`'s 64 MiB (the client
//! reads under the smaller number and treats a longer frame as a framing fault that kills the session),
//! by `vike_market_stream::frame::write_capped`, which takes the ceiling as an argument. What is the
//! server's own: a server that must also notice a peer going away (a running fetch, a silent
//! stream) waits for readability with `peek`, never with a timed-out frame read, because a read timeout that fires mid-frame loses
//! the bytes already consumed and desyncs the stream.

use std::io;
use std::net::TcpStream;
use std::time::Duration;

use vike_gatewayhub_client::proto::{GW_UP_FRAME_CEILING_BYTES, read_frame_raw_capped};
use vike_market_stream::frame::is_timeout;

/// What [`wait_readable`] found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ready {
    /// At least one byte of a frame is waiting.
    Data,
    /// The peer closed its half.
    Eof,
    /// Nothing arrived within the wait.
    Timeout,
}

/// Wait up to `wait` for a frame to BEGIN, without consuming a byte. A `peek`, so the stream cannot
/// be left half-read by the timeout.
pub fn wait_readable(stream: &TcpStream, wait: Duration) -> io::Result<Ready> {
    stream.set_read_timeout(Some(wait.max(Duration::from_millis(1))))?;
    let mut probe = [0u8; 1];
    match stream.peek(&mut probe) {
        Ok(0) => Ok(Ready::Eof),
        Ok(_) => Ok(Ready::Data),
        Err(e) if is_timeout(&e) || e.kind() == io::ErrorKind::Interrupted => Ok(Ready::Timeout),
        Err(e) => Err(e),
    }
}

/// Read the body of a frame whose first byte [`wait_readable`] reported, under the client-frame
/// ceiling and with `timeout` for the rest of it. A framing fault or a timeout is `Err`: the stream is
/// desynced and the caller closes the connection.
pub fn read_client_body(stream: &mut TcpStream, timeout: Duration) -> io::Result<Vec<u8>> {
    stream.set_read_timeout(Some(timeout.max(Duration::from_millis(1))))?;
    read_frame_raw_capped(stream, GW_UP_FRAME_CEILING_BYTES)
}

#[cfg(test)]
#[path = "frame_tests.rs"]
mod frame_tests;
