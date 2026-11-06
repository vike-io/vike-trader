//! The tube's test double more than one crate's suites share, compiled ONCE here behind `test-support`
//! (only DEV edges turn it on): the datahub's strategy-lane suite (`crates/vike-datahub/tests/default_build/strat_lane.rs`)
//! and the tradehub's any-symbol twin (`crates/vike-tradehub/tests/hop/any_symbol_twin.rs`) each carried a copy
//! until 2026-10-31.

use std::net::TcpStream;
use std::time::Duration;

use crate::opener::{DataStream, Opener, StreamLimits, StreamTimings};

/// An opener that dials a fake pump at the address it holds directly (no handshake: the pump is a fake
/// peer), with short reconnect waits and the production frame ceilings.
pub struct DirectOpener(pub String);

impl Opener for DirectOpener {
    fn addr(&self) -> &str {
        &self.0
    }
    fn limits(&self) -> StreamLimits {
        StreamLimits {
            up_frame_ceiling_bytes: 64 * 1024,
            down_frame_ceiling_bytes: 8 * 1024 * 1024,
            max_symbol_bytes: 1024,
        }
    }
    fn timings(&self) -> StreamTimings {
        StreamTimings {
            sub_reply: Duration::from_secs(5),
            stream_read_floor: Duration::from_secs(45),
            reconnect_base: Duration::from_millis(50),
            reconnect_max: Duration::from_millis(200),
        }
    }
    fn failure_text(&self, e: &dyn std::fmt::Display) -> String {
        format!("fake pump {}: {e}", self.0)
    }
    fn open(&self, _venue: &str, observe: &dyn Fn(&TcpStream)) -> std::io::Result<DataStream> {
        let stream = TcpStream::connect(&self.0)?;
        observe(&stream);
        Ok(DataStream {
            stream,
            session: 1,
            epoch: 1,
            heartbeat: Duration::from_secs(5),
            delivery_class: true,
        })
    }
}
