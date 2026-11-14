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

use vike_datahub_client::proto::{Request, Response, read_frame_raw, write_frame};
use vike_datahub_client::upload::CsvUploadSpec;

use crate::import::ImportLane;
use crate::import::upload::begin;

/// The answer to a chunk or an end on a connection with no upload open.
pub(super) const NO_UPLOAD_OPEN: &str = "no CSV upload is open on this connection: it was refused, \
     or it ended, or another request broke it off. Nothing of it was stored; open it again with \
     CsvUpload.";

/// Run one upload. `false` when the connection must close (a transport fault or an idle peer), `true`
/// when the positional loop goes on.
pub(super) fn serve(
    stream: &mut TcpStream,
    lane: Option<&ImportLane>,
    spec: CsvUploadSpec,
    peer: Option<SocketAddr>,
) -> bool {
    let mut session = match begin(lane, spec) {
        Ok(session) => session,
        Err(why) => return answer(stream, &Response::Error(why), peer),
    };
    if !answer(stream, &Response::CsvUploadReady, peer) {
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
        let reply = match serde_json::from_slice::<Request>(&body) {
            Ok(Request::CsvUploadChunk { text }) => match session.push(&text) {
                Ok(bytes) => Response::CsvUploadReceived { bytes },
                Err(why) => return answer(stream, &Response::Error(why), peer),
            },
            Ok(Request::CsvUploadEnd) => {
                let done = session.end(vike_model::now_ms());
                return answer(stream, &done, peer);
            }
            // Any other frame ends the upload unserved: the session owned this socket until its end.
            _ => {
                let why = "CsvUpload: a request other than the upload's next chunk or its end \
                           arrived, so the upload was dropped and nothing was stored, and that \
                           request was not served. Send the upload again, then the request.";
                return answer(stream, &Response::Error(why.to_string()), peer);
            }
        };
        if !answer(stream, &reply, peer) {
            return false;
        }
    }
}

/// Write `response`; `false` (logged) when the write failed and the connection must close.
fn answer(stream: &mut TcpStream, response: &Response, peer: Option<SocketAddr>) -> bool {
    match write_frame(stream, response) {
        Ok(()) => true,
        Err(e) => {
            tracing::warn!(?peer, error = %e, "vike-datahub: write fault, closing connection");
            false
        }
    }
}
