//! `DatahubClient`'s execution-stream verbs: `open_execution_stream`, the wire's third mode switch
//! (`Request::ExecutionStreamOpen`), which CONSUMES the client and hands back the raw socket, and the status read
//! `execution_streams`.
//!
//! What follows the open reply is `crate::execution_stream`'s frames, read and written by the tradehub's
//! publisher. The open is here because the socket and the handshake state are the client's private fields. The
//! dial is the caller's: the publisher connects under its datahub trade key, and a key-less loopback datahub
//! serves it unauthenticated, as it serves every verb but the delete.

use super::*;
use crate::execution_stream::{
    ExecutionAccountPositionsWire, ExecutionStreamStatusWire, validate_journal_id,
};
use crate::proto::FEATURE_EXECUTION_STREAM;

impl DatahubClient {
    /// Refuse, sending nothing, a datahub that does not advertise [`FEATURE_EXECUTION_STREAM`].
    fn check_execution_stream(&self) -> Result<(), HubCallError> {
        if self.features().iter().any(|f| f == FEATURE_EXECUTION_STREAM) {
            return Ok(());
        }
        Err(HubCallError::NotOffered {
            feature: FEATURE_EXECUTION_STREAM.to_string(),
            text: format!(
                "datahub does not advertise `{FEATURE_EXECUTION_STREAM}` (an older build, or a store that refuses \
                 writes): it records no live executions. Nothing was sent."
            ),
        })
    }

    /// **Open the execution stream of one journal on this connection**, CONSUMING the client: on success the
    /// socket speaks `crate::execution_stream`'s frames and no positional verb may ever be sent on it again (see
    /// [`crate::proto::Request::ExecutionStreamOpen`]), so the raw [`TcpStream`] is handed back with its WRITE
    /// bound armed (a datahub that stops reading must not hold the publisher's thread) and its read bound CLEARED
    /// for the publisher to arm.
    ///
    /// Refused LOCALLY, sending nothing: a datahub without [`FEATURE_EXECUTION_STREAM`]
    /// ([`HubCallError::NotOffered`]) and a malformed `journal_id` ([`HubCallError::Protocol`]). A daemon-side
    /// refusal ([`Response::Error`]: no writable store, a malformed id) is [`HubCallError::Refused`] carrying the
    /// daemon's text unchanged. `reply` bounds the open reply (one cursor read).
    pub fn open_execution_stream(
        mut self,
        journal_id: &str,
        tradehub: &str,
        oldest_sequence: u64,
        head_sequence: u64,
        reply: Duration,
        write_timeout: Duration,
    ) -> Result<(ExecutionStreamOpenedInfo, TcpStream), HubCallError> {
        self.check_execution_stream()?;
        validate_journal_id(journal_id)
            .map_err(|why| HubCallError::Protocol(format!("{why}. Nothing was sent.").into()))?;
        self.set_reply_timeout(Some(reply));
        let request = Request::ExecutionStreamOpen {
            journal_id: journal_id.to_string(),
            tradehub: tradehub.to_string(),
            oldest_sequence,
            head_sequence,
        };
        match self.exchange(&request)? {
            Response::ExecutionStreamOpened {
                stored_through,
                stored_through_recorded_ms,
                heartbeat_ms,
            } => {
                // The datahub has opened the stream: a socket option refused now is not "not sent".
                self.stream().set_read_timeout(None).map_err(HubCallError::OutcomeUnknown)?;
                self.stream()
                    .set_write_timeout(Some(write_timeout))
                    .map_err(HubCallError::OutcomeUnknown)?;
                Ok((
                    ExecutionStreamOpenedInfo {
                        stored_through,
                        stored_through_recorded_ms,
                        heartbeat_ms,
                        codec: self.conn.body_codec(),
                    },
                    self.conn.into_stream(),
                ))
            }
            Response::Error(text) => Err(HubCallError::Refused { id: None, text }),
            Response::Refused(refusal) => {
                Err(HubCallError::Refused { id: Some(refusal.id), text: refusal.text })
            }
            other => Err(HubCallError::Protocol(
                format!("datahub: expected ExecutionStreamOpened, got {}", response_kind(&other))
                    .into(),
            )),
        }
    }

    /// **Every journal's execution stream this datahub keeps** ([`crate::proto::Request::ExecutionStreams`]):
    /// its cursor, whether a tradehub streams it now, and the windows it knows it lacks. Refused LOCALLY, nothing
    /// sent, by a datahub that does not advertise [`FEATURE_EXECUTION_STREAM`]; a daemon-side refusal is its text.
    pub fn execution_streams(&mut self) -> Result<Vec<ExecutionStreamStatusWire>, String> {
        self.check_execution_stream().map_err(|e| e.to_string())?;
        match self.exchange_shown(&Request::ExecutionStreams)? {
            Response::ExecutionStreams(streams) => Ok(streams),
            Response::Error(msg) => Err(msg),
            other => Err(format!(
                "protocol desync: expected ExecutionStreams, got {}",
                response_kind(&other)
            )),
        }
    }

    /// **Every account's latest position snapshot at or before `at_or_before_ms`**
    /// ([`crate::proto::Request::ExecutionAccountPositions`]): one per account, what its window opens from. Refused
    /// LOCALLY, nothing sent, by a datahub that does not advertise [`FEATURE_EXECUTION_STREAM`]; a daemon-side
    /// refusal is its text.
    pub fn execution_account_positions(
        &mut self,
        at_or_before_ms: i64,
    ) -> Result<Vec<ExecutionAccountPositionsWire>, String> {
        self.check_execution_stream().map_err(|e| e.to_string())?;
        let request = Request::ExecutionAccountPositions { at_or_before_ms };
        match self.exchange_shown(&request)? {
            Response::ExecutionAccountPositions(rows) => Ok(rows),
            Response::Error(msg) => Err(msg),
            other => Err(format!(
                "protocol desync: expected ExecutionAccountPositions, got {}",
                response_kind(&other)
            )),
        }
    }
}
