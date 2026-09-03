use std::io::Read;

use vike_ctrader::framing::{encode, FrameReader};
use vike_ctrader::proto::pt;

#[test]
fn frame_roundtrips_through_reader() {
    // build a heartbeat frame, feed its bytes to the reader, get the message back
    let frame = encode(pt::HEARTBEAT_EVENT, &[], "hb-1");
    let mut reader = FrameReader::new(std::io::Cursor::new(frame));
    let msg = reader.next_frame().unwrap().expect("one frame");
    assert_eq!(msg.payload_type, pt::HEARTBEAT_EVENT);
    assert_eq!(msg.client_msg_id.as_deref(), Some("hb-1"));
}

#[test]
fn partial_frame_returns_none_then_completes() {
    let frame = encode(pt::HEARTBEAT_EVENT, &[], "hb");
    let (head, tail) = frame.split_at(2); // deliver length prefix in two reads
    let mut reader = FrameReader::new(ChunkReader::new(vec![head.to_vec(), tail.to_vec()]));
    // first read has only 2 bytes -> not enough for a frame
    assert!(reader.next_frame().unwrap().is_none());
    assert!(reader.next_frame().unwrap().is_some());
}

/// A `Read` that yields one preset byte chunk per call, then EOF.
struct ChunkReader {
    chunks: std::collections::VecDeque<Vec<u8>>,
}

impl ChunkReader {
    fn new(chunks: Vec<Vec<u8>>) -> Self {
        Self { chunks: chunks.into() }
    }
}

impl Read for ChunkReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self.chunks.pop_front() {
            Some(chunk) => {
                let n = chunk.len();
                buf[..n].copy_from_slice(&chunk);
                Ok(n)
            }
            None => Ok(0),
        }
    }
}
