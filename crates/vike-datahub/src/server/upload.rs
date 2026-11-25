//! A user's CSV UPLOAD on its connection (decision 0272): the door's answer, the chunks read off the
//! socket, and the end's answer, then back to the positional loop. The door, the session and the store
//! are `crate::import::upload`'s; this file is only the frames.
//!
//! The session reads its own frames under the connection's idle read timeout, so a client that stops
//! sending is closed like any idle one. Whatever ends a session other than a complete `CsvUploadEnd`
//! (another verb, a frame that does not decode, a chunk past the declared size, a read fault) drops the
//! text held in memory, and nothing is stored.

use std::io;
use std::net::{SocketAddr, TcpStream};

use vike_datahub_client::proto::{Request, Response, request_kind};
use vike_datahub_client::upload::CsvUploadSpec;
use vike_hub_protocol::access::ACCESS_DENIED;
use vike_hub_protocol::frame::{BodyCodec, decode_body, read_frame_raw, write_reply_as};

use vike_datahub_client::refusals::DatahubRefusal;

use crate::import::ImportLane;
use crate::import::upload::begin;

/// Run one upload. `false` when the connection must close (a transport fault or an idle peer), `true`
/// when the positional loop goes on.
///
/// `still_admitted` is the connection's per-write re-check (decision 0288 item 4), asked before each chunk
/// and before the end with the verb's name: a device revoked mid-upload is answered `access denied`, the
/// upload is dropped and nothing is stored.
///
/// `codec` is the form the `CsvUpload` came in, which its answer is written in; each chunk's and the end's answer
/// is written in that frame's own form, the connection loop's rule. `reads_frame` is what the connection said it
/// reads: a refusal frame goes to a client that does not as the `Error` it always read (`Response::older_form`).
pub(super) fn serve(
    stream: &mut TcpStream,
    lane: Option<&ImportLane>,
    spec: CsvUploadSpec,
    peer: Option<SocketAddr>,
    still_admitted: &dyn Fn(&str) -> bool,
    mut codec: BodyCodec,
    reads_frame: bool,
) -> bool {
    // `answer` in the form this client reads.
    let send = |stream: &mut TcpStream, codec: BodyCodec, response: &Response| {
        let older = response.older_form(reads_frame);
        answer(stream, codec, older.as_ref().unwrap_or(response), peer)
    };
    let refused = |refusal: DatahubRefusal| Response::Refused(refusal.into());
    let mut session = match begin(lane, spec) {
        Ok(session) => session,
        Err(refusal) => return send(stream, codec, &refused(refusal)),
    };
    if !send(stream, codec, &Response::CsvUploadReady) {
        return false;
    }
    loop {
        let body = match read_frame_raw(stream) {
            Ok(body) => body,
            Err(e) => {
                let idle = matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut);
                tracing::info!(
                    ?peer,
                    idle,
                    error = %e,
                    "vike-datahub: CsvUpload broken off — the file held in memory is dropped and \
                     nothing was stored; closing the connection"
                );
                return false;
            }
        };
        codec = BodyCodec::of(&body);
        let request = decode_body::<Request>(&body);
        if let Ok(write @ (Request::CsvUploadChunk { .. } | Request::CsvUploadEnd)) = &request
            && !still_admitted(request_kind(write))
        {
            return send(stream, codec, &Response::Error(ACCESS_DENIED.to_string()));
        }
        let reply = match request {
            Ok(Request::CsvUploadChunk { text }) => match session.push(&text) {
                Ok(bytes) => Response::CsvUploadReceived { bytes },
                Err(refusal) => return send(stream, codec, &refused(refusal)),
            },
            Ok(Request::CsvUploadEnd) => {
                let done = session.end(vike_model::now_ms());
                return send(stream, codec, &done);
            }
            // Any other frame ends the upload unserved: the session owned this socket until its end.
            _ => return send(stream, codec, &refused(DatahubRefusal::UploadBrokenOff)),
        };
        if !send(stream, codec, &reply) {
            return false;
        }
    }
}

/// Write `response` in `codec`'s form; `false` (logged) when the write failed and the connection must close.
fn answer(
    stream: &mut TcpStream,
    codec: BodyCodec,
    response: &Response,
    peer: Option<SocketAddr>,
) -> bool {
    match write_reply_as(stream, codec, response) {
        Ok(()) => true,
        Err(e) => {
            tracing::warn!(?peer, error = %e, "vike-datahub: write fault, closing connection");
            false
        }
    }
}
